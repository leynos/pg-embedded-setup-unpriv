//! Capability-based filesystem helpers for bootstrap integration tests.

use camino::Utf8Path;
use color_eyre::eyre::{Context, Result};

use pg_embedded_setup_unpriv::test_support::set_permissions as shared_set_permissions;
#[expect(
    unused_imports,
    reason = "Re-export capability helpers for bootstrap integration tests"
)]
pub use pg_embedded_setup_unpriv::test_support::{CapabilityTempDir, ambient_dir_and_path};

/// Splits an absolute or relative path into a capability directory and the relative path.
///
/// Absolute paths are rebased under the ambient root directory. Relative paths reuse the
/// current working directory.
/// Applies the provided POSIX mode to the path when it exists.
pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    shared_set_permissions(path, mode)
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
        Err(err) => Err(err).with_context(|| format!("remove {path}")),
    }
}
