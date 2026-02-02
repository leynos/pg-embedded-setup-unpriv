//! Integration tests for data directory recovery (Issue #80).
//!
//! This module tests the `pg_worker` binary's ability to detect and recover
//! from partial data directories (missing `global/pg_filenode.map`).
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{Result, ensure, eyre};
use pg_embedded_setup_unpriv::test_support::create_partial_data_dir;

// =============================================================================
// Binary Invocation Helpers
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
/// unavailable.
fn run_pg_worker(args: &[&str]) -> Result<Option<std::process::Output>> {
    let Some(binary) = pg_worker_binary() else {
        return Ok(None);
    };

    let output = Command::new(binary).args(args).output()?;
    Ok(Some(output))
}

// =============================================================================
// Worker Config Helper
// =============================================================================

/// Creates a minimal worker config for testing recovery scenarios.
///
/// The config points to the specified data directory and creates an
/// installation directory that exists but does not contain `PostgreSQL`
/// binaries. This is sufficient to trigger recovery detection before the
/// actual setup fails (or succeeds if binaries are cached elsewhere).
fn create_minimal_worker_config(temp_dir: &Path, data_dir: &Path) -> Result<std::path::PathBuf> {
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

// =============================================================================
// Stderr Predicate Helper
// =============================================================================

/// Checks if stderr indicates a missing `PostgreSQL` installation.
///
/// This predicate returns `true` if the stderr output suggests the failure
/// was due to missing `PostgreSQL` binaries (expected) rather than some
/// other unexpected error.
fn stderr_indicates_missing_postgres(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("postgres")
        || lower.contains("initdb")
        || lower.contains("no such file")
        || lower.contains("not found")
}

// =============================================================================
// Recovery Integration Test
// =============================================================================

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

    // Create a partial data directory using the shared helper
    create_partial_data_dir(&data_dir)?;

    let config_path = create_minimal_worker_config(temp_dir.path(), &data_dir)?;
    let config_str = config_path.to_string_lossy();

    let output =
        run_pg_worker(&["setup", &config_str])?.ok_or_else(|| eyre!("binary unavailable"))?;

    let status = output.status;
    let stderr = String::from_utf8_lossy(&output.stderr);

    // If the binary failed, verify it's due to missing PostgreSQL installation
    // (not due to recovery failure or other unexpected reasons)
    if !status.success() && !stderr_indicates_missing_postgres(&stderr) {
        return Err(eyre!(
            concat!(
                "pg_worker setup failed for an unexpected reason; ",
                "stderr should mention missing/fake PostgreSQL installation; ",
                "binary exit code: {:?}, stderr: {}"
            ),
            status.code(),
            stderr
        ));
    }

    // Verify the partial directory was removed by recovery.
    // If setup succeeded, a fresh data directory will exist; if it failed,
    // the directory should not exist. Either way, the ORIGINAL partial
    // directory (which had PG_VERSION but no global/pg_filenode.map) was
    // removed by recovery. We verify this by checking that either:
    // - The directory doesn't exist (setup failed after recovery), or
    // - The directory exists WITH the marker file (fresh init succeeded)
    let has_filenode_map = data_dir.join("global/pg_filenode.map").exists();
    ensure!(
        !data_dir.exists() || has_filenode_map,
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
