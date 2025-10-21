//! Unit coverage for the `TestCluster` RAII guard.
#![cfg(unix)]

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, eyre};
use pg_embedded_setup_unpriv::TestCluster;
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
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

use env_snapshot::EnvSnapshot;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};
use skip::skip_message;

#[rstest]
fn drops_stop_cluster_and_restore_environment(serial_guard: ScenarioSerialGuard) -> Result<()> {
    let _ = &serial_guard;
    let sandbox = TestSandbox::new("test-cluster-unit").context("create test cluster sandbox")?;
    sandbox.reset()?;
    let tz_override = sandbox.env_with_timezone_override(sandbox.install_dir());
    assert!(
        tz_override.iter().any(|(key, _)| key == "TZDIR"),
        "timezone override should include TZDIR"
    );

    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        let before_cluster = EnvSnapshot::capture();
        let cluster = TestCluster::new().map_err(color_eyre::Report::from)?;
        let during_cluster = EnvSnapshot::capture();

        let data_dir = Utf8PathBuf::from_path_buf(cluster.settings().data_dir.clone())
            .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;

        assert!(
            data_dir.join("postmaster.pid").exists(),
            "postmaster.pid should exist while the cluster runs",
        );
        assert!(
            during_cluster.pgpassfile.is_some(),
            "PGPASSFILE should be set for clients whilst the cluster runs",
        );
        assert_ne!(
            during_cluster, before_cluster,
            "the environment should change whilst the cluster runs",
        );

        drop(cluster);
        Ok::<Utf8PathBuf, color_eyre::Report>(data_dir)
    });

    let data_dir = match result {
        Ok(path) => path,
        Err(err) => {
            let message = err.to_string();
            if let Some(reason) = skip_message("SKIP-TEST-CLUSTER", &message, None) {
                eprintln!("{reason}");
                return Ok(());
            }
            return Err(err);
        }
    };

    let env_after = EnvSnapshot::capture();
    assert_eq!(
        env_before, env_after,
        "the environment should be restored after the cluster drops",
    );
    assert!(
        !data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should be removed once the cluster stops",
    );

    Ok(())
}
