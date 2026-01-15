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
        .with_context(|| format!("open {path}"))
}

/// Removes a directory tree when present, ignoring `NotFound` errors.
///
/// If the parent directory does not exist, the target cannot exist either,
/// so this returns `Ok(())` in that case.
pub fn remove_tree(path: &Utf8Path) -> Result<()> {
    let (dir, relative) = match ambient_dir_and_path(path) {
        Ok(result) => result,
        Err(err) if is_not_found(&err) => return Ok(()),
        Err(err) => return Err(err),
    };
    if relative.as_str().is_empty() {
        return Ok(());
    }

    match dir.remove_dir_all(relative.as_std_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {path}")),
    }
}

/// Checks whether an eyre error chain contains a `NotFound` IO error.
fn is_not_found(err: &color_eyre::Report) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|io_err| io_err.kind() == std::io::ErrorKind::NotFound)
}
