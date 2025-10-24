//! Coordinates subprocess worker invocations for privileged cluster actions.
//!
//! The helpers serialise worker payloads, prepare commands, and enforce timeouts
//! so `TestCluster` can remain focused on orchestration logic.

use crate::cluster::WorkerOperation;
use crate::error::{BootstrapError, BootstrapResult};
use crate::worker::WorkerPayload;
use camino::Utf8Path;
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::Settings;
use serde_json::to_writer;
use std::borrow::Cow;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
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

pub(crate) struct WorkerRequest<'a> {
    pub(crate) worker: &'a Utf8Path,
    pub(crate) settings: &'a Settings,
    pub(crate) env_vars: &'a [(String, Option<String>)],
    pub(crate) operation: WorkerOperation,
    pub(crate) timeout: Duration,
}

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
        Self::drop_privileges(payload_path, &mut command)?;
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        Ok(command)
    }

    fn run_command_with_timeout(&self, command: &mut Command) -> BootstrapResult<Output> {
        let mut child = command.spawn().context("failed to spawn worker command")?;

        let wait_result = child
            .wait_timeout(self.request.timeout)
            .context("failed to wait for worker command")?;
        let timed_out = wait_result.is_none();

        if timed_out {
            match child.kill() {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::InvalidInput => {}
                Err(err) => {
                    return Err(BootstrapError::from(eyre!(
                        "failed to terminate worker after {}s: {err}",
                        self.request.timeout.as_secs(),
                    )));
                }
            }
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

    fn drop_privileges(payload_path: &Path, command: &mut Command) -> BootstrapResult<()> {
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
            if Self::skip_privilege_drop_for_tests() {
                return Ok(());
            }

            let user = User::from_name("nobody")
                .context("failed to resolve user 'nobody'")?
                .ok_or_else(|| eyre!("user 'nobody' not found"))?;
            let uid = user.uid.as_raw();
            let gid = user.gid.as_raw();

            chown(
                payload_path,
                Some(Uid::from_raw(uid)),
                Some(Gid::from_raw(gid)),
            )
            .context("failed to chown worker payload to nobody")?;

            unsafe {
                // SAFETY: This closure executes immediately before `exec` whilst the process
                // still owns elevated credentials. The synchronous UID/GID demotion mirrors the
                // previous inlined implementation in `TestCluster::spawn_worker`.
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
            let _ = payload_path;
            let _ = command;
        }

        Ok(())
    }

    fn truncate_output(text: Cow<'_, str>) -> String {
        if text.chars().count() <= Self::OUTPUT_CHAR_LIMIT {
            return text.into_owned();
        }

        let mut truncated =
            String::with_capacity(Self::OUTPUT_CHAR_LIMIT + Self::TRUNCATION_SUFFIX.len());
        for (idx, ch) in text.chars().enumerate() {
            if idx < Self::OUTPUT_CHAR_LIMIT {
                truncated.push(ch);
                continue;
            }

            truncated.push_str(Self::TRUNCATION_SUFFIX);
            break;
        }

        truncated
    }

    fn combine_errors(primary: BootstrapError, cleanup: BootstrapError) -> BootstrapError {
        let primary_report = primary.into_report();
        let cleanup_report = cleanup.into_report();
        BootstrapError::from(eyre!(
            "{primary_report}\nSecondary failure whilst removing worker payload: {cleanup_report}"
        ))
    }

    #[cfg(all(
        test,
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    ))]
    fn skip_privilege_drop_for_tests() -> bool {
        SKIP_PRIVILEGE_DROP.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(not(all(
        test,
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    )))]
    const fn skip_privilege_drop_for_tests() -> bool {
        false
    }
}

