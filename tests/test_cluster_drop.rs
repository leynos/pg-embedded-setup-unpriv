//! Unit coverage for the `TestCluster` RAII guard.
#![cfg(unix)]

use camino::Utf8PathBuf;
use color_eyre::Report;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{CleanupMode, TestCluster};
use rstest::rstest;
use std::{thread, time::Duration};

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

fn run_cluster_lifecycle_test() -> std::result::Result<Utf8PathBuf, Report> {
    run_cluster_lifecycle_with_cleanup_mode(CleanupMode::DataOnly).map(|(data, _)| data)
}

fn run_cluster_lifecycle_with_cleanup_mode(
    cleanup_mode: CleanupMode,
) -> std::result::Result<(Utf8PathBuf, Utf8PathBuf), Report> {
    let before_cluster = EnvSnapshot::capture();
    let cluster = TestCluster::new()
        .map_err(Report::from)?
        .with_cleanup_mode(cleanup_mode);
    let during_cluster = EnvSnapshot::capture();
    let data_dir = extract_data_dir(&cluster)?;
    let install_dir = extract_install_dir(&cluster)?;
    verify_cluster_running(&data_dir, &before_cluster, &during_cluster)?;
    drop(cluster);
    Ok((data_dir, install_dir))
}

fn extract_data_dir(cluster: &TestCluster) -> std::result::Result<Utf8PathBuf, Report> {
    Utf8PathBuf::from_path_buf(cluster.settings().data_dir.clone())
        .map_err(|_| eyre!("data_dir is not valid UTF-8"))
}

fn extract_install_dir(cluster: &TestCluster) -> std::result::Result<Utf8PathBuf, Report> {
    Utf8PathBuf::from_path_buf(cluster.settings().installation_dir.clone())
        .map_err(|_| eyre!("installation_dir is not valid UTF-8"))
}

fn verify_cluster_running(
    data_dir: &Utf8PathBuf,
    before_cluster: &EnvSnapshot,
    during_cluster: &EnvSnapshot,
) -> Result<()> {
    ensure!(
        data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should exist while the cluster runs",
    );
    ensure!(
        during_cluster.pgpassfile.is_some(),
        "PGPASSFILE should be set for clients whilst the cluster runs",
    );
    ensure!(
        during_cluster != before_cluster,
        "the environment should change whilst the cluster runs",
    );
    Ok(())
}

fn should_skip_test<T>(result: &std::result::Result<T, Report>) -> bool {
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

fn verify_environment_restored(env_before: &EnvSnapshot) -> Result<()> {
    let env_after = EnvSnapshot::capture();
    ensure!(
        env_before == &env_after,
        "the environment should be restored after the cluster drops",
    );
    Ok(())
}

fn wait_for_postmaster_shutdown(data_dir: &Utf8PathBuf) -> Result<()> {
    let pid = data_dir.join("postmaster.pid");
    for _ in 0..50 {
        if !pid.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        !pid.exists(),
        "postmaster.pid should be removed once the cluster stops",
    );
    Ok(())
}

fn wait_for_dir_cleanup(dir: &Utf8PathBuf, label: &str) -> Result<()> {
    for _ in 0..50 {
        if !dir.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        !dir.exists(),
        "{label} directory should be removed once the cluster drops",
    );
    Ok(())
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds the guard even though the test ignores it"
)]
#[rstest]
fn drops_stop_cluster_and_restore_environment(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("test-cluster-unit").context("create test cluster sandbox")?;
    sandbox.reset()?;
    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), run_cluster_lifecycle_test);
    if should_skip_test(&result) {
        return Ok(());
    }
    let data_dir = result?;
    verify_environment_restored(&env_before)?;
    wait_for_postmaster_shutdown(&data_dir)?;
    wait_for_dir_cleanup(&data_dir, "data")?;
    Ok(())
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds the guard even though the test ignores it"
)]
#[rstest]
fn drops_remove_install_dir_when_cleanup_full(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("test-cluster-unit-full-cleanup")
        .context("create test cluster sandbox")?;
    sandbox.reset()?;
    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        run_cluster_lifecycle_with_cleanup_mode(CleanupMode::Full)
    });
    if should_skip_test(&result) {
        return Ok(());
    }
    let (data_dir, install_dir) = result?;
    verify_environment_restored(&env_before)?;
    wait_for_postmaster_shutdown(&data_dir)?;
    wait_for_dir_cleanup(&data_dir, "data")?;
    wait_for_dir_cleanup(&install_dir, "installation")?;
    Ok(())
}
