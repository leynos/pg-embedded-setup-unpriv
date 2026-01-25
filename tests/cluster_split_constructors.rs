//! Tests for `TestCluster` split constructors and `Deref` behaviour.
//!
//! These tests verify that `new_split()` and `start_async_split()` produce
//! working handle/guard pairs, and that `TestCluster` correctly derefs to
//! `ClusterHandle`.
#![cfg(unix)]

use std::{thread, time::Duration};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::TestCluster;
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/env.rs"]
mod env;
#[path = "support/env_snapshot.rs"]
mod env_snapshot;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use env_snapshot::EnvSnapshot;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

// ============================================================================
// new_split() tests
// ============================================================================

/// Tests that `new_split()` creates a working handle/guard pair.
///
/// Verifies:
/// - Handle provides access to cluster settings
/// - Guard manages cluster lifecycle
/// - Dropping guard stops the cluster
/// - Environment is restored after guard drops
#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds the guard even though the test ignores it"
)]
#[rstest]
fn new_split_creates_working_handle_and_guard(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("split-constructor").context("create test sandbox")?;
    sandbox.reset()?;

    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), run_split_lifecycle_test);

    if should_skip_on_error(&result) {
        return Ok(());
    }
    let data_dir = result?;

    // Verify environment restored
    let env_after = EnvSnapshot::capture();
    ensure!(
        env_before == env_after,
        "environment should be restored after guard drops"
    );

    // Verify cluster stopped
    wait_for_postmaster_shutdown(&data_dir)?;
    Ok(())
}

fn run_split_lifecycle_test() -> std::result::Result<Utf8PathBuf, color_eyre::Report> {
    let (handle, guard) = TestCluster::new_split().map_err(color_eyre::Report::from)?;

    // Verify handle provides access to settings
    let data_dir = Utf8PathBuf::from_path_buf(handle.settings().data_dir.clone())
        .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;

    // Verify cluster is running
    ensure!(
        data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should exist while cluster runs"
    );

    // Verify handle methods work
    ensure!(
        handle
            .database_exists("postgres")
            .map_err(color_eyre::Report::from)?,
        "postgres database should exist"
    );

    // Drop guard to trigger shutdown
    drop(guard);
    Ok(data_dir)
}

// ============================================================================
// Deref behaviour tests
// ============================================================================

/// Tests that `TestCluster` derefs to `ClusterHandle`.
///
/// Verifies that methods available on `ClusterHandle` can be called
/// directly on `TestCluster` through the `Deref` implementation.
#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds the guard even though the test ignores it"
)]
#[rstest]
fn test_cluster_derefs_to_cluster_handle(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("deref-test").context("create test sandbox")?;
    sandbox.reset()?;

    let result = sandbox.with_env(sandbox.env_without_timezone(), run_deref_test);

    if should_skip_on_error(&result) {
        return Ok(());
    }
    result?;
    Ok(())
}

fn run_deref_test() -> std::result::Result<(), color_eyre::Report> {
    let cluster = TestCluster::new().map_err(color_eyre::Report::from)?;

    // These methods are on ClusterHandle, accessed via Deref
    let _settings = cluster.settings();
    let _environment = cluster.environment();
    let _bootstrap = cluster.bootstrap();

    // Verify delegation methods work through Deref
    ensure!(
        cluster
            .database_exists("postgres")
            .map_err(color_eyre::Report::from)?,
        "should access database_exists through Deref"
    );

    // The fact that settings(), environment(), bootstrap(), and database_exists()
    // all work on `cluster` proves that Deref to ClusterHandle is working,
    // since these methods are defined on ClusterHandle, not TestCluster.

    Ok(())
}

/// Generic skip helper for tests returning `Result<T, color_eyre::Report>`.
fn should_skip_on_error<T>(result: &std::result::Result<T, color_eyre::Report>) -> bool {
    let Err(err) = result else {
        return false;
    };
    let message = err.to_string();
    let debug = format!("{err:?}");
    cluster_skip_message(&message, Some(&debug))
        .map(|reason| {
            tracing::warn!("{reason}");
        })
        .is_some()
}

// ============================================================================
// start_async_split() tests
// ============================================================================

/// Tests that `start_async_split()` creates a working handle/guard pair.
#[cfg(feature = "async-api")]
#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds the guard even though the test ignores it"
)]
#[rstest]
fn start_async_split_creates_working_handle_and_guard(
    _serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    let sandbox = TestSandbox::new("async-split").context("create test sandbox")?;
    sandbox.reset()?;

    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| eyre!("failed to build runtime: {e}"))?
            .block_on(run_async_split_lifecycle_test())
    });

    if should_skip_on_error(&result) {
        return Ok(());
    }
    // Shutdown is verified inside run_async_split_lifecycle_test() to ensure
    // the spawned cleanup task has time to execute before the runtime drops.
    result?;

    // Verify environment restored
    let env_after = EnvSnapshot::capture();
    ensure!(
        env_before == env_after,
        "environment should be restored after guard drops"
    );

    Ok(())
}

#[cfg(feature = "async-api")]
async fn run_async_split_lifecycle_test() -> std::result::Result<(), color_eyre::Report> {
    let (handle, guard) = TestCluster::start_async_split()
        .await
        .map_err(color_eyre::Report::from)?;

    // Verify handle provides access to settings
    let data_dir = Utf8PathBuf::from_path_buf(handle.settings().data_dir.clone())
        .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;

    // Verify cluster is running
    ensure!(
        data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should exist while cluster runs"
    );

    // Drop guard to trigger shutdown
    drop(guard);

    // Wait for cleanup inside async context so the spawned cleanup task can complete.
    // The guard's Drop spawns a fire-and-forget task; we must stay in the async context
    // long enough for it to run before the runtime is dropped.
    wait_for_postmaster_shutdown_async(&data_dir).await?;

    Ok(())
}

// ============================================================================
// Test helpers
// ============================================================================

const POSTMASTER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const POSTMASTER_POLL_INTERVAL: Duration = Duration::from_millis(50);
const POSTMASTER_SHUTDOWN_ERROR: &str =
    "postmaster.pid should be removed once cluster stops (waited 10s)";

fn wait_for_postmaster_shutdown(data_dir: &Utf8PathBuf) -> Result<()> {
    use std::time::Instant;

    let pid = data_dir.join("postmaster.pid");
    let deadline = Instant::now() + POSTMASTER_SHUTDOWN_TIMEOUT;

    while pid.exists() && Instant::now() < deadline {
        thread::sleep(POSTMASTER_POLL_INTERVAL);
    }

    ensure!(!pid.exists(), POSTMASTER_SHUTDOWN_ERROR);
    Ok(())
}

/// Async version of shutdown wait for use within tokio runtime.
///
/// The guard's `Drop` spawns a fire-and-forget cleanup task. We must poll
/// for completion inside the async context so the spawned task can execute
/// before the runtime is dropped.
#[cfg(feature = "async-api")]
async fn wait_for_postmaster_shutdown_async(
    data_dir: &Utf8PathBuf,
) -> std::result::Result<(), color_eyre::Report> {
    use std::time::Instant;
    use tokio::time::sleep;

    let pid = data_dir.join("postmaster.pid");
    let deadline = Instant::now() + POSTMASTER_SHUTDOWN_TIMEOUT;

    while pid.exists() && Instant::now() < deadline {
        sleep(POSTMASTER_POLL_INTERVAL).await;
    }

    ensure!(!pid.exists(), POSTMASTER_SHUTDOWN_ERROR);
    Ok(())
}
