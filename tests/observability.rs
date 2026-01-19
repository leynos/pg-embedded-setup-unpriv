//! Behavioural coverage for observability instrumentation.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;

use color_eyre::eyre::{Context, Report, Result, ensure, eyre};
use pg_embedded_setup_unpriv::test_support::capture_info_logs_with_spans;
use pg_embedded_setup_unpriv::{BootstrapResult, TestCluster};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

use camino::{Utf8Path, Utf8PathBuf};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
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
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use env_isolation::override_env_path;
use sandbox::TestSandbox;
use scenario::expect_fixture;
use serial::{ScenarioLocalGuard, local_serial_guard};

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

    fn reset(&mut self) {
        self.logs.clear();
        self.error = None;
        self.skip_reason = None;
    }

    fn record_outcome(&mut self, logs: Vec<String>, result: BootstrapResult<()>) {
        self.logs = logs;
        match result {
            Ok(()) => {
                self.error = None;
            }
            Err(err) => {
                let report = err.into_report();
                let message = report.to_string();
                let debug = format!("{report:?}");
                if permission_denied_in_chain(&report) {
                    self.skip_reason = Some("privilege drop unavailable on host".to_owned());
                } else if coverage_mode() && message.contains("postgresql_embedded::setup() failed")
                {
                    self.skip_reason = Some(
                        "skipping observability success scenario under coverage: embedded postgres setup failed"
                            .to_owned(),
                    );
                } else if let Some(reason) = cluster_skip_message(&message, Some(&debug)) {
                    self.skip_reason = Some(reason);
                }
                self.error = Some(message);
            }
        }
    }
}

type WorldFixture = Result<RefCell<ObservabilityWorld>>;

fn borrow_world(world: &WorldFixture) -> Result<&RefCell<ObservabilityWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!("observability world failed: {err}"))
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
        ensure_runtime_dir_matches(&env_vars, world_ref.sandbox.install_dir())?;
        assert_worker_present(&env_vars)?;
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
        let data_file = world_ref.sandbox.install_dir().join("data-as-file");
        if let Some(parent) = data_file.parent() {
            fs::create_dir_all(parent.as_std_path())
                .with_context(|| format!("create parent directory for {data_file}"))?;
        }
        fs::write(data_file.as_std_path(), "not a directory")
            .with_context(|| format!("create data file at {data_file}"))?;
        let mut vars = world_ref.sandbox.env_without_timezone();
        override_env_path(&mut vars, "PG_DATA_DIR", data_file.as_ref());
        ensure_runtime_dir_matches(&vars, world_ref.sandbox.install_dir())?;
        assert_worker_present(&vars)?;

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
    if world_ref.skip_reason.is_some() {
        return Ok(());
    }
    if let Some(err) = &world_ref.error {
        return Err(eyre!("cluster bootstrap failed unexpectedly: {err}"));
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
            (line.contains("lifecycle operation completed")
                || line.contains("lifecycle_operation")
                || line.contains("worker operation completed successfully"))
                && line.contains("operation=\"setup\"")
        }),
        "expected setup lifecycle log or span, got {:?}",
        world_ref.logs
    );
    ensure!(
        world_ref.logs.iter().any(|line| {
            (line.contains("lifecycle operation completed")
                || line.contains("lifecycle_operation")
                || line.contains("worker operation completed successfully"))
                && line.contains("operation=\"start\"")
        }),
        "expected start lifecycle log or span, got {:?}",
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
    if world_ref.skip_reason.is_some() {
        return Ok(());
    }
    let Some(err) = &world_ref.error else {
        return Err(eyre!("expected bootstrap failure for invalid runtime path"));
    };

    let data_file = world_ref.sandbox.install_dir().join("data-as-file");
    let data_str = data_file.to_string();
    ensure!(
        world_ref.logs.iter().any(|line| {
            line.contains("failed to ensure directory exists") && line.contains(&data_str)
        }),
        "expected directory failure log for data file, got {:?}",
        world_ref.logs
    );
    ensure!(
        err.contains(&data_str) || err.contains("data"),
        "expected error message to reference data path, got {err}"
    );

    let has_lifecycle_span = world_ref
        .logs
        .iter()
        .any(|line| line.contains("lifecycle_operation"));
    if has_lifecycle_span {
        ensure!(
            world_ref.logs.iter().any(|line| {
                line.contains("lifecycle operation failed") && line.contains("setup")
            }),
            "expected lifecycle failure log for setup, got {:?}",
            world_ref.logs
        );
    }
    Ok(())
}

fn permission_denied_in_chain(report: &Report) -> bool {
    use std::io::ErrorKind;

    report.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == ErrorKind::PermissionDenied)
    })
}

fn coverage_mode() -> bool {
    std::env::var("CARGO_LLVM_COV").is_ok()
}

fn ensure_runtime_dir_matches(
    env_vars: &[(OsString, Option<OsString>)],
    install_dir: &Utf8Path,
) -> Result<()> {
    let runtime = env_vars
        .iter()
        .find(|(key, _)| key == &OsString::from("PG_RUNTIME_DIR"))
        .and_then(|(_, value)| value.as_ref())
        .ok_or_else(|| eyre!("PG_RUNTIME_DIR missing from sandbox env"))?;
    ensure!(
        runtime == install_dir.as_os_str(),
        "PG_RUNTIME_DIR expected {}, got {:?}",
        install_dir,
        runtime
    );
    Ok(())
}

fn assert_worker_present(env_vars: &[(OsString, Option<OsString>)]) -> Result<()> {
    let worker = env_vars
        .iter()
        .find(|(key, _)| key == &OsString::from("PG_EMBEDDED_WORKER"))
        .and_then(|(_, value)| value.as_ref())
        .ok_or_else(|| eyre!("PG_EMBEDDED_WORKER not configured in observability sandbox"))?;

    let path = Utf8PathBuf::from_path_buf(std::path::PathBuf::from(worker))
        .map_err(|_| eyre!("worker path not valid UTF-8"))?;
    ensure!(
        path.exists(),
        "worker binary {path} must exist for observability scenarios"
    );
    tracing::info!(worker = %path, "using worker binary for observability scenario");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata =
            std::fs::metadata(path.as_std_path()).with_context(|| format!("stat {path}"))?;
        ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "worker binary {path} must be executable"
        );
    }
    Ok(())
}

#[scenario(path = "tests/features/observability.feature", index = 0)]
fn scenario_observability_success(local_serial_guard: ScenarioLocalGuard, world: WorldFixture) {
    let _guard = local_serial_guard;
    let _ = expect_fixture(world, "observability success world");
}

#[scenario(path = "tests/features/observability.feature", index = 1)]
fn scenario_observability_failure(local_serial_guard: ScenarioLocalGuard, world: WorldFixture) {
    let _guard = local_serial_guard;
    let _ = expect_fixture(world, "observability failure world");
}
