use camino::Utf8PathBuf;

use crate::error::{BootstrapError, BootstrapResult};

#[cfg(unix)]
use nix::unistd::geteuid;

/// Represents the privileges the process is running with when bootstrapping `PostgreSQL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPrivileges {
    /// The process owns `root` privileges and must drop to `nobody` for filesystem work.
    Root,
    /// The process is already unprivileged, so bootstrap tasks run with the current UID/GID.
    Unprivileged,
}

/// Selects how `PostgreSQL` lifecycle commands run when privileged execution is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Execute lifecycle commands directly within the current process.
    ///
    /// This mode is only appropriate when the process already runs without elevated privileges.
    InProcess,
    /// Delegate lifecycle commands to a helper subprocess executed with reduced privileges.
    Subprocess,
}

#[must_use]
pub fn detect_execution_privileges() -> ExecutionPrivileges {
    #[cfg(unix)]
    {
        if geteuid().is_root() {
            ExecutionPrivileges::Root
        } else {
            ExecutionPrivileges::Unprivileged
        }
    }

    #[cfg(not(unix))]
    {
        ExecutionPrivileges::Unprivileged
    }
}

pub(super) fn determine_execution_mode(
    privileges: ExecutionPrivileges,
    worker_binary: Option<&Utf8PathBuf>,
) -> BootstrapResult<ExecutionMode> {
    #[cfg(unix)]
    {
        match privileges {
            ExecutionPrivileges::Root => {
                if worker_binary.is_none() {
                    Err(BootstrapError::from(color_eyre::eyre::eyre!(
                        "PG_EMBEDDED_WORKER must be set when running with root privileges"
                    )))
                } else {
                    Ok(ExecutionMode::Subprocess)
                }
            }
            ExecutionPrivileges::Unprivileged => Ok(ExecutionMode::InProcess),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = worker_binary;
        let _ = privileges;
        Ok(ExecutionMode::InProcess)
    }
}
