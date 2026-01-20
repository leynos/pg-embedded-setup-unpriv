//! Tests for worker binary staging logic.

use super::*;
use rstest::{fixture, rstest};

#[cfg(unix)]
use std::path::PathBuf;

/// Guard that cleans up a staged directory when dropped.
#[cfg(unix)]
struct StagedDirCleanup(Option<PathBuf>);

#[cfg(unix)]
impl StagedDirCleanup {
    /// Creates a new cleanup guard for the staged path's parent directory.
    fn new(staged_path: &std::path::Path) -> Self {
        Self(staged_path.parent().map(std::path::Path::to_path_buf))
    }
}

#[cfg(unix)]
impl Drop for StagedDirCleanup {
    fn drop(&mut self) {
        if let Some(ref staged_dir) = self.0 {
            drop(fs::remove_dir_all(staged_dir));
        }
    }
}

/// Creates a temporary directory with a fake target/debug structure for testing.
#[cfg(unix)]
#[fixture]
fn debug_target_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let debug_dir = dir.path().join("target").join("debug");
    fs::create_dir_all(&debug_dir).expect("create debug dir");
    dir
}

#[cfg(unix)]
#[rstest]
fn staged_worker_is_world_executable_and_in_temp_dir(debug_target_dir: tempfile::TempDir) {
    let debug_dir = debug_target_dir.path().join("target").join("debug");
    let source = debug_dir.join("pg_worker");
    fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("write worker");
    let mut perms = fs::metadata(&source).expect("metadata").permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&source, perms).expect("set perms");

    let staged = try_stage_worker_binary(&source.into_os_string()).expect("stage worker");
    let staged_path = PathBuf::from(&staged);
    let _cleanup = StagedDirCleanup::new(&staged_path);

    // Verify staged path is in the system temp directory
    let temp_dir = std::env::temp_dir();
    assert!(
        staged_path.starts_with(&temp_dir),
        "staged worker should be in temp dir {}, got: {}",
        temp_dir.display(),
        staged_path.display()
    );

    // Verify path follows expected pattern {temp_dir}/pg-worker-{profile}-{hash}/
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

    // Verify the temp directory has the expected permissions: world-writable with sticky bit.
    // The sticky bit restricts deletion/renaming of files by other users, but does
    // not prevent read or execute access to the directory itself.
    let tmp_meta = fs::metadata(&temp_dir).expect("temp dir must exist for staging to work");
    assert!(tmp_meta.is_dir(), "temp dir must be a directory");
    let tmp_mode = tmp_meta.permissions().mode();

    // World-executable (anyone can traverse the directory)
    assert_ne!(
        tmp_mode & 0o001,
        0,
        "temp dir must be world-executable for nobody to access staged binary"
    );
}

/// Asserts that `find_staging_directory` produces the expected results for a given profile.
#[cfg(unix)]
fn assert_staging_directory_for_profile(
    input_path: &str,
    expected_profile: &str,
    expected_target_dir: &str,
) {
    let path = PathBuf::from(input_path);
    let (staged_dir, target_dir) = find_staging_directory(&path);

    let temp_dir = std::env::temp_dir();
    assert!(
        staged_dir.starts_with(&temp_dir),
        "staged dir should be in temp dir {}, got: {}",
        temp_dir.display(),
        staged_dir.display()
    );

    let staged_name = staged_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let expected_prefix = format!("pg-worker-{expected_profile}-");
    assert!(
        staged_name.starts_with(&expected_prefix),
        "staged dir should match {expected_prefix}*, got: {staged_name}"
    );

    assert_eq!(
        target_dir,
        Some(PathBuf::from(expected_target_dir)),
        "target dir should match expected path"
    );
}

#[cfg(unix)]
#[rstest]
fn find_staging_directory_detects_debug_profile() {
    assert_staging_directory_for_profile(
        "/project/target/debug/deps/pg_worker-abc123",
        "debug",
        "/project/target/debug",
    );
}

#[cfg(unix)]
#[rstest]
fn find_staging_directory_detects_release_profile() {
    assert_staging_directory_for_profile(
        "/project/target/release/pg_worker",
        "release",
        "/project/target/release",
    );
}

#[cfg(unix)]
#[rstest]
fn should_restage_returns_true_for_missing_staged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    fs::write(&source, b"content").expect("write source");
    let staged = dir.path().join("staged");

    assert!(should_restage(&source, &staged).expect("should_restage"));
}

#[cfg(unix)]
#[rstest]
fn should_restage_returns_false_when_staged_is_newer() {
    use std::time::{Duration, SystemTime};

    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let staged = dir.path().join("staged");

    fs::write(&source, b"content").expect("write source");
    fs::write(&staged, b"content").expect("write staged");

    // Set source mtime to 1 hour ago to ensure staged is newer.
    // This avoids flaky tests due to filesystem mtime resolution.
    let past = SystemTime::now() - Duration::from_secs(3600);
    let past_filetime = filetime::FileTime::from_system_time(past);
    filetime::set_file_mtime(&source, past_filetime).expect("set source mtime");

    assert!(!should_restage(&source, &staged).expect("should_restage"));
}

#[cfg(unix)]
#[rstest]
fn pointer_file_written_to_target_dir(debug_target_dir: tempfile::TempDir) {
    let debug_dir = debug_target_dir.path().join("target").join("debug");
    let source = debug_dir.join("pg_worker");
    fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("write worker");
    let mut perms = fs::metadata(&source).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&source, perms).expect("set perms");

    let staged = try_stage_worker_binary(&source.into_os_string()).expect("stage worker");
    let staged_path = PathBuf::from(&staged);
    let _cleanup = StagedDirCleanup::new(&staged_path);

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
}
