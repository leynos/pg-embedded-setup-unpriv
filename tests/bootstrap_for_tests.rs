//! Behavioural coverage for the `bootstrap_for_tests` helper.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsString;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{
    TestBootstrapSettings, bootstrap_for_tests, detect_execution_privileges,
};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/mod.rs"]
mod support;

use support::{
    cap_fs::{CapabilityTempDir, remove_tree, set_permissions},
    env::{ScopedEnvVars, build_env, with_scoped_env},
};

#[derive(Debug)]
struct TestBootstrapSandbox {
    _guard: CapabilityTempDir,
    base_dir: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
}

impl TestBootstrapSandbox {
    fn new() -> Result<Self> {
        let guard = CapabilityTempDir::new("bootstrap-tests")
            .context("create bootstrap sandbox tempdir")?;
        let base_dir = guard.path().to_owned();
        set_permissions(&base_dir, 0o777)?;
        let install_dir = base_dir.join("install");
        let data_dir = base_dir.join("data");

        Ok(Self {
            _guard: guard,
            base_dir,
            install_dir,
            data_dir,
        })
    }

    fn install_dir(&self) -> &Utf8Path {
        &self.install_dir
    }

    fn data_dir(&self) -> &Utf8Path {
        &self.data_dir
    }

    fn base_env(&self) -> ScopedEnvVars {
        build_env([
            ("PG_RUNTIME_DIR", self.install_dir.as_str()),
            ("PG_DATA_DIR", self.data_dir.as_str()),
            ("PG_SUPERUSER", "postgres"),
            ("PG_PASSWORD", "postgres"),
        ])
    }

    fn env_without_timezone(&self) -> ScopedEnvVars {
        let mut vars = self.base_env();
        vars.push((OsString::from("TZDIR"), None));
        vars.push((OsString::from("TZ"), None));
        vars
    }

    fn env_with_timezone_override(&self, tz_dir: &Utf8Path) -> ScopedEnvVars {
        let mut vars = self.base_env();
        vars.push((
            OsString::from("TZDIR"),
            Some(OsString::from(tz_dir.as_str())),
        ));
        vars
    }

    fn with_env<R>(&self, vars: ScopedEnvVars, body: impl FnOnce() -> R) -> R {
        with_scoped_env(vars, body)
    }

    fn reset(&self) -> Result<()> {
        remove_tree(self.install_dir())?;
        remove_tree(self.data_dir())?;
        set_permissions(&self.base_dir, 0o777)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct EnvSnapshot {
    pgpassfile: Option<String>,
    tzdir: Option<String>,
    timezone: Option<String>,
}

impl EnvSnapshot {
    fn capture() -> Self {
        Self {
            pgpassfile: std::env::var("PGPASSFILE").ok(),
            tzdir: std::env::var("TZDIR").ok(),
            timezone: std::env::var("TZ").ok(),
        }
    }
}

#[derive(Debug)]
struct BootstrapWorld {
    sandbox: TestBootstrapSandbox,
    settings: Option<TestBootstrapSettings>,
    error: Option<String>,
    skip_reason: Option<String>,
    tzdir_before: Option<OsString>,
    env_after: Option<EnvSnapshot>,
}

impl BootstrapWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestBootstrapSandbox::new().context("create bootstrap sandbox")?,
            settings: None,
            error: None,
            skip_reason: None,
            tzdir_before: None,
            env_after: None,
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

    fn record_settings(&mut self, settings: TestBootstrapSettings) {
        self.settings = Some(settings);
        self.error = None;
    }

    fn record_error(&mut self, message: String) {
        self.error = Some(message);
        self.settings = None;
        self.env_after = None;
    }

    fn record_env(&mut self, snapshot: EnvSnapshot) {
        self.env_after = Some(snapshot);
    }

    fn handle_outcome(&mut self, outcome: Result<TestBootstrapSettings, String>) -> Result<()> {
        match outcome {
            Ok(settings) => {
                self.record_settings(settings);
                Ok(())
            }
            Err(message) => {
                const SKIP_CONDITIONS: &[(&str, &str)] = &[(
                    "rate limit exceeded",
                    "SKIP-BOOTSTRAP-FOR-TESTS: rate limit exceeded whilst downloading PostgreSQL",
                )];
                if let Some((_, reason)) = SKIP_CONDITIONS
                    .iter()
                    .find(|(needle, _)| message.contains(needle))
                {
                    self.mark_skip(format!("{reason}: {message}"));
                    Ok(())
                } else {
                    self.record_error(message.clone());
                    Err(eyre!(message))
                }
            }
        }
    }

    fn settings(&self) -> Result<&TestBootstrapSettings> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.settings
            .as_ref()
            .ok_or_else(|| eyre!("bootstrap_for_tests did not return settings"))
    }

