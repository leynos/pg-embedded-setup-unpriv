#![cfg(unix)]
//! Behaviour and unit coverage for the public `rstest` fixture.

use std::{
    any::Any,
    cell::RefCell,
    collections::HashSet,
    ffi::{OsStr, OsString},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use libc::pid_t;
use pg_embedded_setup_unpriv::{TestCluster, test_support::test_cluster};
use rstest::{fixture, rstest};
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use env::ScopedEnvVars;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

fn set_env_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    // Safety: nightly toolchains mark environment mutation as unsafe because it
    // touches process-global state. Tests serialise these calls via the sandbox
    // guards so we can mutate the environment deterministically.
    unsafe { std::env::set_var(key, value) };
}

fn remove_env_var<K>(key: K)
where
    K: AsRef<OsStr>,
{
    // Safety: see `set_env_var` above; we only mutate the environment whilst
    // the guard is active and no references escape.
    unsafe { std::env::remove_var(key) };
}

struct EnvIsolationGuard {
    snapshot: Vec<(OsString, OsString)>,
}

impl EnvIsolationGuard {
    fn capture() -> Self {
        Self {
            snapshot: std::env::vars_os().collect(),
        }
    }
}

impl Drop for EnvIsolationGuard {
    fn drop(&mut self) {
        let saved_keys: HashSet<OsString> =
            self.snapshot.iter().map(|(key, _)| key.clone()).collect();
        let current_keys: Vec<OsString> = std::env::vars_os().map(|(key, _)| key).collect();
        for key in current_keys {
            if !saved_keys.contains(&key) {
                remove_env_var(&key);
            }
        }
        for (key, value) in &self.snapshot {
            set_env_var(key, value);
        }
    }
}

