//! Behavioural coverage for the `bootstrap_for_tests` helper.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{
    TestBootstrapEnvironment, TestBootstrapSettings, bootstrap_for_tests,
    detect_execution_privileges,
};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;

use cap_fs::{remove_tree, set_permissions};
use env::{ScopedEnvVars, build_env, with_scoped_env};
use pg_embedded_setup_unpriv::test_support::CapabilityTempDir;

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EnvSnapshot {
    pgpassfile: Option<OsString>,
    tzdir: Option<OsString>,
    timezone: Option<OsString>,
}

impl EnvSnapshot {
    fn capture() -> Self {
        Self {
            pgpassfile: std::env::var_os("PGPASSFILE"),
            tzdir: std::env::var_os("TZDIR"),
            timezone: std::env::var_os("TZ"),
        }
    }

    fn from_environment(environment: &TestBootstrapEnvironment) -> Self {
        environment
            .to_env()
            .into_iter()
            .fold(Self::default(), |mut snapshot, (key, value)| {
                let value = OsString::from(value);
                match key.as_str() {
                    "PGPASSFILE" => snapshot.pgpassfile = Some(value),
                    "TZDIR" => snapshot.tzdir = Some(value),
                    "TZ" => snapshot.timezone = Some(value),
                    _ => {}
                }
                snapshot
            })
    }
}

#[derive(Debug)]
struct BootstrapWorld {
    sandbox: TestBootstrapSandbox,
    settings: Option<TestBootstrapSettings>,
    error: Option<String>,
    skip_reason: Option<String>,
    env_before: Option<EnvSnapshot>,
    env_restored: Option<EnvSnapshot>,
    env_expected: Option<EnvSnapshot>,
}

impl BootstrapWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestBootstrapSandbox::new().context("create bootstrap sandbox")?,
            settings: None,
            error: None,
            skip_reason: None,
            env_before: None,
            env_restored: None,
            env_expected: None,
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
        self.env_before = None;
        self.env_restored = None;
        self.env_expected = None;
    }

    fn record_restored_env(&mut self, snapshot: EnvSnapshot) {
        self.env_restored = Some(snapshot);
    }

    fn record_expected_env(&mut self, snapshot: EnvSnapshot) {
        self.env_expected = Some(snapshot);
    }

    fn handle_outcome(&mut self, outcome: Result<TestBootstrapSettings, String>) -> Result<()> {
        match outcome {
            Ok(settings) => {
                self.record_settings(settings);
                if let Some(settings) = self.settings.as_ref() {
                    let expected = EnvSnapshot::from_environment(&settings.environment);
                    self.record_expected_env(expected);
                }
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

    fn restored_env(&self) -> Result<&EnvSnapshot> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.env_restored
            .as_ref()
            .ok_or_else(|| eyre!("bootstrap_for_tests did not set environment"))
    }

    fn expected_env(&self) -> Result<&EnvSnapshot> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.env_expected
            .as_ref()
            .ok_or_else(|| eyre!("bootstrap_for_tests did not surface expected environment"))
    }

    fn env_before(&self) -> Result<&EnvSnapshot> {
        if self.is_skipped() {
            return Err(eyre!("scenario skipped"));
        }
        self.env_before
            .as_ref()
            .ok_or_else(|| eyre!("bootstrap_for_tests did not capture the initial environment"))
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

#[when("bootstrap_for_tests runs without time zone overrides")]
fn when_bootstrap_for_tests(world: &RefCell<BootstrapWorld>) -> Result<()> {
    let env_vars = world.borrow().sandbox.env_without_timezone();
    let (outcome, before, snapshot) = world.borrow().sandbox.with_env(env_vars, || {
        let before = EnvSnapshot::capture();
        let outcome = bootstrap_for_tests().map_err(|err| err.to_string());
        let snapshot = EnvSnapshot::capture();
        (outcome, before, snapshot)
    });
    let mut world_mut = world.borrow_mut();
    world_mut.env_before = Some(before);
    world_mut.record_restored_env(snapshot);
    world_mut.handle_outcome(outcome)
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
    let restored = world_ref.restored_env()?;
    let expected = world_ref.expected_env()?;
    let before = world_ref.env_before()?;

    ensure!(
        env_settings.home == world_ref.sandbox.install_dir,
        "HOME should match the install directory"
    );
    ensure!(
        env_settings.xdg_cache_home == env_settings.home.join("cache"),
        "XDG_CACHE_HOME should sit under the install directory"
    );
    ensure!(
        env_settings.xdg_runtime_dir == env_settings.home.join("run"),
        "XDG_RUNTIME_DIR should sit under the install directory"
    );
    ensure!(
        !env_settings.timezone.is_empty(),
        "time zone should not be empty"
    );
    ensure!(
        env_settings
            .tz_dir
            .as_ref()
            .is_some_and(|path| path.exists()),
        "TZDIR should record the discovered time zone directory"
    );
    ensure!(
        env_settings.timezone == "UTC",
        "Expected default time zone to be UTC when unset"
    );
    ensure!(
        restored == before,
        "bootstrap_for_tests must restore the environment"
    );
    if let Some(ref tz_dir) = env_settings.tz_dir {
        ensure!(
            expected
                .tzdir
                .as_ref()
                .is_some_and(|value| value == OsStr::new(tz_dir.as_str())),
            "TZDIR should equal the discovered directory"
        );
    } else {
        ensure!(
            expected.tzdir.is_none(),
            "TZDIR should be absent when discovery fails"
        );
    }
    ensure!(
        expected
            .timezone
            .as_ref()
            .is_some_and(|value| value == OsStr::new(env_settings.timezone.as_str())),
        "TZ should reflect the prepared time zone"
    );
    Ok(())
}

#[when("bootstrap_for_tests runs with a missing time zone database")]
fn when_bootstrap_missing_timezone(world: &RefCell<BootstrapWorld>) -> Result<()> {
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

#[then("the helper reports a time zone error")]
fn then_timezone_error(world: &RefCell<BootstrapWorld>) -> Result<()> {
    let world_ref = world.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref
        .error
        .as_ref()
        .ok_or_else(|| eyre!("expected time zone error message"))?;
    ensure!(
        message.contains("time zone database"),
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
