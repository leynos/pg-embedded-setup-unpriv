//! Worker binary discovery and validation logic.
//!
//! Provides utilities for locating and validating `pg_worker` binary
//! used during privileged bootstrap operations.

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Report;

use crate::error::{
    error_chain_contains_not_found, BootstrapError, BootstrapErrorKind, BootstrapResult,
};
use crate::fs::ambient_dir_and_path;

#[cfg(unix)]
use cap_std::fs::PermissionsExt;

/// Attempts to locate the worker binary using environment configuration.
///
/// Checks `PG_EMBEDDED_WORKER` environment variable first, then searches
/// `PATH` for a binary matching `worker_name`. Returns `None` if the worker
/// is not found.
///
/// # Arguments
///
/// * `worker_name` - Optional name of the worker binary to search for.
///   Defaults to `pg_worker` if not provided.
///
/// # Returns
///
/// * `Ok(Some(path))` - Path to the worker binary if found and validated.
/// * `Ok(None)` - Worker binary not found in environment or PATH.
/// * `Err(...)` - Error locating or validating the worker binary.
///
/// # Errors
///
/// Returns an error if:
/// * `PG_EMBEDDED_WORKER` is set but contains a non-UTF-8 value
/// * `PG_EMBEDDED_WORKER` is set but is not an absolute path
/// * `PG_EMBEDDED_WORKER` points to a non-existent location
/// * `PG_EMBEDDED_WORKER` points to a directory instead of a file
/// * `PG_EMBEDDED_WORKER` points to a non-executable file (Unix only)
/// * A PATH directory contains a non-UTF-8 name
/// * A PATH directory is world-writable and validation fails
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

/// Parses and validates a worker binary path from an environment variable value.
///
/// Validates that the path is UTF-8 encoded, non-empty, does not point to
/// the filesystem root, and is an absolute path.
///
/// # Arguments
///
/// * `raw` - Raw OS string from the environment variable.
///
/// # Returns
///
/// * `Ok(path)` - Validated UTF-8 absolute path.
/// * `Err(...)` - Error if the path is invalid.
///
/// # Errors
///
/// Returns an error if:
/// * The path contains non-UTF-8 bytes
/// * The path is empty
/// * The path is `/` (the filesystem root)
/// * The path is relative rather than absolute
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
    if !path.is_absolute() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must be an absolute path"
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

/// Searches for the worker binary in directories listed in the `PATH` environment variable.
///
/// Iterates through each directory in `PATH`, skipping entries that fail
/// validation (non-UTF-8, non-absolute, world-writable). Returns the first
/// executable worker binary found.
///
/// # Arguments
///
/// * `worker_name` - Name of the worker binary to search for.
///
/// # Returns
///
/// * `Ok(Some(path))` - Path to the worker binary if found.
/// * `Ok(None)` - Worker binary not found in any PATH directory.
/// * `Err(...)` - Error if a PATH directory cannot be processed.
///
/// # Errors
///
/// Returns an error if:
/// * A PATH directory name contains non-UTF-8 bytes
/// * Accessing filesystem metadata for a PATH directory fails
///
/// # Platform-specific behavior
///
/// * On Unix, searches for `worker_name` with the executable bit set
/// * On Windows, searches for `worker_name.exe`
/// * On Unix, skips world-writable directories for security
/// * On non-Unix platforms, only checks that the path is absolute
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
/// Validates that a PATH directory is absolute and not world-writable.
///
/// On Unix platforms, this ensures PATH entries are absolute paths and
/// do not allow arbitrary code execution via world-writable directories.
///
/// # Arguments
///
/// * `dir` - A directory path from PATH to validate.
///
/// # Returns
///
/// * `true` if the directory is absolute and not world-writable.
/// * `false` if the directory is relative or world-writable.
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
/// Validates that a PATH directory is absolute (non-Unix platforms).
///
/// On non-Unix platforms, this only ensures PATH entries are absolute paths.
/// Additional security validations may be required for specific platforms.
///
/// # Arguments
///
/// * `dir` - A directory path from PATH to validate.
///
/// # Returns
///
/// * `true` if the directory path is absolute.
/// * `false` if the directory path is relative.
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
    let is_not_found = error_chain_contains_not_found(&err);
    let context = format!("failed to access PG_EMBEDDED_WORKER at {path}: {err}");
    let report = err.wrap_err(context);

    if is_not_found {
        BootstrapError::new(BootstrapErrorKind::WorkerBinaryMissing, report)
    } else {
        BootstrapError::from(report)
    }
}
