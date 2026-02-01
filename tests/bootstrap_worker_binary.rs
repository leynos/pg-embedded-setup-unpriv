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
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use color_eyre::eyre::{Result, ensure, eyre};
use pg_embedded_setup_unpriv::{BootstrapErrorKind, bootstrap_for_tests};
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;

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

/// Returns the `pg_worker` binary path if available via Cargo's test harness.
///
/// Returns `None` when `CARGO_BIN_EXE_pg_worker` is not set, which can occur
/// when running tests without building the binary target.
const fn pg_worker_binary() -> Option<&'static str> {
    option_env!("CARGO_BIN_EXE_pg_worker")
}

/// Runs the `pg_worker` binary with the given arguments.
///
/// Returns the command output on success, or `None` if the binary path is
/// unavailable. Returns an error if the command fails to execute (not to be
/// confused with the command returning a non-zero exit code, which is expected
/// for error tests).
///
/// When the binary is unavailable, this returns `None` and the calling test
/// should return early. The `pg_worker_binary_is_available` test ensures that
/// missing binaries are caught in CI when running with `--all-targets`.
fn run_pg_worker(args: &[&str]) -> Result<Option<std::process::Output>> {
    let Some(binary) = pg_worker_binary() else {
        return Ok(None);
    };

    let output = Command::new(binary).args(args).output()?;
    Ok(Some(output))
}

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

// =============================================================================
// Data Directory Recovery Integration Tests (Issue #80)
// =============================================================================

/// Creates a minimal worker config for testing recovery scenarios.
///
/// The config points to the specified data directory and uses a non-existent
/// installation directory, which is sufficient to trigger recovery detection
/// before the actual setup fails.
fn create_minimal_worker_config(
    temp_dir: &std::path::Path,
    data_dir: &std::path::Path,
) -> Result<std::path::PathBuf> {
    use pg_embedded_setup_unpriv::worker::{SettingsSnapshot, WorkerPayload};
    use postgresql_embedded::Settings;

    let install_dir = temp_dir.join("install");
    fs::create_dir_all(&install_dir)?;

    let settings = Settings {
        installation_dir: install_dir,
        data_dir: data_dir.to_path_buf(),
        password_file: temp_dir.join(".pgpass"),
        trust_installation_dir: true,
        ..Settings::default()
    };

    let snapshot = SettingsSnapshot::try_from(&settings)
        .map_err(|e| eyre!("failed to create settings snapshot: {e}"))?;
    let payload = WorkerPayload {
        settings: snapshot,
        environment: vec![],
    };

    let config_path = temp_dir.join("config.json");
    let config_json = serde_json::to_string(&payload)?;
    fs::write(&config_path, config_json)?;

    Ok(config_path)
}

/// Issue #80: Validates that `pg_worker` detects a partial data directory
/// (missing `global/pg_filenode.map`) and triggers recovery.
///
/// This test creates a partial data directory with `PG_VERSION` but without
/// the marker file, runs `pg_worker` with setup operation, and verifies:
/// 1. The partial data directory is removed by recovery
/// 2. If the binary fails, it's due to missing `PostgreSQL` installation
///    (not due to recovery failure or other unexpected reasons)
///
/// Note: The setup may succeed if `PostgreSQL` binaries are cached, or fail
/// if no real installation is available. Either outcome is acceptable as
/// long as the partial directory was removed by recovery first.
#[test]
fn pg_worker_binary_detects_partial_data_dir_and_triggers_recovery() -> Result<()> {
    let Some(_) = pg_worker_binary() else {
        return Ok(());
    };

    let temp_dir = tempfile::tempdir()?;
    let data_dir = temp_dir.path().join("data");

    // Create a partial data directory: has structure but missing the marker file
    fs::create_dir_all(data_dir.join("global"))?;
    fs::write(data_dir.join("PG_VERSION"), "16\n")?;
    fs::create_dir_all(data_dir.join("base"))?;

    let config_path = create_minimal_worker_config(temp_dir.path(), &data_dir)?;
    let config_str = config_path.to_string_lossy();

    let output =
        run_pg_worker(&["setup", &config_str])?.ok_or_else(|| eyre!("binary unavailable"))?;

    let status = output.status;
    let stderr = String::from_utf8_lossy(&output.stderr);

    // If the binary failed, verify it's due to missing PostgreSQL installation
    // (not due to recovery failure or other unexpected reasons)
    if !status.success() {
        ensure!(
            stderr.to_lowercase().contains("postgres")
                || stderr.to_lowercase().contains("initdb")
                || stderr.to_lowercase().contains("no such file")
                || stderr.to_lowercase().contains("not found"),
            eyre!(
                concat!(
                    "pg_worker setup failed for an unexpected reason; ",
                    "stderr should mention missing/fake PostgreSQL installation; ",
                    "binary exit code: {:?}, stderr: {}"
                ),
                status.code(),
                stderr
            )
        );
    }

    // Verify the partial directory was removed by recovery.
    // If setup succeeded, a fresh data directory will exist; if it failed,
    // the directory should not exist. Either way, the ORIGINAL partial
    // directory (which had PG_VERSION but no global/pg_filenode.map) was
    // removed by recovery. We verify this by checking that either:
    // - The directory doesn't exist (setup failed after recovery), or
    // - The directory exists WITH the marker file (fresh init succeeded)
    let marker_exists = data_dir.join("global/pg_filenode.map").exists();
    ensure!(
        !data_dir.exists() || marker_exists,
        eyre!(
            concat!(
                "partial data directory should be removed by recovery; ",
                "expected either no directory or fresh directory with marker; ",
                "binary exit code: {:?}, stderr: {}"
            ),
            status.code(),
            stderr
        )
    );

    Ok(())
}

// Note on Issue #80 testing strategy:
//
// The integration test for partial data directory recovery is
// `pg_worker_binary_detects_partial_data_dir_and_triggers_recovery`.
//
// The validation that a valid data directory (with `global/pg_filenode.map`)
// is NOT removed by recovery is covered by the unit test
// `has_valid_data_dir::valid_data_dir_detected` in `src/bin/pg_worker.rs`.
//
// We cannot easily test that a valid data directory survives the full
// binary invocation because `pg.setup()` may modify or reset the data directory
// when the PostgreSQL installation is incomplete. The recovery logic itself
// correctly preserves valid directories - this is verified by unit tests.
