//! Integration tests for the `pg_worker` binary.
//!
//! This module covers:
//! - Bootstrap failure paths when the worker binary is misconfigured, ensuring
//!   the bootstrapper validates helper paths eagerly so privileged orchestration
//!   does not defer errors to runtime.
//! - Binary invocation tests validating argument parsing, error messages, and
//!   output formatting.
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;

use color_eyre::eyre::{Result, ensure, eyre};
use nix::unistd::geteuid;
use pg_embedded_setup_unpriv::{BootstrapErrorKind, bootstrap_for_tests};
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/pg_worker_helpers.rs"]
mod pg_worker_helpers;
#[path = "support/sandbox.rs"]
mod sandbox;

use pg_worker_helpers::{pg_worker_binary, run_pg_worker};
use sandbox::TestSandbox;

#[test]
fn bootstrap_fails_when_worker_binary_missing() -> Result<()> {
    let sandbox = TestSandbox::new("missing-worker-binary")?;
    let missing_worker = sandbox.install_dir().join("nonexistent-worker");
    ensure!(
        !missing_worker.as_std_path().exists(),
        "expected test sandbox to start without a worker binary",
    );

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(missing_worker.as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let error = outcome
        .expect_err("bootstrap_for_tests should fail fast when the worker binary is missing");
    ensure!(
        error.kind() == BootstrapErrorKind::WorkerBinaryMissing,
        "expected structured worker-missing error but observed {:?}",
        error.kind()
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn bootstrap_fails_when_worker_path_is_directory() -> Result<()> {
    let sandbox = TestSandbox::new("worker-path-directory")?;
    fs::create_dir_all(sandbox.install_dir().as_std_path())?;

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(sandbox.install_dir().as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("bootstrap_for_tests should reject directory worker paths");
    let message = err.to_string();
    ensure!(
        message.contains("must reference a regular file"),
        eyre!("expected regular-file error, got: {message}")
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn bootstrap_fails_when_worker_binary_not_executable() -> Result<()> {
    let sandbox = TestSandbox::new("worker-path-non-executable")?;
    fs::create_dir_all(sandbox.install_dir().as_std_path())?;
    let worker_path = sandbox.install_dir().join("pg_worker_stub");
    fs::write(worker_path.as_std_path(), b"#!/bin/sh\nexit 0\n")?;
    let mut perms = fs::metadata(worker_path.as_std_path())?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(worker_path.as_std_path(), perms)?;

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(worker_path.as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("bootstrap_for_tests should reject non-executable workers");
    let message = err.to_string();
    ensure!(
        message.contains("must be executable"),
        eyre!("expected non-executable error, got: {message}")
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn bootstrap_fails_on_non_utf8_path_entry_when_root() -> Result<()> {
    if !geteuid().is_root() {
        tracing::warn!("skipping non-utf8 PATH test: requires root privileges");
        return Ok(());
    }

    let sandbox = TestSandbox::new("non-utf8-path-entry")?;
    let temp = tempfile::tempdir()?;
    let valid_dir = temp.path().join("valid");
    fs::create_dir_all(&valid_dir)?;

    let worker_path = valid_dir.join("pg_worker");
    fs::write(&worker_path, b"#!/bin/sh\nexit 0\n")?;
    let mut perms = fs::metadata(&worker_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&worker_path, perms)?;

    let non_utf8_component = OsString::from_vec(vec![0xff, 0xfe, 0xfd]);
    let non_utf8_dir = temp.path().join(&non_utf8_component);
    fs::create_dir_all(&non_utf8_dir)?;

    let path_value = std::env::join_paths([non_utf8_dir, valid_dir]).expect("join PATH");
    let mut env_vars = sandbox.env_without_timezone();
    env_vars.retain(|(key, _)| key.as_os_str() != OsStr::new("PG_EMBEDDED_WORKER"));
    env_vars.push((OsString::from("PG_EMBEDDED_WORKER"), None));
    env_vars.push((OsString::from("PATH"), Some(path_value)));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("bootstrap should fail on non-UTF-8 PATH entry");
    ensure!(
        err.kind() == BootstrapErrorKind::WorkerBinaryPathNonUtf8,
        "expected non-UTF-8 PATH error kind but observed {:?}",
        err.kind()
    );
    ensure!(
        err.to_string().contains("non-UTF-8"),
        "expected non-UTF-8 PATH error message, got: {err}"
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn env_without_timezone_removes_tz_variable() -> Result<()> {
    let sandbox = TestSandbox::new("env-without-timezone")?;
    let env_vars = sandbox.env_without_timezone();
    let tz_removed = env_vars
        .iter()
        .any(|(key, value)| key == OsStr::new("TZ") && value.is_none());
    let tzdir_removed = env_vars
        .iter()
        .any(|(key, value)| key == OsStr::new("TZDIR") && value.is_none());
    ensure!(
        tz_removed,
        "expected time zone helper to remove the TZ variable"
    );
    ensure!(
        tzdir_removed,
        "expected time zone helper to remove the TZDIR variable"
    );

    sandbox.reset()?;

    Ok(())
}

// =============================================================================
// Binary Invocation Integration Tests
// =============================================================================

/// Generates a unique, platform-appropriate temporary config path for testing.
///
/// The path does not need to exist; it is used only to test argument parsing.
/// Uses a unique suffix to prevent collisions in parallel test runs.
fn temp_config_path() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let unique_id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let thread_id = std::thread::current().id();
    std::env::temp_dir()
        .join(format!(
            "pg_worker_test_config_{thread_id:?}_{unique_id}.json"
        ))
        .to_string_lossy()
        .into_owned()
}

/// Asserts that `pg_worker` fails with a specific error message in stderr.
fn assert_pg_worker_fails_with_message(
    args: &[&str],
    expected_message: &str,
    test_description: &str,
) -> Result<()> {
    let Some(output) = run_pg_worker(args)? else {
        return Ok(());
    };

    ensure!(
        !output.status.success(),
        "pg_worker should fail with {test_description}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        stderr.contains(expected_message),
        eyre!("stderr should contain '{expected_message}', got: {stderr}")
    );

    Ok(())
}

/// Verifies that the `pg_worker` binary is available when running full test suite.
///
/// This test explicitly fails (rather than silently passing) when the binary is
/// missing, ensuring CI catches misconfiguration. The test is skipped when running
/// without `--all-targets` since the binary won't be built in that case.
#[test]
fn pg_worker_binary_is_available() {
    // This test exists to catch CI misconfiguration. If you're running tests
    // without building binaries (e.g., `cargo test --lib`), this test will be
    // skipped. When running `cargo test --all-targets`, this ensures the binary
    // was actually built.
    if std::env::var("CARGO_BIN_EXE_pg_worker").is_err() {
        // Check if we're likely running with --all-targets by looking for other
        // binary environment variables that Cargo sets
        let other_binaries_present = std::env::vars().any(|(k, _)| k.starts_with("CARGO_BIN_EXE_"));
        assert!(
            !other_binaries_present,
            concat!(
                "pg_worker binary not found but other binaries are present. ",
                "This suggests the pg_worker binary failed to build."
            )
        );
        // Not running with --all-targets, so binary tests are expected to be skipped
        return;
    }

    let binary = pg_worker_binary().expect("binary should be available");
    assert!(
        std::path::Path::new(binary).exists(),
        "pg_worker binary path exists but file is missing: {binary}"
    );
}

#[test]
fn pg_worker_binary_rejects_invalid_operation() -> Result<()> {
    let config_path = temp_config_path();
    let Some(output) = run_pg_worker(&["invalid_op", &config_path])? else {
        return Ok(());
    };

    ensure!(
        !output.status.success(),
        "pg_worker should fail with invalid operation"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        stderr.contains("unknown operation 'invalid_op'"),
        eyre!("stderr should contain 'unknown operation', got: {stderr}")
    );
    ensure!(
        stderr.contains("expected setup, start, stop, cleanup, or cleanup-full"),
        eyre!("stderr should list valid operations, got: {stderr}")
    );

    Ok(())
}

#[rstest]
#[case::missing_operation(&[], "missing operation")]
#[case::missing_config(&["setup"], "missing config path")]
fn pg_worker_binary_shows_expected_errors(
    #[case] args: &[&str],
    #[case] expected_message: &str,
) -> Result<()> {
    assert_pg_worker_fails_with_message(args, expected_message, expected_message)
}

#[test]
fn pg_worker_binary_error_format_uses_prefix() -> Result<()> {
    let config_path = temp_config_path();
    let Some(output) = run_pg_worker(&["invalid_op", &config_path])? else {
        return Ok(());
    };

    ensure!(
        !output.status.success(),
        "pg_worker should fail with invalid operation"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        stderr.contains("InvalidArgs"),
        eyre!("error output should contain 'InvalidArgs' prefix, got: {stderr}")
    );

    Ok(())
}

// Note: Data directory recovery integration tests (Issue #80) are in
// tests/recovery_integration.rs to keep this file focused on bootstrap
// and binary invocation tests.
