//! Integration test for `ClusterHandle::register_shutdown_on_exit()`.
//!
//! Verifies that the shutdown hook can be registered successfully on a running
//! cluster. End-to-end process lifecycle verification is in
//! `shutdown_hook_lifecycle.rs`.
#![cfg(unix)]

#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use color_eyre::eyre::{Result, ensure};
use pg_embedded_setup_unpriv::TestCluster;
use tracing::warn;

/// Returns `true` if the error should cause a soft skip rather than a hard
/// failure.
fn should_skip(message: &str, debug: &str) -> bool {
    cluster_skip_message(message, Some(debug)).is_some()
        || debug.contains("another server might be running")
}

/// Verifies that `register_shutdown_on_exit()` succeeds for a running cluster
/// created via `new_split()`, including idempotent re-registration.
#[test]
fn register_shutdown_on_exit_succeeds_for_running_cluster() -> Result<()> {
    let Ok((handle, guard)) = create_cluster() else {
        return Ok(());
    };

    ensure!(
        handle.register_shutdown_on_exit().is_ok(),
        "register_shutdown_on_exit should succeed"
    );

    // Second call should also succeed (idempotent).
    ensure!(
        handle.register_shutdown_on_exit().is_ok(),
        "idempotent call should succeed"
    );

    drop(guard);
    Ok(())
}

/// Creates a cluster, returning `Err` (and logging a skip message) when the
/// environment cannot support cluster creation.
fn create_cluster() -> Result<(
    pg_embedded_setup_unpriv::ClusterHandle,
    pg_embedded_setup_unpriv::ClusterGuard,
)> {
    let split_result = TestCluster::new_split();

    if let Err(ref err) = split_result {
        let message = err.to_string();
        let debug = format!("{err:?}");
        if should_skip(&message, &debug) {
            warn!("SKIP: {message}");
            return Err(color_eyre::eyre::eyre!("skipped"));
        }
    }

    Ok(split_result?)
}
