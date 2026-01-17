//! Async API tests for `TestCluster`.
//!
//! These tests verify that `TestCluster::start_async()` and `stop_async()` work
//! correctly within async contexts like `#[tokio::test]`.
//!
//! All tests are serialized using `file_serial` to prevent conflicts with other
//! cluster tests that use the same default data directory.

#![cfg(all(unix, feature = "async-api"))]

use color_eyre::eyre::{Result, ensure};
use pg_embedded_setup_unpriv::test_support::ensure_worker_env;
use pg_embedded_setup_unpriv::{BootstrapResult, TestCluster};
use rstest::{fixture, rstest};
use serial_test::file_serial;

/// Async fixture that provides a running `TestCluster`.
///
/// This fixture starts the cluster asynchronously and should be used with
/// `#[rstest]` and `#[tokio::test]`. Note that callers are responsible for
/// calling `stop_async()` when done, as the fixture cannot perform async cleanup.
#[fixture]
fn cluster_future() -> impl std::future::Future<Output = BootstrapResult<TestCluster>> {
    let worker_guard = ensure_worker_env();
    async move {
        let _guard = worker_guard;
        TestCluster::start_async().await
    }
}

/// Verifies that `start_async()` can be called from within an async context.
///
/// This is the primary test - it confirms that the async API does not panic with
/// "Cannot start a runtime from within a runtime" when called from `#[tokio::test]`.
#[rstest]
#[tokio::test]
#[file_serial(cluster)]
async fn start_async_succeeds_in_async_context(
    cluster_future: impl std::future::Future<Output = BootstrapResult<TestCluster>>,
) -> Result<()> {
    let cluster = cluster_future.await?;

    // Verify the cluster is functional by checking settings.
    let settings = cluster.settings();
    ensure!(
        settings.port > 0,
        "cluster should have a valid port assigned"
    );

    // Clean up explicitly.
    cluster.stop_async().await?;
    Ok(())
}

/// Verifies that `stop_async()` properly shuts down the cluster.
#[rstest]
#[tokio::test]
#[file_serial(cluster)]
async fn stop_async_cleans_up_resources(
    cluster_future: impl std::future::Future<Output = BootstrapResult<TestCluster>>,
) -> Result<()> {
    let cluster = cluster_future.await?;

    // Stop should succeed.
    cluster.stop_async().await?;

    Ok(())
}

/// Verifies that connection metadata is available from async-created clusters.
#[rstest]
#[tokio::test]
#[file_serial(cluster)]
async fn async_cluster_provides_connection_metadata(
    cluster_future: impl std::future::Future<Output = BootstrapResult<TestCluster>>,
) -> Result<()> {
    let cluster = cluster_future.await?;

    let connection = cluster.connection();
    let metadata = connection.metadata();

    // Verify metadata is populated.
    ensure!(!metadata.host().is_empty(), "host should not be empty");
    ensure!(metadata.port() > 0, "port should be positive");
    ensure!(
        !metadata.superuser().is_empty(),
        "superuser should not be empty"
    );

    cluster.stop_async().await?;
    Ok(())
}

/// Verifies that `database_url` can be constructed from async-created clusters.
#[rstest]
#[tokio::test]
#[file_serial(cluster)]
async fn async_cluster_provides_database_url(
    cluster_future: impl std::future::Future<Output = BootstrapResult<TestCluster>>,
) -> Result<()> {
    let cluster = cluster_future.await?;

    let url = cluster.connection().database_url("test_db");

    // Verify URL format.
    ensure!(
        url.starts_with("postgresql://"),
        "URL should start with postgresql://"
    );
    ensure!(url.contains("test_db"), "URL should contain database name");

    cluster.stop_async().await?;
    Ok(())
}

/// Verifies that dropping an async cluster without calling `stop_async()` does not panic.
///
/// This exercises the best-effort cleanup path in `Drop`, which spawns a cleanup task
/// on the current runtime handle if available.
#[rstest]
#[tokio::test]
#[file_serial(cluster)]
async fn async_drop_without_stop_does_not_panic(
    cluster_future: impl std::future::Future<Output = BootstrapResult<TestCluster>>,
) -> Result<()> {
    let cluster = cluster_future.await?;

    // Use the cluster briefly to ensure it's functional.
    let _port = cluster.settings().port;

    // Drop without calling stop_async() - this should trigger best-effort cleanup
    // and log a warning, but not panic.
    drop(cluster);

    // Give the cleanup task a moment to complete.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    Ok(())
}
