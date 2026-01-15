//! RAII wrapper that boots an embedded `PostgreSQL` instance for tests.
//!
//! The cluster starts during [`TestCluster::new`] and shuts down automatically when the
//! value drops out of scope.
//!
//! # Examples
//! ```no_run
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//! let cluster = TestCluster::new()?;
//! let url = cluster.settings().url("my_database");
//! // Perform test database work here.
//! drop(cluster); // `PostgreSQL` stops automatically.
//! # Ok(())
//! # }
//! ```

mod connection;
mod delegation;
mod lifecycle;
mod runtime;
mod temporary_database;
mod worker_invoker;
mod worker_operation;

pub use self::connection::{ConnectionMetadata, TestClusterConnection};
pub use self::lifecycle::DatabaseName;
pub use self::temporary_database::TemporaryDatabase;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use self::worker_invoker::WorkerInvoker;
#[doc(hidden)]
pub use self::worker_operation::WorkerOperation;

use self::runtime::build_runtime;
#[cfg(feature = "async-api")]
use self::worker_invoker::AsyncInvoker;
use self::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use postgresql_embedded::{PostgreSQL, Settings};
use std::fmt::Display;
use tokio::runtime::Runtime;
use tokio::time;
use tracing::{info, info_span};

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    /// Owned runtime for synchronous API; `None` when using async API.
    runtime: Option<Runtime>,
    /// `true` when created via `start_async()`, indicating async cleanup is expected.
    is_async_mode: bool,
    postgres: Option<PostgreSQL>,
    bootstrap: TestBootstrapSettings,
    is_managed_via_worker: bool,
    env_vars: Vec<(String, Option<String>)>,
    worker_guard: Option<ScopedEnv>,
    _env_guard: ScopedEnv,
    // Keeps the cluster span alive for the lifetime of the guard.
    _cluster_span: tracing::Span,
}

struct StartupOutcome {
    bootstrap: TestBootstrapSettings,
    postgres: Option<PostgreSQL>,
    is_managed_via_worker: bool,
}

