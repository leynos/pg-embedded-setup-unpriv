//! Coordinates subprocess worker invocations for privileged cluster actions.
//!
//! The helpers serialise worker payloads, prepare commands, and enforce timeouts
//! so `TestCluster` can remain focused on orchestration logic.

mod output;
mod privileges;

pub(crate) use self::output::render_failure_for_tests;
use self::output::{append_error_context, combine_errors, render_failure};
use crate::cluster::worker_operation::WorkerOperation;
use crate::error::{BootstrapError, BootstrapResult};
use crate::observability::LOG_TARGET;
use crate::worker::WorkerPayload;
use camino::Utf8Path;
use color_eyre::eyre::{Context, Report, eyre};
use postgresql_embedded::Settings;
use serde_json::to_writer;
use std::io::ErrorKind;
use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;
use tempfile::{NamedTempFile, TempPath};
use tracing::{info, info_span};
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
pub(crate) use privileges::{PrivilegeDropGuard, disable_privilege_drop_for_tests};

/// Captures inputs for launching a worker subprocess.
///
/// Bundles the worker binary path, cluster settings, environment variables,
/// requested operation, and execution timeout into a single value consumed by
/// [`run`].
///
/// # Examples
///
/// ```ignore
/// use std::time::Duration;
///
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::cluster::WorkerOperation;
/// use pg_embedded_setup_unpriv::worker_process::WorkerRequest;
/// use postgresql_embedded::Settings;
///
/// let worker = Utf8Path::new("/usr/local/bin/pg_worker");
/// let settings = Settings::default();
/// let env_vars: Vec<(String, Option<String>)> = Vec::new();
/// let request = WorkerRequest::new(
///     worker,
///     &settings,
///     &env_vars,
///     WorkerOperation::Setup,
///     Duration::from_secs(30),
/// );
/// # let _ = request;
/// ```
pub(crate) struct WorkerRequest<'a> {
    /// Filesystem path to the worker binary that will execute the requested
    /// privileged operation.
    worker: &'a Utf8Path,
    /// Cluster configuration that is serialised into the worker payload so the
    /// child process can bootstrap itself consistently with the parent.
    settings: &'a Settings,
    /// Environment variable overrides propagated to the worker to reproduce the
    /// launcher context (values may be unset via `None`).
    env_vars: &'a [(String, Option<String>)],
    /// Specific worker action that determines the command-line flag passed to
    /// the worker binary.
    operation: WorkerOperation,
    /// Maximum duration the worker is allowed to run before it is terminated
    /// and treated as a timeout failure.
    timeout: Duration,
}

impl<'a> WorkerRequest<'a> {
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "request captures all invocation context"
    )]
    /// Constructs a new worker request that can be executed via [`run`].
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use std::time::Duration;
    ///
    /// use camino::Utf8Path;
    /// use pg_embedded_setup_unpriv::cluster::WorkerOperation;
    /// use pg_embedded_setup_unpriv::worker_process::WorkerRequest;
    /// use postgresql_embedded::Settings;
    ///
    /// let worker = Utf8Path::new("/usr/local/bin/pg_worker");
    /// let settings = Settings::default();
    /// let env_vars: Vec<(String, Option<String>)> = Vec::new();
    /// let request = WorkerRequest::new(
    ///     worker,
    ///     &settings,
    ///     &env_vars,
    ///     WorkerOperation::Setup,
    ///     Duration::from_secs(30),
    /// );
    /// # let _ = request;
    /// ```
    pub(crate) const fn new(
        worker: &'a Utf8Path,
        settings: &'a Settings,
        env_vars: &'a [(String, Option<String>)],
        operation: WorkerOperation,
        timeout: Duration,
    ) -> Self {
        Self {
            worker,
            settings,
            env_vars,
            operation,
            timeout,
        }
    }
}

/// Executes the worker binary for a privileged cluster operation with
/// timeout-aware error handling.
///
/// The helper serialises the request payload, drops privileges where
/// applicable, and runs the worker command whilst enforcing the configured
/// timeout. Failures bubble up as [`BootstrapError`] with truncated stdout and
/// stderr to keep diagnostics readable.
///
/// # Errors
///
/// Returns an error when:
/// - the worker payload cannot be created, serialised, or flushed to disk;
/// - the worker command cannot be spawned or its output cannot be collected;
/// - the worker exceeds the configured timeout and must be terminated; or
/// - the worker exits unsuccessfully, in which case the captured output is
///   surfaced for context.
///
/// # Examples
///
/// ```ignore
/// use std::time::Duration;
///
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::cluster::WorkerOperation;
/// use pg_embedded_setup_unpriv::error::BootstrapResult;
/// use pg_embedded_setup_unpriv::worker_process::{run, WorkerRequest};
///
/// fn invoke_worker() -> BootstrapResult<()> {
///     let worker = Utf8Path::new("/usr/local/bin/pg_worker");
///     let settings = load_cluster_settings();
///     let env = vec![("DATABASE_URL".to_owned(), Some("postgres://...".to_owned()))];
///     let request = WorkerRequest::new(
///         worker,
///         &settings,
///         &env,
///         WorkerOperation::Setup,
///         Duration::from_secs(30),
///     );
///
///     run(&request)
/// }
///
/// fn load_cluster_settings() -> postgresql_embedded::Settings {
///     todo!("application-specific settings initialisation")
/// }
/// ```
pub(crate) fn run(request: &WorkerRequest<'_>) -> BootstrapResult<()> {
    WorkerProcess::new(request).run()
}

struct WorkerProcess<'a> {
    request: &'a WorkerRequest<'a>,
}

