//! Tests for bootstrap environment discovery helpers.

use super::*;
use rstest::rstest;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;

#[rstest]
fn discover_worker_errors_on_non_utf8_path_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let valid_dir = temp.path().join("valid");
    std::fs::create_dir_all(&valid_dir).expect("create valid dir");

    let non_utf8_component = OsString::from_vec(vec![0xff, 0xfe, 0xfd]);
    let non_utf8_dir = temp.path().join(&non_utf8_component);
    std::fs::create_dir_all(&non_utf8_dir).expect("create non-utf8 dir");

    let worker_path = valid_dir.join(WORKER_BINARY_NAME);
    std::fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("create pg_worker");
    let mut perms = std::fs::metadata(&worker_path)
        .expect("stat pg_worker")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&worker_path, perms).expect("chmod pg_worker");

    let path_value = std::env::join_paths([non_utf8_dir, valid_dir]).expect("join PATH");
    let _guard = crate::test_support::scoped_env(vec![(OsString::from("PATH"), Some(path_value))]);

    let err = discover_worker_from_path()
        .expect_err("discover_worker_from_path should reject non-UTF-8 PATH entries");
    let message = err.to_string().to_lowercase();
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::WorkerBinaryPathNonUtf8,
        "expected PATH UTF-8 error kind"
    );
    assert!(
        message.contains("path") && message.contains("non-utf-8"),
        "expected error mentioning PATH non-UTF-8, got: {message}"
    );
}
