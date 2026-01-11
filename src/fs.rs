//! Shared filesystem helpers that operate within the capability sandbox.

use crate::observability::LOG_TARGET;
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Metadata, Permissions, PermissionsExt},
};
use color_eyre::eyre::{Context, Result};
use std::io::ErrorKind;
use tracing::{error, info, info_span};

/// Resolves a path to an ambient directory handle paired with the relative path component.
///
/// Absolute paths are opened relative to their parent directory; relative paths reuse the current
/// working directory.
pub(crate) fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    if path.has_root() {
        let (dir_path, relative) = match path.parent() {
            Some(parent) => {
                let relative = path
                    .strip_prefix(parent)
                    .with_context(|| {
                        format!("strip parent {} from {}", parent.as_str(), path.as_str())
                    })?
                    .to_path_buf();
                (parent, relative)
            }
            None => (path, Utf8PathBuf::new()),
        };
        let dir = Dir::open_ambient_dir(dir_path.as_std_path(), ambient_authority())
            .context("open ambient directory for absolute path")?;
        Ok((dir, relative))
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
    ensure_dir_exists_inner(path, &dir, &relative)
}

fn ensure_dir_exists_inner(path: &Utf8Path, dir: &Dir, relative: &Utf8PathBuf) -> Result<()> {
    if relative.as_str().is_empty() {
        return Ok(());
    }

    match dir.create_dir_all(relative.as_std_path()) {
        Ok(()) => {
            log_dir_created(path);
            Ok(())
        }
        Err(err) => handle_dir_creation_error(path, err),
    }
}

fn log_dir_created(path: &Utf8Path) {
    info!(target: LOG_TARGET, path = %path, "ensured directory exists");
}

fn handle_dir_creation_error(path: &Utf8Path, err: std::io::Error) -> Result<()> {
    if err.kind() == ErrorKind::AlreadyExists {
        return ensure_existing_path_is_dir(path);
    }

    error!(
        target: LOG_TARGET,
        path = %path,
        error = %err,
        "failed to ensure directory exists"
    );
    Err(err).with_context(|| format!("create {}", path.as_str()))
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
    set_permissions_inner(path, mode, &dir, &relative)
}

fn set_permissions_inner(
    path: &Utf8Path,
    mode: u32,
    dir: &Dir,
    relative: &Utf8PathBuf,
) -> Result<()> {
    if relative.as_str().is_empty() {
        return Ok(());
    }

    match dir.set_permissions(relative.as_std_path(), Permissions::from_mode(mode)) {
        Ok(()) => {
            log_permissions_applied(path, mode);
            Ok(())
        }
        Err(err) => handle_permission_error(path, mode, err),
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

fn handle_permission_error(path: &Utf8Path, mode: u32, err: std::io::Error) -> Result<()> {
    error!(
        target: LOG_TARGET,
        path = %path,
        mode_octal = format_args!("{mode:o}"),
        error = %err,
        "failed to apply permissions"
    );
    Err(err).with_context(|| format!("chmod {}", path.as_str()))
}

fn ensure_existing_path_is_dir(path: &Utf8Path) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    let metadata_result = if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    };

    match metadata_result {
        Ok(metadata) => handle_existing_metadata(path, &metadata),
        Err(err) => Err(log_dir_metadata_error(path, err))
            .with_context(|| format!("create {}", path.as_str())),
    }
}

fn handle_existing_metadata(path: &Utf8Path, metadata: &Metadata) -> Result<()> {
    if metadata.is_dir() {
        info!(target: LOG_TARGET, path = %path, "directory already existed");
        Ok(())
    } else {
        let err = std::io::Error::new(
            ErrorKind::AlreadyExists,
            format!("{path} exists but is not a directory"),
        );
        Err(log_dir_metadata_error(path, err)).with_context(|| format!("create {}", path.as_str()))
    }
}

fn log_dir_metadata_error(path: &Utf8Path, err: std::io::Error) -> std::io::Error {
    error!(
        target: LOG_TARGET,
        path = %path,
        error = %err,
        "failed to ensure directory exists"
    );
    err
}

#[cfg(test)]
mod tests {
    use super::ensure_existing_path_is_dir;
    use camino::{Utf8Path, Utf8PathBuf};
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn ensure_existing_path_is_dir_accepts_directory() {
        let temp = tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");

        ensure_existing_path_is_dir(&path).expect("existing directory should be accepted");
    }

    #[test]
    fn ensure_existing_path_is_dir_rejects_files() {
        let temp = tempdir().expect("tempdir");
        let file_path = temp.path().join("file");
        File::create(&file_path).expect("create file");
        let path = Utf8PathBuf::from_path_buf(file_path).expect("utf8 file path");

        let err = ensure_existing_path_is_dir(&path).expect_err("file path should not be accepted");
        let message = err
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        assert!(
            message.contains("exists but is not a directory"),
            "unexpected error chain: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_existing_path_is_dir_allows_ambient_root() {
        ensure_existing_path_is_dir(Utf8Path::new("/"))
            .expect("ambient root should be treated as a directory");
    }
}
