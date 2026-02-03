//! Shared directory removal helpers with safety guards.

use std::io::ErrorKind;
use std::path::{Component, Path};

/// Records the outcome of a guarded directory removal attempt.
///
/// # Examples
/// ```rust,ignore
/// use std::path::Path;
///
/// let path = Path::new("/tmp/example");
/// let outcome = pg_embedded_setup_unpriv::cleanup_helpers::try_remove_dir_all(path);
/// assert!(outcome.is_ok());
/// ```
#[derive(Clone, Copy, Debug)]
pub(crate) enum RemovalOutcome {
    Removed,
    Missing,
}

/// Checks whether a path contains any parent-directory (`..`) components.
///
/// Args:
/// - path: `&Path` to inspect for `Component::ParentDir` entries.
///
/// Returns `true` when any parent-dir component is present.
///
/// This is the canonical helper used by cleanup code to detect upward
/// traversal in paths.
pub(crate) fn has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Attempts to remove a directory tree, rejecting unsafe paths before deletion.
///
/// # Examples
/// ```rust,ignore
/// use std::path::Path;
///
/// let path = Path::new("/tmp/example");
/// let _ = pg_embedded_setup_unpriv::cleanup_helpers::try_remove_dir_all(path);
/// ```
pub(crate) fn try_remove_dir_all(path: &Path) -> Result<RemovalOutcome, std::io::Error> {
    guard_removal_path(path)?;
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(RemovalOutcome::Removed),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(RemovalOutcome::Missing),
        Err(err) => Err(err),
    }
}

fn guard_removal_path(path: &Path) -> Result<(), std::io::Error> {
    if is_empty_or_root(path) || has_parent_dir(path) {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("refuse to remove unsafe path {}", path.display()),
        ));
    }
    Ok(())
}

fn is_empty_or_root(path: &Path) -> bool {
    let mut components = path.components();
    match components.next() {
        None => true,
        Some(Component::CurDir | Component::RootDir) => components.next().is_none(),
        Some(Component::Prefix(_)) => match components.next() {
            None => true,
            Some(Component::RootDir) => components.next().is_none(),
            _ => false,
        },
        _ => false,
    }
}
