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
use crate::worker::WorkerPayload;
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use camino::Utf8Path;
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::{PostgreSQL, Settings};
use serde_json::to_writer;
use std::fmt::Display;
use std::future::Future;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::runtime::{Builder, Runtime};
use tokio::time;
use wait_timeout::ChildExt;

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
use nix::unistd::{Gid, Uid, User, chown};
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
use std::os::unix::process::CommandExt;

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
/// Binds runtime, bootstrap settings, and environment variables for
/// privilege-aware cluster operations.
pub struct PrivilegedOperationContext<'a> {
    runtime: &'a Runtime,
    bootstrap: &'a TestBootstrapSettings,
    env_vars: &'a [(String, Option<String>)],
}

impl<'a> PrivilegedOperationContext<'a> {
    /// Creates a new context for executing a privileged worker operation.
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

    /// Returns the execution privileges configured for the current bootstrap.
    const fn privileges(&self) -> ExecutionPrivileges {
        self.bootstrap.privileges
    }

    /// Returns the Tokio runtime used to execute asynchronous operations.
    const fn runtime(&self) -> &'a Runtime {
        self.runtime
    }

    /// Returns the bootstrap settings associated with the operation.
    const fn bootstrap(&self) -> &'a TestBootstrapSettings {
        self.bootstrap
    }

    /// Returns the scoped environment variables for the worker invocation.
    const fn env_vars(&self) -> &'a [(String, Option<String>)] {
        self.env_vars
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

        {
            let ctx = PrivilegedOperationContext::new(&runtime, &bootstrap, &env_vars);
            Self::with_privileges(&ctx, WorkerOperation::Setup, async {
                embedded.setup().await
            })?;
            Self::with_privileges(&ctx, WorkerOperation::Start, async {
                embedded.start().await
            })?;
        }

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

    #[doc(hidden)]
    pub(crate) fn with_privileges<Fut>(
        ctx: &PrivilegedOperationContext<'_>,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match ctx.privileges() {
            crate::ExecutionPrivileges::Unprivileged => {
                Self::block_in_process(ctx.runtime(), in_process_op, operation.error_context())
            }
            crate::ExecutionPrivileges::Root => {
                Self::run_root_operation(ctx.bootstrap(), ctx.env_vars(), operation)
            }
        }
    }

    fn block_in_process<Fut>(
        runtime: &Runtime,
        future: Fut,
        ctx: &'static str,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        runtime
            .block_on(future)
            .context(ctx)
            .map_err(BootstrapError::from)
    }

    fn run_root_operation(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
    ) -> BootstrapResult<()> {
        #[cfg(any(test, feature = "cluster-unit-tests"))]
        {
            let hook_slot = crate::test_support::run_root_operation_hook()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if let Some(hook) = hook_slot {
                return hook(bootstrap, env_vars, operation);
            }
        }

        match bootstrap.execution_mode {
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
                    return Self::spawn_worker(bootstrap, env_vars, operation);
                }

                #[expect(unreachable_code, reason = "cfg guard ensures all targets handled")]
                Err(BootstrapError::from(eyre!(
                    "privilege drop support unexpectedly unavailable"
                )))
            }
        }
    }

    fn spawn_worker(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
    ) -> BootstrapResult<()> {
        let worker = bootstrap.worker_binary.as_ref().ok_or_else(|| {
            BootstrapError::from(eyre!(
                "PG_EMBEDDED_WORKER must be set when using ExecutionMode::Subprocess"
            ))
        })?;

        let payload_path = Self::write_worker_payload(&bootstrap.settings, env_vars)?;
        let mut command = Self::configure_worker_command(worker, payload_path.as_ref(), operation)?;
        let timeout = operation.timeout(bootstrap);
        let output = Self::run_worker(&mut command, timeout, operation)?;

        payload_path
            .close()
            .context("failed to clean up worker payload file")
            .map_err(BootstrapError::from)?;

        if output.status.success() {
            Ok(())
        } else {
            Err(BootstrapError::from(eyre!(
                "{}\nstdout: {}\nstderr: {}",
                operation.error_context(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    fn write_worker_payload(
        settings: &Settings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<tempfile::TempPath> {
        let payload = WorkerPayload::new(settings, env_vars.to_vec())?;
        let mut file = NamedTempFile::new()
            .context("failed to create worker payload file")
            .map_err(BootstrapError::from)?;
        to_writer(&mut file, &payload)
            .context("failed to serialize worker payload")
            .map_err(BootstrapError::from)?;
        file.flush()
            .context("failed to flush worker payload")
            .map_err(BootstrapError::from)?;
        Ok(file.into_temp_path())
    }

    fn configure_worker_command(
        worker: &Utf8Path,
        payload_path: &Path,
        operation: WorkerOperation,
    ) -> BootstrapResult<Command> {
        let mut command = Command::new(worker.as_std_path());
        command.arg(operation.as_str());
        command.arg(payload_path);

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
            let user = User::from_name("nobody")
                .context("failed to resolve user 'nobody'")
                .map_err(BootstrapError::from)?
                .ok_or_else(|| BootstrapError::from(eyre!("user 'nobody' not found")))?;
            let uid = user.uid.as_raw();
            let gid = user.gid.as_raw();

            chown(
                payload_path,
                Some(Uid::from_raw(uid)),
                Some(Gid::from_raw(gid)),
            )
            .context("failed to chown worker payload to nobody")
            .map_err(BootstrapError::from)?;

            unsafe {
                // SAFETY: The closure executes immediately before exec while the process still
                // holds root privileges. We synchronously drop supplementary groups, gid, and uid
                // so the worker starts with the 'nobody' identity only.
                command.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setgid(gid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setuid(uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        Ok(command)
    }

    fn run_worker(
        command: &mut Command,
        timeout: Duration,
        operation: WorkerOperation,
    ) -> BootstrapResult<std::process::Output> {
        let mut child = command
            .spawn()
            .context("failed to spawn pg_worker")
            .map_err(BootstrapError::from)?;

        let wait_result = child
            .wait_timeout(timeout)
            .context("failed to wait for pg_worker")
            .map_err(BootstrapError::from)?;
        let timed_out = wait_result.is_none();

        if timed_out {
            match child.kill() {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::InvalidInput => {}
                Err(err) => {
                    return Err(BootstrapError::from(eyre!(
                        "failed to terminate pg_worker after {}s: {err}",
                        timeout.as_secs(),
                    )));
                }
            }
        }

        let output = child
            .wait_with_output()
            .context("failed to collect pg_worker output")
            .map_err(BootstrapError::from)?;

        if timed_out {
            return Err(BootstrapError::from(eyre!(
                "{} timed out after {}s\nstdout: {}\nstderr: {}",
                operation.error_context(),
                timeout.as_secs(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(output)
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
        let context = Self::stop_context(&self.bootstrap.settings);

        if self.is_managed_via_worker {
            self.stop_via_worker(&context);
            return;
        }

        if let Some(mut postgres) = self.postgres.take() {
            self.stop_in_process(&mut postgres, &context);
        }
        // `env_guard` drops after this block, restoring the environment.
    }
}

impl TestCluster {
    fn stop_context(settings: &Settings) -> String {
        let data_dir = settings.data_dir.display();
        let version = settings.version.to_string();
        format!("version {version}, data_dir {data_dir}")
    }

    fn stop_via_worker(&self, context: &str) {
        let env_vars = self.bootstrap.environment.to_env();
        if let Err(err) =
            Self::run_root_operation(&self.bootstrap, &env_vars, WorkerOperation::Stop)
        {
            Self::warn_stop_failure(context, &err);
        }
    }

    fn stop_in_process(&mut self, postgres: &mut PostgreSQL, context: &str) {
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

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../tests/test_cluster.rs"]
mod tests;
