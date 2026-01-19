//! Async lifecycle management for `TestCluster`.

use super::{StartupOutcome, TestCluster};
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};
use postgresql_embedded::PostgreSQL;
use tokio::time;
use tracing::{info, info_span};

use super::drop_support::StopContext;
use super::runtime::build_runtime;
use super::worker_invoker::AsyncInvoker;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::worker_operation;
use super::{refresh_worker_installation_dir, refresh_worker_port_async};

impl TestCluster {
    /// Boots a `PostgreSQL` instance asynchronously for use in `#[tokio::test]` contexts.
    ///
    /// Unlike [`TestCluster::new`], this constructor does not create its own Tokio runtime.
    /// Instead, it runs on the caller's async runtime, making it safe to call from within
    /// `#[tokio::test]` and other async contexts.
    ///
    /// **Important:** Clusters created with `start_async()` should be shut down explicitly
    /// using [`stop_async()`](Self::stop_async). The `Drop` implementation will attempt
    /// best-effort cleanup but may not succeed if the runtime is no longer available.
    ///
    /// # Errors
    ///
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting
    /// the embedded cluster fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// #[tokio::test]
    /// async fn test_with_embedded_postgres() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    ///     let cluster = TestCluster::start_async().await?;
    ///     let url = cluster.settings().url("my_database");
    ///     // ... async database operations ...
    ///     cluster.stop_async().await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn start_async() -> crate::error::BootstrapResult<Self> {
        use tracing::Instrument;

        let span = info_span!(target: LOG_TARGET, "test_cluster", async_mode = true);

        // Sync bootstrap preparation (no await needed).
        let initial_bootstrap = bootstrap_for_tests()?;
        let env_vars = initial_bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);

        // Async postgres startup, instrumented with the span.
        // Box::pin to avoid large future on the stack.
        let outcome = Box::pin(Self::start_postgres_async(initial_bootstrap, &env_vars))
            .instrument(span.clone())
            .await?;

        Ok(Self {
            runtime: super::ClusterRuntime::Async,
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        })
    }

    /// Async variant of `start_postgres` that runs on the caller's runtime.
    async fn start_postgres_async(
        mut bootstrap: TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> crate::error::BootstrapResult<StartupOutcome> {
        let privileges = bootstrap.privileges;
        Self::log_lifecycle_start(privileges, &bootstrap);

        let (is_managed_via_worker, postgres) = if privileges == ExecutionPrivileges::Root {
            Box::pin(Self::invoke_lifecycle_root_async(&mut bootstrap, env_vars)).await?;
            (true, None)
        } else {
            let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
            Box::pin(Self::invoke_lifecycle_async(
                &mut bootstrap,
                env_vars,
                &mut embedded,
            ))
            .await?;
            (
                false,
                Self::prepare_postgres_handle(false, &mut bootstrap, embedded),
            )
        };

        Self::log_lifecycle_complete(privileges, is_managed_via_worker);
        Ok(StartupOutcome {
            bootstrap,
            postgres,
            is_managed_via_worker,
        })
    }

    fn log_lifecycle_start(privileges: ExecutionPrivileges, bootstrap: &TestBootstrapSettings) {
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            mode = ?bootstrap.execution_mode,
            async_mode = true,
            "starting embedded postgres lifecycle"
        );
    }

    fn log_lifecycle_complete(privileges: ExecutionPrivileges, is_managed_via_worker: bool) {
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            worker_managed = is_managed_via_worker,
            async_mode = true,
            "embedded postgres started"
        );
    }

    /// Async variant of `invoke_lifecycle`.
    async fn invoke_lifecycle_async(
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> crate::error::BootstrapResult<()> {
        // Scope ensures the setup invoker releases its borrows before we refresh the settings.
        {
            let invoker = AsyncInvoker::new(bootstrap, env_vars);
            Box::pin(
                invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                    embedded.setup().await
                }),
            )
            .await?;
        }
        refresh_worker_installation_dir(bootstrap);
        let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
                embedded.start().await
            }),
        )
        .await?;
        refresh_worker_port_async(bootstrap).await
    }

    async fn invoke_lifecycle_root_async(
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> crate::error::BootstrapResult<()> {
        let setup_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                Ok::<(), postgresql_embedded::Error>(())
            }),
        )
        .await?;
        refresh_worker_installation_dir(bootstrap);
        let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
                Ok::<(), postgresql_embedded::Error>(())
            }),
        )
        .await?;
        refresh_worker_port_async(bootstrap).await
    }

    /// Explicitly shuts down an async cluster.
    ///
    /// This method should be called for clusters created with [`start_async()`](Self::start_async)
    /// to ensure proper cleanup. It consumes `self` to prevent the `Drop` implementation from
    /// attempting duplicate shutdown.
    ///
    /// For worker-managed clusters (root privileges), the worker subprocess is invoked
    /// synchronously via `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown operation fails. The cluster resources are released
    /// regardless of whether shutdown succeeds.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// #[tokio::test]
    /// async fn test_explicit_shutdown() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    ///     let cluster = TestCluster::start_async().await?;
    ///     // ... use cluster ...
    ///     cluster.stop_async().await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn stop_async(mut self) -> crate::error::BootstrapResult<()> {
        let context = StopContext::new(Self::stop_context(&self.bootstrap.settings));
        Self::log_async_stop(&context, self.is_managed_via_worker);

        if self.is_managed_via_worker {
            Self::stop_worker_managed_async(&self.bootstrap, &self.env_vars, &context).await
        } else if let Some(postgres) = self.postgres.take() {
            Self::stop_in_process_async(postgres, self.bootstrap.shutdown_timeout, &context).await
        } else {
            Ok(())
        }
    }

    fn log_async_stop(context: &StopContext, is_managed_via_worker: bool) {
        info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = is_managed_via_worker,
            async_mode = true,
            "stopping embedded postgres cluster"
        );
    }

    async fn stop_worker_managed_async(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        context: &StopContext,
    ) -> crate::error::BootstrapResult<()> {
        let owned_bootstrap = bootstrap.clone();
        let owned_env_vars = env_vars.to_vec();
        let owned_context = context.clone();
        tokio::task::spawn_blocking(move || {
            Self::stop_via_worker_sync(&owned_bootstrap, &owned_env_vars, &owned_context)
        })
        .await
        .map_err(|err| {
            crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "worker stop task panicked: {err}"
            ))
        })?
    }

    async fn stop_in_process_async(
        postgres: PostgreSQL,
        timeout: std::time::Duration,
        context: &StopContext,
    ) -> crate::error::BootstrapResult<()> {
        match time::timeout(timeout, postgres.stop()).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                Self::warn_stop_failure(context, &err);
                Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                    "failed to stop postgres: {err}"
                )))
            }
            Err(_) => {
                let timeout_secs = timeout.as_secs();
                Self::warn_stop_timeout(timeout_secs, context);
                Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                    "stop timed out after {timeout_secs}s"
                )))
            }
        }
    }

    /// Synchronous worker stop for use with `spawn_blocking`.
    fn stop_via_worker_sync(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        context: &StopContext,
    ) -> crate::error::BootstrapResult<()> {
        let runtime = build_runtime()?;
        let invoker = ClusterWorkerInvoker::new(&runtime, bootstrap, env_vars);
        invoker
            .invoke_as_root(worker_operation::WorkerOperation::Stop)
            .inspect_err(|err| Self::warn_stop_failure(context, err))
    }
}
