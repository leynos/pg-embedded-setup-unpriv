use crate::error::{BootstrapError, BootstrapResult};
use crate::worker::WorkerPayload;
use camino::Utf8Path;
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::Settings;
use serde_json::to_writer;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Output};
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
    pub(crate) command: &'a str,
    pub(crate) error_context: &'a str,
    pub(crate) timeout: Duration,
}

pub(crate) fn run(request: &WorkerRequest<'_>) -> BootstrapResult<()> {
    let payload_path = write_payload(request.settings, request.env_vars)?;
    let mut command = configure_command(request.worker, payload_path.as_ref(), request.command)?;
    let output = execute(&mut command, request.timeout, request.error_context)?;

    payload_path
        .close()
        .context("failed to clean up worker payload file")
        .map_err(BootstrapError::from)?;

    if output.status.success() {
        Ok(())
    } else {
        Err(BootstrapError::from(eyre!(
            "{}\nstdout: {}\nstderr: {}",
            request.error_context,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn write_payload(
    settings: &Settings,
    env_vars: &[(String, Option<String>)],
) -> BootstrapResult<TempPath> {
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

fn configure_command(
    worker: &Utf8Path,
    payload_path: &Path,
    operation: &str,
) -> BootstrapResult<Command> {
    let mut command = Command::new(worker.as_std_path());
    command.arg(operation);
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

fn execute(
    command: &mut Command,
    timeout: Duration,
    error_context: &str,
) -> BootstrapResult<Output> {
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
            "{error_context} timed out after {}s\nstdout: {}\nstderr: {}",
            timeout.as_secs(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output)
}
