//! Refresh worker-managed cluster ports from `postmaster.pid`.

use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::eyre;

use crate::error::{BootstrapError, BootstrapResult};
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};

pub(crate) const POSTMASTER_PORT_ATTEMPTS: usize = 10;
pub(crate) const POSTMASTER_PORT_DELAY: Duration = Duration::from_millis(100);

pub(crate) fn refresh_worker_port_impl<F, R>(
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

pub(crate) fn refresh_worker_port(bootstrap: &mut TestBootstrapSettings) -> BootstrapResult<()> {
    refresh_worker_port_impl(bootstrap, read_postmaster_port_with_retry)
}

#[cfg(feature = "async-api")]
pub(crate) async fn refresh_worker_port_async(
    bootstrap: &mut TestBootstrapSettings,
) -> BootstrapResult<()> {
    if bootstrap.privileges != ExecutionPrivileges::Root {
        return Ok(());
    }

    let pid_path = bootstrap.settings.data_dir.join("postmaster.pid");
    if let Some(port) = read_postmaster_port_with_retry_async(&pid_path).await? {
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

pub(crate) fn read_postmaster_port_with_retry(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    for _ in 0..POSTMASTER_PORT_ATTEMPTS {
        if let Some(port) = read_postmaster_port(pid_path)? {
            return Ok(Some(port));
        }
        std::thread::sleep(POSTMASTER_PORT_DELAY);
    }
    Ok(None)
}

#[cfg(feature = "async-api")]
pub(crate) async fn read_postmaster_port_with_retry_async(
    pid_path: &Path,
) -> BootstrapResult<Option<u16>> {
    for _ in 0..POSTMASTER_PORT_ATTEMPTS {
        if let Some(port) = read_postmaster_port(pid_path)? {
            return Ok(Some(port));
        }
        tokio::time::sleep(POSTMASTER_PORT_DELAY).await;
    }
    Ok(None)
}

pub(crate) fn read_postmaster_port(pid_path: &Path) -> BootstrapResult<Option<u16>> {
    let contents = match std::fs::read_to_string(pid_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(err) => {
            return Err(BootstrapError::from(eyre!(
                "failed to read postmaster pid at {}: {err}",
                pid_path.display()
            )));
        }
    };
    let Some(port_line) = contents.lines().nth(3) else {
        return Ok(None);
    };
    let port = port_line.trim().parse::<u16>().map_err(|err| {
        BootstrapError::from(eyre!(
            "failed to parse postmaster port from {}: {err}",
            pid_path.display()
        ))
    })?;
    Ok(Some(port))
}
