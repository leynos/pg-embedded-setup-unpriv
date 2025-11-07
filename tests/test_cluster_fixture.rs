#![cfg(unix)]
//! Behaviour and unit coverage for the public `rstest` fixture.

use std::{any::Any, cell::RefCell, ffi::OsString, panic::AssertUnwindSafe, process::Command};

use camino::Utf8PathBuf;
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

use pg_embedded_setup_unpriv::test_support::cluster_skip_message;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

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

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
fn fixture_restores_environment_after_use(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let tracked = [
        "HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "PGPASSFILE",
        "TZ",
        "TZDIR",
    ];
    let before: Vec<(String, Option<OsString>)> = tracked
        .iter()
        .map(|key| ((*key).to_owned(), std::env::var_os(key)))
        .collect();

    run_unit_fixture_test("rstest-fixture-env-restore", |test_cluster| {
        let env = test_cluster.environment();
        ensure!(
            std::env::var_os("HOME") == Some(OsString::from(env.home.as_str())),
            "HOME should point at the sandbox home inside the fixture",
        );
        ensure!(
            std::env::var_os("PGPASSFILE") == Some(OsString::from(env.pgpass_file.as_str())),
            "PGPASSFILE should point at the sandbox pgpass file inside the fixture",
        );
        ensure!(
            std::env::var_os("TZ") == Some(OsString::from(env.timezone.as_str())),
            "TZ should match the sandbox timezone inside the fixture",
        );
        let expected_tzdir = env.tz_dir.as_ref().map(|dir| OsString::from(dir.as_str()));
        ensure!(
            std::env::var_os("TZDIR") == expected_tzdir,
            "TZDIR should mirror the sandbox setting inside the fixture",
        );
        Ok(())
    })?;

    for (key, original) in before {
        ensure!(
            std::env::var_os(&key) == original,
            "{key} should be restored after the fixture teardown",
        );
    }
    Ok(())
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
fn fixture_teardown_resource_cleanup(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("rstest-fixture-cleanup")
        .context("create sandbox for teardown cleanup test")?;
    sandbox
        .reset()
        .context("reset sandbox before exercising teardown cleanup")?;
    let sandbox_root = sandbox_root(&sandbox);
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        std::panic::catch_unwind(AssertUnwindSafe(test_cluster))
    });
    let cluster = match result {
        Ok(cluster) => cluster,
        Err(payload) => return handle_fixture_panic(payload),
    };

    let pid_path = cluster.settings().data_dir.join("postmaster.pid");
    ensure!(
        pid_path.exists(),
        "postmaster.pid should exist before the fixture is dropped",
    );
    let postgres_pid = read_postmaster_pid(pid_path.as_path())?;

    drop(cluster);

    ensure!(
        !pid_path.exists(),
        "postmaster.pid should be removed after the fixture is dropped",
    );
    ensure!(
        !postgres_process_running(postgres_pid),
        "postgres process {postgres_pid} should stop after the fixture drops",
    );

    drop(sandbox);
    ensure!(
        !sandbox_root.as_std_path().exists(),
        "sandbox temporary directories should be removed once the guard drops",
    );

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
enum FixtureEnvProfile {
    #[default]
    Default,
    MissingTimezone,
    MissingWorkerBinary,
    ReadOnlySandbox,
    InvalidShutdownTimeout,
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
        if let Some(reason) = cluster_skip_message(&message, None) {
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

#[given("the rstest fixture runs without a worker binary")]
fn given_missing_worker(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::MissingWorkerBinary;
    Ok(())
}

#[given("the rstest fixture cannot write to its sandbox")]
fn given_read_only_sandbox(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::ReadOnlySandbox;
    Ok(())
}

#[given("the rstest fixture uses an invalid configuration")]
fn given_invalid_configuration(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::InvalidShutdownTimeout;
    Ok(())
}

#[when("test_cluster is invoked via rstest")]
fn when_fixture_runs(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let env_profile = { world_cell.borrow().env_profile };
    let mut permission_guard = None;
    let result = {
        let world_ref = world_cell.borrow();
        let mut vars = env_for_profile(&world_ref.sandbox, env_profile);
        augment_env_for_profile(&mut vars, &world_ref.sandbox, env_profile);
        if matches!(env_profile, FixtureEnvProfile::ReadOnlySandbox) {
            permission_guard = Some(SandboxPermissionGuard::lock(sandbox_root(
                &world_ref.sandbox,
            ))?);
        }
        world_ref.sandbox.with_env(vars, || {
            std::panic::catch_unwind(AssertUnwindSafe(test_cluster))
        })
    };
    drop(permission_guard);

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

#[then("the fixture reports a missing worker binary error")]
fn then_fixture_reports_missing_worker_binary_error(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref.panic_message()?;
    ensure!(
        message.contains("worker binary") || message.contains("PG_EMBEDDED_WORKER"),
        "expected a missing worker binary error but observed: {message}",
    );
    Ok(())
}

#[then("the fixture reports a permission error")]
fn then_fixture_reports_permission_error(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref.panic_message()?;
    ensure!(
        message.contains("Permission denied") || message.contains("permission"),
        "expected a permission error but observed: {message}",
    );
    Ok(())
}

#[then("the fixture reports an invalid configuration error")]
fn then_fixture_reports_invalid_configuration_error(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref.panic_message()?;
    ensure!(
        message.contains("PG_SHUTDOWN_TIMEOUT_SECS")
            || message.contains("invalid")
            || message.contains("configuration"),
        "expected an invalid configuration error but observed: {message}",
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

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 2)]
fn scenario_fixture_missing_worker(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) -> Result<()> {
    let _guard = serial_guard;
    let _ = world?;
    Ok(())
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 3)]
fn scenario_fixture_permission_error(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) -> Result<()> {
    let _guard = serial_guard;
    let _ = world?;
    Ok(())
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 4)]
fn scenario_fixture_invalid_config(
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
    cluster_skip_message(&message, None).map_or_else(
        || Err(eyre!(message)),
        |reason| {
            tracing::warn!("{reason}");
            Ok(())
        },
    )
}

fn read_postmaster_pid(path: &std::path::Path) -> Result<i32> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("read postmaster.pid at {}", path.display()))?;
    contents
        .lines()
        .next()
        .and_then(|line| line.trim().parse::<i32>().ok())
        .ok_or_else(|| eyre!("postmaster.pid missing PID entry at {}", path.display()))
}

