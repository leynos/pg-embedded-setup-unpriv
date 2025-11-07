#![cfg(all(unix, feature = "rstest-fixtures"))]
//! Behavioural coverage for the exported `rstest` `TestCluster` fixture.

use std::cell::RefCell;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::test_support::{test_cluster, try_test_cluster};
use pg_embedded_setup_unpriv::{BootstrapError, TestCluster};
use rstest::{fixture, rstest};
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

use env::ScopedEnvVars;
use env_snapshot::EnvSnapshot;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};
use skip::skip_message;

struct FixtureWorld {
    sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    env_before: Option<EnvSnapshot>,
    env_during: Option<EnvSnapshot>,
    env_after: Option<EnvSnapshot>,
    error: Option<String>,
    skip_reason: Option<String>,
}

type FixtureWorldFixture = Result<RefCell<FixtureWorld>>;

impl FixtureWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("rstest-cluster-fixture")
                .context("create rstest fixture sandbox")?,
            cluster: None,
            env_before: None,
            env_during: None,
            env_after: None,
            error: None,
            skip_reason: None,
        })
    }

    const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    fn mark_skip(&mut self, reason: impl Into<String>) {
        let message = reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    fn ensure_not_skipped(&self) -> Result<()> {
        if self.is_skipped() {
            Err(eyre!("scenario skipped"))
        } else {
            Ok(())
        }
    }

    fn drop_cluster(&mut self) {
        if let Some(cluster) = self.cluster.take() {
            drop(cluster);
            self.env_after = Some(EnvSnapshot::capture());
        }
    }

    fn record_cluster(&mut self, cluster: TestCluster, before: EnvSnapshot) {
        self.env_before = Some(before);
        self.env_during = Some(EnvSnapshot::capture());
        self.error = None;
        self.cluster = Some(cluster);
    }

    fn record_error(&mut self, err: &BootstrapError) {
        let message = err.to_string();
        let debug = format!("{err:?}");
        self.error = Some(message.clone());
        self.cluster = None;
        self.env_before = None;
        self.env_during = None;
        self.env_after = None;
        if let Some(reason) = skip_message("SKIP-TEST-CLUSTER", &message, Some(&debug)) {
            self.mark_skip(reason);
        }
    }

    fn run_with_env(&mut self, vars: ScopedEnvVars) {
        let before = EnvSnapshot::capture();
        match self.sandbox.with_env(vars, try_test_cluster) {
            Ok(cluster) => self.record_cluster(cluster, before),
            Err(err) => self.record_error(&err),
        }
    }

    fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("fixture failed to produce a TestCluster"))
    }

    fn baseline_env(&self) -> Result<&EnvSnapshot> {
        self.ensure_not_skipped()?;
        self.env_before
            .as_ref()
            .ok_or_else(|| eyre!("baseline environment snapshot missing"))
    }

    fn env_during(&self) -> Result<&EnvSnapshot> {
        self.ensure_not_skipped()?;
        self.env_during
            .as_ref()
            .ok_or_else(|| eyre!("environment snapshot during fixture missing"))
    }

    fn env_after(&self) -> Result<&EnvSnapshot> {
        self.ensure_not_skipped()?;
        self.env_after
            .as_ref()
            .ok_or_else(|| eyre!("environment snapshot after drop missing"))
    }

    fn error(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.error
            .as_deref()
            .ok_or_else(|| eyre!("expected fixture to error"))
    }
}

impl Drop for FixtureWorld {
    fn drop(&mut self) {
        self.cluster.take();
    }
}

fn borrow_world(world: &FixtureWorldFixture) -> Result<&RefCell<FixtureWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("fixture world unavailable: {err}")))
}

#[fixture]
fn world() -> FixtureWorldFixture {
    FixtureWorld::new().map(RefCell::new)
}

#[given("an rstest fixture sandbox")]
fn given_rstest_fixture_sandbox(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?;
    world_ref.borrow().sandbox.reset()?;
    Ok(())
}

fn run_with_vars(world: &FixtureWorldFixture, vars: ScopedEnvVars) -> Result<()> {
    let world_ref = borrow_world(world)?;
    world_ref.borrow_mut().run_with_env(vars);
    Ok(())
}

#[when("the rstest fixture runs with the default environment")]
fn when_fixture_runs(world: &FixtureWorldFixture) -> Result<()> {
    let vars = borrow_world(world)?.borrow().sandbox.base_env();
    run_with_vars(world, vars)
}

#[when("the rstest fixture runs without time zone data")]
fn when_fixture_runs_without_timezone(world: &FixtureWorldFixture) -> Result<()> {
    let vars = borrow_world(world)?.borrow().sandbox.env_without_timezone();
    run_with_vars(world, vars)
}

#[then("the fixture starts a cluster bound to the sandbox paths")]
fn then_fixture_aligns_with_sandbox(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let cluster = world_ref.cluster()?;
    let settings = cluster.settings();
    ensure!(
        settings
            .installation_dir
            .starts_with(world_ref.sandbox.install_dir()),
        "installation dir should live under the sandbox",
    );
    ensure!(
        settings.data_dir.starts_with(world_ref.sandbox.data_dir()),
        "data dir should live under the sandbox",
    );
    let during = world_ref.env_during()?;
    ensure!(
        during.pgpassfile.is_some(),
        "PGPASSFILE must be set whilst the fixture runs",
    );
    Ok(())
}

#[then("dropping the fixture restores the process environment")]
fn then_fixture_restores_environment(world: &FixtureWorldFixture) -> Result<()> {
    let mut world_mut = borrow_world(world)?.borrow_mut();
    if world_mut.is_skipped() {
        return Ok(());
    }
    let before = world_mut.baseline_env()?.clone();
    world_mut.drop_cluster();
    let after = world_mut.env_after()?.clone();
    ensure!(
        before == after,
        "environment should match the snapshot captured before the fixture ran",
    );
    Ok(())
}

#[then("the fixture surfaces a time zone bootstrap error")]
fn then_fixture_reports_timezone_error(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let error = world_ref.error()?;
    ensure!(
        error.contains("time zone"),
        "expected a time zone bootstrap error, got {error}",
    );
    Ok(())
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 0)]
fn scenario_fixture_happy_path(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) -> Result<()> {
    let _guard = serial_guard;
    let _ = world?;
    Ok(())
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 1)]
fn scenario_fixture_timezone_error(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) -> Result<()> {
    let _guard = serial_guard;
    let _ = world?;
    Ok(())
}

#[rstest]
fn fixture_supports_zero_setup(test_cluster: TestCluster) -> Result<()> {
    let metadata = test_cluster.connection().metadata();
    ensure!(
        metadata
            .database_url("postgres")
            .starts_with("postgresql://"),
        "fixture should yield a usable connection URL",
    );
    Ok(())
}

#[rstest]
fn fixture_reports_timezone_errors(serial_guard: ScenarioSerialGuard) -> Result<()> {
    let _guard = serial_guard;
    let sandbox =
        TestSandbox::new("rstest-fixture-timezone").context("create timezone error sandbox")?;
    let result = sandbox.with_env(sandbox.env_without_timezone(), try_test_cluster);
    match result {
        Ok(_) => Err(eyre!(
            "expected missing time zone data to surface as an error"
        )),
        Err(err) => {
            ensure!(
                err.to_string().contains("time zone"),
                "unexpected error: {err}",
            );
            Ok(())
        }
    }
}
