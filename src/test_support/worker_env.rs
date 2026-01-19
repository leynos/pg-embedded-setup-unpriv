//! Resolves and stages worker binaries for privileged test runs.

use std::ffi::OsString;
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::{fs, io};

use tempfile::TempDir;

/// Returns the worker binary path staged for privileged test execution.
///
/// The path is resolved once per process and, on Unix, staged into a
/// world-executable location so privilege-dropped subprocesses can launch it.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::worker_binary_for_tests;
///
/// let worker = worker_binary_for_tests();
/// # let _ = worker;
/// ```
#[must_use]
pub fn worker_binary_for_tests() -> Option<OsString> {
    worker_binary()
}

pub(super) fn worker_binary() -> Option<OsString> {
    static WORKER_PATH: OnceLock<Option<(OsString, Option<TempDir>)>> = OnceLock::new();
    WORKER_PATH
        .get_or_init(|| {
            let original =
                std::env::var_os("CARGO_BIN_EXE_pg_worker").or_else(locate_worker_binary)?;
            Some(stage_worker_binary(original))
        })
        .as_ref()
        .map(|(path, _)| path.clone())
}

fn stage_worker_binary(original: OsString) -> (OsString, Option<TempDir>) {
    #[cfg(unix)]
    if let Ok((path, tempdir)) = try_stage_worker_binary(&original) {
        return (path, Some(tempdir));
    }

    (original, None)
}

#[cfg(unix)]
fn try_stage_worker_binary(original: &OsString) -> io::Result<(OsString, TempDir)> {
    let source = PathBuf::from(original);
    let filename = source.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "worker path missing filename")
    })?;
    let tempdir = tempfile::tempdir()?;
    let mut dir_perms = fs::metadata(tempdir.path())?.permissions();
    dir_perms.set_mode(0o755);
    fs::set_permissions(tempdir.path(), dir_perms)?;
    let staged = tempdir.path().join(filename);
    fs::copy(&source, &staged)?;
    let mut perms = fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&staged, perms)?;
    Ok((staged.into_os_string(), tempdir))
}

fn locate_worker_binary() -> Option<OsString> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_dir = deps_dir.parent()?;
    let worker_path = target_dir.join("pg_worker");
    if worker_path.is_file() {
        return Some(worker_path.into_os_string());
    }

    let entries = std::fs::read_dir(deps_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_worker_binary(&path) {
            return Some(path.into_os_string());
        }
    }

    None
}

fn is_worker_binary(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    if !name.starts_with("pg_worker") {
        return false;
    }

    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("d"))
    {
        return false;
    }

    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn staged_worker_is_world_executable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("pg_worker");
        fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("write worker");
        let mut perms = fs::metadata(&source).expect("metadata").permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&source, perms).expect("set perms");

        let (staged, _guard) =
            try_stage_worker_binary(&source.into_os_string()).expect("stage worker");
        let mode = fs::metadata(&staged)
            .expect("staged metadata")
            .permissions()
            .mode();
        assert!(
            mode & 0o001 != 0,
            "staged worker should be executable by others"
        );
    }
}
