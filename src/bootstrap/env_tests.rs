//! Tests for the [`crate::bootstrap::env`] module.
//!
//! Covers worker path parsing, PATH-based discovery, and security hardening
//! for trusted directory filtering.

use std::ffi::{OsStr, OsString};
use std::fs;

use rstest::rstest;
use tempfile::tempdir;

use crate::bootstrap::worker_discovery::{
    discover_worker_from_path, is_trusted_path_directory, parse_worker_path_from_env,
};
use crate::env::ScopedEnv;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

// Tests for parse_worker_path_from_env

#[rstest]
#[case(OsStr::new(""), true, "must not be empty", None)]
#[case(OsStr::new("/"), true, "must not point at the filesystem root", None)]
#[case(
    OsStr::new("/usr/local/bin/pg_worker"),
    false,
    "",
    Some("/usr/local/bin/pg_worker")
)]
#[cfg_attr(
    unix,
    case(
        OsStr::from_bytes(b"/path/with/invalid/\xff/bytes"),
        true,
        "non-UTF-8 value",
        None
    )
)]
fn parse_worker_path_cases(
    #[case] input: &OsStr,
    #[case] should_fail: bool,
    #[case] expected_msg: &str,
    #[case] expected_path: Option<&str>,
) {
    let result = parse_worker_path_from_env(input);

    if should_fail {
        let err = result.expect_err(&format!("should reject input: {input:?}"));
        assert!(
            err.to_string().contains(expected_msg),
            "expected error containing '{expected_msg}', got: {err}"
        );
    } else {
        let msg = format!("should accept input: {input:?}");
        let path = result.expect(&msg);
        let expected = expected_path.expect("expected_path must be provided for success cases");
        assert_eq!(path.as_str(), expected);
    }
}

// Tests for discover_worker_from_path

#[cfg(unix)]
#[test]
fn discover_worker_finds_binary_in_path() {
    let temp = tempdir().expect("create tempdir");
    let worker_name = "pg_worker";

    let worker_path = temp.path().join(worker_name);
    fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("write worker");
    let mut perms = fs::metadata(&worker_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&worker_path, perms).expect("set permissions");

    let key = OsString::from("PATH");
    let value = Some(OsString::from(temp.path().to_string_lossy().to_string()));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path(worker_name).expect("should not error during worker discovery");

    let expected_msg = format!("should find {worker_name}");
    let found = result.expect(&expected_msg);
    assert!(
        found.as_str().contains(worker_name),
        "found path should contain {worker_name}: {found}"
    );
}

#[test]
fn discover_worker_returns_none_for_empty_path() {
    let key = OsString::from("PATH");
    let value = Some(OsString::from(""));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result = discover_worker_from_path("pg_worker").expect("should not error on empty PATH");

    assert!(result.is_none(), "empty PATH should return None");
}

#[cfg(unix)]
#[test]
fn discover_worker_skips_directories() {
    let temp = tempdir().expect("create tempdir");
    let worker_dir = temp.path().join("pg_worker");
    fs::create_dir(&worker_dir).expect("create directory");

    let key = OsString::from("PATH");
    let value = Some(OsString::from(temp.path().to_string_lossy().to_string()));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path("pg_worker").expect("should not error during worker discovery");

    assert!(
        result.is_none(),
        "should not find pg_worker when it is a directory"
    );
}

#[test]
fn discover_worker_returns_none_when_not_found() {
    let temp = tempdir().expect("create tempdir");

    let key = OsString::from("PATH");
    let value = Some(OsString::from(temp.path().to_string_lossy().to_string()));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path("pg_worker").expect("should not error during worker discovery");

    assert!(result.is_none(), "should return None when worker not found");
}

// Tests for security hardening (is_executable, is_trusted_path_directory)

#[cfg(unix)]
#[test]
fn discover_worker_skips_non_executable_and_finds_later_entry() {
    let temp1 = tempdir().expect("create tempdir1");
    let temp2 = tempdir().expect("create tempdir2");

    // Create non-executable pg_worker in first directory
    let non_exec = temp1.path().join("pg_worker");
    fs::write(&non_exec, b"#!/bin/sh\nexit 0\n").expect("write non-exec");

    // Create executable pg_worker in second directory
    let exec = temp2.path().join("pg_worker");
    fs::write(&exec, b"#!/bin/sh\nexit 0\n").expect("write exec");
    let mut perms = fs::metadata(&exec).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&exec, perms).expect("set permissions");

    let new_path = format!("{}:{}", temp1.path().display(), temp2.path().display());

    let key = OsString::from("PATH");
    let value = Some(OsString::from(&new_path));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path("pg_worker").expect("should not error during worker discovery");

    let found = result.expect("should find executable worker in second directory");
    assert!(
        found
            .as_str()
            .contains(temp2.path().to_str().expect("temp2 path")),
        "should find worker in temp2, not temp1: {found}"
    );
}

