#![cfg(unix)]
//! Behaviour and unit coverage for the public `rstest` fixture.

use std::{any::Any, cell::RefCell, panic::AssertUnwindSafe};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{TestCluster, test_support::test_cluster};
use rstest::{fixture, rstest};
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};
use skip::skip_message;

fn run_unit_fixture_test<F>(name: &str, test: F) -> Result<()>
where
    F: FnOnce(TestCluster) -> Result<()>,
{
    let sandbox =
        TestSandbox::new(name).context("create rstest fixture sandbox for unit coverage")?;
    sandbox
        .reset()
        .context("reset rstest fixture sandbox before applying env")?;
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        std::panic::catch_unwind(AssertUnwindSafe(test_cluster))
    });
    result.map_or_else(handle_fixture_panic, test)
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
fn fixture_exposes_connection_metadata(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    run_unit_fixture_test("rstest-fixture-metadata", |test_cluster| {
        let metadata = test_cluster.connection().metadata();
        ensure!(
            metadata.port() > 0,
            "fixture should yield a running PostgreSQL instance",
        );
        ensure!(
            metadata.superuser() == "postgres",
            "tests rely on deterministic sandbox credentials",
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
fn fixture_reuses_cluster_environment(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    run_unit_fixture_test("rstest-fixture-env", |test_cluster| {
        let env = test_cluster.environment();
        ensure!(
            env.pgpass_file.starts_with(env.home.as_str()),
            "pgpass file should live under HOME for the fixture",
        );
        ensure!(
            test_cluster.settings().data_dir.exists(),
            "data directory should exist whilst the cluster runs",
        );
        Ok(())
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
enum FixtureEnvProfile {
    #[default]
    Default,
    MissingTimezone,
}

struct FixtureWorld {
    sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    panic_message: Option<String>,
    skip_reason: Option<String>,
    env_profile: FixtureEnvProfile,
}

impl FixtureWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("rstest-fixture").context("create fixture sandbox")?,
            cluster: None,
            panic_message: None,
            skip_reason: None,
            env_profile: FixtureEnvProfile::Default,
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

    fn record_cluster(&mut self, cluster: TestCluster) {
        self.cluster = Some(cluster);
        self.panic_message = None;
        self.skip_reason = None;
    }

    fn record_failure(&mut self, payload: Box<dyn Any + Send>) {
        let message = panic_payload_to_string(payload);
        self.cluster = None;
        if let Some(reason) = skip_message("SKIP-TEST-CLUSTER", &message, None) {
            self.mark_skip(reason);
        } else {
            self.panic_message = Some(message);
        }
    }

    fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("test_cluster fixture did not yield a cluster"))
    }

    fn panic_message(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.panic_message
            .as_deref()
            .ok_or_else(|| eyre!("fixture should have recorded a panic"))
    }
}

impl Drop for FixtureWorld {
    fn drop(&mut self) {
        drop(self.cluster.take());
    }
}

type FixtureWorldFixture = Result<RefCell<FixtureWorld>>;

fn borrow_world(world: &FixtureWorldFixture) -> Result<&RefCell<FixtureWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("fixture world failed to initialise: {err}")))
}

#[fixture]
fn world() -> FixtureWorldFixture {
    Ok(RefCell::new(FixtureWorld::new()?))
}

#[given("the rstest fixture uses the default environment")]
fn given_default_fixture(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::Default;
    Ok(())
}

#[given("the rstest fixture runs without time zone data")]
fn given_missing_timezone(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::MissingTimezone;
    Ok(())
}

#[when("test_cluster is invoked via rstest")]
fn when_fixture_runs(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let env_profile = { world_cell.borrow().env_profile };
    let result = {
        let world_ref = world_cell.borrow();
        let vars = match env_profile {
            FixtureEnvProfile::Default => world_ref.sandbox.env_without_timezone(),
            FixtureEnvProfile::MissingTimezone => {
                let missing_dir = world_ref.sandbox.install_dir().join("missing-tz");
                world_ref
                    .sandbox
                    .env_with_timezone_override(missing_dir.as_ref())
            }
        };
        world_ref.sandbox.with_env(vars, || {
            std::panic::catch_unwind(AssertUnwindSafe(test_cluster))
        })
    };

    match result {
        Ok(cluster) => {
            world_cell.borrow_mut().record_cluster(cluster);
            Ok(())
        }
        Err(payload) => {
            world_cell.borrow_mut().record_failure(payload);
            Ok(())
        }
    }
}

#[then("the fixture yields a running TestCluster")]
fn then_fixture_yields_cluster(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let cluster = world_ref.cluster()?;
    let data_dir = &cluster.settings().data_dir;
    ensure!(
        data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should be present whilst the fixture is active",
    );
    Ok(())
}

#[then("the fixture reports a missing timezone error")]
fn then_fixture_reports_timezone_error(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref.panic_message()?;
    ensure!(
        message.contains("time zone"),
        "expected a timezone error but observed: {message}",
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
fn scenario_fixture_missing_timezone(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) -> Result<()> {
    let _guard = serial_guard;
    let _ = world?;
    Ok(())
}

fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(fallback) => fallback.downcast::<&'static str>().map_or_else(
            |_| "non-string panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    }
}

fn handle_fixture_panic(payload: Box<dyn Any + Send>) -> Result<()> {
    let message = panic_payload_to_string(payload);
    skip_message("SKIP-TEST-CLUSTER", &message, None).map_or_else(
        || Err(eyre!(message)),
        |reason| {
            tracing::warn!("{reason}");
            Ok(())
        },
    )
}
