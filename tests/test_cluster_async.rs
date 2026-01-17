//! Async API tests for `TestCluster`.
//!
//! These tests verify that `TestCluster::start_async()` and `stop_async()` work
//! correctly within async contexts like `#[tokio::test]`.
//!
//! All tests acquire the local scenario guard while using sandboxed directories
//! to prevent environment lock ordering issues without sharing global data
//! paths.

#![cfg(all(unix, feature = "async-api"))]

use color_eyre::eyre::{Result, ensure};
use pg_embedded_setup_unpriv::test_support::scoped_env;
use pg_embedded_setup_unpriv::{ScopedEnv, TestCluster};
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;

use sandbox::TestSandbox;
use serial::{ScenarioLocalGuard, local_serial_guard};

async fn start_sandboxed_cluster() -> Result<(TestCluster, TestSandbox, ScopedEnv)> {
    let sandbox = TestSandbox::new("test-cluster-async")?;
    sandbox.reset()?;
    let env_vars = sandbox.env_without_timezone();
    let runtime_dir = sandbox.with_env(env_vars.clone(), || std::env::var("PG_RUNTIME_DIR").ok());
    ensure!(
        runtime_dir.as_deref() == Some(sandbox.install_dir().as_str()),
        "PG_RUNTIME_DIR should match sandbox install dir"
    );
    let env_guard = scoped_env(env_vars);
    let cluster = TestCluster::start_async().await?;
    Ok((cluster, sandbox, env_guard))
}

/// Verifies that `start_async()` can be called from within an async context.
///
/// This is the primary test - it confirms that the async API does not panic with
/// "Cannot start a runtime from within a runtime" when called from `#[tokio::test]`.
#[rstest]
#[tokio::test(flavor = "current_thread")]
async fn start_async_succeeds_in_async_context(
    local_serial_guard: ScenarioLocalGuard,
) -> Result<()> {
    let _guard = local_serial_guard;
    let (cluster, _sandbox, _env_guard) = start_sandboxed_cluster().await?;

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
#[tokio::test(flavor = "current_thread")]
async fn stop_async_cleans_up_resources(local_serial_guard: ScenarioLocalGuard) -> Result<()> {
    let _guard = local_serial_guard;
    let (cluster, _sandbox, _env_guard) = start_sandboxed_cluster().await?;

    // Stop should succeed.
    cluster.stop_async().await?;

    Ok(())
}

/// Verifies that connection metadata is available from async-created clusters.
#[rstest]
#[tokio::test(flavor = "current_thread")]
async fn async_cluster_provides_connection_metadata(
    local_serial_guard: ScenarioLocalGuard,
) -> Result<()> {
    let _guard = local_serial_guard;
    let (cluster, _sandbox, _env_guard) = start_sandboxed_cluster().await?;

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
#[tokio::test(flavor = "current_thread")]
async fn async_cluster_provides_database_url(local_serial_guard: ScenarioLocalGuard) -> Result<()> {
    let _guard = local_serial_guard;
    let (cluster, _sandbox, _env_guard) = start_sandboxed_cluster().await?;

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
#[tokio::test(flavor = "current_thread")]
async fn async_drop_without_stop_does_not_panic(
    local_serial_guard: ScenarioLocalGuard,
) -> Result<()> {
    let _guard = local_serial_guard;
    let (cluster, _sandbox, _env_guard) = start_sandboxed_cluster().await?;

    // Use the cluster briefly to ensure it's functional.
    let _port = cluster.settings().port;

    // Drop without calling stop_async() - this should trigger best-effort cleanup
    // and log a warning, but not panic.
    drop(cluster);

    // Give the cleanup task a moment to complete.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    Ok(())
}
