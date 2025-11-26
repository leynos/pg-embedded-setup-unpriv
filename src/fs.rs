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
#[expect(
    clippy::cognitive_complexity,
    reason = "observability logging and capability checks require explicit branching"
)]
pub(crate) fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    let span = info_span!(target: LOG_TARGET, "ensure_dir_exists", path = %path);
    let _entered = span.enter();
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    match dir.create_dir_all(relative.as_std_path()) {
        Ok(()) => {
            info!(target: LOG_TARGET, path = %path, "ensured directory exists");
            Ok(())
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => ensure_existing_path_is_dir(path),
        Err(err) => {
            error!(
                target: LOG_TARGET,
                path = %path,
                error = %err,
                "failed to ensure directory exists"
            );
            Err(err).with_context(|| format!("create {}", path.as_str()))
        }
    }
}

/// Applies the provided POSIX mode to the given path when it exists.
#[expect(
    clippy::cognitive_complexity,
    reason = "observability logging and capability checks require explicit branching"
)]
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

    match dir.set_permissions(relative.as_std_path(), Permissions::from_mode(mode)) {
        Ok(()) => {
            info!(
                target: LOG_TARGET,
                path = %path,
                mode_octal = format_args!("{mode:o}"),
                "applied permissions"
            );
            Ok(())
        }
        Err(err) => {
            error!(
                target: LOG_TARGET,
                path = %path,
                mode_octal = format_args!("{mode:o}"),
                error = %err,
                "failed to apply permissions"
            );
            Err(err).with_context(|| format!("chmod {}", path.as_str()))
        }
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "metadata inspection plus logging branches beyond clippy threshold"
)]
fn ensure_existing_path_is_dir(path: &Utf8Path) -> Result<()> {
    match std::fs::metadata(path.as_std_path()) {
        Ok(metadata) if metadata.is_dir() => {
            info!(target: LOG_TARGET, path = %path, "directory already existed");
            Ok(())
        }
        Ok(_) => {
            let err = std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!("{path} exists but is not a directory"),
            );
            error!(
                target: LOG_TARGET,
                path = %path,
                error = %err,
                "failed to ensure directory exists"
            );
            Err(err).with_context(|| format!("create {}", path.as_str()))
        }
        Err(meta_err) => {
            error!(
                target: LOG_TARGET,
                path = %path,
                error = %meta_err,
                "failed to ensure directory exists"
            );
            Err(meta_err).with_context(|| format!("create {}", path.as_str()))
        }
    }
}
