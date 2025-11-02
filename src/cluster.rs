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
    env_vars: Vec<(String, Option<String>)>,
    _env_guard: ScopedEnv,
}

/// Executes worker operations whilst respecting configured privileges.
#[doc(hidden)]
#[derive(Debug)]
pub struct WorkerInvoker<'a> {
    runtime: &'a Runtime,
    bootstrap: &'a TestBootstrapSettings,
    env_vars: &'a [(String, Option<String>)],
}

impl<'a> WorkerInvoker<'a> {
    /// Creates an invoker bound to a runtime, bootstrap configuration, and
    /// derived environment variables.
    ///
    /// # Examples
    /// ```
    /// # use camino::Utf8PathBuf;
    /// # use pg_embedded_setup_unpriv::{
    /// #     ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    /// #     WorkerInvoker,
    /// # };
    /// # use postgresql_embedded::Settings;
    /// # use std::time::Duration;
    /// # fn build_settings() -> TestBootstrapSettings {
    /// #     TestBootstrapSettings {
    /// #         privileges: ExecutionPrivileges::Unprivileged,
    /// #         execution_mode: ExecutionMode::InProcess,
    /// #         settings: Settings::default(),
    /// #         environment: TestBootstrapEnvironment {
    /// #             home: Utf8PathBuf::from("/tmp"),
    /// #             xdg_cache_home: Utf8PathBuf::from("/tmp/cache"),
    /// #             xdg_runtime_dir: Utf8PathBuf::from("/tmp/run"),
    /// #             pgpass_file: Utf8PathBuf::from("/tmp/.pgpass"),
    /// #             tz_dir: None,
    /// #             timezone: "UTC".into(),
    /// #         },
    /// #         worker_binary: None,
    /// #         setup_timeout: Duration::from_secs(1),
    /// #         start_timeout: Duration::from_secs(1),
    /// #         shutdown_timeout: Duration::from_secs(1),
    /// #     }
    /// # }
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().map(|runtime| {
    /// #     let bootstrap = build_settings();
    /// #     let env_vars = bootstrap.environment.to_env();
    /// #     WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    /// # });
    /// ```
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
    ///
    /// # Examples
    /// ```
    /// # use camino::Utf8PathBuf;
    /// # use pg_embedded_setup_unpriv::{
    /// #     ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    /// #     WorkerInvoker, WorkerOperation,
    /// # };
    /// # use postgresql_embedded::Settings;
    /// # use std::time::Duration;
    /// # async fn do_nothing() -> Result<(), postgresql_embedded::Error> { Ok(()) }
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().map(|runtime| {
    /// #     let bootstrap = TestBootstrapSettings {
    /// #         privileges: ExecutionPrivileges::Unprivileged,
    /// #         execution_mode: ExecutionMode::InProcess,
    /// #         settings: Settings::default(),
    /// #         environment: TestBootstrapEnvironment {
    /// #             home: Utf8PathBuf::from("/tmp"),
    /// #             xdg_cache_home: Utf8PathBuf::from("/tmp/cache"),
    /// #             xdg_runtime_dir: Utf8PathBuf::from("/tmp/run"),
    /// #             pgpass_file: Utf8PathBuf::from("/tmp/.pgpass"),
    /// #             tz_dir: None,
    /// #             timezone: "UTC".into(),
    /// #         },
    /// #         worker_binary: None,
    /// #         setup_timeout: Duration::from_secs(1),
    /// #         start_timeout: Duration::from_secs(1),
    /// #         shutdown_timeout: Duration::from_secs(1),
    /// #     };
    /// #     let env_vars = bootstrap.environment.to_env();
    /// #     let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    /// #     let _ = invoker
    /// #         .invoke(WorkerOperation::Setup, do_nothing())
    /// #         .expect("invocation should succeed");
    /// # });
    /// ```
    pub fn invoke<Fut>(&self, operation: WorkerOperation, in_process_op: Fut) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match self.bootstrap.privileges {
            ExecutionPrivileges::Unprivileged => {
                self.invoke_unprivileged(in_process_op, operation.error_context())
            }
            ExecutionPrivileges::Root => self.invoke_as_root(operation),
        }
    }

    fn invoke_unprivileged<Fut>(&self, future: Fut, ctx: &'static str) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        self.runtime
            .block_on(future)
            .context(ctx)
            .map_err(BootstrapError::from)
    }

    fn invoke_as_root(&self, operation: WorkerOperation) -> BootstrapResult<()> {
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

        invoker.invoke(WorkerOperation::Setup, async { embedded.setup().await })?;
        invoker.invoke(WorkerOperation::Start, async { embedded.start().await })?;

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
            let invoker = WorkerInvoker::new(&self.runtime, &self.bootstrap, &self.env_vars);
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

#[cfg(test)]
mod drop_logging_tests {
    use super::*;
    use std::io::{Result as IoResult, Write};
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing::subscriber::with_default;
    use tracing_subscriber::fmt;

    struct BufferWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
            let mut guard = self
                .buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    fn capture_warn_logs<F>(action: F) -> Vec<String>
    where
        F: FnOnce(),
    {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer_buffer = Arc::clone(&buffer);
        let subscriber = fmt()
            .with_max_level(Level::WARN)
            .without_time()
            .with_writer(move || BufferWriter {
                buffer: Arc::clone(&writer_buffer),
            })
            .finish();

        with_default(subscriber, action);

        let bytes = buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let content = String::from_utf8(bytes)
            .unwrap_or_else(|err| panic!("logs should be valid UTF-8: {err}"));
        content.lines().map(str::to_owned).collect()
    }

    #[test]
    fn warn_stop_timeout_emits_warning() {
        let logs = capture_warn_logs(|| TestCluster::warn_stop_timeout(5, "ctx"));
        assert!(
            logs.iter()
                .any(|line| line.contains("stop() timed out after 5s (ctx)")),
            "expected timeout warning, got {logs:?}"
        );
    }

    #[test]
    fn warn_stop_failure_emits_warning() {
        let logs = capture_warn_logs(|| TestCluster::warn_stop_failure("ctx", &"boom"));
        assert!(
            logs.iter()
                .any(|line| line.contains("failed to stop embedded postgres instance")),
            "expected failure warning, got {logs:?}"
        );
    }
}

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../tests/test_cluster.rs"]
mod tests;
