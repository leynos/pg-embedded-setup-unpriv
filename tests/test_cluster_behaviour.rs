//! Behavioural coverage for the `TestCluster` lifecycle.
#![cfg(unix)]

use std::cell::RefCell;

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

use env_snapshot::EnvSnapshot;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

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
        let reason = reason.into();
        eprintln!("{reason}");
        self.skip_reason = Some(reason);
    }

    fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
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

    fn record_error(&mut self, err: impl std::fmt::Display + std::fmt::Debug) -> Result<()> {
        let message = err.to_string();
        let debug = format!("{err:?}");
        self.error = Some(message.clone());
        self.cluster = None;
        const SKIP_CONDITIONS: &[(&str, &str)] = &[
            (
                "rate limit exceeded",
                "SKIP-TEST-CLUSTER: rate limit exceeded whilst downloading PostgreSQL",
            ),
            (
                "No such file or directory",
                "SKIP-TEST-CLUSTER: postgres binary unavailable for test cluster",
            ),
            (
                "failed to read worker config",
                "SKIP-TEST-CLUSTER: worker helper cannot access its configuration",
            ),
            (
                "Permission denied",
                "SKIP-TEST-CLUSTER: worker helper lacks filesystem permissions",
            ),
        ];
        if let Some((_, reason)) = SKIP_CONDITIONS
            .iter()
            .find(|(needle, _)| message.contains(needle) || debug.contains(needle))
        {
            self.mark_skip(format!("{reason}: {message}"));
            Ok(())
        } else {
            Ok(())
        }
    }

    fn data_dir(&self) -> Result<&Utf8PathBuf> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.data_dir
            .as_ref()
            .ok_or_else(|| eyre!("cluster did not record a data directory"))
    }

    fn cluster(&self) -> Result<&TestCluster> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("TestCluster was not created"))
    }

    fn env_before(&self) -> Result<&EnvSnapshot> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.env_before
            .as_ref()
            .ok_or_else(|| eyre!("environment snapshot before cluster missing"))
    }

    fn env_during(&self) -> Result<&EnvSnapshot> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.env_during
            .as_ref()
            .ok_or_else(|| eyre!("environment snapshot during cluster missing"))
    }

    fn error(&self) -> Result<&str> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
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

#[fixture]
fn world() -> RefCell<ClusterWorld> {
    RefCell::new(ClusterWorld::new().expect("create cluster world"))
}

#[given("a cluster sandbox for tests")]
fn given_cluster_sandbox(world: &RefCell<ClusterWorld>) -> Result<()> {
    world.borrow().sandbox.reset()
}

#[when("a TestCluster is created")]
fn when_cluster_created(world: &RefCell<ClusterWorld>) -> Result<()> {
    let before = EnvSnapshot::capture();
    let result = {
        let world_ref = world.borrow();
        let vars = world_ref.sandbox.env_without_timezone();
        world_ref.sandbox.with_env(vars, TestCluster::new)
    };
    match result {
        Ok(cluster) => world.borrow_mut().record_cluster(cluster, before),
        Err(err) => world.borrow_mut().record_error(err),
    }
}

#[when("a TestCluster is created without a time zone database")]
fn when_cluster_created_without_timezone(world: &RefCell<ClusterWorld>) -> Result<()> {
    let (result, missing_dir) = {
        let world_ref = world.borrow();
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
            world.borrow_mut().record_error(eyre!(
                "expected cluster creation to fail with missing time zone dir {}",
                missing_dir
            ))
        }
        Err(err) => world.borrow_mut().record_error(err),
    }
}

#[then("the cluster reports sandbox-aligned settings")]
fn then_cluster_reports_settings(world: &RefCell<ClusterWorld>) -> Result<()> {
    let world_ref = world.borrow();
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
        install_dir == world_ref.sandbox.install_dir(),
        "expected install dir {} but observed {}",
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
fn then_environment_applied(world: &RefCell<ClusterWorld>) -> Result<()> {
    let world_ref = world.borrow();
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
fn then_cluster_stops(world: &RefCell<ClusterWorld>) -> Result<()> {
    let mut world_mut = world.borrow_mut();
    if world_mut.is_skipped() {
        return Ok(());
    }
    let data_dir = world_mut.data_dir()?.clone();
    let before = world_mut.env_before()?.clone();

    drop(world_mut.cluster.take());
    let after = EnvSnapshot::capture();

    ensure!(
        !data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should be removed once the cluster drops",
    );
    ensure!(
        after == before,
        "environment should revert to the pre-cluster snapshot",
    );

    Ok(())
}

#[then("the cluster creation reports a time zone error")]
fn then_cluster_reports_timezone_error(world: &RefCell<ClusterWorld>) -> Result<()> {
    let world_ref = world.borrow();
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
fn scenario_cluster_lifecycle(_serial_guard: ScenarioSerialGuard, world: RefCell<ClusterWorld>) {}

#[scenario(path = "tests/features/test_cluster.feature", index = 1)]
fn scenario_cluster_timezone_error(
    _serial_guard: ScenarioSerialGuard,
    world: RefCell<ClusterWorld>,
) {
}
