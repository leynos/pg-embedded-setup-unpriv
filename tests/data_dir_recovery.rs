//! End-to-end integration test for partial data directory recovery (Issue #80).
//!
//! This test verifies that when the `pg_worker` binary is invoked with a partial
//! data directory (missing `global/pg_filenode.map`), the recovery logic:
//! 1. Detects the partial directory as invalid
//! 2. Removes it to allow fresh initialisation
//! 3. Successfully initialises a new data directory with both `PG_VERSION` and
//!    `global/pg_filenode.map` present
//!
//! This requires real `PostgreSQL` binaries from `postgresql_embedded` and
//! runs only when the `privileged-tests` feature is enabled (since the worker
//! subprocess needs permission to modify the data directory).
#![cfg(all(unix, feature = "cluster-unit-tests", feature = "privileged-tests"))]

use std::fs;
use std::time::Duration;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::{Gid, Uid, User, chown, geteuid};
use pg_embedded_setup_unpriv::bootstrap_for_tests;
use pg_embedded_setup_unpriv::test_support::worker_binary_for_tests;
use pg_embedded_setup_unpriv::worker_process_test_api::{
    WorkerOperation, WorkerRequest, WorkerRequestArgs,
};
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/partial_data_dir.rs"]
mod partial_data_dir;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;

use partial_data_dir::create_partial_data_dir;
use sandbox::TestSandbox;
use serial::{ScenarioLocalGuard, local_serial_guard};

/// Recursively changes ownership of a directory tree to a user.
///
/// Symlinks are explicitly skipped to avoid following unexpected paths
/// during recursive ownership changes.
fn chown_recursive(path: &std::path::Path, uid: Uid, gid: Gid) -> Result<()> {
    // Skip symlinks to avoid following unexpected paths
    if path.is_symlink() {
        return Ok(());
    }

    chown(path, Some(uid), Some(gid)).context("chown directory")?;
    if path.is_dir() {
        for raw_entry in fs::read_dir(path).context("read dir")? {
            let dir_entry = raw_entry.context("dir entry")?;
            chown_recursive(&dir_entry.path(), uid, gid)?;
        }
    }
    Ok(())
}

/// Issue #80: Validates that `pg_worker` detects a partial data directory
/// (missing `global/pg_filenode.map`), removes it via recovery, and then
/// performs fresh initialisation that creates both `PG_VERSION` and
/// `global/pg_filenode.map`.
///
/// This is a true end-to-end test using real `PostgreSQL` binaries.
/// Requires root privileges and `privileged-tests` feature.
#[rstest]
fn partial_data_dir_recovery_then_fresh_init(local_serial_guard: ScenarioLocalGuard) -> Result<()> {
    let _guard = local_serial_guard;

    // This test must run as root for proper privilege handling
    if !geteuid().is_root() {
        tracing::warn!("Skipping test: must run as root");
        return Ok(());
    }

    // Skip if worker binary is not available
    let Some(worker_os) = worker_binary_for_tests() else {
        tracing::warn!("Skipping test: PG_EMBEDDED_WORKER not set");
        return Ok(());
    };
    let worker = Utf8PathBuf::try_from(std::path::PathBuf::from(worker_os))
        .map_err(|_| eyre!("worker path not UTF-8"))?;

    let sandbox = TestSandbox::new("data-dir-recovery")?;
    sandbox.reset()?;

    // Bootstrap the environment to get valid PostgreSQL settings
    let env_vars = sandbox.env_without_timezone();
    let bootstrap_result = sandbox.with_env(env_vars.clone(), bootstrap_for_tests);

    let bootstrap = match bootstrap_result {
        Ok(b) => b,
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("rate limit") || msg.contains("download") {
                tracing::warn!("Skipping test due to network issue: {msg}");
                return Ok(());
            }
            return Err(err).context("bootstrap_for_tests failed");
        }
    };

    let data_dir = Utf8PathBuf::from_path_buf(bootstrap.settings.data_dir.clone())
        .map_err(|_| eyre!("data_dir not UTF-8"))?;

    // Create a PARTIAL data directory using the shared helper
    create_partial_data_dir(data_dir.as_std_path()).context("create partial data dir")?;

    // Change ownership of the data directory to 'nobody' so the worker
    // subprocess can delete it during recovery
    let nobody = User::from_name("nobody")
        .context("resolve nobody")?
        .ok_or_else(|| eyre!("nobody user not found"))?;
    chown_recursive(data_dir.as_std_path(), nobody.uid, nobody.gid)?;

    // Verify the marker file does NOT exist before setup
    ensure!(
        !data_dir.join("global/pg_filenode.map").exists(),
        "marker file should not exist before recovery"
    );

    // Invoke pg_worker with Setup operation - this should:
    // 1. Detect the partial data dir as invalid (missing marker)
    // 2. Remove it via recovery
    // 3. Run initdb to create a fresh data dir
    let env_pairs: Vec<(String, Option<String>)> = Vec::new();
    let args = WorkerRequestArgs {
        worker: worker.as_path(),
        settings: &bootstrap.settings,
        env_vars: &env_pairs,
        operation: WorkerOperation::Setup,
        timeout: Duration::from_secs(120),
    };
    let request = WorkerRequest::new(args);

    sandbox.with_env(env_vars, || {
        pg_embedded_setup_unpriv::worker_process_test_api::run(&request)
            .map_err(|e| eyre!("worker setup failed: {e}"))
    })?;

    // Verify fresh initialisation succeeded: both files should now exist
    ensure!(
        data_dir.join("PG_VERSION").exists(),
        "PG_VERSION should exist after fresh init"
    );
    ensure!(
        data_dir.join("global/pg_filenode.map").exists(),
        "global/pg_filenode.map should exist after fresh init (Issue #80 requirement)"
    );

    Ok(())
}
