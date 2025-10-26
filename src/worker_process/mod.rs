//! Coordinates subprocess worker invocations for privileged cluster actions.
//!
//! The helpers serialise worker payloads, prepare commands, and enforce timeouts
//! so `TestCluster` can remain focused on orchestration logic.

mod privileges;

use crate::cluster::WorkerOperation;
use crate::error::{BootstrapError, BootstrapResult};
use crate::worker::WorkerPayload;
use camino::Utf8Path;
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::Settings;
use serde_json::to_writer;
use std::borrow::Cow;
use std::fmt::Write as FmtWrite;
use std::io::ErrorKind;
use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;
use tempfile::{NamedTempFile, TempPath};
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
    worker: &'a Utf8Path,
    settings: &'a Settings,
    env_vars: &'a [(String, Option<String>)],
    operation: WorkerOperation,
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
    const OUTPUT_CHAR_LIMIT: usize = 2_048;
    const TRUNCATION_SUFFIX: &'static str = "… [truncated]";

    const fn new(request: &'a WorkerRequest<'a>) -> Self {
        Self { request }
    }

    fn run(&self) -> BootstrapResult<()> {
        let payload_path = self.write_payload()?;
        let mut command = self.configure_command(payload_path.as_ref())?;
        let run_result = self.run_command_with_timeout(&mut command);
        let cleanup_result = Self::close_payload(payload_path);

        match (run_result, cleanup_result) {
            (Ok(output), Ok(())) => Self::handle_exit(self.request.operation, &output),
            (Err(err), Ok(())) | (Ok(_), Err(err)) => Err(err),
            (Err(run_err), Err(cleanup_err)) => Err(Self::combine_errors(run_err, cleanup_err)),
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

    fn run_command_with_timeout(&self, command: &mut Command) -> BootstrapResult<Output> {
        let mut child = command.spawn().context("failed to spawn worker command")?;

        // Ensure the worker is reaped even if waiting fails unexpectedly.
        let wait_result = match child.wait_timeout(self.request.timeout) {
            Ok(result) => result,
            Err(error) => return self.handle_wait_error(child, error),
        };
        let timed_out = wait_result.is_none();

        if timed_out {
            self.handle_timeout(&mut child)?;
        }

        let output = child
            .wait_with_output()
            .context("failed to collect worker output")?;

        if timed_out {
            return Err(Self::render_failure(
                &format!(
                    "{} timed out after {}s",
                    self.request.operation.error_context(),
                    self.request.timeout.as_secs()
                ),
                &output,
            ));
        }

        Ok(output)
    }

    #[expect(
        clippy::unused_self,
        reason = "request context may be added to diagnostics"
    )]
    fn handle_wait_error(
        &self,
        mut child: Child,
        error: std::io::Error,
    ) -> BootstrapResult<Output> {
        let kill_result = child.kill();
        let wait_result = child.wait_with_output();
        let mut message = format!("failed to wait for worker command: {error}");
        drop(error);
        if let Err(kill_err) = kill_result {
            Self::append_error_context(
                &mut message,
                "additionally failed to terminate worker",
                &kill_err,
                "; additionally failed to report worker termination error",
            );
        }
        if let Err(wait_err) = wait_result {
            Self::append_error_context(
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
            Err(Self::render_failure(operation.error_context(), output))
        }
    }

    fn render_failure(context: &str, output: &Output) -> BootstrapError {
        let stdout = Self::truncate_output(String::from_utf8_lossy(&output.stdout));
        let stderr = Self::truncate_output(String::from_utf8_lossy(&output.stderr));
        BootstrapError::from(eyre!("{context}\nstdout: {stdout}\nstderr: {stderr}"))
    }

    fn close_payload(payload_path: TempPath) -> BootstrapResult<()> {
        payload_path
            .close()
            .context("failed to clean up worker payload file")?;
        Ok(())
    }

    fn truncate_output(text: Cow<'_, str>) -> String {
        let mut out =
            String::with_capacity(Self::OUTPUT_CHAR_LIMIT + Self::TRUNCATION_SUFFIX.len());
        let mut chars = text.chars();
        for _ in 0..Self::OUTPUT_CHAR_LIMIT {
            match chars.next() {
                Some(ch) => out.push(ch),
                None => return text.into_owned(),
            }
        }

        if chars.next().is_none() {
            return text.into_owned();
        }

        out.push_str(Self::TRUNCATION_SUFFIX);
        out
    }

    fn combine_errors(primary: BootstrapError, cleanup: BootstrapError) -> BootstrapError {
        let primary_report = primary.into_report();
        let cleanup_report = cleanup.into_report();
        BootstrapError::from(eyre!(
            "{primary_report}\nSecondary failure whilst removing worker payload: {cleanup_report}"
        ))
    }

    fn append_error_context(
        message: &mut String,
        detail: &str,
        error: &impl std::fmt::Display,
        fallback: &str,
    ) {
        if FmtWrite::write_fmt(message, format_args!("; {detail}: {error}")).is_err() {
            message.push_str(fallback);
        }
    }
}
#[doc(hidden)]
#[must_use]
pub(crate) fn render_failure_for_tests(context: &str, output: &Output) -> BootstrapError {
    WorkerProcess::render_failure(context, output)
}
