#![cfg(all(unix, feature = "async-api"))]
//! Async API tests for `TestCluster`.
//!
//! These tests verify that `TestCluster::start_async()` and `stop_async()` work
//! correctly within async contexts like `#[tokio::test]`.
//!
//! All tests are serialized using `file_serial` to prevent conflicts with other
//! cluster tests that use the same default data directory.

use color_eyre::eyre::{Result, ensure};
use pg_embedded_setup_unpriv::TestCluster;
use serial_test::file_serial;

/// Verifies that `start_async()` can be called from within an async context.
///
/// This is the primary test - it confirms that the async API does not panic with
/// "Cannot start a runtime from within a runtime" when called from `#[tokio::test]`.
#[tokio::test]
#[file_serial(cluster)]
async fn start_async_succeeds_in_async_context() -> Result<()> {
    let cluster = TestCluster::start_async().await?;

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
#[tokio::test]
#[file_serial(cluster)]
async fn stop_async_cleans_up_resources() -> Result<()> {
    let cluster = TestCluster::start_async().await?;

    // Stop should succeed.
    cluster.stop_async().await?;

    Ok(())
}

/// Verifies that connection metadata is available from async-created clusters.
#[tokio::test]
#[file_serial(cluster)]
async fn async_cluster_provides_connection_metadata() -> Result<()> {
    let cluster = TestCluster::start_async().await?;

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
#[tokio::test]
#[file_serial(cluster)]
async fn async_cluster_provides_database_url() -> Result<()> {
    let cluster = TestCluster::start_async().await?;

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
