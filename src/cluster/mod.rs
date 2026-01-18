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
use color_eyre::eyre::eyre;
use postgresql_embedded::{PostgreSQL, Settings};
use std::fmt::Display;
use std::path::Path;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::time;
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

const POSTMASTER_PORT_ATTEMPTS: usize = 10;
const POSTMASTER_PORT_DELAY: Duration = Duration::from_millis(100);

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
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            mode = ?bootstrap.execution_mode,
            "starting embedded postgres lifecycle"
        );

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

    fn invoke_lifecycle_root(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<()> {
        let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        setup_invoker.invoke_as_root(worker_operation::WorkerOperation::Setup)?;
        Self::refresh_worker_installation_dir(bootstrap);
        let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        start_invoker.invoke_as_root(worker_operation::WorkerOperation::Start)?;
        Self::refresh_worker_port(bootstrap)
    }

    fn invoke_lifecycle(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
        // Scope ensures the setup invoker releases its borrows before we refresh the settings.
        {
            let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
            invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                embedded.setup().await
            })?;
        }
        Self::refresh_worker_installation_dir(bootstrap);
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        })?;
        Self::refresh_worker_port(bootstrap)
    }

    /// Refreshes the installation directory after worker setup for root runs.
    ///
    /// The worker helper may install `PostgreSQL` under a subdirectory, so we
    /// re-resolve the installation directory before starting the server.
    fn refresh_worker_installation_dir(bootstrap: &mut TestBootstrapSettings) {
        if bootstrap.privileges != ExecutionPrivileges::Root {
            return;
        }

        if let Some(installed_dir) = Self::resolve_installed_dir(&bootstrap.settings) {
            bootstrap.settings.installation_dir = installed_dir;
        }
    }

    fn refresh_worker_port(bootstrap: &mut TestBootstrapSettings) -> BootstrapResult<()> {
        if bootstrap.privileges != ExecutionPrivileges::Root {
            return Ok(());
        }

        let pid_path = bootstrap.settings.data_dir.join("postmaster.pid");
        if let Some(port) = Self::read_postmaster_port_with_retry(&pid_path)? {
            bootstrap.settings.port = port;
            return Ok(());
        }

        tracing::debug!(
            target: LOG_TARGET,
            path = %pid_path.display(),
            "postmaster.pid missing after start; keeping configured port"
        );
        Ok(())
    }

    #[cfg(feature = "async-api")]
    async fn refresh_worker_port_async(
        bootstrap: &mut TestBootstrapSettings,
    ) -> BootstrapResult<()> {
        if bootstrap.privileges != ExecutionPrivileges::Root {
            return Ok(());
        }

        let pid_path = bootstrap.settings.data_dir.join("postmaster.pid");
        if let Some(port) = Self::read_postmaster_port_with_retry_async(&pid_path).await? {
            bootstrap.settings.port = port;
            return Ok(());
        }

        tracing::debug!(
            target: LOG_TARGET,
            path = %pid_path.display(),
            "postmaster.pid missing after start; keeping configured port"
        );
        Ok(())
    }

    fn read_postmaster_port_with_retry(pid_path: &Path) -> BootstrapResult<Option<u16>> {
        for _ in 0..POSTMASTER_PORT_ATTEMPTS {
            if let Some(port) = Self::read_postmaster_port(pid_path)? {
                return Ok(Some(port));
            }
            std::thread::sleep(POSTMASTER_PORT_DELAY);
        }
        Ok(None)
    }

    #[cfg(feature = "async-api")]
    async fn read_postmaster_port_with_retry_async(
        pid_path: &Path,
    ) -> BootstrapResult<Option<u16>> {
        for _ in 0..POSTMASTER_PORT_ATTEMPTS {
            if let Some(port) = Self::read_postmaster_port(pid_path)? {
                return Ok(Some(port));
            }
            tokio::time::sleep(POSTMASTER_PORT_DELAY).await;
        }
        Ok(None)
    }

    fn read_postmaster_port(pid_path: &Path) -> BootstrapResult<Option<u16>> {
        let contents = match std::fs::read_to_string(pid_path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(err) => {
                return Err(crate::error::BootstrapError::from(eyre!(
                    "failed to read postmaster pid at {}: {err}",
                    pid_path.display()
                )));
            }
        };
        let port_line = contents.lines().nth(3).ok_or_else(|| {
            crate::error::BootstrapError::from(eyre!(
                "postmaster.pid missing port line at {}",
                pid_path.display()
            ))
        })?;
        let port = port_line.trim().parse::<u16>().map_err(|err| {
            crate::error::BootstrapError::from(eyre!(
                "failed to parse postmaster port from {}: {err}",
                pid_path.display()
            ))
        })?;
        Ok(Some(port))
    }

    fn resolve_installed_dir(settings: &Settings) -> Option<std::path::PathBuf> {
        let install_dir = &settings.installation_dir;

        if install_dir.join("bin").is_dir() {
            return Some(install_dir.clone());
        }

        if settings.trust_installation_dir {
            return Some(install_dir.clone());
        }

        let mut candidates = std::fs::read_dir(install_dir)
            .ok()?
            .filter_map(|dir_entry| {
                let entry = dir_entry.ok()?;
                if !entry.file_type().ok()?.is_dir() {
                    return None;
                }
                let path = entry.path();
                path.join("bin").is_dir().then_some(path)
            })
            .collect::<Vec<_>>();
        candidates.sort();
        candidates.pop()
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
        let initial_bootstrap = bootstrap_for_tests()?;
        let env_vars = initial_bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);

        // Async postgres startup, instrumented with the span.
        // Box::pin to avoid large future on the stack.
        let outcome = Box::pin(Self::start_postgres_async(initial_bootstrap, &env_vars))
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
    ) -> BootstrapResult<StartupOutcome> {
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
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
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
        Self::refresh_worker_installation_dir(bootstrap);
        let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
                embedded.start().await
            }),
        )
        .await?;
        Self::refresh_worker_port_async(bootstrap).await
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
        Self::refresh_worker_installation_dir(bootstrap);
        let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
        Box::pin(
            start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
                Ok::<(), postgresql_embedded::Error>(())
            }),
        )
        .await?;
        Self::refresh_worker_port_async(bootstrap).await
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
    /// For worker-managed clusters, attempts to invoke the worker stop operation.
    fn drop_async_cluster(&mut self, context: &str) {
        Self::warn_async_drop_without_stop(context);

        if self.is_managed_via_worker {
            self.drop_async_worker_managed(context);
        } else if let Some(postgres) = self.postgres.take() {
            self.drop_async_in_process(context, postgres);
        }
        // If neither worker-managed nor has postgres handle, already cleaned up via stop_async().
    }

    /// Best-effort worker stop for async clusters dropped without `stop_async()`.
    fn drop_async_worker_managed(&self, context: &str) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            Self::error_no_runtime_for_cleanup(context);
            return;
        };

        let bootstrap = self.bootstrap.clone();
        let env_vars = self.env_vars.clone();
        let owned_context = context.to_owned();

        drop(handle.spawn(spawn_worker_stop_task(bootstrap, env_vars, owned_context)));
    }

    /// Best-effort in-process stop for async clusters dropped without `stop_async()`.
    fn drop_async_in_process(&self, context: &str, postgres: PostgreSQL) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            Self::error_no_runtime_for_cleanup(context);
            return;
        };

        spawn_async_cleanup(&handle, postgres, self.bootstrap.shutdown_timeout);
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
    use std::fs;

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

    #[test]
    fn refresh_worker_port_reads_postmaster_pid() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let pid_path = temp_dir.path().join("postmaster.pid");
        let contents = format!("12345\n{}\n1700000000\n54321\n", temp_dir.path().display());
        fs::write(&pid_path, contents).expect("write postmaster.pid");

        let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
        bootstrap.settings.data_dir = temp_dir.path().to_path_buf();
        bootstrap.settings.port = 0;

        TestCluster::refresh_worker_port(&mut bootstrap).expect("refresh worker port");
        assert_eq!(bootstrap.settings.port, 54321);
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

