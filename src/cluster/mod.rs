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
mod runtime;
mod worker_invoker;

pub use self::connection::{ConnectionMetadata, TestClusterConnection};
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use self::worker_invoker::WorkerInvoker;

use self::runtime::build_runtime;
use self::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use postgresql_embedded::{PostgreSQL, Settings};
use std::fmt::Display;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::time;
use tracing::{info, info_span};

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    runtime: Runtime,
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
            runtime,
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
        invoker.invoke(WorkerOperation::Setup, async { embedded.setup().await })?;
        invoker.invoke(WorkerOperation::Start, async { embedded.start().await })
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
            runtime,
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

#[doc(hidden)]
/// Identifies worker lifecycle operations executed via the helper binary.
#[derive(Clone, Copy)]
pub enum WorkerOperation {
    Setup,
    Start,
    Stop,
}

impl WorkerOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }

    #[must_use]
    pub const fn error_context(self) -> &'static str {
        match self {
            Self::Setup => "postgresql_embedded::setup() failed",
            Self::Start => "postgresql_embedded::start() failed",
            Self::Stop => "postgresql_embedded::stop() failed",
        }
    }

    #[must_use]
    pub const fn timeout(self, bootstrap: &TestBootstrapSettings) -> Duration {
        match self {
            Self::Setup => bootstrap.setup_timeout,
            Self::Start => bootstrap.start_timeout,
            Self::Stop => bootstrap.shutdown_timeout,
        }
    }
}

impl Drop for TestCluster {
    #[expect(
        clippy::cognitive_complexity,
        reason = "drop path must branch between worker and in-process shutdown with logging"
    )]
    fn drop(&mut self) {
        let context = Self::stop_context(&self.bootstrap.settings);
        info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = self.is_managed_via_worker,
            "stopping embedded postgres cluster"
        );

        if self.is_managed_via_worker {
            let invoker = ClusterWorkerInvoker::new(&self.runtime, &self.bootstrap, &self.env_vars);
            if let Err(err) = invoker.invoke_as_root(WorkerOperation::Stop) {
                Self::warn_stop_failure(&context, &err);
            }
        } else if let Some(postgres) = self.postgres.take() {
            let timeout = self.bootstrap.shutdown_timeout;
            let timeout_secs = timeout.as_secs();
            let outcome = self
                .runtime
                .block_on(async { time::timeout(timeout, postgres.stop()).await });

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