    fn env(&self) -> Result<&EnvSnapshot> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.env_after
            .as_ref()
            .ok_or_else(|| eyre!("bootstrap_for_tests did not set environment"))
    }
}

#[fixture]
fn world() -> RefCell<BootstrapWorld> {
    RefCell::new(BootstrapWorld::new().expect("create bootstrap world"))
}

#[given("a bootstrap sandbox for tests")]
fn given_bootstrap_sandbox(world: &RefCell<BootstrapWorld>) -> Result<()> {
    world.borrow().sandbox.reset()
}

#[when("bootstrap_for_tests runs without timezone overrides")]
fn when_bootstrap_for_tests(world: &RefCell<BootstrapWorld>) -> Result<()> {
    world.borrow_mut().tzdir_before = std::env::var_os("TZDIR");
    let env_vars = world.borrow().sandbox.env_without_timezone();
    let (outcome, snapshot) = world.borrow().sandbox.with_env(env_vars, || {
        let outcome = bootstrap_for_tests().map_err(|err| err.to_string());
        let snapshot = EnvSnapshot::capture();
        (outcome, snapshot)
    });
    world.borrow_mut().record_env(snapshot);
    world.borrow_mut().handle_outcome(outcome)
}

#[then("the helper returns sandbox-aligned settings")]
fn then_returns_settings(world: &RefCell<BootstrapWorld>) -> Result<()> {
    let world_ref = world.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let settings = world_ref.settings()?;
    let install_dir = Utf8PathBuf::from_path_buf(settings.settings.installation_dir.clone())
        .map_err(|_| eyre!("installation_dir is not valid UTF-8"))?;
    let data_dir = Utf8PathBuf::from_path_buf(settings.settings.data_dir.clone())
        .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;

    ensure!(
        install_dir == world_ref.sandbox.install_dir,
        "expected install dir {} but observed {}",
        world_ref.sandbox.install_dir,
        install_dir
    );
    ensure!(
        data_dir == world_ref.sandbox.data_dir,
        "expected data dir {} but observed {}",
        world_ref.sandbox.data_dir,
        data_dir
    );
    ensure!(
        settings.environment.pgpass_file == world_ref.sandbox.install_dir.join(".pgpass"),
        "expected pgpass location to reside under install dir"
    );
    let expected_privileges = detect_execution_privileges();
    ensure!(
        settings.privileges == expected_privileges,
        "expected {:?} execution",
        expected_privileges
    );

    Ok(())
}

#[then("the helper prepares default environment variables")]
fn then_prepares_env(world: &RefCell<BootstrapWorld>) -> Result<()> {
    let world_ref = world.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let settings = world_ref.settings()?;
    let env_settings = &settings.environment;
    let captured = world_ref.env()?;

    ensure!(
        captured.pgpassfile.as_deref() == Some(env_settings.pgpass_file.as_str()),
        "PGPASSFILE was not set to the password file"
    );
    ensure!(
        captured.tzdir.as_deref() == Some(env_settings.tz_dir.as_str()),
        "TZDIR was not populated"
    );
    ensure!(
        captured.timezone.as_deref() == Some(env_settings.timezone.as_str()),
        "TZ was not populated"
    );
    ensure!(
        world_ref.tzdir_before.is_none(),
        "expected TZDIR to be absent before bootstrap"
    );

    Ok(())
}

#[when("bootstrap_for_tests runs with a missing timezone database")]
fn when_bootstrap_missing_timezone(world: &RefCell<BootstrapWorld>) -> Result<()> {
    world.borrow_mut().tzdir_before = std::env::var_os("TZDIR");
    let missing = world.borrow().sandbox.install_dir.join("missing-tzdir");
    let env_vars = world.borrow().sandbox.env_with_timezone_override(&missing);
    let outcome = world.borrow().sandbox.with_env(env_vars, || {
        bootstrap_for_tests().map_err(|err| err.to_string())
    });
    match outcome {
        Ok(settings) => {
            world.borrow_mut().record_settings(settings);
            Err(eyre!("expected bootstrap_for_tests to fail"))
        }
        Err(err) => {
            world.borrow_mut().record_error(err);
            Ok(())
        }
    }
}

#[then("the helper reports a timezone error")]
fn then_timezone_error(world: &RefCell<BootstrapWorld>) -> Result<()> {
    let world_ref = world.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref
        .error
        .as_ref()
        .ok_or_else(|| eyre!("expected timezone error message"))?;
    ensure!(
        message.contains("timezone database"),
        "unexpected error: {message}"
    );
    Ok(())
}

#[scenario(path = "tests/features/bootstrap_for_tests.feature", index = 0)]
fn bootstrap_for_tests_defaults(world: RefCell<BootstrapWorld>) {
    let _ = world;
}

#[scenario(path = "tests/features/bootstrap_for_tests.feature", index = 1)]
fn bootstrap_for_tests_missing_timezone(world: RefCell<BootstrapWorld>) {
    let _ = world;
}
