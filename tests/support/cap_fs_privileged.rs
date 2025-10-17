//! Capability helpers used by privileged integration tests.

use camino::Utf8Path;
use cap_std::{ambient_authority, fs::Dir};
use color_eyre::eyre::{Context, Result};

pub use pg_embedded_setup_unpriv::test_support::ambient_dir_and_path;
use pg_embedded_setup_unpriv::test_support::{
    ensure_dir_exists as shared_ensure_dir_exists, set_permissions as shared_set_permissions,
};

/// Splits an absolute or relative path into a capability directory and the relative path.
/// Ensures a directory exists with the provided permissions mode.
pub fn ensure_dir(path: &Utf8Path, mode: u32) -> Result<()> {
    shared_ensure_dir_exists(path)?;
    shared_set_permissions(path, mode)
}

/// Opens a capability directory handle to the specified path.
pub fn open_dir(path: &Utf8Path) -> Result<Dir> {
    Dir::open_ambient_dir(path.as_std_path(), ambient_authority())
        .with_context(|| format!("open {}", path))
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
