//! Behavioural coverage for the `bootstrap_for_tests` helper.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Report, Result, ensure, eyre};
use nix::unistd::User;
use pg_embedded_setup_unpriv::test_support::worker_binary_for_tests;
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges, TestBootstrapSettings, bootstrap_for_tests, detect_execution_privileges,
};
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
#[path = "support/scenario.rs"]
mod scenario;
#[path = "support/skip.rs"]
mod skip;

use env_snapshot::EnvSnapshot;
use sandbox::TestSandbox;
use scenario::expect_fixture;
use skip::skip_message;

#[derive(Debug)]
struct BootstrapWorld {
    sandbox: TestSandbox,
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
            sandbox: TestSandbox::new("bootstrap-tests").context("create bootstrap sandbox")?,
            settings: None,
            error: None,
            skip_reason: None,
            env_before: None,
            env_restored: None,
            env_expected: None,
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

    fn handle_outcome(&mut self, outcome: Result<TestBootstrapSettings, Report>) -> Result<()> {
        match outcome {
            Ok(settings) => {
                self.record_settings(settings);
                if let Some(stored_settings) = self.settings.as_ref() {
                    let expected = EnvSnapshot::from_environment(&stored_settings.environment);
                    self.record_expected_env(expected);
                }
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                let debug = format!("{err:?}");
                if let Some(reason) =
                    skip_message("SKIP-BOOTSTRAP-FOR-TESTS", &message, Some(&debug))
                {
                    self.mark_skip(reason);
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

type BootstrapWorldFixture = Result<RefCell<BootstrapWorld>>;

fn borrow_world(world: &BootstrapWorldFixture) -> Result<&RefCell<BootstrapWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("bootstrap fixture construction failed: {err}")))
}

#[fixture]
fn world() -> BootstrapWorldFixture {
    Ok(RefCell::new(BootstrapWorld::new()?))
}

#[given("a bootstrap sandbox for tests")]
fn given_bootstrap_sandbox(world: &BootstrapWorldFixture) -> Result<()> {
    borrow_world(world)?.borrow().sandbox.reset()
}

#[when("bootstrap_for_tests runs without time zone overrides")]
fn when_bootstrap_for_tests(world: &BootstrapWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let env_vars = world_cell.borrow().sandbox.env_without_timezone();
    let (outcome, before, snapshot) = world_cell.borrow().sandbox.with_env(env_vars, || {
        let before = EnvSnapshot::capture();
        let outcome = bootstrap_for_tests().map_err(Report::from);
        let snapshot = EnvSnapshot::capture();
        (outcome, before, snapshot)
    });
    let mut world_mut = world_cell.borrow_mut();
    world_mut.env_before = Some(before);
    world_mut.record_restored_env(snapshot);
    world_mut.handle_outcome(outcome)
}

#[then("the helper returns sandbox-aligned settings")]
fn then_returns_settings(world: &BootstrapWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let settings = world_ref.settings()?;
    let install_dir = Utf8PathBuf::from_path_buf(settings.settings.installation_dir.clone())
        .map_err(|_| eyre!("installation_dir is not valid UTF-8"))?;
    let data_dir = Utf8PathBuf::from_path_buf(settings.settings.data_dir.clone())
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
        settings.environment.pgpass_file == world_ref.sandbox.install_dir().join(".pgpass"),
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
fn then_prepares_env(world: &BootstrapWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let settings = world_ref.settings()?;
    let env_settings = &settings.environment;
    let restored = world_ref.restored_env()?;
    let expected = world_ref.expected_env()?;
    let before = world_ref.env_before()?;

    ensure!(
        env_settings.home == world_ref.sandbox.install_dir(),
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
fn when_bootstrap_missing_timezone(world: &BootstrapWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let missing = world_cell
        .borrow()
        .sandbox
        .install_dir()
        .join("missing-tzdir");
    let env_vars = world_cell
        .borrow()
        .sandbox
        .env_with_timezone_override(&missing);
    let outcome = world_cell.borrow().sandbox.with_env(env_vars, || {
        bootstrap_for_tests().map_err(|err| err.to_string())
    });
    match outcome {
        Ok(settings) => {
            world_cell.borrow_mut().record_settings(settings);
            Err(eyre!("expected bootstrap_for_tests to fail"))
        }
        Err(err) => {
            world_cell.borrow_mut().record_error(err);
            Ok(())
        }
    }
}

#[then("the helper reports a time zone error")]
fn then_timezone_error(world: &BootstrapWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
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
fn bootstrap_for_tests_defaults(world: BootstrapWorldFixture) {
    let _ = expect_fixture(world, "bootstrap_for_tests defaults");
}

#[scenario(path = "tests/features/bootstrap_for_tests.feature", index = 1)]
fn bootstrap_for_tests_missing_timezone(world: BootstrapWorldFixture) {
    let _ = expect_fixture(world, "bootstrap_for_tests missing timezone");
}

#[test]
fn bootstrap_for_tests_sets_pgpass_permissions_and_owner() -> Result<()> {
    if detect_execution_privileges() == ExecutionPrivileges::Root
        && worker_binary_for_tests().is_none()
    {
        tracing::warn!(
            "Skipping pgpass permission test because PG_EMBEDDED_WORKER is unavailable."
        );
        return Ok(());
    }

    let sandbox = TestSandbox::new("bootstrap-pgpass")?;
    sandbox.reset()?;
    fs::create_dir_all(sandbox.install_dir().as_std_path()).context("create install dir")?;
    fs::create_dir_all(sandbox.data_dir().as_std_path()).context("create data dir")?;

    let pgpass_path = sandbox.install_dir().join(".pgpass");
    fs::write(pgpass_path.as_std_path(), b"pgpass").context("write pgpass")?;
    let mut perms = fs::metadata(pgpass_path.as_std_path())
        .context("pgpass metadata")?
        .permissions();
    perms.set_mode(0o644);
    fs::set_permissions(pgpass_path.as_std_path(), perms).context("seed pgpass permissions")?;

    let env_vars = sandbox.base_env();
    let bootstrap = sandbox
        .with_env(env_vars, bootstrap_for_tests)
        .context("bootstrap_for_tests")?;

    let expected_user = match bootstrap.privileges {
        ExecutionPrivileges::Root => User::from_name("nobody")
            .context("resolve nobody user")?
            .ok_or_else(|| eyre!("user 'nobody' not found"))?,
        ExecutionPrivileges::Unprivileged => User::from_uid(nix::unistd::geteuid())
            .context("resolve current user")?
            .ok_or_else(|| eyre!("current user not found"))?,
    };

    let metadata = fs::metadata(pgpass_path.as_std_path()).context("pgpass metadata")?;
    let observed_mode = metadata.permissions().mode() & 0o777;
    ensure!(
        observed_mode == 0o600,
        "expected pgpass mode 0o600, got 0o{observed_mode:03o}"
    );
    ensure!(
        metadata.uid() == expected_user.uid.as_raw(),
        "expected uid {}, got {}",
        expected_user.uid,
        metadata.uid()
    );
    ensure!(
        metadata.gid() == expected_user.gid.as_raw(),
        "expected gid {}, got {}",
        expected_user.gid,
        metadata.gid()
    );
    ensure!(
        bootstrap.environment.pgpass_file == pgpass_path,
        "expected pgpass path to remain aligned with install directory"
    );

    Ok(())
}