#[cfg(all(
    test,
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
static SKIP_PRIVILEGE_DROP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn test_mutex() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_privilege_drop_disabled<T>(f: impl FnOnce() -> T) -> T {
        let _guard = test_mutex()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        SKIP_PRIVILEGE_DROP.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = f();
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
        SKIP_PRIVILEGE_DROP.store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    fn sample_settings(root: &std::path::Path) -> Settings {
        Settings {
            installation_dir: root.join("install"),
            password_file: root.join("pgpass"),
            data_dir: root.join("data"),
            timeout: Some(Duration::from_secs(30)),
            ..Settings::default()
        }
    }

    fn write_script(
        root: &std::path::Path,
        name: &str,
        body: &str,
    ) -> BootstrapResult<Utf8PathBuf> {
        let path = root.join(name);
        fs::write(&path, body).context("write script")?;
        let mut perms = fs::metadata(&path)
            .context("script metadata")?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).context("set script permissions")?;
        let utf8 =
            Utf8PathBuf::from_path_buf(path).map_err(|_| eyre!("script path must be UTF-8"))?;
        Ok(utf8)
    }

    const fn request<'a>(
        worker: &'a Utf8Path,
        settings: &'a Settings,
        env: &'a [(String, Option<String>)],
        timeout: Duration,
    ) -> WorkerRequest<'a> {
        WorkerRequest {
            worker,
            settings,
            env_vars: env,
            operation: WorkerOperation::Setup,
            timeout,
        }
    }

    fn require_contains(message: &str, needle: &str, description: &str) -> BootstrapResult<()> {
        if message.contains(needle) {
            Ok(())
        } else {
            Err(BootstrapError::from(eyre!("{description}: {message}")))
        }
    }

    #[test]
    fn run_succeeds_when_worker_exits_successfully() -> BootstrapResult<()> {
        with_privilege_drop_disabled(|| -> BootstrapResult<()> {
            let sandbox = tempdir().context("create sandbox")?;
            fs::create_dir_all(sandbox.path().join("install")).context("install dir")?;
            fs::create_dir_all(sandbox.path().join("data")).context("data dir")?;
            fs::write(sandbox.path().join("pgpass"), b"").context("pgpass")?;
            let settings = sample_settings(sandbox.path());
            let env_vars = Vec::new();
            let worker = Utf8Path::new("/bin/true");
            let request = request(worker, &settings, &env_vars, Duration::from_secs(1));

            run(&request)
        })
    }

    #[test]
    fn run_truncates_stdout_and_stderr_on_failure() -> BootstrapResult<()> {
        with_privilege_drop_disabled(|| -> BootstrapResult<()> {
            let sandbox = tempdir().context("create sandbox")?;
            fs::create_dir_all(sandbox.path().join("install")).context("install dir")?;
            fs::create_dir_all(sandbox.path().join("data")).context("data dir")?;
            fs::write(sandbox.path().join("pgpass"), b"").context("pgpass")?;
            let settings = sample_settings(sandbox.path());
            let env_vars = Vec::new();
            let long_output = "A".repeat(5_000);
            let script_body = format!(
                "#!/bin/sh\ncat <<'EOF'\n{long_output}\nEOF\ncat <<'EOF' >&2\n{long_output}\nEOF\nexit 1\n"
            );
            let worker_path = write_script(sandbox.path(), "fail.sh", &script_body)?;
            let request = request(
                worker_path.as_path(),
                &settings,
                &env_vars,
                Duration::from_secs(1),
            );

            match run(&request) {
                Ok(()) => Err(BootstrapError::from(eyre!("worker must fail"))),
                Err(err) => {
                    let message = err.to_string();
                    require_contains(&message, "stdout:", "missing stdout")?;
                    require_contains(&message, "stderr:", "missing stderr")?;
                    require_contains(
                        &message,
                        WorkerProcess::TRUNCATION_SUFFIX,
                        "error should mention truncation",
                    )?;
                    Ok(())
                }
            }
        })
    }

    #[test]
    fn run_reports_timeout_errors() -> BootstrapResult<()> {
        with_privilege_drop_disabled(|| -> BootstrapResult<()> {
            let sandbox = tempdir().context("create sandbox")?;
            fs::create_dir_all(sandbox.path().join("install")).context("install dir")?;
            fs::create_dir_all(sandbox.path().join("data")).context("data dir")?;
            fs::write(sandbox.path().join("pgpass"), b"").context("pgpass")?;
            let settings = sample_settings(sandbox.path());
            let env_vars = Vec::new();
            let script_body = "#!/bin/sh\nsleep 5\n";
            let worker_path = write_script(sandbox.path(), "sleep.sh", script_body)?;
            let request = request(
                worker_path.as_path(),
                &settings,
                &env_vars,
                Duration::from_millis(50),
            );

            match run(&request) {
                Ok(()) => Err(BootstrapError::from(eyre!("worker should time out"))),
                Err(err) => {
                    let message = err.to_string();
                    require_contains(&message, "timed out", "timeout context missing")?;
                    Ok(())
                }
            }
        })
    }

    #[test]
    fn render_failure_truncates_outputs() -> BootstrapResult<()> {
        let long = "B".repeat(4_096);
        let output = Output {
            status: ExitStatusExt::from_raw(0),
            stdout: long.as_bytes().to_vec(),
            stderr: long.as_bytes().to_vec(),
        };

        let err = WorkerProcess::render_failure("ctx", &output);
        let message = err.to_string();
        require_contains(
            &message,
            WorkerProcess::TRUNCATION_SUFFIX,
            "error should mention truncation",
        )?;
        Ok(())
    }
}
