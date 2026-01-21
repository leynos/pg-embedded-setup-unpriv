//! RAII wrapper that boots an embedded `PostgreSQL` instance for tests.
//!
//! The cluster starts during [`TestCluster::new`] and shuts down automatically when the
//! value drops out of scope.
//!
//! # Synchronous API
//!
//! Use [`TestCluster::new`] from synchronous contexts or when you want the cluster to
//! own its own Tokio runtime:
//!
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
//!
//! # Async API
//!
//! When running within an existing async runtime (e.g., `#[tokio::test]`), use
//! [`TestCluster::start_async`] to avoid the "Cannot start a runtime from within a
//! runtime" panic that occurs when nesting Tokio runtimes:
//!
//! ```ignore
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! #[tokio::test]
//! async fn test_with_embedded_postgres() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//!     let cluster = TestCluster::start_async().await?;
//!     let url = cluster.settings().url("my_database");
//!     // ... async database operations ...
//!     cluster.stop_async().await?;
//!     Ok(())
//! }
//! ```
//!
//! The async API requires the `async-api` feature flag:
//!
//! ```toml
//! [dependencies]
//! pg-embedded-setup-unpriv = { version = "...", features = ["async-api"] }
//! ```

mod cache_integration;
mod connection;
mod delegation;
mod installation;
mod lifecycle;
mod runtime;
mod shutdown;
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
use crate::cache::BinaryCacheConfig;
use crate::env::ScopedEnv;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use postgresql_embedded::{PostgreSQL, Settings};
use tokio::runtime::Runtime;
use tracing::{info, info_span};

/// Encodes the runtime mode for a `TestCluster`.
///
/// This enum eliminates the need for separate `runtime: Option<Runtime>` and
/// `is_async_mode: bool` fields, preventing invalid states where the two could
/// disagree.
#[derive(Debug)]
enum ClusterRuntime {
    /// Synchronous mode: the cluster owns its own Tokio runtime.
    Sync(Runtime),
    /// Async mode: the cluster runs on the caller's runtime.
    #[cfg_attr(
        not(feature = "async-api"),
        expect(dead_code, reason = "used when async-api feature is enabled")
    )]
    Async,
}

