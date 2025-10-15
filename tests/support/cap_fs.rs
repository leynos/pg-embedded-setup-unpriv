//! Capability-based filesystem helpers for tests.

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Metadata, Permissions, PermissionsExt},
};
use color_eyre::eyre::{Context, Result};

/// Splits an absolute or relative path into a capability directory and the relative path.
///
/// Absolute paths are rebased under the ambient root directory. Relative paths reuse the
/// current working directory.
pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    if path.has_root() {
        let stripped = path
            .strip_prefix("/")
            .map(Utf8Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf());
        let dir = Dir::open_ambient_dir("/", ambient_authority())
            .context("open ambient root directory")?;
        Ok((dir, stripped))
    } else {
        let dir = Dir::open_ambient_dir(".", ambient_authority())
            .context("open ambient working directory")?;
        Ok((dir, path.to_path_buf()))
    }
}

/// Ensures a directory exists with the provided permissions mode.
///
/// A missing directory is created recursively; existing directories have their permissions
/// re-applied to guarantee consistency across repeated calls.
#[allow(dead_code)] // Integration tests compile this module per crate; some suites use only subsets.
pub fn ensure_dir(path: &Utf8Path, mode: u32) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    dir.create_dir_all(relative.as_std_path())
        .with_context(|| format!("create {}", path))?;
    dir.set_permissions(relative.as_std_path(), Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", path))?;
    Ok(())
}

/// Applies the provided POSIX mode to the path when it exists.
#[allow(dead_code)] // Integration tests compile this module per crate; some suites use only subsets.
pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    dir.set_permissions(relative.as_std_path(), Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", path))
}

/// Removes a directory tree when present, ignoring `NotFound` errors.
pub fn remove_tree(path: &Utf8Path) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    match dir.remove_dir_all(relative.as_std_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path)),
    }
}

/// Retrieves metadata for the path using capability APIs.
#[allow(dead_code)] // Integration tests compile this module per crate; some suites use only subsets.
pub fn metadata(path: &Utf8Path) -> std::io::Result<Metadata> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| std::io::Error::other(err.to_string()))?;
    if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    }
}

/// Opens a capability directory handle to the specified path.
#[allow(dead_code)] // Integration tests compile this module per crate; some suites use only subsets.
pub fn open_dir(path: &Utf8Path) -> Result<Dir> {
    Dir::open_ambient_dir(path.as_std_path(), ambient_authority())
        .with_context(|| format!("open {}", path))
}
