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

use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::{BootstrapError, BootstrapResult};
use crate::worker_process::{self, WorkerRequest};
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::{PostgreSQL, Settings};
use std::fmt::Display;
use std::future::Future;
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
    _env_guard: ScopedEnv,
}

#[doc(hidden)]
#[derive(Debug)]
/// Executes worker operations whilst respecting configured privileges.
pub struct WorkerInvoker<'a> {
    runtime: &'a Runtime,
    bootstrap: &'a TestBootstrapSettings,
    env_vars: &'a [(String, Option<String>)],
}

impl<'a> WorkerInvoker<'a> {
    /// Creates an invoker bound to a runtime, bootstrap configuration, and
    /// derived environment variables.
    pub const fn new(
        runtime: &'a Runtime,
        bootstrap: &'a TestBootstrapSettings,
        env_vars: &'a [(String, Option<String>)],
    ) -> Self {
        Self {
            runtime,
            bootstrap,
            env_vars,
        }
    }

    /// Executes an operation either in-process or via the privileged worker,
    /// depending on the configured privilege level.
    ///
    /// # Errors
    /// Returns a [`BootstrapError`] when the worker invocation fails or when
    /// the in-process operation returns an error.
    pub fn run<Fut>(&self, operation: WorkerOperation, in_process_op: Fut) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match self.bootstrap.privileges {
            ExecutionPrivileges::Unprivileged => {
                self.block_in_process(in_process_op, operation.error_context())
            }
            ExecutionPrivileges::Root => self.run_root_operation(operation),
        }
    }

    fn block_in_process<Fut>(&self, future: Fut, ctx: &'static str) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        self.runtime
            .block_on(future)
            .context(ctx)
            .map_err(BootstrapError::from)
    }

    fn run_root_operation(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        #[cfg(any(test, feature = "cluster-unit-tests"))]
        {
            let hook_slot = crate::test_support::run_root_operation_hook()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if let Some(hook) = hook_slot {
                return hook(self.bootstrap, self.env_vars, operation);
            }
        }

        match self.bootstrap.execution_mode {
            crate::ExecutionMode::InProcess => Err(BootstrapError::from(eyre!(concat!(
                "ExecutionMode::InProcess is unsafe for root because process-wide ",
                "UID/GID changes race in multi-threaded tests; switch to ",
                "ExecutionMode::Subprocess"
            )))),
            crate::ExecutionMode::Subprocess => {
                #[cfg(not(all(
                    unix,
                    any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "freebsd",
                        target_os = "openbsd",
                        target_os = "dragonfly",
                    ),
                )))]
                {
                    return Err(BootstrapError::from(eyre!(
                        "privilege drop not supported on this target; refusing to run as root: {}",
                        operation.error_context()
                    )));
                }

                #[cfg(all(
                    unix,
                    any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "freebsd",
                        target_os = "openbsd",
                        target_os = "dragonfly",
                    ),
                ))]
                {
                    return self.spawn_worker(operation);
                }

                #[expect(unreachable_code, reason = "cfg guard ensures all targets handled")]
                Err(BootstrapError::from(eyre!(
                    "privilege drop support unexpectedly unavailable"
                )))
            }
        }
    }

    fn spawn_worker(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        let worker = self.bootstrap.worker_binary.as_ref().ok_or_else(|| {
            BootstrapError::from(eyre!(
                "PG_EMBEDDED_WORKER must be set when using ExecutionMode::Subprocess"
            ))
        })?;

        let request = WorkerRequest::new(
            worker,
            &self.bootstrap.settings,
            self.env_vars,
            operation,
            operation.timeout(self.bootstrap),
        );

        worker_process::run(&request)
    }
}

#[derive(Debug)]
struct Stopper<'a> {
    runtime: &'a Runtime,
    bootstrap: &'a TestBootstrapSettings,
    is_managed_via_worker: bool,
}

impl<'a> Stopper<'a> {
    const fn new(
        runtime: &'a Runtime,
        bootstrap: &'a TestBootstrapSettings,
        is_managed_via_worker: bool,
    ) -> Self {
        Self {
            runtime,
            bootstrap,
            is_managed_via_worker,
        }
    }

    fn stop(&self, postgres: Option<PostgreSQL>) {
        let context = Self::stop_context(&self.bootstrap.settings);

        if self.is_managed_via_worker {
            self.stop_via_worker(&context);
            return;
        }

        if let Some(mut embedded) = postgres {
            self.stop_in_process(&mut embedded, &context);
        }
    }

    fn stop_context(settings: &Settings) -> String {
        let data_dir = settings.data_dir.display();
        let version = settings.version.to_string();
        format!("version {version}, data_dir {data_dir}")
    }

    fn stop_via_worker(&self, context: &str) {
        let env_vars = self.bootstrap.environment.to_env();
        let invoker = WorkerInvoker::new(self.runtime, self.bootstrap, &env_vars);
        if let Err(err) = invoker.run_root_operation(WorkerOperation::Stop) {
            Self::warn_stop_failure(context, &err);
        }
    }

    fn stop_in_process(&self, postgres: &mut PostgreSQL, context: &str) {
        let timeout = self.bootstrap.shutdown_timeout;
        let timeout_secs = timeout.as_secs();
        let outcome = self
            .runtime
            .block_on(async { time::timeout(timeout, postgres.stop()).await });
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(err)) => Self::warn_stop_failure(context, &err),
            Err(_) => Self::warn_stop_timeout(timeout_secs, context),
        }
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
        let bootstrap = bootstrap_for_tests()?;
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create Tokio runtime for TestCluster")
            .map_err(BootstrapError::from)?;

        let env_vars = bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);
        let privileges = bootstrap.privileges;
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());

        let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);

        invoker.run(WorkerOperation::Setup, async { embedded.setup().await })?;
        invoker.run(WorkerOperation::Start, async { embedded.start().await })?;

        let is_managed_via_worker = matches!(privileges, crate::ExecutionPrivileges::Root);
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
        let stopper = Stopper::new(&self.runtime, &self.bootstrap, self.is_managed_via_worker);
        stopper.stop(self.postgres.take());
        // `env_guard` drops after this block, restoring the environment.
    }
}

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../tests/test_cluster.rs"]
mod tests;
