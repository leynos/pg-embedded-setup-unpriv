//! Shared helpers for invoking the `pg_worker` binary in tests.

use std::process::Command;

use color_eyre::eyre::Result;

/// Returns the `pg_worker` binary path if available via Cargo's test harness.
///
/// Returns `None` when `CARGO_BIN_EXE_pg_worker` is not set, which can occur
/// when running tests without building the binary target.
pub const fn pg_worker_binary() -> Option<&'static str> {
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
pub fn run_pg_worker(args: &[&str]) -> Result<Option<std::process::Output>> {
    let Some(binary) = pg_worker_binary() else {
        return Ok(None);
    };

    let output = Command::new(binary).args(args).output()?;
    Ok(Some(output))
}