/// Spawns async cleanup of a `PostgreSQL` instance on the provided runtime handle.
///
/// The task is fire-and-forget; errors during shutdown are logged at debug level.
fn spawn_async_cleanup(
    handle: &tokio::runtime::Handle,
    postgres: PostgreSQL,
    timeout: std::time::Duration,
) {
    drop(handle.spawn(async move {
        match time::timeout(timeout, postgres.stop()).await {
            Ok(Ok(())) => {
                tracing::debug!(target: LOG_TARGET, "async cleanup completed successfully");
            }
            Ok(Err(err)) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    error = %err,
                    "async cleanup failed during postgres stop"
                );
            }
            Err(_) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    timeout_secs = timeout.as_secs(),
                    "async cleanup timed out"
                );
            }
        }
    }));
}

/// Spawns a blocking task to stop a worker-managed cluster.
///
/// Used by the async drop path to invoke the worker stop operation without
/// blocking the current async context.
#[expect(
    clippy::cognitive_complexity,
    reason = "complexity is from spawn_blocking + error! macro expansion, not logic"
)]
async fn spawn_worker_stop_task(
    bootstrap: TestBootstrapSettings,
    env_vars: Vec<(String, Option<String>)>,
    context: String,
) {
    let result =
        tokio::task::spawn_blocking(move || worker_stop_sync(&bootstrap, &env_vars, &context))
            .await;

    if let Err(err) = result {
        tracing::error!(
            target: LOG_TARGET,
            error = %err,
            "worker stop task panicked during async drop"
        );
    }
}

/// Synchronous worker stop for async drop cleanup.
///
/// Builds a temporary runtime to invoke the worker stop operation.
fn worker_stop_sync(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    let Ok(runtime) = build_runtime() else {
        tracing::error!(
            target: LOG_TARGET,
            "failed to build runtime for worker stop during async drop"
        );
        return;
    };

    let invoker = ClusterWorkerInvoker::new(&runtime, bootstrap, env_vars);
    if let Err(err) = invoker.invoke_as_root(worker_operation::WorkerOperation::Stop) {
        TestCluster::warn_stop_failure(context, &err);
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        let context = Self::stop_context(&self.bootstrap.settings);
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
            self.drop_async_cluster(&context);
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

        if self.is_managed_via_worker {
            let invoker = ClusterWorkerInvoker::new(runtime, &self.bootstrap, &self.env_vars);
            if let Err(err) = invoker.invoke_as_root(worker_operation::WorkerOperation::Stop) {
                Self::warn_stop_failure(context, &err);
            }
            return;
        }

        let Some(postgres) = self.postgres.take() else {
            return;
        };

        let timeout = self.bootstrap.shutdown_timeout;
        let timeout_secs = timeout.as_secs();
        let outcome = runtime.block_on(async { time::timeout(timeout, postgres.stop()).await });

        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(err)) => Self::warn_stop_failure(context, &err),
            Err(_) => Self::warn_stop_timeout(timeout_secs, context),
        }
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