fn run_unit_fixture_test<F>(name: &str, test: F) -> Result<()>
where
    F: FnOnce(TestCluster) -> Result<()>,
{
    let _env_reset = EnvIsolationGuard::capture();
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
fn fixture_environment_variable_isolation(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    const LEAK_VAR: &str = "RSTEST_CLUSTER_ENV_POLLUTION";
    run_unit_fixture_test("rstest-fixture-env-pollution", |_test_cluster| {
        set_env_var(LEAK_VAR, "polluted_value");
        Ok(())
    })?;
    ensure!(
        std::env::var(LEAK_VAR).is_err(),
        "{leak} should not leak across fixture runs",
        leak = LEAK_VAR,
    );
    run_unit_fixture_test("rstest-fixture-env-clean", |_test_cluster| {
        ensure!(
            std::env::var(LEAK_VAR).is_err(),
            "{leak} should be absent when the fixture starts",
            leak = LEAK_VAR,
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
fn fixture_teardown_resource_cleanup(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("rstest-fixture-teardown")
        .context("create sandbox for teardown coverage")?;
    sandbox
        .reset()
        .context("reset sandbox before teardown coverage")?;
    let sandbox_root = sandbox_root_path(&sandbox)?;
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        std::panic::catch_unwind(AssertUnwindSafe(test_cluster))
    });
    let cluster = match result {
        Ok(cluster) => cluster,
        Err(payload) => {
            handle_fixture_panic(payload)?;
            return Ok(());
        }
    };
    let data_dir = cluster.settings().data_dir.clone();
    let pid = read_postmaster_pid(data_dir.as_path())?;
    drop(cluster);
    wait_for_process_exit(pid)?;
    wait_for_pid_file_removal(data_dir.as_path())?;
    drop(sandbox);
    ensure!(
        !sandbox_root.exists(),
        "sandbox root should be removed once the guard drops",
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
enum FixtureEnvProfile {
    #[default]
    Default,
    MissingTimezone,
    MissingWorkerBinary,
    PermissionDenied,
    InvalidConfiguration,
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

#[given("the rstest fixture encounters filesystem permission issues")]
fn given_permission_denied(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::PermissionDenied;
    Ok(())
}

#[given("the rstest fixture uses an invalid configuration override")]
fn given_invalid_configuration(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = FixtureEnvProfile::InvalidConfiguration;
    Ok(())
}

#[when("test_cluster is invoked via rstest")]
fn when_fixture_runs(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let env_profile = { world_cell.borrow().env_profile };
    let vars = {
        let world_ref = world_cell.borrow();
        env_for_profile(&world_ref.sandbox, env_profile)?
    };
    let result = {
        let world_ref = world_cell.borrow();
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

#[then("the fixture reports a missing worker binary error")]
fn then_fixture_reports_missing_worker_binary_error(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref.panic_message()?;
    ensure!(
        message.contains("worker binary") || message.contains("No such file"),
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
        message.contains("Permission") || message.contains("permission"),
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
        message.contains("configuration") || message.contains("invalid"),
        "expected a configuration error but observed: {message}",
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
    cluster_skip_message(&message, None).map_or_else(
        || Err(eyre!(message)),
        |reason| {
            tracing::warn!("{reason}");
            Ok(())
        },
    )
}

fn env_for_profile(sandbox: &TestSandbox, profile: FixtureEnvProfile) -> Result<ScopedEnvVars> {
    match profile {
        FixtureEnvProfile::Default => Ok(sandbox.env_without_timezone()),
        FixtureEnvProfile::MissingTimezone => {
            let missing_dir = sandbox.install_dir().join("missing-tz");
            std::fs::create_dir_all(missing_dir.as_std_path())
                .with_context(|| format!("create missing timezone directory at {missing_dir}"))?;
            Ok(sandbox.env_with_timezone_override(missing_dir.as_ref()))
        }
        FixtureEnvProfile::MissingWorkerBinary => {
            let mut vars = sandbox.env_without_timezone();
            let fake_worker = sandbox
                .install_dir()
                .join("missing-worker")
                .join("pg_worker");
            override_env_os(
                &mut vars,
                "PG_EMBEDDED_WORKER",
                Some(OsString::from(fake_worker.as_str())),
            );
            Ok(vars)
        }
        FixtureEnvProfile::PermissionDenied => {
            let mut vars = sandbox.env_without_timezone();
            let runtime = sandbox.install_dir().join("denied-runtime");
            let data = sandbox.install_dir().join("denied-data");
            prepare_read_only_dir(&runtime)?;
            prepare_read_only_dir(&data)?;
            override_env_path(&mut vars, "PG_RUNTIME_DIR", runtime.as_ref());
            override_env_path(&mut vars, "PG_DATA_DIR", data.as_ref());
            Ok(vars)
        }
        FixtureEnvProfile::InvalidConfiguration => {
            let mut vars = sandbox.env_without_timezone();
            override_env_os(&mut vars, "PG_PORT", Some(OsString::from("not-a-port")));
            Ok(vars)
        }
    }
}

fn override_env_os(vars: &mut ScopedEnvVars, key: &str, value: Option<OsString>) {
    if let Some((_, existing_value)) = vars
        .iter_mut()
        .find(|(candidate, _)| candidate.as_os_str() == OsStr::new(key))
    {
        *existing_value = value;
    } else {
        vars.push((OsString::from(key), value));
    }
}

fn override_env_path(vars: &mut ScopedEnvVars, key: &str, value: &Utf8Path) {
    override_env_os(vars, key, Some(OsString::from(value.as_str())));
}

fn prepare_read_only_dir(path: &Utf8PathBuf) -> Result<()> {
    std::fs::create_dir_all(path.as_std_path())
        .with_context(|| format!("create read-only directory at {path}"))?;
    cap_fs::set_permissions(path.as_ref(), 0o555)
        .with_context(|| format!("restrict permissions for {path}"))
}

fn sandbox_root_path(sandbox: &TestSandbox) -> Result<PathBuf> {
    let Some(root) = sandbox.install_dir().parent() else {
        return Err(eyre!("sandbox install directory is missing a parent"));
    };
    Ok(root.to_path_buf().into_std_path_buf())
}

fn read_postmaster_pid(data_dir: impl AsRef<Path>) -> Result<Option<pid_t>> {
    let data_dir_ref = data_dir.as_ref();
    let pid_file = data_dir_ref.join("postmaster.pid");
    if !pid_file.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&pid_file)
        .with_context(|| format!("read postmaster pid file at {}", pid_file.display()))?;
    let Some(first_line) = contents.lines().next() else {
        return Ok(None);
    };
    let pid: pid_t = first_line
        .trim()
        .parse()
        .with_context(|| format!("parse postmaster pid from '{first_line}'"))?;
    Ok(Some(pid))
}

fn wait_for_process_exit(pid: Option<pid_t>) -> Result<()> {
    let Some(child_pid) = pid else {
        return Ok(());
    };
    for _ in 0..100 {
        if !process_is_running(child_pid) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(eyre!(format!(
        "PostgreSQL process {child_pid} should exit after the fixture drops"
    )))
}

fn wait_for_pid_file_removal(data_dir: impl AsRef<Path>) -> Result<()> {
    let data_dir_ref = data_dir.as_ref();
    let pid_file = data_dir_ref.join("postmaster.pid");
    for _ in 0..100 {
        if !pid_file.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(eyre!(format!(
        "postmaster.pid should be removed from {:?} once PostgreSQL stops",
        data_dir_ref
    )))
}

fn process_is_running(pid: pid_t) -> bool {
    // SAFETY: `kill` with signal `0` probes whether the process exists without sending a signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::ESRCH
    )
}
