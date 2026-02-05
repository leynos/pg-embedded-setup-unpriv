//! Tests for bootstrap environment discovery helpers.

use super::*;
use rstest::rstest;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

#[rstest]
fn discover_worker_errors_on_non_utf8_path_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let valid_dir = temp.path().join("valid");
    std::fs::create_dir_all(&valid_dir).expect("create valid dir");

    let non_utf8_component = OsString::from_vec(vec![0xff, 0xfe, 0xfd]);
    let non_utf8_dir = temp.path().join(&non_utf8_component);
    std::fs::create_dir_all(&non_utf8_dir).expect("create non-utf8 dir");

    let path_value = std::env::join_paths([non_utf8_dir, valid_dir]).expect("join PATH");
    let _guard = crate::test_support::scoped_env(vec![(OsString::from("PATH"), Some(path_value))]);

    let err = discover_worker_from_path()
        .expect_err("discover_worker_from_path should reject non-UTF-8 PATH entries");
    let message = err.to_string().to_lowercase();
    assert!(
        message.contains("path") && message.contains("utf-8"),
        "expected error mentioning PATH UTF-8, got: {message}"
    );
}
