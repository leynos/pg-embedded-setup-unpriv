//! Installation directory and port resolution for `TestCluster`.
//!
//! Handles refreshing the installation directory and port after worker setup,
//! as well as reading `postmaster.pid` for port discovery.

use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};
use color_eyre::eyre::eyre;
use postgresql_embedded::{Settings, Version};
use std::path::Path;
use std::time::Duration;

/// Number of attempts to read the postmaster port.
pub(super) const POSTMASTER_PORT_ATTEMPTS: usize = 10;

/// Delay between postmaster port read attempts.
pub(super) const POSTMASTER_PORT_DELAY: Duration = Duration::from_millis(100);

/// Refreshes the installation directory after worker setup for root runs.
///
/// The worker helper may install `PostgreSQL` under a subdirectory, so we
/// re-resolve the installation directory before starting the server.
pub(super) fn refresh_worker_installation_dir(bootstrap: &mut TestBootstrapSettings) {
    if bootstrap.privileges != ExecutionPrivileges::Root {
        return;
    }

    if let Some(installed_dir) = resolve_installed_dir(&bootstrap.settings) {
        bootstrap.settings.installation_dir = installed_dir;
    }
}

/// Internal macro to implement `refresh_worker_port` logic.
macro_rules! refresh_worker_port_impl {
    ($bootstrap:expr, $retry_call:expr) => {{
        if $bootstrap.privileges != ExecutionPrivileges::Root {
            return Ok(());
        }

        let pid_path = $bootstrap.settings.data_dir.join("postmaster.pid");
        if let Some(port) = $retry_call? {
            $bootstrap.settings.port = port;
            return Ok(());
        }

        tracing::debug!(
            target: LOG_TARGET,
            path = %pid_path.display(),
            "postmaster.pid missing after start; keeping configured port"
        );
        Ok(())
    }};
}

/// Refreshes the worker port (synchronous variant).
pub(super) fn refresh_worker_port(bootstrap: &mut TestBootstrapSettings) -> BootstrapResult<()> {
    refresh_worker_port_impl!(
        bootstrap,
        read_postmaster_port_with_retry(&bootstrap.settings.data_dir.join("postmaster.pid"))
    )
}

/// Refreshes the worker port (async variant).
#[cfg(feature = "async-api")]
pub(super) async fn refresh_worker_port_async(
    bootstrap: &mut TestBootstrapSettings,
) -> BootstrapResult<()> {
    refresh_worker_port_impl!(
        bootstrap,
        read_postmaster_port_with_retry_async(&bootstrap.settings.data_dir.join("postmaster.pid"))
            .await
    )
}

/// Reads the postmaster port with retry (synchronous).
fn read_postmaster_port_with_retry(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    for _ in 0..POSTMASTER_PORT_ATTEMPTS {
        if let Some(port) = read_postmaster_port(pid_path)? {
            return Ok(Some(port));
        }
        std::thread::sleep(POSTMASTER_PORT_DELAY);
    }
    Ok(None)
}

/// Reads the postmaster port with retry (async).
#[cfg(feature = "async-api")]
async fn read_postmaster_port_with_retry_async(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    for _ in 0..POSTMASTER_PORT_ATTEMPTS {
        if let Some(port) = read_postmaster_port_async(pid_path).await? {
            return Ok(Some(port));
        }
        tokio::time::sleep(POSTMASTER_PORT_DELAY).await;
    }
    Ok(None)
}

/// Reads the port from a postmaster.pid file (async).
///
/// Returns `Ok(None)` for retryable conditions: file not found, missing port line,
/// or unparseable port value. Returns `Err` only for unexpected I/O errors.
#[cfg(feature = "async-api")]
async fn read_postmaster_port_async(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    let Some(contents) = read_pid_file_contents_async(pid_path).await? else {
        return Ok(None);
    };
    Ok(parse_port_from_pid_contents(&contents, pid_path))
}

/// Internal macro to implement `read_pid_file_contents` logic.
macro_rules! read_pid_file_contents_impl {
    ($read_fn:expr, $pid_path:expr) => {
        match $read_fn {
            Ok(contents) => Ok(Some(contents)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(crate::error::BootstrapError::from(eyre!(
                "failed to read postmaster pid at {}: {err}",
                $pid_path.display()
            ))),
        }
    };
}

/// Reads the contents of a postmaster.pid file if it exists (async).
#[cfg(feature = "async-api")]
async fn read_pid_file_contents_async(pid_path: &Path) -> BootstrapResult<Option<String>> {
    read_pid_file_contents_impl!(tokio::fs::read_to_string(pid_path).await, pid_path)
}

/// Reads the port from a postmaster.pid file.
///
/// Returns `Ok(None)` for retryable conditions: file not found, missing port line,
/// or unparseable port value. Returns `Err` only for unexpected I/O errors.
fn read_postmaster_port(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    let Some(contents) = read_pid_file_contents(pid_path)? else {
        return Ok(None);
    };
    Ok(parse_port_from_pid_contents(&contents, pid_path))
}

/// Reads the contents of a postmaster.pid file if it exists.
fn read_pid_file_contents(pid_path: &Path) -> BootstrapResult<Option<String>> {
    read_pid_file_contents_impl!(std::fs::read_to_string(pid_path), pid_path)
}

/// Parses the port from postmaster.pid contents.
///
/// Returns `None` if the port line is missing or cannot be parsed.
fn parse_port_from_pid_contents(contents: &str, pid_path: &Path) -> Option<u16> {
    let port_line = contents.lines().nth(3)?;
    port_line
        .trim()
        .parse::<u16>()
        .inspect_err(|err| log_port_parse_failure(pid_path, err, port_line))
        .ok()
}

/// Logs a debug message when port parsing fails.
fn log_port_parse_failure(pid_path: &Path, err: &std::num::ParseIntError, port_line: &str) {
    tracing::debug!(
        target: LOG_TARGET,
        path = %pid_path.display(),
        error = %err,
        port_line = %port_line,
        "failed to parse postmaster port, will retry"
    );
}

/// Resolves the installed directory from settings.
///
/// Searches for a directory containing a `bin/` subdirectory within the
/// installation directory.
pub(super) fn resolve_installed_dir(settings: &Settings) -> Option<std::path::PathBuf> {
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
    candidates.sort_by(|a, b| {
        let version_a = a
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|s| Version::parse(s).ok());
        let version_b = b
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|s| Version::parse(s).ok());

        match (version_a, version_b) {
            (Some(va), Some(vb)) => va.cmp(&vb),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => a.cmp(b),
        }
    });
    candidates.pop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::dummy_settings;
    use std::fs;

    #[test]
    fn refresh_worker_port_reads_postmaster_pid() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let pid_path = temp_dir.path().join("postmaster.pid");
        let contents = format!("12345\n{}\n1700000000\n54321\n", temp_dir.path().display());
        fs::write(&pid_path, contents).expect("write postmaster.pid");

        let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
        bootstrap.settings.data_dir = temp_dir.path().to_path_buf();
        bootstrap.settings.port = 0;

        refresh_worker_port(&mut bootstrap).expect("refresh worker port");
        assert_eq!(bootstrap.settings.port, 54321);
    }
}
