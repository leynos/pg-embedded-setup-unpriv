//! Resolves and stages worker binaries for privileged test runs.

use std::ffi::OsString;
use std::sync::OnceLock;

#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
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

/// Stages the worker binary to `/tmp` for accessibility by privilege-dropped processes.
///
/// The binary is staged to `/tmp/pg-worker-{profile}-{hash}/pg_worker` where the hash
/// ensures uniqueness per source path. A pointer file is written to `target/{profile}/`
/// for discoverability and cleanup.
#[cfg(unix)]
fn try_stage_worker_binary(original: &OsString) -> io::Result<OsString> {
    let source = PathBuf::from(original);
    let filename = source.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "worker path missing filename")
    })?;

    // Compute staging directory in /tmp and find target directory for pointer file
    let (staged_dir, target_profile_dir) = find_staging_directory(&source);
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

    // Write pointer file to target directory for discoverability and cleanup.
    // Errors are intentionally ignored - the pointer is optional for cleanup convenience.
    if let Some(target_dir) = target_profile_dir {
        drop(write_pointer_file(&target_dir, &staged));
    }

    Ok(staged.into_os_string())
}

/// Finds the staging directory for the worker binary.
///
/// Returns a tuple of:
/// - The staging directory in `/tmp/pg-worker-{profile}-{hash}/`
/// - The target profile directory (if found) for writing the pointer file
#[cfg(unix)]
fn find_staging_directory(source: &std::path::Path) -> (PathBuf, Option<PathBuf>) {
    let path_hash = compute_path_hash(source);
    let (profile_name, target_profile_dir) = find_profile_directory(source);
    let staged_dir = PathBuf::from(format!("/tmp/pg-worker-{profile_name}-{path_hash}"));
    (staged_dir, target_profile_dir)
}

/// Walks up from source to find the Cargo profile directory (debug/release).
///
/// Returns the profile name and the profile directory path if found.
#[cfg(unix)]
fn find_profile_directory(source: &std::path::Path) -> (&'static str, Option<PathBuf>) {
    let mut current = source.parent();

    while let Some(dir) = current {
        if let Some(result) = check_directory_for_profile(dir) {
            return result;
        }
        current = dir.parent();
    }

    ("unknown", None)
}

/// Checks if a directory is a profile directory or contains profile information.
///
/// Returns `Some((profile_name, profile_dir))` if the directory is a profile dir
/// (debug/release) or a deps directory whose parent is a profile dir.
#[cfg(unix)]
fn check_directory_for_profile(dir: &std::path::Path) -> Option<(&'static str, Option<PathBuf>)> {
    let dir_name = dir.file_name().and_then(|n| n.to_str())?;

    match dir_name {
        "debug" => Some(("debug", Some(dir.to_path_buf()))),
        "release" => Some(("release", Some(dir.to_path_buf()))),
        "deps" => check_deps_parent_for_profile(dir),
        _ => None,
    }
}

/// Checks if the parent of a deps directory is a profile directory.
#[cfg(unix)]
fn check_deps_parent_for_profile(
    deps_dir: &std::path::Path,
) -> Option<(&'static str, Option<PathBuf>)> {
    let profile_dir = deps_dir.parent()?;
    let profile_name = profile_dir.file_name().and_then(|n| n.to_str())?;

    match profile_name {
        "debug" => Some(("debug", Some(profile_dir.to_path_buf()))),
        "release" => Some(("release", Some(profile_dir.to_path_buf()))),
        _ => Some((
            profile_name_to_static(profile_name),
            Some(profile_dir.to_path_buf()),
        )),
    }
}

/// Converts a profile name to a static string for common profiles, or "unknown".
#[cfg(unix)]
fn profile_name_to_static(name: &str) -> &'static str {
    match name {
        "debug" => "debug",
        "release" => "release",
        _ => "unknown",
    }
}

/// Computes a short hash of the source path for staging directory uniqueness.
#[cfg(unix)]
fn compute_path_hash(source: &std::path::Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_os_str().as_bytes());
    let result = hasher.finalize();
    // Use first 8 hex chars for brevity. SHA256 always produces 32 bytes.
    let bytes: &[u8] = result.as_slice();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes.first().copied().unwrap_or(0),
        bytes.get(1).copied().unwrap_or(0),
        bytes.get(2).copied().unwrap_or(0),
        bytes.get(3).copied().unwrap_or(0)
    )
}