impl<'a> WorkerProcess<'a> {
    const fn new(request: &'a WorkerRequest<'a>) -> Self {
        Self { request }
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "worker orchestration includes span setup, timeout handling, and cleanup branches"
    )]
    fn run(&self) -> BootstrapResult<()> {
        let span = info_span!(
            target: LOG_TARGET,
            "worker_process",
            operation = self.request.operation.as_str(),
            timeout_secs = self.request.timeout.as_secs()
        );
        let _entered = span.enter();
        let payload_path = self.write_payload()?;
        let mut command = self.configure_command(payload_path.as_ref())?;
        info!(
            target: LOG_TARGET,
            operation = self.request.operation.as_str(),
            payload = %payload_path.display(),
            worker = %self.request.worker,
            "launching worker command"
        );

        let output = self.run_worker(payload_path, &mut command)?;
        let result = Self::handle_exit(self.request.operation, &output);
        if result.is_ok() {
            info!(
                target: LOG_TARGET,
                operation = self.request.operation.as_str(),
                "worker operation completed successfully"
            );
        }
        result
    }

    fn run_worker(&self, payload_path: TempPath, command: &mut Command) -> BootstrapResult<Output> {
        let run_result = self.run_command_with_timeout(command);
        let cleanup_result = Self::close_payload(payload_path);
        match (run_result, cleanup_result) {
            (Ok(output), Ok(())) => Ok(output),
            (Err(err), Ok(())) | (Ok(_), Err(err)) => Err(err),
            (Err(run_err), Err(cleanup_err)) => Err(combine_errors(run_err, cleanup_err)),
        }
    }

    fn write_payload(&self) -> BootstrapResult<TempPath> {
        let payload = WorkerPayload::new(self.request.settings, self.request.env_vars.to_vec())?;
        let mut file = NamedTempFile::new().context("failed to create worker payload file")?;
        to_writer(&mut file, &payload).context("failed to serialise worker payload")?;
        file.flush().context("failed to flush worker payload")?;
        Ok(file.into_temp_path())
    }

    fn configure_command(&self, payload_path: &Path) -> BootstrapResult<Command> {
        let mut command = Command::new(self.request.worker.as_std_path());
        command.arg(self.request.operation.as_str());
        command.arg(payload_path);
        privileges::apply(payload_path, &mut command)?;
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        Ok(command)
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "timeout handling needs explicit branching for diagnostics"
    )]
    fn run_command_with_timeout(&self, command: &mut Command) -> BootstrapResult<Output> {
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                tracing::error!(
                    target: LOG_TARGET,
                    operation = self.request.operation.as_str(),
                    error = %err,
                    "failed to spawn worker command"
                );
                let Err(report) =
                    Err::<(), Report>(Report::new(err)).context("failed to spawn worker command")
                else {
                    tracing::error!(
                        target: LOG_TARGET,
                        operation = self.request.operation.as_str(),
                        "failed to spawn worker command without error detail"
                    );
                    return Err(BootstrapError::from(eyre!(
                        "failed to spawn worker command"
                    )));
                };
                return Err(BootstrapError::from(report));
            }
        };

        let wait_result = match child.wait_timeout(self.request.timeout) {
            Ok(result) => result,
            Err(error) => return Self::handle_wait_error(child, error),
        };
        let timed_out = wait_result.is_none();

        if timed_out {
            self.handle_timeout(&mut child)?;
        }

        let output = child
            .wait_with_output()
            .context("failed to collect worker output")?;

        if timed_out {
            let timeout_secs = self.request.timeout.as_secs();
            tracing::warn!(
                operation = self.request.operation.as_str(),
                timeout_secs,
                "SKIP-TEST-CLUSTER: worker {} timed out after {timeout_secs}s",
                self.request.operation.as_str()
            );
            return Err(render_failure(
                &format!(
                    "{} timed out after {}s",
                    self.request.operation.error_context(),
                    timeout_secs
                ),
                &output,
            ));
        }

        Ok(output)
    }

    fn handle_wait_error(mut child: Child, error: std::io::Error) -> BootstrapResult<Output> {
        let kill_result = child.kill();
        let wait_result = child.wait_with_output();
        let mut message = format!("failed to wait for worker command: {error}");
        drop(error);
        if let Err(kill_err) = kill_result {
            append_error_context(
                &mut message,
                "additionally failed to terminate worker",
                &kill_err,
                "; additionally failed to report worker termination error",
            );
        }
        if let Err(wait_err) = wait_result {
            append_error_context(
                &mut message,
                "additionally failed to reap worker output",
                &wait_err,
                "; additionally failed to describe worker reap error",
            );
        }
        Err(BootstrapError::from(eyre!(message)))
    }

    fn handle_timeout(&self, child: &mut Child) -> BootstrapResult<()> {
        match child.kill() {
            Ok(()) => Ok(()),
            // `InvalidInput` indicates the child has already exited; ignore it.
            Err(err) if err.kind() == ErrorKind::InvalidInput => Ok(()),
            Err(err) => Err(BootstrapError::from(eyre!(
                "failed to terminate worker after {}s: {err}",
                self.request.timeout.as_secs(),
            ))),
        }
    }

    fn handle_exit(operation: WorkerOperation, output: &Output) -> BootstrapResult<()> {
        if output.status.success() {
            Ok(())
        } else {
            Err(render_failure(operation.error_context(), output))
        }
    }

    fn close_payload(payload_path: TempPath) -> BootstrapResult<()> {
        payload_path
            .close()
            .context("failed to clean up worker payload file")?;
        Ok(())
    }
}
