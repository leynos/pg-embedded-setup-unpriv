//! Shared filesystem helpers that operate within the capability sandbox.

use crate::observability::LOG_TARGET;
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Permissions, PermissionsExt},
};
use color_eyre::eyre::{Context, Result};
use std::io::ErrorKind;
use tracing::{error, info, info_span};

/// Resolves a path to an ambient directory handle paired with the relative path component.
///
/// Absolute paths are opened relative to the ambient root; relative paths reuse the current
/// working directory.
pub(crate) fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    if path.has_root() {
        let stripped = path
            .strip_prefix("/")
            .map_or_else(|_| path.to_path_buf(), Utf8Path::to_path_buf);
        let dir = Dir::open_ambient_dir("/", ambient_authority())
            .context("open ambient root directory")?;
        Ok((dir, stripped))
    } else {
        let dir = Dir::open_ambient_dir(".", ambient_authority())
            .context("open ambient working directory")?;
        Ok((dir, path.to_path_buf()))
    }
}

/// Ensures the provided path exists, creating intermediate directories when required.
pub(crate) fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    let span = info_span!(target: LOG_TARGET, "ensure_dir_exists", path = %path);
    let _entered = span.enter();
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    let creation_result = dir.create_dir_all(relative.as_std_path());
    handle_dir_creation(path, creation_result)
}

/// Applies the provided POSIX mode to the given path when it exists.
pub(crate) fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    let span = info_span!(
        target: LOG_TARGET,
        "set_permissions",
        path = %path,
        mode_octal = format_args!("{mode:o}")
    );
    let _entered = span.enter();
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    dir.set_permissions(relative.as_std_path(), Permissions::from_mode(mode))
        .map(|()| log_permissions_applied(path, mode))
        .map_err(|err| log_permission_error(path, mode, err))
        .with_context(|| format!("chmod {}", path.as_str()))
}

fn handle_dir_creation(path: &Utf8Path, result: std::io::Result<()>) -> Result<()> {
    match result {
        Ok(()) => {
            log_dir_created(path);
            Ok(())
        }
        Err(err) => handle_dir_error(path, err),
    }
}

fn log_permissions_applied(path: &Utf8Path, mode: u32) {
    info!(
        target: LOG_TARGET,
        path = %path,
        mode_octal = format_args!("{mode:o}"),
        "applied permissions"
    );
}

fn log_permission_error(path: &Utf8Path, mode: u32, err: std::io::Error) -> std::io::Error {
    error!(
        target: LOG_TARGET,
        path = %path,
        mode_octal = format_args!("{mode:o}"),
        error = %err,
        "failed to apply permissions"
    );
    err
}

fn log_dir_created(path: &Utf8Path) {
    info!(target: LOG_TARGET, path = %path, "ensured directory exists");
}

fn handle_dir_error(path: &Utf8Path, err: std::io::Error) -> Result<()> {
    if err.kind() == ErrorKind::AlreadyExists {
        return handle_existing_path(path);
    }

    Err(log_dir_creation_failure(path, err)).with_context(|| format!("create {}", path.as_str()))
}

fn handle_existing_path(path: &Utf8Path) -> Result<()> {
    match std::fs::metadata(path.as_std_path()) {
        Ok(metadata) if metadata.is_dir() => {
            log_dir_exists(path);
            Ok(())
        }
        Ok(_) => Err(log_dir_creation_failure(
            path,
            std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!("{path} exists but is not a directory"),
            ),
        ))
        .with_context(|| format!("create {}", path.as_str())),
        Err(meta_err) => Err(log_dir_creation_failure(path, meta_err))
            .with_context(|| format!("create {}", path.as_str())),
    }
}

fn log_dir_exists(path: &Utf8Path) {
    info!(target: LOG_TARGET, path = %path, "directory already existed");
}

fn log_dir_creation_failure(path: &Utf8Path, err: std::io::Error) -> std::io::Error {
    error!(
        target: LOG_TARGET,
        path = %path,
        error = %err,
        "failed to ensure directory exists"
    );
    err
}
