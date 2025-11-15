//! Behavioural coverage for the `TestCluster` lifecycle.
#![cfg(unix)]

use std::{cell::RefCell, thread, time::Duration};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::TestCluster;
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

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

struct ClusterWorld {
    sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    data_dir: Option<Utf8PathBuf>,
    error: Option<String>,
    skip_reason: Option<String>,
    env_before: Option<EnvSnapshot>,
    env_during: Option<EnvSnapshot>,
}

impl ClusterWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("test-cluster-bdd").context("create cluster sandbox")?,
            cluster: None,
            data_dir: None,
            error: None,
            skip_reason: None,
            env_before: None,
            env_during: None,
        })
    }

    fn mark_skip(&mut self, reason: impl Into<String>) {
        let message = reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    fn ensure_not_skipped(&self) -> Result<()> {
        if self.is_skipped() {
            Err(eyre!("scenario skipped"))
        } else {
            Ok(())
        }
    }

    fn record_cluster(&mut self, cluster: TestCluster, before: EnvSnapshot) -> Result<()> {
        let during = EnvSnapshot::capture();
        let data_dir = Utf8PathBuf::from_path_buf(cluster.settings().data_dir.clone())
            .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;
        self.env_before = Some(before);
        self.env_during = Some(during);
        self.data_dir = Some(data_dir);
        self.error = None;
        self.cluster = Some(cluster);
        Ok(())
    }

    fn record_error(&mut self, err: impl std::fmt::Display + std::fmt::Debug) {
        let message = err.to_string();
        let debug = format!("{err:?}");
        self.error = Some(message.clone());
        self.cluster = None;
        if let Some(reason) = skip_message("SKIP-TEST-CLUSTER", &message, Some(&debug)) {
            self.mark_skip(reason);
        }
    }

    fn data_dir(&self) -> Result<&Utf8PathBuf> {
        self.ensure_not_skipped()?;
        self.data_dir
            .as_ref()
            .ok_or_else(|| eyre!("cluster did not record a data directory"))
    }

    fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("TestCluster was not created"))
    }

    fn env_before(&self) -> Result<&EnvSnapshot> {
        self.ensure_not_skipped()?;
        self.env_before
            .as_ref()
            .ok_or_else(|| eyre!("environment snapshot before cluster missing"))
    }

    fn env_during(&self) -> Result<&EnvSnapshot> {
        self.ensure_not_skipped()?;
        self.env_during
            .as_ref()
            .ok_or_else(|| eyre!("environment snapshot during cluster missing"))
    }

    fn error(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.error
            .as_deref()
            .ok_or_else(|| eyre!("cluster creation succeeded unexpectedly"))
    }
}

impl Drop for ClusterWorld {
    fn drop(&mut self) {
        drop(self.cluster.take());
    }
}

type ClusterWorldFixture = Result<RefCell<ClusterWorld>>;

fn borrow_world(world: &ClusterWorldFixture) -> Result<&RefCell<ClusterWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("cluster fixture construction failed: {err}")))
}

#[fixture]
fn world() -> ClusterWorldFixture {
    Ok(RefCell::new(ClusterWorld::new()?))
}

#[given("a cluster sandbox for tests")]
fn given_cluster_sandbox(world: &ClusterWorldFixture) -> Result<()> {
    borrow_world(world)?.borrow().sandbox.reset()
}

#[when("a TestCluster is created")]
fn when_cluster_created(world: &ClusterWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let before = EnvSnapshot::capture();
    let result = {
        let world_ref = world_cell.borrow();
        let vars = world_ref.sandbox.env_without_timezone();
        world_ref.sandbox.with_env(vars, TestCluster::new)
    };
    match result {
        Ok(cluster) => world_cell.borrow_mut().record_cluster(cluster, before),
        Err(err) => {
            world_cell.borrow_mut().record_error(err);
            Ok(())
        }
    }
}