impl TestCluster {
    /// Boots a `PostgreSQL` instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    ///
    /// # Errors
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting the
    /// embedded cluster fails.
    pub fn new() -> BootstrapResult<Self> {
        let span = info_span!(target: LOG_TARGET, "test_cluster");
        let (runtime, env_vars, env_guard, outcome) = {
            let _entered = span.enter();
            let initial_bootstrap = bootstrap_for_tests()?;
            let runtime = build_runtime()?;
            let env_vars = initial_bootstrap.environment.to_env();
            let env_guard = ScopedEnv::apply(&env_vars);
            let outcome = Self::start_postgres(&runtime, initial_bootstrap, &env_vars)?;
            (runtime, env_vars, env_guard, outcome)
        };

        Ok(Self {
            runtime: Some(runtime),
            is_async_mode: false,
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        })
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "privilege-aware lifecycle setup requires explicit branching for observability"
    )]
    fn start_postgres(
        runtime: &Runtime,
        mut bootstrap: TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<StartupOutcome> {
        let privileges = bootstrap.privileges;
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            mode = ?bootstrap.execution_mode,
            "starting embedded postgres lifecycle"
        );

        let invoker = ClusterWorkerInvoker::new(runtime, &bootstrap, env_vars);
        Self::invoke_lifecycle(&invoker, &mut embedded)?;

        let is_managed_via_worker = matches!(privileges, ExecutionPrivileges::Root);
        let postgres =
            Self::prepare_postgres_handle(is_managed_via_worker, &mut bootstrap, embedded);

        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            worker_managed = is_managed_via_worker,
            "embedded postgres started"
        );
        Ok(StartupOutcome {
            bootstrap,
            postgres,
            is_managed_via_worker,
        })
    }

    fn prepare_postgres_handle(
        is_managed_via_worker: bool,
        bootstrap: &mut TestBootstrapSettings,
        embedded: PostgreSQL,
    ) -> Option<PostgreSQL> {
        if is_managed_via_worker {
            None
        } else {
            bootstrap.settings = embedded.settings().clone();
            Some(embedded)
        }
    }

    fn invoke_lifecycle(
        invoker: &ClusterWorkerInvoker<'_>,
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
        invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            embedded.setup().await
        })?;
        invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        })
    }

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
    #[cfg(feature = "async-api")]
    pub async fn start_async() -> BootstrapResult<Self> {
        let span = info_span!(target: LOG_TARGET, "test_cluster", async_mode = true);
        let (env_vars, env_guard, outcome) = {
            let _entered = span.enter();
            let initial_bootstrap = bootstrap_for_tests()?;
            let env_vars = initial_bootstrap.environment.to_env();
            let env_guard = ScopedEnv::apply(&env_vars);
            // Box::pin to avoid large future on the stack.
            let outcome =
                Box::pin(Self::start_postgres_async(initial_bootstrap, &env_vars)).await?;
            (env_vars, env_guard, outcome)
        };

        Ok(Self {
            runtime: None,
            is_async_mode: true,
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
    #[cfg(feature = "async-api")]
    async fn start_postgres_async(
        mut bootstrap: TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<StartupOutcome> {
        let privileges = bootstrap.privileges;
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        Self::log_lifecycle_start(privileges, &bootstrap);

        let invoker = AsyncInvoker::new(&bootstrap, env_vars);
        Box::pin(Self::invoke_lifecycle_async(&invoker, &mut embedded)).await?;

        let is_managed_via_worker = matches!(privileges, ExecutionPrivileges::Root);
        let postgres =
            Self::prepare_postgres_handle(is_managed_via_worker, &mut bootstrap, embedded);

        Self::log_lifecycle_complete(privileges, is_managed_via_worker);
        Ok(StartupOutcome {
            bootstrap,
            postgres,
            is_managed_via_worker,
        })
    }

    #[cfg(feature = "async-api")]
    fn log_lifecycle_start(privileges: ExecutionPrivileges, bootstrap: &TestBootstrapSettings) {
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            mode = ?bootstrap.execution_mode,
            async_mode = true,
            "starting embedded postgres lifecycle"
        );
    }

    #[cfg(feature = "async-api")]
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
    #[cfg(feature = "async-api")]
    async fn invoke_lifecycle_async(
        invoker: &AsyncInvoker<'_>,
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
        Box::pin(
            invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                embedded.setup().await
            }),
        )
        .await?;
        Box::pin(
            invoker.invoke(worker_operation::WorkerOperation::Start, async {
                embedded.start().await
            }),
        )
        .await
    }

    /// Extends the cluster lifetime to cover additional scoped environment guards.
    ///
    /// Primarily used by fixtures that need to ensure `PG_EMBEDDED_WORKER` remains set for the
    /// duration of the cluster lifetime.
    #[doc(hidden)]
    #[must_use]
    pub fn with_worker_guard(mut self, worker_guard: Option<ScopedEnv>) -> Self {
        self.worker_guard = worker_guard;
        self
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
    #[cfg(feature = "async-api")]
    pub async fn stop_async(mut self) -> BootstrapResult<()> {
        let context = Self::stop_context(&self.bootstrap.settings);
        Self::log_async_stop(&context, self.is_managed_via_worker);

        if self.is_managed_via_worker {
            Self::stop_worker_managed_async(&self.bootstrap, &self.env_vars, &context).await
        } else if let Some(postgres) = self.postgres.take() {
            Self::stop_in_process_async(postgres, self.bootstrap.shutdown_timeout, &context).await
        } else {
            Ok(())
        }
    }

    #[cfg(feature = "async-api")]
    fn log_async_stop(context: &str, is_managed_via_worker: bool) {
        info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = is_managed_via_worker,
            async_mode = true,
            "stopping embedded postgres cluster"
        );
    }

    #[cfg(feature = "async-api")]
    async fn stop_worker_managed_async(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        context: &str,
    ) -> BootstrapResult<()> {
        let owned_bootstrap = bootstrap.clone();
        let owned_env_vars = env_vars.to_vec();
        let owned_context = context.to_owned();
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

    #[cfg(feature = "async-api")]
    async fn stop_in_process_async(
        postgres: PostgreSQL,
        timeout: std::time::Duration,
        context: &str,
    ) -> BootstrapResult<()> {
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
    #[cfg(feature = "async-api")]
    fn stop_via_worker_sync(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        context: &str,
    ) -> BootstrapResult<()> {
        let runtime = build_runtime()?;
        let invoker = ClusterWorkerInvoker::new(&runtime, bootstrap, env_vars);
        invoker
            .invoke_as_root(worker_operation::WorkerOperation::Stop)
            .inspect_err(|err| Self::warn_stop_failure(context, err))
    }

    /// Returns the prepared `PostgreSQL` settings for the running cluster.
    pub const fn settings(&self) -> &Settings {
        &self.bootstrap.settings
    }

    /// Returns the environment required for clients to interact with the cluster.
    pub const fn environment(&self) -> &TestBootstrapEnvironment {
        &self.bootstrap.environment
    }

    /// Returns the bootstrap metadata captured when the cluster was started.
    pub const fn bootstrap(&self) -> &TestBootstrapSettings {
        &self.bootstrap
    }

    /// Returns helper methods for constructing connection artefacts.
    ///
    /// # Examples
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// let metadata = cluster.connection().metadata();
    /// println!(
    ///     "postgresql://{}:***@{}:{}/postgres",
    ///     metadata.superuser(),
    ///     metadata.host(),
    ///     metadata.port(),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn connection(&self) -> TestClusterConnection {
        TestClusterConnection::new(&self.bootstrap)
    }

    fn stop_context(settings: &Settings) -> String {
        let data_dir = settings.data_dir.display();
        let version = settings.version.to_string();
        format!("version {version}, data_dir {data_dir}")
    }

    /// Best-effort cleanup for async clusters dropped without `stop_async()`.
    ///
    /// Attempts to spawn cleanup on the current runtime handle if available.
    fn drop_async_cluster(&mut self, context: &str) {
        let Some(postgres) = self.postgres.take() else {
            return; // Already cleaned up via stop_async() or nothing to clean.
        };

        Self::warn_async_drop_without_stop(context);

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => spawn_async_cleanup(&handle, postgres, self.bootstrap.shutdown_timeout),
            Err(_) => Self::error_no_runtime_for_cleanup(context),
        }
    }

    fn warn_async_drop_without_stop(context: &str) {
        tracing::warn!(
            target: LOG_TARGET,
            context = %context,
            concat!(
                "async TestCluster dropped without calling stop_async(); ",
                "attempting best-effort cleanup"
            )
        );
    }

    fn error_no_runtime_for_cleanup(context: &str) {
        tracing::error!(
            target: LOG_TARGET,
            context = %context,
            "no async runtime available for cleanup; resources may leak"
        );
    }

    fn warn_stop_failure(context: &str, err: &impl Display) {
        tracing::warn!(
            "SKIP-TEST-CLUSTER: failed to stop embedded postgres instance ({}): {}",
            context,
            err
        );
    }

    fn warn_stop_timeout(timeout_secs: u64, context: &str) {
        tracing::warn!(
            "SKIP-TEST-CLUSTER: stop() timed out after {timeout_secs}s ({context}); proceeding with drop"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::ExecutionPrivileges;
    use crate::test_support::{dummy_settings, scoped_env};

    #[test]
    fn with_worker_guard_restores_environment() {
        const KEY: &str = "PG_EMBEDDED_WORKER_GUARD_TEST";
        let baseline = std::env::var(KEY).ok();
        let guard = scoped_env(vec![(OsString::from(KEY), Some(OsString::from("guarded")))]);
        let cluster = dummy_cluster().with_worker_guard(Some(guard));
        assert_eq!(
            std::env::var(KEY).as_deref(),
            Ok("guarded"),
            "worker guard should remain active whilst the cluster runs",
        );
        drop(cluster);
        match baseline {
            Some(value) => assert_eq!(
                std::env::var(KEY).as_deref(),
                Ok(value.as_str()),
                "worker guard should restore the previous value"
            ),
            None => assert!(
                std::env::var(KEY).is_err(),
                "worker guard should unset the variable once the cluster drops"
            ),
        }
    }

    fn dummy_cluster() -> TestCluster {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let span = info_span!(target: LOG_TARGET, "test_cluster");
        let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        let env_vars = bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);
        TestCluster {
            runtime: Some(runtime),
            is_async_mode: false,
            postgres: None,
            bootstrap,
            is_managed_via_worker: false,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        }
    }
}

/// Spawns async cleanup of a `PostgreSQL` instance on the provided runtime handle.
///
/// The task is fire-and-forget; errors during shutdown are silently ignored.
fn spawn_async_cleanup(
    handle: &tokio::runtime::Handle,
    postgres: PostgreSQL,
    timeout: std::time::Duration,
) {
    drop(handle.spawn(async move {
        drop(time::timeout(timeout, postgres.stop()).await);
    }));
}

impl Drop for TestCluster {
    #[expect(
        clippy::cognitive_complexity,
        reason = "drop path must branch between async/sync and worker/in-process shutdown"
    )]
    fn drop(&mut self) {
        let context = Self::stop_context(&self.bootstrap.settings);
        info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = self.is_managed_via_worker,
            async_mode = self.is_async_mode,
            "stopping embedded postgres cluster"
        );

        // Async clusters should use stop_async() explicitly; attempt best-effort cleanup.
        if self.is_async_mode {
            self.drop_async_cluster(&context);
            return;
        }

        // Sync path: runtime is guaranteed to be Some.
        let Some(ref runtime) = self.runtime else {
            tracing::error!(
                target: LOG_TARGET,
                "sync TestCluster missing runtime in drop; cannot clean up"
            );
            return;
        };

        if self.is_managed_via_worker {
            let invoker = ClusterWorkerInvoker::new(runtime, &self.bootstrap, &self.env_vars);
            if let Err(err) = invoker.invoke_as_root(worker_operation::WorkerOperation::Stop) {
                Self::warn_stop_failure(&context, &err);
            }
        } else if let Some(postgres) = self.postgres.take() {
            let timeout = self.bootstrap.shutdown_timeout;
            let timeout_secs = timeout.as_secs();
            let outcome = runtime.block_on(async { time::timeout(timeout, postgres.stop()).await });

            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(err)) => Self::warn_stop_failure(&context, &err),
                Err(_) => Self::warn_stop_timeout(timeout_secs, &context),
            }
        }
        // Environment guards drop after this block, restoring the process state.
    }
}

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod drop_logging_tests {
    use super::*;
    use crate::test_support::capture_warn_logs;

    #[test]
    fn warn_stop_timeout_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| TestCluster::warn_stop_timeout(5, "ctx"));
        assert!(
            logs.iter()
                .any(|line| line.contains("stop() timed out after 5s (ctx)")),
            "expected timeout warning, got {logs:?}"
        );
    }

    #[test]
    fn warn_stop_failure_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| TestCluster::warn_stop_failure("ctx", &"boom"));
        assert!(
            logs.iter()
                .any(|line| line.contains("failed to stop embedded postgres instance")),
            "expected failure warning, got {logs:?}"
        );
    }
}

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../../tests/test_cluster.rs"]
mod tests;