fn postgres_process_running(pid: i32) -> bool {
    Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .skip(1)
                    .any(|line| !line.trim().is_empty())
        })
        .unwrap_or(false)
}

fn sandbox_root(sandbox: &TestSandbox) -> Utf8PathBuf {
    sandbox
        .install_dir()
        .parent()
        .map_or_else(|| sandbox.install_dir().to_owned(), Utf8PathBuf::from)
}

fn env_for_profile(sandbox: &TestSandbox, profile: FixtureEnvProfile) -> env::ScopedEnvVars {
    if matches!(profile, FixtureEnvProfile::MissingTimezone) {
        let missing_dir = sandbox.install_dir().join("missing-tz");
        return sandbox.env_with_timezone_override(missing_dir.as_ref());
    }
    sandbox.env_without_timezone()
}

fn augment_env_for_profile(
    vars: &mut env::ScopedEnvVars,
    sandbox: &TestSandbox,
    profile: FixtureEnvProfile,
) {
    if matches!(profile, FixtureEnvProfile::MissingWorkerBinary) {
        let missing_worker = sandbox.install_dir().join("missing-worker");
        vars.push((
            OsString::from("PG_EMBEDDED_WORKER"),
            Some(OsString::from(missing_worker.as_str())),
        ));
    }
    if matches!(profile, FixtureEnvProfile::InvalidShutdownTimeout) {
        vars.push((
            OsString::from("PG_SHUTDOWN_TIMEOUT_SECS"),
            Some(OsString::from("0")),
        ));
    }
}

struct SandboxPermissionGuard {
    root: Utf8PathBuf,
}

impl SandboxPermissionGuard {
    fn lock(root: Utf8PathBuf) -> Result<Self> {
        cap_fs::set_permissions(&root, 0o555)
            .context("lock sandbox root before invoking fixture")?;
        Ok(Self { root })
    }
}

impl Drop for SandboxPermissionGuard {
    fn drop(&mut self) {
        if let Err(err) = cap_fs::set_permissions(&self.root, 0o755) {
            tracing::warn!("failed to restore sandbox permissions: {err:?}");
        }
    }
}
