//! Behavioural coverage for observability instrumentation.
#![cfg(unix)]

use std::cell::RefCell;
use std::fs;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::test_support::capture_info_logs_with_spans;
use pg_embedded_setup_unpriv::{BootstrapResult, TestCluster};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/env_isolation.rs"]
mod env_isolation;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/scenario.rs"]
mod scenario;
#[path = "support/serial.rs"]
mod serial;

use env_isolation::override_env_path;
use sandbox::TestSandbox;
use scenario::expect_fixture;
use serial::{ScenarioSerialGuard, serial_guard};

struct ObservabilityWorld {
    sandbox: TestSandbox,
    logs: Vec<String>,
    error: Option<String>,
}

impl ObservabilityWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("observability")?,
            logs: Vec::new(),
            error: None,
        })
    }

    fn reset(&mut self) {
        self.logs.clear();
        self.error = None;
    }

    fn record_outcome(&mut self, logs: Vec<String>, result: BootstrapResult<()>) {
        self.logs = logs;
        self.error = result.err().map(|err| err.to_string());
    }
}

type WorldFixture = Result<RefCell<ObservabilityWorld>>;

fn borrow_world(world: &WorldFixture) -> Result<&RefCell<ObservabilityWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("observability world failed: {err}")))
}

#[fixture]
fn world() -> WorldFixture {
    Ok(RefCell::new(ObservabilityWorld::new()?))
}

#[given("an observability sandbox")]
fn given_sandbox(world: &WorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    {
        let world_ref = world_cell.borrow();
        world_ref
            .sandbox
            .reset()
            .context("reset observability sandbox")?;
    }
    world_cell.borrow_mut().reset();
    Ok(())
}

#[when("a cluster boots successfully with observability enabled")]
fn when_cluster_boots(world: &WorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (logs, result) = {
        let world_ref = world_cell.borrow();
        let env_vars = world_ref.sandbox.env_without_timezone();
        capture_info_logs_with_spans(|| {
            world_ref
                .sandbox
                .with_env(env_vars, || -> BootstrapResult<()> {
                    let cluster = TestCluster::new()?;
                    drop(cluster);
                    Ok(())
                })
        })
    };
    world_cell.borrow_mut().record_outcome(logs, result);
    Ok(())
}

#[when("cluster bootstrap fails due to an invalid runtime path")]
fn when_cluster_bootstrap_fails(world: &WorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (logs, result) = {
        let world_ref = world_cell.borrow();
        let runtime_file = world_ref.sandbox.install_dir().join("runtime-as-file");
        fs::write(runtime_file.as_std_path(), "not a directory")
            .with_context(|| format!("create runtime file at {runtime_file}"))?;
        let mut vars = world_ref.sandbox.env_without_timezone();
        override_env_path(&mut vars, "PG_RUNTIME_DIR", runtime_file.as_ref());

        capture_info_logs_with_spans(|| {
            world_ref.sandbox.with_env(vars, || -> BootstrapResult<()> {
                let cluster = TestCluster::new()?;
                drop(cluster);
                Ok(())
            })
        })
    };
    world_cell.borrow_mut().record_outcome(logs, result);
    Ok(())
}

#[then("logs include lifecycle, directory, and environment events")]
fn then_logs_cover_lifecycle(world: &WorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if let Some(err) = &world_ref.error {
        return Err(eyre!(format!(
            "cluster bootstrap failed unexpectedly: {err}"
        )));
    }

    ensure!(
        world_ref
            .logs
            .iter()
            .any(|line| line.contains("applied scoped environment variables")),
        "expected environment application log, got {:?}",
        world_ref.logs
    );
    ensure!(
        world_ref.logs.iter().any(|line| line.contains("HOME=set")),
        "expected environment summary, got {:?}",
        world_ref.logs
    );

    let install = world_ref.sandbox.install_dir().to_string();
    ensure!(
        world_ref
            .logs
            .iter()
            .any(|line| { line.contains("ensured directory exists") && line.contains(&install) }),
        "expected directory mutation log for install dir, got {:?}",
        world_ref.logs
    );

    ensure!(
        world_ref.logs.iter().any(|line| {
            line.contains("lifecycle operation completed") && line.contains("operation=setup")
        }),
        "expected setup lifecycle log, got {:?}",
        world_ref.logs
    );
    ensure!(
        world_ref.logs.iter().any(|line| {
            line.contains("lifecycle operation completed") && line.contains("operation=start")
        }),
        "expected start lifecycle log, got {:?}",
        world_ref.logs
    );
    ensure!(
        world_ref
            .logs
            .iter()
            .any(|line| line.contains("stopping embedded postgres cluster")),
        "expected stop log, got {:?}",
        world_ref.logs
    );
    Ok(())
}

#[then("logs capture the directory failure context")]
fn then_logs_capture_failure(world: &WorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    let Some(err) = &world_ref.error else {
        return Err(eyre!("expected bootstrap failure for invalid runtime path"));
    };

    let runtime = world_ref.sandbox.install_dir().join("runtime-as-file");
    let runtime_str = runtime.to_string();
    ensure!(
        world_ref.logs.iter().any(|line| {
            line.contains("failed to ensure directory exists") && line.contains(&runtime_str)
        }),
        "expected directory failure log for runtime file, got {:?}",
        world_ref.logs
    );
    ensure!(
        err.contains(&runtime_str) || err.contains("runtime") || err.contains("installation"),
        "expected error message to reference runtime path, got {err}"
    );
    Ok(())
}

#[scenario(path = "tests/features/observability.feature", index = 0)]
fn scenario_observability_success(serial_guard: ScenarioSerialGuard, world: WorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "observability success world");
}

#[scenario(path = "tests/features/observability.feature", index = 1)]
fn scenario_observability_failure(serial_guard: ScenarioSerialGuard, world: WorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "observability failure world");
}
