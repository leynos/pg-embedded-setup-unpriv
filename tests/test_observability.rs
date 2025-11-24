#![cfg(unix)]
//! Observability coverage for lifecycle, filesystem, and environment tracing.

use std::cell::RefCell;

use color_eyre::{
    Report,
    eyre::{Result, ensure, eyre},
};
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges, TestCluster, WorkerInvoker, WorkerOperation,
    test_support::{capture_info_logs, dummy_settings, test_runtime},
};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/scenario.rs"]
mod scenario;
#[path = "support/skip.rs"]
mod skip;

use sandbox::TestSandbox;
use scenario::expect_fixture;
use skip::skip_message;

struct ObservabilityWorld {
    sandbox: TestSandbox,
    logs: Vec<String>,
    error: Option<String>,
    skip_reason: Option<String>,
}

impl ObservabilityWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("observability")?,
            logs: Vec::new(),
            error: None,
            skip_reason: None,
        })
    }

    fn record_logs(&mut self, logs: Vec<String>, result: Result<()>) {
        self.logs = logs;
        match result {
            Ok(()) => {
                self.error = None;
                self.skip_reason = None;
            }
            Err(err) => {
                let message = err.to_string();
                let debug = format!("{err:?}");
                if let Some(reason) = skip_message("SKIP-TEST-CLUSTER", &message, Some(&debug)) {
                    self.skip_reason = Some(reason);
                    self.error = None;
                } else {
                    self.error = Some(message);
                }
            }
        }
    }

    fn ensure_not_skipped(&self) -> Result<()> {
        if let Some(reason) = &self.skip_reason {
            Err(eyre!(format!("scenario skipped: {reason}")))
        } else {
            Ok(())
        }
    }

    fn assert_success(&self) -> Result<()> {
        self.ensure_not_skipped()?;
        ensure!(
            self.error.is_none(),
            "expected success but got {:?}",
            self.error
        );
        Ok(())
    }

    fn expect_failure(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.error
            .as_deref()
            .ok_or_else(|| eyre!("expected failure but scenario succeeded"))
    }
}

type ObservabilityWorldFixture = Result<RefCell<ObservabilityWorld>>;

#[fixture]
fn world() -> ObservabilityWorldFixture {
    Ok(RefCell::new(ObservabilityWorld::new()?))
}

fn borrow_world(world: &ObservabilityWorldFixture) -> Result<&RefCell<ObservabilityWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("observability world failed: {err}")))
}

#[given("a fresh observability sandbox")]
fn given_sandbox(world: &ObservabilityWorldFixture) -> Result<()> {
    borrow_world(world)?.borrow().sandbox.reset()
}

#[given("observability log capture is installed")]
fn given_log_capture(_world: &ObservabilityWorldFixture) -> Result<()> {
    // Logging capture is applied per action via `capture_info_logs`.
    Ok(())
}

#[when("a TestCluster boots successfully")]
fn when_cluster_boots(world: &ObservabilityWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (logs, result) = {
        let world_ref = world_cell.borrow();
        capture_info_logs(|| {
            let vars = world_ref.sandbox.base_env();
            world_ref.sandbox.with_env(vars, || {
                let cluster = TestCluster::new();
                match cluster {
                    Ok(cluster) => {
                        drop(cluster);
                        Ok(())
                    }
                    Err(err) => Err(Report::from(err)),
                }
            })
        })
    };

    world_cell.borrow_mut().record_logs(logs, result);
    Ok(())
}

#[when("a lifecycle operation fails")]
fn when_lifecycle_fails(world: &ObservabilityWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (logs, result) = capture_info_logs(|| {
        let runtime = test_runtime().map_err(Report::from)?;
        let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        let env_vars = bootstrap.environment.to_env();
        let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);

        invoker
            .invoke(WorkerOperation::Setup, async {
                Err::<(), postgresql_embedded::Error>(
                    postgresql_embedded::Error::DatabaseStartError("boom".into()),
                )
            })
            .map_err(Report::from)
    });

    world_cell.borrow_mut().record_logs(logs, result);
    Ok(())
}

#[then("lifecycle, environment, and filesystem events are logged")]
fn then_observability_logged(world: &ObservabilityWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    world_ref.assert_success()?;

    let logs = &world_ref.logs;
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: applying scoped environment")),
        "expected environment scope application log, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: restoring environment to prior values")),
        "expected environment restoration log, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: ensuring directory exists")),
        "expected filesystem creation log, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: applying permissions")),
        "expected permission log, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .filter(|line| line.contains("observability: invoking lifecycle operation"))
            .count()
            >= 2,
        "expected lifecycle logs for setup and start, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: cluster bootstrap complete")),
        "expected bootstrap completion log, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: stopping cluster")),
        "expected drop log, got {logs:?}",
    );

    Ok(())
}

#[then("the failure is reported in the logs")]
fn then_failure_logged(world: &ObservabilityWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    let error = world_ref.expect_failure()?;
    ensure!(
        error.contains("setup() failed"),
        "expected setup failure context, got {error:?}",
    );

    let logs = &world_ref.logs;
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: lifecycle operation failed")),
        "expected lifecycle failure log, got {logs:?}",
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("observability: invoking lifecycle operation")),
        "expected lifecycle invocation log, got {logs:?}",
    );

    Ok(())
}

#[scenario(path = "tests/features/observability.feature", index = 0)]
fn scenario_observability_logs(world: ObservabilityWorldFixture) {
    let _ = expect_fixture(world, "observability world");
}

#[scenario(path = "tests/features/observability.feature", index = 1)]
fn scenario_lifecycle_failure(world: ObservabilityWorldFixture) {
    let _ = expect_fixture(world, "observability world");
}
