//! Installation directory and port resolution for `TestCluster`.
//!
//! Handles refreshing the installation directory and port after worker setup,
//! as well as reading `postmaster.pid` for port discovery.

use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};
use color_eyre::eyre::eyre;
use postgresql_embedded::Settings;
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

/// Implementation for refreshing worker port with a generic retry function.
pub(super) fn refresh_worker_port_impl<F, R>(
    bootstrap: &mut TestBootstrapSettings,
    retry_fn: F,
) -> BootstrapResult<()>
where
    F: FnOnce(&Path) -> R,
    R: Into<BootstrapResult<Option<u16>>>,
{
    if bootstrap.privileges != ExecutionPrivileges::Root {
        return Ok(());
    }

    let pid_path = bootstrap.settings.data_dir.join("postmaster.pid");
    if let Some(port) = retry_fn(&pid_path).into()? {
        bootstrap.settings.port = port;
        return Ok(());
    }

    tracing::debug!(
        target: LOG_TARGET,
        path = %pid_path.display(),
        "postmaster.pid missing after start; keeping configured port"
    );
    Ok(())
}

/// Refreshes the worker port (synchronous variant).
pub(super) fn refresh_worker_port(bootstrap: &mut TestBootstrapSettings) -> BootstrapResult<()> {
    refresh_worker_port_impl(bootstrap, read_postmaster_port_with_retry)
}

/// Refreshes the worker port (async variant).
#[cfg(feature = "async-api")]
pub(super) async fn refresh_worker_port_async(
    bootstrap: &mut TestBootstrapSettings,
) -> BootstrapResult<()> {
    let pid_path = bootstrap.settings.data_dir.join("postmaster.pid");
    let result = read_postmaster_port_with_retry_async(&pid_path).await;
    refresh_worker_port_impl(bootstrap, |_| result)
}

/// Implements the retry loop for reading the postmaster port.
///
/// This macro unifies the sync and async retry logic, accepting either
/// `std::thread::sleep()` or `tokio::time::sleep().await` as the sleep expression.
macro_rules! read_postmaster_port_with_retry_impl {
    ($pid_path:expr, $sleep:expr) => {{
        for _ in 0..POSTMASTER_PORT_ATTEMPTS {
            if let Some(port) = read_postmaster_port($pid_path)? {
                return Ok(Some(port));
            }
            $sleep;
        }
        Ok(None)
    }};
}

/// Reads the postmaster port with retry (synchronous).
pub(super) fn read_postmaster_port_with_retry(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    read_postmaster_port_with_retry_impl!(pid_path, std::thread::sleep(POSTMASTER_PORT_DELAY))
}

/// Reads the postmaster port with retry (async).
#[cfg(feature = "async-api")]
pub(super) async fn read_postmaster_port_with_retry_async(
    pid_path: &Path,
) -> BootstrapResult<Option<u16>> {
    read_postmaster_port_with_retry_impl!(pid_path, tokio::time::sleep(POSTMASTER_PORT_DELAY).await)
}

/// Reads the port from a postmaster.pid file.
fn read_postmaster_port(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    let contents = match std::fs::read_to_string(pid_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(err) => {
            return Err(crate::error::BootstrapError::from(eyre!(
                "failed to read postmaster pid at {}: {err}",
                pid_path.display()
            )));
        }
    };
    let port_line = contents.lines().nth(3).ok_or_else(|| {
        crate::error::BootstrapError::from(eyre!(
            "postmaster.pid missing port line at {}",
            pid_path.display()
        ))
    })?;
    let port = port_line.trim().parse::<u16>().map_err(|err| {
        crate::error::BootstrapError::from(eyre!(
            "failed to parse postmaster port from {}: {err}",
            pid_path.display()
        ))
    })?;
    Ok(Some(port))
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
    candidates.sort();
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
