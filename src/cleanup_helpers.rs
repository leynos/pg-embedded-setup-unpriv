//! Shared directory removal helpers with safety guards.

use std::io::ErrorKind;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug)]
pub(crate) enum RemovalOutcome {
    Removed,
    Missing,
}

pub(crate) fn try_remove_dir_all(path: &Path) -> Result<RemovalOutcome, std::io::Error> {
    guard_removal_path(path)?;
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(RemovalOutcome::Removed),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(RemovalOutcome::Missing),
        Err(err) => Err(err),
    }
}

fn guard_removal_path(path: &Path) -> Result<(), std::io::Error> {
    if is_empty_or_root(path) {
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
