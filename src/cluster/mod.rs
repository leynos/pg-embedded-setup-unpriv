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
mod worker_invoker;

pub use self::connection::{ConnectionMetadata, TestClusterConnection};
pub use self::worker_invoker::WorkerInvoker;

use self::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::{BootstrapError, BootstrapResult};
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use color_eyre::eyre::Context;
use postgresql_embedded::{PostgreSQL, Settings};
use std::fmt::Display;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tokio::time;

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    runtime: Runtime,
    postgres: Option<PostgreSQL>,
    bootstrap: TestBootstrapSettings,
    is_managed_via_worker: bool,
    env_vars: Vec<(String, Option<String>)>,
    _env_guard: ScopedEnv,
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
        let mut bootstrap = bootstrap_for_tests()?;
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create Tokio runtime for TestCluster")
            .map_err(BootstrapError::from)?;

        let env_vars = bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);
        let privileges = bootstrap.privileges;
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());

        let invoker = ClusterWorkerInvoker::new(&runtime, &bootstrap, &env_vars);

        invoker.invoke(WorkerOperation::Setup, async { embedded.setup().await })?;
        invoker.invoke(WorkerOperation::Start, async { embedded.start().await })?;

        let is_managed_via_worker = matches!(privileges, ExecutionPrivileges::Root);
        if !is_managed_via_worker {
            // Capture runtime mutations such as dynamically assigned ports so connection
            // metadata reflects the live cluster rather than the pre-bootstrap defaults.
            bootstrap.settings.port = embedded.settings().port;
        }
        let postgres = if is_managed_via_worker {
            None
        } else {
            Some(embedded)
        };

        Ok(Self {
            runtime,
            postgres,
            bootstrap,
            is_managed_via_worker,
            env_vars,
            _env_guard: env_guard,
        })
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
    fn drop(&mut self) {
        let context = Self::stop_context(&self.bootstrap.settings);

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
        // `env_guard` drops after this block, restoring the environment.
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
