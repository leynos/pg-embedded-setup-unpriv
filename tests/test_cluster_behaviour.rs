//! Behavioural coverage for the `TestCluster` lifecycle.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

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

use env::ScopedEnvVars;
use env_snapshot::EnvSnapshot;
use once_cell::sync::Lazy;
use sandbox::TestSandbox;

static ENV_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
static SCENARIO_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

struct ScenarioSerialGuard {
    _guard: MutexGuard<'static, ()>,
}

#[fixture]
fn serial_guard() -> ScenarioSerialGuard {
    let guard = SCENARIO_MUTEX
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    ScenarioSerialGuard { _guard: guard }
}

struct ClusterWorld {
    sandbox: TestSandbox,
    env_guard: Option<Vec<(OsString, Option<OsString>)>>,
    env_lock: Option<MutexGuard<'static, ()>>,
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
            env_guard: None,
            env_lock: None,
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

    fn apply_env(&mut self, vars: ScopedEnvVars) {
        if self.env_guard.is_some() {
            return;
        }
        let lock = ENV_MUTEX.lock().expect("environment mutex poisoned");
        let mut saved = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            let previous = std::env::var_os(&key);
            match value {
                Some(ref new_value) => unsafe {
                    // SAFETY: access is serialised by `env_lock` and restored before releasing it.
                    std::env::set_var(&key, new_value);
                },
                None => unsafe {
                    std::env::remove_var(&key);
                },
            }
            saved.push((key, previous));
        }
        self.env_guard = Some(saved);
        self.env_lock = Some(lock);
    }

    fn restore_env(&mut self) {
        if let Some(saved) = self.env_guard.take() {
            for (key, value) in saved.into_iter().rev() {
                match value {
                    Some(previous) => unsafe {
                        std::env::set_var(&key, previous);
                    },
                    None => unsafe {
                        std::env::remove_var(&key);
                    },
                }
            }
        }
        self.env_lock.take();
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
        self.restore_env();
        const SKIP_CONDITIONS: &[(&str, &str)] = &[
            (
                "rate limit exceeded",
                "SKIP-TEST-CLUSTER: rate limit exceeded whilst downloading PostgreSQL",
            ),
            (
                "No such file or directory",
                "SKIP-TEST-CLUSTER: postgres binary unavailable for test cluster",
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
        self.restore_env();
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
    let mut world_mut = world.borrow_mut();
    let vars = world_mut.sandbox.env_without_timezone();
    world_mut.apply_env(vars);
    let before = EnvSnapshot::capture();
    match TestCluster::new() {
        Ok(cluster) => world_mut.record_cluster(cluster, before),
        Err(err) => world_mut.record_error(err),
    }
}

#[when("a TestCluster is created without a timezone database")]
fn when_cluster_created_without_timezone(world: &RefCell<ClusterWorld>) -> Result<()> {
    let mut world_mut = world.borrow_mut();
    let missing_dir = world_mut.sandbox.install_dir().join("missing-tz");
    let vars = world_mut.sandbox.env_with_timezone_override(&missing_dir);
    world_mut.apply_env(vars);
    match TestCluster::new() {
        Ok(cluster) => {
            drop(cluster);
            world_mut.record_error(eyre!("expected cluster creation to fail"))
        }
        Err(err) => world_mut.record_error(err),
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
    world_mut.restore_env();

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
