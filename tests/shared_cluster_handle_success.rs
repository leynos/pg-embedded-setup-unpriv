//! Tests that `shared_cluster_handle()` caches successful initialisation.
//!
//! This test file runs in its own process (Cargo compiles each `tests/*.rs`
//! file as a separate binary), ensuring the global `OnceLock` state is isolated
//! from other tests.
#![cfg(unix)]

#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use pg_embedded_setup_unpriv::BootstrapError;
use pg_embedded_setup_unpriv::test_support::shared_cluster_handle;
use tracing::warn;

/// Returns true if the error indicates another server is running.
fn is_parallel_execution_conflict(debug: &str) -> bool {
    debug.contains("another server might be running")
}

/// Checks if the test should be skipped due to environment conditions.
///
/// Returns `Some(reason)` if the test should skip, `None` otherwise.
fn skip_reason(err: &BootstrapError) -> Option<String> {
    let debug = format!("{err:?}");

    if is_parallel_execution_conflict(&debug) {
        return Some("another server is running (likely parallel test execution)".to_owned());
    }

    let message = err.to_string();
    cluster_skip_message(&message, Some(&debug))
}

/// Verifies that `shared_cluster_handle()` returns the same leaked reference
/// on subsequent calls, proving that successful initialisation is cached.
#[expect(
    clippy::cognitive_complexity,
    reason = "Test readability benefits from linear flow rather than excessive fragmentation"
)]
#[test]
fn caches_successful_initialisation() {
    let result1 = shared_cluster_handle();

    // Handle skip conditions (e.g., PostgreSQL not available, or another
    // server already running - which can happen when running test binaries
    // in parallel)
    if let Err(ref err) = result1 {
        if let Some(reason) = skip_reason(err) {
            warn!("SKIP: {reason}");
            return;
        }
    }

    let handle1 = result1.expect("first call should succeed");

    // Verify cluster is usable (if connection works)
    // This may fail in some CI environments, so we log but don't fail the test
    if let Err(e) = handle1.database_exists("postgres") {
        warn!("NOTE: database_exists check failed (cluster may still be starting): {e}");
    }

    // Second call must return identical leaked reference
    let handle2 = shared_cluster_handle().expect("second call should succeed");

    assert!(
        std::ptr::eq(handle1, handle2),
        "shared_cluster_handle did not return same leaked reference"
    );

    // Third call for good measure
    let handle3 = shared_cluster_handle().expect("third call should succeed");

    assert!(
        std::ptr::eq(handle1, handle3),
        "shared_cluster_handle did not return same leaked reference on third call"
    );
}