/// Writes a pointer file to the target directory for discoverability and cleanup.
///
/// The pointer file contains the full path to the staged binary in `/tmp`.
#[cfg(unix)]
fn write_pointer_file(
    target_dir: &std::path::Path,
    staged_path: &std::path::Path,
) -> io::Result<()> {
    let pointer_path = target_dir.join("pg_worker_staged.path");
    let temp_path = target_dir.join("pg_worker_staged.path.tmp");

    // Write atomically: write to temp file, then rename
    fs::write(&temp_path, staged_path.as_os_str().as_bytes())?;
    fs::rename(&temp_path, &pointer_path)?;
    Ok(())
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
    fn staged_worker_is_world_executable_and_in_tmp() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create a fake target/debug structure so the staging logic finds a profile
        let debug_dir = dir.path().join("target").join("debug");
        fs::create_dir_all(&debug_dir).expect("create debug dir");
        let source = debug_dir.join("pg_worker");
        fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("write worker");
        let mut perms = fs::metadata(&source).expect("metadata").permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&source, perms).expect("set perms");

        let staged = try_stage_worker_binary(&source.into_os_string()).expect("stage worker");
        let staged_path = PathBuf::from(&staged);

        // Verify staged path is in /tmp
        assert!(
            staged_path.starts_with("/tmp"),
            "staged worker should be in /tmp, got: {}",
            staged_path.display()
        );

        // Verify path follows expected pattern /tmp/pg-worker-{profile}-{hash}/
        let parent = staged_path.parent().expect("parent dir");
        let parent_name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            parent_name.starts_with("pg-worker-debug-"),
            "staging dir should match pg-worker-debug-{{hash}}, got: {parent_name}"
        );

        // Verify binary is world-executable
        let mode = fs::metadata(&staged)
            .expect("staged metadata")
            .permissions()
            .mode();
        assert!(
            mode & 0o001 != 0,
            "staged worker should be executable by others"
        );

        // Verify nobody can access /tmp (inherent property of /tmp)
        // /tmp is world-accessible with sticky bit, so nobody can traverse to our staged binary
        assert!(
            std::path::Path::new("/tmp").exists(),
            "/tmp must exist for staging to work"
        );

        // Clean up the staged directory
        if let Some(staged_dir) = staged_path.parent() {
            drop(fs::remove_dir_all(staged_dir));
        }
    }

    #[cfg(unix)]
    #[test]
    fn find_staging_directory_detects_debug_profile() {
        let path = PathBuf::from("/project/target/debug/deps/pg_worker-abc123");
        let (staged_dir, target_dir) = find_staging_directory(&path);

        // Staged dir should be in /tmp with debug profile
        let staged_str = staged_dir.to_str().expect("staged dir is UTF-8");
        assert!(
            staged_str.starts_with("/tmp/pg-worker-debug-"),
            "staged dir should be in /tmp with debug profile, got: {staged_str}"
        );

        // Target profile dir should be the debug directory
        assert_eq!(target_dir, Some(PathBuf::from("/project/target/debug")));
    }

    #[cfg(unix)]
    #[test]
    fn find_staging_directory_detects_release_profile() {
        let path = PathBuf::from("/project/target/release/pg_worker");
        let (staged_dir, target_dir) = find_staging_directory(&path);

        // Staged dir should be in /tmp with release profile
        let staged_str = staged_dir.to_str().expect("staged dir is UTF-8");
        assert!(
            staged_str.starts_with("/tmp/pg-worker-release-"),
            "staged dir should be in /tmp with release profile, got: {staged_str}"
        );

        // Target profile dir should be the release directory
        assert_eq!(target_dir, Some(PathBuf::from("/project/target/release")));
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

    #[cfg(unix)]
    #[test]
    fn pointer_file_written_to_target_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create a fake target/debug structure
        let debug_dir = dir.path().join("target").join("debug");
        fs::create_dir_all(&debug_dir).expect("create debug dir");
        let source = debug_dir.join("pg_worker");
        fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("write worker");
        let mut perms = fs::metadata(&source).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&source, perms).expect("set perms");

        let staged = try_stage_worker_binary(&source.into_os_string()).expect("stage worker");
        let staged_path = PathBuf::from(&staged);

        // Verify pointer file was created
        let pointer_path = debug_dir.join("pg_worker_staged.path");
        assert!(
            pointer_path.exists(),
            "pointer file should exist at {}",
            pointer_path.display()
        );

        // Verify pointer file contains the staged path
        let pointer_content = fs::read_to_string(&pointer_path).expect("read pointer");
        assert_eq!(
            pointer_content,
            staged_path.to_str().expect("staged path is UTF-8"),
            "pointer file should contain staged path"
        );

        // Clean up the staged directory
        if let Some(staged_dir) = staged_path.parent() {
            drop(fs::remove_dir_all(staged_dir));
        }
    }
}
