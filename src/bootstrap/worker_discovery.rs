//! Worker binary discovery and validation logic.
//!
//! Provides utilities for locating and validating `pg_worker` binary
//! used during privileged bootstrap operations.

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Report;
use std::io::ErrorKind;

use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};
use crate::fs::ambient_dir_and_path;

#[cfg(unix)]
use cap_std::fs::PermissionsExt;

pub(super) fn worker_binary_from_env(
    worker_name: Option<&str>,
) -> BootstrapResult<Option<Utf8PathBuf>> {
    let name = worker_name.unwrap_or("pg_worker");

    if let Some(raw) = std::env::var_os("PG_EMBEDDED_WORKER") {
        let path = parse_worker_path_from_env(&raw)?;
        validate_worker_binary(&path)?;
        return Ok(Some(path));
    }

    if let Some(path) = discover_worker_from_path(name)? {
        validate_worker_binary(&path)?;
        return Ok(Some(path));
    }

    Ok(None)
}

pub(crate) fn parse_worker_path_from_env(raw: &std::ffi::OsStr) -> BootstrapResult<Utf8PathBuf> {
    let path = Utf8PathBuf::from_path_buf(std::path::PathBuf::from(raw)).map_err(|_| {
        let invalid_value = raw.to_string_lossy().to_string();
        let msg = format!(
            "PG_EMBEDDED_WORKER contains a non-UTF-8 value: {invalid_value:?}. Provide a UTF-8 encoded absolute path to the worker binary."
        );
        BootstrapError::from(color_eyre::eyre::eyre!(msg))
    })?;

    if path.as_str().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must not be empty"
        )));
    }
    if path.as_str() == "/" {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must not point at the filesystem root"
        )));
    }

    Ok(path)
}

fn try_find_worker_in_directory(
    dir: std::path::PathBuf,
    worker_name: &str,
) -> BootstrapResult<Option<Utf8PathBuf>> {
    let dir_str = Utf8PathBuf::from_path_buf(dir).map_err(|non_utf8_dir| {
        let invalid_dir = non_utf8_dir.to_string_lossy().to_string();
        let msg = format!(
            "PATH contains non-UTF-8 directory: {invalid_dir:?}. Provide UTF-8 encoded paths or set PG_EMBEDDED_WORKER explicitly."
        );
        BootstrapError::from(color_eyre::eyre::eyre!(msg))
    })?;

    if !is_trusted_path_directory(dir_str.as_std_path()) {
        return Ok(None);
    }

    let candidate = dir_str.join(worker_name);
    #[cfg(windows)]
    let candidate = candidate.with_extension("exe");

    if is_executable(candidate.as_std_path()) {
        Ok(Some(candidate))
    } else {
        Ok(None)
    }
}

pub(crate) fn discover_worker_from_path(worker_name: &str) -> BootstrapResult<Option<Utf8PathBuf>> {
    let Some(path_var) = std::env::var_os("PATH") else {
        return Ok(None);
    };

    for dir in std::env::split_paths(&path_var) {
        match try_find_worker_in_directory(dir, worker_name) {
            Ok(Some(worker)) => return Ok(Some(worker)),
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }

    Ok(None)
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

#[cfg(unix)]
pub(crate) fn is_trusted_path_directory(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !dir.is_absolute() {
        return false;
    }
    std::fs::metadata(dir)
        .map(|m| m.is_dir() && (m.permissions().mode() & 0o002) == 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn is_trusted_path_directory(dir: &std::path::Path) -> bool {
    dir.is_absolute()
}

fn validate_worker_binary(path: &Utf8PathBuf) -> BootstrapResult<()> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| worker_binary_error(path, err))?;
    let metadata = dir
        .metadata(relative.as_std_path())
        .map_err(|err| worker_binary_error(path, Report::new(err)))?;

    if !metadata.is_file() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must reference a regular file: {path}"
        )));
    }

    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "PG_EMBEDDED_WORKER must be executable: {path}"
            )));
        }
    }

    Ok(())
}

fn worker_binary_error(path: &Utf8Path, err: Report) -> BootstrapError {
    let is_not_found = error_chain_has_not_found(&err);
    let context = format!("failed to access PG_EMBEDDED_WORKER at {path}: {err}");
    let report = err.wrap_err(context);

    if is_not_found {
        BootstrapError::new(BootstrapErrorKind::WorkerBinaryMissing, report)
    } else {
        BootstrapError::from(report)
    }
}

fn error_chain_has_not_found(err: &Report) -> bool {
    err.chain()
        .filter_map(|source| source.downcast_ref::<std::io::Error>())
        .any(|source| source.kind() == ErrorKind::NotFound)
}