#[when("a TestCluster is created without a time zone database")]
fn when_cluster_created_without_timezone(world: &ClusterWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (result, missing_dir) = {
        let world_ref = world_cell.borrow();
        let missing_dir = world_ref.sandbox.install_dir().join("missing-tz");
        (
            world_ref.sandbox.with_env(
                world_ref.sandbox.env_with_timezone_override(&missing_dir),
                TestCluster::new,
            ),
            missing_dir,
        )
    };
    match result {
        Ok(cluster) => {
            drop(cluster);
            world_cell.borrow_mut().record_error(eyre!(
                "expected cluster creation to fail with missing time zone dir {missing_dir}"
            ));
            Ok(())
        }
        Err(err) => {
            world_cell.borrow_mut().record_error(err);
            Ok(())
        }
    }
}

#[then("the cluster reports sandbox-aligned settings")]
fn then_cluster_reports_settings(world: &ClusterWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let cluster = world_ref.cluster()?;
    let settings = cluster.settings();
    let install_dir = Utf8PathBuf::from_path_buf(settings.installation_dir.clone())
        .map_err(|_| eyre!("installation_dir is not valid UTF-8"))?;
    let data_dir = Utf8PathBuf::from_path_buf(settings.data_dir.clone())
        .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;

    ensure!(
        install_dir.starts_with(world_ref.sandbox.install_dir()),
        "expected install dir to reside under {} but observed {}",
        world_ref.sandbox.install_dir(),
        install_dir
    );
    ensure!(
        data_dir == world_ref.sandbox.data_dir(),
        "expected data dir {} but observed {}",
        world_ref.sandbox.data_dir(),
        data_dir
    );
    ensure!(
        cluster.bootstrap().environment.pgpass_file
            == world_ref.sandbox.install_dir().join(".pgpass"),
        "expected pgpass to reside under the sandbox install dir",
    );

    Ok(())
}

#[then("the environment remains applied whilst the cluster runs")]
fn then_environment_applied(world: &ClusterWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let before = world_ref.env_before()?;
    let during = world_ref.env_during()?;
    let env = world_ref.cluster()?.environment();

    ensure!(
        env.home == world_ref.sandbox.install_dir(),
        "HOME should match the sandbox install directory",
    );
    ensure!(
        during.pgpassfile.is_some(),
        "PGPASSFILE must be populated whilst the cluster runs",
    );
    ensure!(
        during != before,
        "environment should change whilst the cluster runs",
    );

    Ok(())
}

#[then("the cluster stops automatically on drop")]
fn then_cluster_stops(world: &ClusterWorldFixture) -> Result<()> {
    let mut world_mut = borrow_world(world)?.borrow_mut();
    if world_mut.is_skipped() {
        return Ok(());
    }
    let data_dir = world_mut.data_dir()?.clone();
    let pid = data_dir.join("postmaster.pid");
    let before = world_mut.env_before()?.clone();

    drop(world_mut.cluster.take());
    let after = EnvSnapshot::capture();

    for _ in 0..50 {
        if !pid.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    ensure!(
        !pid.exists(),
        "postmaster.pid should be removed once the cluster drops",
    );
    ensure!(
        after == before,
        "environment should revert to the pre-cluster snapshot",
    );

    Ok(())
}

#[then("the cluster creation reports a time zone error")]
fn then_cluster_reports_timezone_error(world: &ClusterWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let error = world_ref.error()?;
    ensure!(
        error.contains("time zone"),
        "expected a time zone error but observed: {error}",
    );
    Ok(())
}

#[scenario(path = "tests/features/test_cluster.feature", index = 0)]
fn scenario_cluster_lifecycle(serial_guard: ScenarioSerialGuard, world: ClusterWorldFixture) {
    let _guard = serial_guard;
    let _ = world.expect("cluster world fixture failed");
}

#[scenario(path = "tests/features/test_cluster.feature", index = 1)]
fn scenario_cluster_timezone_error(serial_guard: ScenarioSerialGuard, world: ClusterWorldFixture) {
    let _guard = serial_guard;
    let _ = world.expect("cluster world fixture failed");
}
