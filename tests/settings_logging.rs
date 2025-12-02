//! Behavioural coverage for settings observability and redaction.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs;

use color_eyre::eyre::{Context, Report, Result, ensure, eyre};
use pg_embedded_setup_unpriv::test_support::capture_debug_logs;
use pg_embedded_setup_unpriv::{BootstrapResult, bootstrap_for_tests};
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
mod scenario_support;
#[path = "support/serial.rs"]
mod serial;

use env_isolation::{override_env_os, override_env_path};
use sandbox::TestSandbox;
use scenario_support::expect_fixture;
use serial::{ScenarioSerialGuard, serial_guard};

struct SettingsLoggingWorld {
    sandbox: TestSandbox,
    logs: Vec<String>,
    error: Option<String>,
    skip_reason: Option<String>,
}

impl SettingsLoggingWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("settings-logging")
                .context("create settings logging sandbox")?,
            logs: Vec::new(),
            error: None,
            skip_reason: None,
        })
    }

    fn reset(&mut self) -> Result<()> {
        self.sandbox.reset()?;
        self.logs.clear();
        self.error = None;
        self.skip_reason = None;
        Ok(())
    }

    fn record_outcome<T>(&mut self, logs: Vec<String>, outcome: BootstrapResult<T>) {
        self.logs = logs;
        match outcome {
            Ok(_) => {
                self.error = None;
                self.skip_reason = None;
            }
            Err(err) => {
                let report = Report::from(err);
                if permission_denied_in_chain(&report) {
                    self.skip_reason = Some("privilege drop unavailable on host".to_owned());
                    self.error = None;
                    return;
                }
                self.error = Some(report.to_string());
            }
        }
    }

    fn logs(&self) -> Result<&[String]> {
        if let Some(reason) = &self.skip_reason {
            return Err(eyre!("scenario skipped: {reason}"));
        }
        Ok(&self.logs)
    }
}

type WorldFixture = Result<RefCell<SettingsLoggingWorld>>;

fn borrow_world(world: &WorldFixture) -> Result<&RefCell<SettingsLoggingWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!("settings logging fixture failed: {err}"))
}

#[fixture]
fn world() -> WorldFixture {
    Ok(RefCell::new(SettingsLoggingWorld::new()?))
}

#[given("a settings logging sandbox")]
fn given_settings_logging_sandbox(world: &WorldFixture) -> Result<()> {
    borrow_world(world)?.borrow_mut().reset()
}

#[when("bootstrap prepares settings with debug logging enabled")]
fn when_bootstrap_logs(world: &WorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (logs, outcome) = {
        let world_ref = world_cell.borrow();
        let mut vars = world_ref.sandbox.base_env();
        override_password_and_port(&mut vars);
        capture_debug_logs(|| world_ref.sandbox.with_env(vars, bootstrap_for_tests))
    };
    world_cell.borrow_mut().record_outcome(logs, outcome);
    Ok(())
}

#[when("bootstrap fails while preparing settings with debug logging enabled")]
fn when_bootstrap_logging_fails(world: &WorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let (logs, outcome) = {
        let world_ref = world_cell.borrow();
        let data_file = world_ref.sandbox.install_dir().join("data-as-file");
        if let Some(parent) = data_file.parent() {
            fs::create_dir_all(parent.as_std_path())
                .with_context(|| format!("create parent directory for {data_file}"))?;
        }
        fs::write(data_file.as_std_path(), "not a directory")
            .with_context(|| format!("create file at {data_file}"))?;

        let mut vars = world_ref.sandbox.env_without_timezone();
        override_password_and_port(&mut vars);
        override_env_path(&mut vars, "PG_DATA_DIR", data_file.as_ref());

        capture_debug_logs(|| world_ref.sandbox.with_env(vars, bootstrap_for_tests))
    };
    world_cell.borrow_mut().record_outcome(logs, outcome);
    Ok(())
}

#[then("the debug logs include sanitized settings")]
fn then_logs_are_sanitized(world: &WorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if let Some(reason) = &world_ref.skip_reason {
        tracing::warn!("{reason}");
        return Ok(());
    }
    if let Some(err) = &world_ref.error {
        return Err(eyre!("bootstrap failed unexpectedly: {err}"));
    }

    let logs = world_ref.logs()?;
    ensure!(
        logs.iter()
            .any(|line| line.contains("prepared postgres settings")),
        "expected settings log, got {logs:?}"
    );
    ensure!(
        logs.iter().any(|line| line.contains("port=55432")),
        "expected port to be logged, got {logs:?}"
    );
    ensure!(
        logs.iter().any(|line| line.contains("installation_dir=")),
        "expected installation directory in logs, got {logs:?}"
    );
    ensure!(
        logs.iter().any(|line| line.contains("data_dir=")),
        "expected data directory in logs, got {logs:?}"
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("password=\"<redacted>\"")),
        "expected redacted password entry, got {logs:?}"
    );
    ensure!(
        !logs
            .iter()
            .any(|line| line.contains("settings-password-secret")),
        "password leaked into logs: {logs:?}"
    );
    Ok(())
}

#[then("the debug logs still redact sensitive values")]
fn then_failure_logs_are_redacted(world: &WorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if let Some(reason) = &world_ref.skip_reason {
        tracing::warn!("{reason}");
        return Ok(());
    }
    let Some(err) = &world_ref.error else {
        return Err(eyre!("expected bootstrap failure to be recorded"));
    };

    let logs = world_ref.logs()?;
    ensure!(
        logs.iter()
            .any(|line| line.contains("prepared postgres settings")),
        "expected settings log even on failure, got {logs:?}"
    );
    ensure!(
        logs.iter()
            .any(|line| line.contains("password=\"<redacted>\"")),
        "expected password to be redacted, got {logs:?}"
    );
    ensure!(
        !logs
            .iter()
            .any(|line| line.contains("settings-password-secret")),
        "password leaked into logs: {logs:?}"
    );
    ensure!(
        err.contains("data-as-file") || err.contains("data"),
        "expected failure to mention the data path, got {err}"
    );
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

fn override_password_and_port(vars: &mut Vec<(OsString, Option<OsString>)>) {
    override_env_os(
        vars,
        "PG_PASSWORD",
        Some(OsString::from("settings-password-secret")),
    );
    override_env_os(vars, "PG_PORT", Some(OsString::from("55432")));
}

#[scenario(path = "tests/features/settings_logging.feature", index = 0)]
fn scenario_settings_logging_success(serial_guard: ScenarioSerialGuard, world: WorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "settings logging world");
}

#[scenario(path = "tests/features/settings_logging.feature", index = 1)]
fn scenario_settings_logging_failure(serial_guard: ScenarioSerialGuard, world: WorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "settings logging world");
}
