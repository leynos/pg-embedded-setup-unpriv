//! Resolves and stages worker binaries for privileged test runs.

use std::ffi::OsString;
use std::sync::OnceLock;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::{fs, io};

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
    static WORKER_PATH: OnceLock<Option<OsString>> = OnceLock::new();
    WORKER_PATH
        .get_or_init(|| {
            let original =
                std::env::var_os("CARGO_BIN_EXE_pg_worker").or_else(locate_worker_binary)?;
            Some(stage_worker_binary(original))
        })
        .clone()
}

fn stage_worker_binary(original: OsString) -> OsString {
    #[cfg(unix)]
    if let Ok(path) = try_stage_worker_binary(&original) {
        return path;
    }

    original
}

/// Stages the worker binary to a stable location in the target directory.
///
/// Unlike a temp directory, this location persists across test runs and doesn't
/// require cleanup. The staged binary is only updated if the source is newer.
#[cfg(unix)]
fn try_stage_worker_binary(original: &OsString) -> io::Result<OsString> {
    let source = PathBuf::from(original);
    let filename = source.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "worker path missing filename")
    })?;

    // Find the target directory from the source binary's location.
    // Source is typically target/debug/deps/pg_worker-<hash> or target/debug/pg_worker
    let staged_dir = find_staging_directory(&source)?;
    fs::create_dir_all(&staged_dir)?;

    // Set directory to world-executable so privilege-dropped subprocesses can access it.
    let mut dir_perms = fs::metadata(&staged_dir)?.permissions();
    dir_perms.set_mode(0o755);
    fs::set_permissions(&staged_dir, dir_perms)?;

    let staged = staged_dir.join(filename);

    // Only copy if source is newer than staged (or staged doesn't exist).
    if should_restage(&source, &staged)? {
        fs::copy(&source, &staged)?;
        let mut perms = fs::metadata(&staged)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staged, perms)?;
    }

    Ok(staged.into_os_string())
}

/// Finds the staging directory for the worker binary.
///
/// Returns `target/{profile}/pg_worker_staged/` based on the source binary path.
#[cfg(unix)]
fn find_staging_directory(source: &std::path::Path) -> io::Result<PathBuf> {
    // Walk up from source to find the target directory.
    // Source is typically: target/debug/pg_worker or target/debug/deps/pg_worker-<hash>
    let mut current = source.parent();
    while let Some(dir) = current {
        let dir_name = dir.file_name().and_then(|n| n.to_str());
        // Look for "debug" or "release" profile directories
        if matches!(dir_name, Some("debug" | "release")) {
            return Ok(dir.join("pg_worker_staged"));
        }
        // Also check for "deps" directory (one level up is the profile dir)
        if dir_name == Some("deps") {
            if let Some(profile_dir) = dir.parent() {
                return Ok(profile_dir.join("pg_worker_staged"));
            }
        }
        current = dir.parent();
    }

    // Fallback: use source directory with a staged subdirectory
    source
        .parent()
        .map(|p| p.join("pg_worker_staged"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot find staging directory"))
}

/// Returns true if the staged binary needs to be updated.
#[cfg(unix)]
fn should_restage(source: &std::path::Path, staged: &std::path::Path) -> io::Result<bool> {
    let staged_meta = match fs::metadata(staged) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    };

    let source_meta = fs::metadata(source)?;
    let source_mtime = source_meta.modified()?;
    let staged_mtime = staged_meta.modified()?;

    Ok(source_mtime > staged_mtime)
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

        let staged = try_stage_worker_binary(&source.into_os_string()).expect("stage worker");
        let mode = fs::metadata(&staged)
            .expect("staged metadata")
            .permissions()
            .mode();
        assert!(
            mode & 0o001 != 0,
            "staged worker should be executable by others"
        );

        // Clean up the staged directory (it's in the temp dir's pg_worker_staged subdir)
        let staged_path = PathBuf::from(&staged);
        if let Some(staged_dir) = staged_path.parent() {
            drop(fs::remove_dir_all(staged_dir));
        }
    }

    #[cfg(unix)]
    #[test]
    fn find_staging_directory_detects_debug_profile() {
        let path = PathBuf::from("/project/target/debug/deps/pg_worker-abc123");
        let staged_dir = find_staging_directory(&path).expect("find staging dir");
        assert_eq!(
            staged_dir,
            PathBuf::from("/project/target/debug/pg_worker_staged")
        );
    }

    #[cfg(unix)]
    #[test]
    fn find_staging_directory_detects_release_profile() {
        let path = PathBuf::from("/project/target/release/pg_worker");
        let staged_dir = find_staging_directory(&path).expect("find staging dir");
        assert_eq!(
            staged_dir,
            PathBuf::from("/project/target/release/pg_worker_staged")
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_restage_returns_true_for_missing_staged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        fs::write(&source, b"content").expect("write source");
        let staged = dir.path().join("staged");

        assert!(should_restage(&source, &staged).expect("should_restage"));
    }

    #[cfg(unix)]
    #[test]
    fn should_restage_returns_false_when_staged_is_newer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let staged = dir.path().join("staged");

        fs::write(&source, b"content").expect("write source");
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&staged, b"content").expect("write staged");

        assert!(!should_restage(&source, &staged).expect("should_restage"));
    }
}