#[cfg(unix)]
#[test]
fn discover_worker_skips_relative_path_entries() {
    let key = OsString::from("PATH");
    let value = Some(OsString::from("relative/path/entry"));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path("pg_worker").expect("should not error during worker discovery");

    assert!(
        result.is_none(),
        "should not find worker in relative PATH entry"
    );
}

#[cfg(unix)]
#[test]
fn discover_worker_skips_world_writable_directories() {
    let temp = tempdir().expect("create tempdir");

    // Create executable pg_worker
    let worker_path = temp.path().join("pg_worker");
    fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("write worker");
    let mut perms = fs::metadata(&worker_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&worker_path, perms).expect("set permissions");

    // Make directory world-writable
    let mut dir_perms = fs::metadata(temp.path())
        .expect("dir metadata")
        .permissions();
    dir_perms.set_mode(0o777);
    fs::set_permissions(temp.path(), dir_perms).expect("set dir permissions");

    let new_path = temp.path().to_string_lossy().to_string();

    let key = OsString::from("PATH");
    let value = Some(OsString::from(&new_path));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path("pg_worker").expect("should not error during worker discovery");

    assert!(
        result.is_none(),
        "should not find worker in world-writable directory"
    );
}

#[cfg(unix)]
#[test]
fn is_trusted_path_directory_accepts_normal_directories() {
    let temp = tempdir().expect("create tempdir");
    // Default permissions should be 0o755 or similar (not world-writable)
    assert!(
        is_trusted_path_directory(temp.path()),
        "normal directory should be trusted"
    );
}

#[cfg(unix)]
#[test]
fn is_trusted_path_directory_rejects_relative_paths() {
    assert!(
        !is_trusted_path_directory(std::path::Path::new("relative/path")),
        "relative path should not be trusted"
    );
    assert!(
        !is_trusted_path_directory(std::path::Path::new(".")),
        "current directory should not be trusted"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "rustc E0277 — https://github.com/leynos/pg-embedded-setup-unpriv/issues/78"]
fn discover_worker_errors_on_non_utf8_path_entry() {
    let temp = tempdir().expect("create tempdir");
    let worker_path = temp.path().join("pg_worker");

    let non_utf8_dir_name = OsStr::from_bytes(b"non_\xff_utf8");
    let non_utf8_dir = temp.path().join(non_utf8_dir_name);
    let new_path_with_non_utf8 = format!("{}:{}", temp.path().display(), non_utf8_dir.display());

    let key = OsString::from("PATH");
    let value = Some(OsString::from(&new_path_with_non_utf8));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("write worker");
    let mut perms = fs::metadata(&worker_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&worker_path, perms).expect("set permissions");

    let result = discover_worker_from_path("pg_worker");

    let err = result.expect_err("should error on non-UTF-8 PATH entry");
    assert!(
        err.to_string()
            .contains("PATH contains non-UTF-8 directory"),
        "error should mention non-UTF-8 PATH: {err}"
    );
}

#[cfg(unix)]
#[test]
fn discover_worker_uses_custom_worker_name() {
    let temp = tempdir().expect("create tempdir");
    let worker_name = "my_custom_worker";

    let worker_path = temp.path().join(worker_name);
    fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("write worker");
    let mut perms = fs::metadata(&worker_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&worker_path, perms).expect("set permissions");

    let key = OsString::from("PATH");
    let value = Some(OsString::from(temp.path().to_string_lossy().to_string()));
    let _env_guard = ScopedEnv::apply_os([(key, value)]);

    let result =
        discover_worker_from_path(worker_name).expect("should not error during worker discovery");

    let expected_msg = format!("should find {worker_name}");
    let found = result.expect(&expected_msg);
    assert!(
        found.as_str().contains(worker_name),
        "found path should contain {worker_name}: {found}"
    );
}