impl ClusterRuntime {
    /// Returns `true` if this is async mode.
    const fn is_async(&self) -> bool {
        matches!(self, Self::Async)
    }
}

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    /// Runtime mode: either owns a runtime (sync) or runs on caller's runtime (async).
    runtime: ClusterRuntime,
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
        // Resolve cache directory BEFORE applying test environment.
        // Otherwise, the test sandbox's XDG_CACHE_HOME would be used.
        let (runtime, env_vars, env_guard, outcome) = {
            let _entered = span.enter();
            let initial_bootstrap = bootstrap_for_tests()?;
            let cache_config = Self::cache_config_from_bootstrap(&initial_bootstrap);
            let runtime = build_runtime()?;
            let env_vars = initial_bootstrap.environment.to_env();
            let env_guard = ScopedEnv::apply(&env_vars);
            let outcome =
                Self::start_postgres(&runtime, initial_bootstrap, &env_vars, &cache_config)?;
            (runtime, env_vars, env_guard, outcome)
        };

        Ok(Self {
            runtime: ClusterRuntime::Sync(runtime),
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        })
    }

    /// Creates a `BinaryCacheConfig` from bootstrap settings.
    ///
    /// Uses the explicitly configured `binary_cache_dir` if set, otherwise
    /// falls back to the default resolution from environment variables.
    fn cache_config_from_bootstrap(bootstrap: &TestBootstrapSettings) -> BinaryCacheConfig {
        bootstrap
            .binary_cache_dir
            .as_ref()
            .map_or_else(BinaryCacheConfig::new, |dir| {
                BinaryCacheConfig::with_dir(dir.clone())
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
        cache_config: &BinaryCacheConfig,
    ) -> BootstrapResult<StartupOutcome> {
        let privileges = bootstrap.privileges;
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            mode = ?bootstrap.execution_mode,
            "starting embedded postgres lifecycle"
        );

        // Try to use cached binaries before starting the lifecycle
        let version_req = bootstrap.settings.version.clone();
        let cache_hit =
            cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

        let (is_managed_via_worker, postgres) = if privileges == ExecutionPrivileges::Root {
            Self::invoke_lifecycle_root(runtime, &mut bootstrap, env_vars)?;
            (true, None)
        } else {
            let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
            Self::invoke_lifecycle(runtime, &mut bootstrap, env_vars, &mut embedded)?;
            (
                false,
                Self::prepare_postgres_handle(false, &mut bootstrap, embedded),
            )
        };

        // Populate cache after successful setup if it was a cache miss
        if !cache_hit {
            cache_integration::try_populate_binary_cache(cache_config, &bootstrap.settings);
        }

        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            worker_managed = is_managed_via_worker,
            cache_hit,
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

    fn invoke_lifecycle_root(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<()> {
        let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        setup_invoker.invoke_as_root(worker_operation::WorkerOperation::Setup)?;
        installation::refresh_worker_installation_dir(bootstrap);
        let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        start_invoker.invoke_as_root(worker_operation::WorkerOperation::Start)?;
        installation::refresh_worker_port(bootstrap)
    }

    fn invoke_lifecycle(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
        // Scope ensures the setup invoker releases its borrows before we refresh the settings.
        let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            embedded.setup().await
        })?;
        installation::refresh_worker_installation_dir(bootstrap);
        let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        })?;
        installation::refresh_worker_port(bootstrap)
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
        use tracing::Instrument;

        let span = info_span!(target: LOG_TARGET, "test_cluster", async_mode = true);

        // Sync bootstrap preparation (no await needed).
        // Resolve cache directory BEFORE applying test environment.
        // Otherwise, the test sandbox's XDG_CACHE_HOME would be used.
        let initial_bootstrap = bootstrap_for_tests()?;
        let cache_config = Self::cache_config_from_bootstrap(&initial_bootstrap);
        let env_vars = initial_bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);

        // Async postgres startup, instrumented with the span.
        // Box::pin to avoid large future on the stack.
        let outcome = Box::pin(Self::start_postgres_async(
            initial_bootstrap,
            &env_vars,
            &cache_config,
        ))
        .instrument(span.clone())
        .await?;

        Ok(Self {
            runtime: ClusterRuntime::Async,
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
        cache_config: &BinaryCacheConfig,
    ) -> BootstrapResult<StartupOutcome> {
        let privileges = bootstrap.privileges;
        Self::log_lifecycle_start(privileges, &bootstrap);

        // Try to use cached binaries before starting the lifecycle
        let version_req = bootstrap.settings.version.clone();
        let cache_hit =
            cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

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

        // Populate cache after successful setup if it was a cache miss
        if !cache_hit {
            cache_integration::try_populate_binary_cache(cache_config, &bootstrap.settings);
        }

        Self::log_lifecycle_complete(privileges, is_managed_via_worker, cache_hit);
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
    fn log_lifecycle_complete(
        privileges: ExecutionPrivileges,
        is_managed_via_worker: bool,
        cache_hit: bool,
    ) {
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            worker_managed = is_managed_via_worker,
            cache_hit = cache_hit,
            async_mode = true,
            "embedded postgres started"
        );
    }

    /// Async variant of `invoke_lifecycle`.
    #[cfg(feature = "async-api")]
    async fn invoke_lifecycle_async(
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
        let invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                embedded.setup().await
            }),
        )
        .await?;
        installation::refresh_worker_installation_dir(bootstrap);
        let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
                embedded.start().await
            }),
        )
        .await?;
        installation::refresh_worker_port_async(bootstrap).await
    }

    #[cfg(feature = "async-api")]
    async fn invoke_lifecycle_root_async(
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<()> {
        let setup_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                Ok::<(), postgresql_embedded::Error>(())
            }),
        )
        .await?;
        installation::refresh_worker_installation_dir(bootstrap);
        let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
                Ok::<(), postgresql_embedded::Error>(())
            }),
        )
        .await?;
        installation::refresh_worker_port_async(bootstrap).await
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
        let context = shutdown::stop_context(&self.bootstrap.settings);
        shutdown::log_async_stop(&context, self.is_managed_via_worker);

        if self.is_managed_via_worker {
            shutdown::stop_worker_managed_async(&self.bootstrap, &self.env_vars, &context).await
        } else if let Some(postgres) = self.postgres.take() {
            shutdown::stop_in_process_async(postgres, self.bootstrap.shutdown_timeout, &context)
                .await
        } else {
            Ok(())
        }
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
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        let context = shutdown::stop_context(&self.bootstrap.settings);
        let is_async = self.runtime.is_async();
        info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = self.is_managed_via_worker,
            async_mode = is_async,
            "stopping embedded postgres cluster"
        );

        if is_async {
            // Async clusters should use stop_async() explicitly; attempt best-effort cleanup.
            shutdown::drop_async_cluster(
                self.is_managed_via_worker,
                &mut self.postgres,
                &self.bootstrap,
                &self.env_vars,
                &context,
            );
        } else {
            self.drop_sync_cluster(&context);
        }
        // Environment guards drop after this block, restoring the process state.
    }
}

impl TestCluster {
    /// Synchronous drop path: stops the cluster using the owned runtime.
    fn drop_sync_cluster(&mut self, context: &str) {
        let ClusterRuntime::Sync(ref runtime) = self.runtime else {
            // Should never happen: drop_sync_cluster is only called for sync mode.
            return;
        };

        shutdown::drop_sync_cluster(
            runtime,
            self.is_managed_via_worker,
            &mut self.postgres,
            &self.bootstrap,
            &self.env_vars,
            context,
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
            runtime: ClusterRuntime::Sync(runtime),
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

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod drop_logging_tests {
    use crate::test_support::capture_warn_logs;

    use super::shutdown;

    #[test]
    fn warn_stop_timeout_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| shutdown::warn_stop_timeout(5, "ctx"));
        assert!(
            logs.iter()
                .any(|line| line.contains("stop() timed out after 5s (ctx)")),
            "expected timeout warning, got {logs:?}"
        );
    }

    #[test]
    fn warn_stop_failure_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| shutdown::warn_stop_failure("ctx", &"boom"));
        assert!(
            logs.iter()
                .any(|line| line.contains("failed to stop embedded postgres instance")),
            "expected failure warning, got {logs:?}"
        );
    }
}

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../../tests/test_cluster.rs"]
mod test_cluster_tests;
