//! Behavioural tests covering privilege-aware bootstrap flows.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::ErrorKind;

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::MetadataExt;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::{Uid, geteuid};
use pg_embedded_setup_unpriv::{ExecutionPrivileges, detect_execution_privileges, nobody_uid};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs_bootstrap;
#[path = "support/env.rs"]
mod env;
#[path = "support/scenario.rs"]
mod scenario;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cap_fs_bootstrap::{remove_tree, set_permissions};
use env::{build_env, with_scoped_env};
use pg_embedded_setup_unpriv::test_support::CapabilityTempDir;
use pg_embedded_setup_unpriv::test_support::metadata;
use scenario::expect_fixture;
use serial::{ScenarioSerialGuard, serial_guard};
use skip::skip_message;

#[derive(Debug)]
struct BootstrapSandbox {
    _tempdir_guard: CapabilityTempDir,
    base_path: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    detected: Option<ExecutionPrivileges>,
    expected_owner: Option<Uid>,
    last_error: Option<String>,
    skip_reason: Option<String>,
}

impl BootstrapSandbox {
    fn new() -> Result<Self> {
        let tempdir_guard =
            CapabilityTempDir::new("bootstrap-sandbox").context("create sandbox tempdir")?;
        let base_path = tempdir_guard.path().to_owned();
        // The worker only needs traversal access; keep the directory
        // read/execute for others to avoid world-writable sandboxes.
        set_permissions(tempdir_guard.path(), 0o755)?;

        let install_dir = base_path.join("install");
        let data_dir = base_path.join("data");

        Ok(Self {
            _tempdir_guard: tempdir_guard,
            base_path,
            install_dir,
            data_dir,
            detected: None,
            expected_owner: None,
            last_error: None,
            skip_reason: None,
        })
    }

    fn base_env(&self) -> Vec<(OsString, Option<OsString>)> {
        build_env([
            ("PG_RUNTIME_DIR", self.install_dir.as_str()),
            ("PG_DATA_DIR", self.data_dir.as_str()),
            ("PG_SUPERUSER", "postgres"),
            ("PG_PASSWORD", "postgres"),
        ])
    }

    fn with_env<F, R>(&self, body: F) -> R
    where
        F: FnOnce() -> R,
    {
        with_scoped_env(self.base_env(), body)
    }

    fn with_env_without_worker<F, R>(&self, body: F) -> R
    where
        F: FnOnce() -> R,
    {
        let worker_key = OsString::from("PG_EMBEDDED_WORKER");
        let mut vars: Vec<_> = self
            .base_env()
            .into_iter()
            .filter(|(key, _)| *key != worker_key)
            .collect();
        vars.push((worker_key, None));
        with_scoped_env(vars, body)
    }

    fn run_bootstrap(&self) -> pg_embedded_setup_unpriv::Result<()> {
        self.with_env(pg_embedded_setup_unpriv::run)
    }

    fn run_bootstrap_without_worker(&self) -> pg_embedded_setup_unpriv::Result<()> {
        self.with_env_without_worker(pg_embedded_setup_unpriv::run)
    }

    fn reset(&mut self) -> Result<()> {
        self.skip_reason = None;
        self.last_error = None;
        Self::remove_if_present(&self.install_dir)?;
        Self::remove_if_present(&self.data_dir)?;
        set_permissions(&self.base_path, 0o777)?;
        Ok(())
    }

    fn remove_if_present(path: &Utf8Path) -> Result<()> {
        remove_tree(path)
    }

    const fn record_privileges(&mut self, privileges: ExecutionPrivileges) {
        self.detected = Some(privileges);
    }

    const fn set_expected_owner(&mut self, uid: Uid) {
        self.expected_owner = Some(uid);
    }

    fn record_error(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    fn mark_skipped(&mut self, skip_reason: impl Into<String>) {
        let message = skip_reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    fn assert_detected(&self, expected: ExecutionPrivileges) -> Result<()> {
        if self.is_skipped() {
            return Ok(());
        }
        let detected = self
            .detected
            .ok_or_else(|| eyre!("behaviour test did not record privilege state"))?;
        ensure!(
            detected == expected,
            "expected {:?} privileges but observed {:?}",
            expected,
            detected
        );
        Ok(())
    }

    fn assert_owned_by_expected_user(&mut self) -> Result<()> {
        if self.is_skipped() {
            return Ok(());
        }
        let expected = self
            .expected_owner
            .ok_or_else(|| eyre!("expected owner not recorded for sandbox"))?;
        if let Some(reason) = {
            let path = self.install_dir.as_ref();
            Self::inspect_path_owner(path, expected)?
        } {
            self.mark_skipped(reason);
        }

        if let Some(reason) = {
            let path = self.data_dir.as_ref();
            Self::inspect_path_owner(path, expected)?
        } {
            self.mark_skipped(reason);
        }
        Ok(())
    }

    fn inspect_path_owner(path: &Utf8Path, expected: Uid) -> Result<Option<String>> {
        let metadata = match metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(Some(format!(
                    "SKIP-BOOTSTRAP: ownership check skipped for {path} (missing): {err}"
                )));
            }
            Err(err) if err.kind() == ErrorKind::PermissionDenied => {
                return Ok(Some(format!(
                    "SKIP-BOOTSTRAP: ownership check skipped for {path} (permission denied): {err}"
                )));
            }
            Err(err) => {
                return Err(err).with_context(|| format!("inspect ownership of {path}"));
            }
        };
        ensure!(
            metadata.uid() == expected.as_raw(),
            "expected {} to be owned by uid {} but found {}",
            path.as_str(),
            expected,
            metadata.uid()
        );
        Ok(None)
    }

    fn handle_outcome(&mut self, outcome: pg_embedded_setup_unpriv::Result<()>) -> Result<()> {
        match outcome {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = err.to_string();
                let debug = format!("{err:?}");
                skip_message("SKIP-BOOTSTRAP", &message, Some(&debug)).map_or_else(
                    || {
                        tracing::warn!("SKIP-BOOTSTRAP-FAILURE: {message}");
                        Err(err.into())
                    },
                    |reason| {
                        self.mark_skipped(reason);
                        Ok(())
                    },
                )
            }
        }
    }
}

#[cfg(feature = "privileged-tests")]
fn run_bootstrap_with_temp_drop(sandbox: &RefCell<BootstrapSandbox>) {
    sandbox.borrow_mut().set_expected_owner(nobody_uid());
    sandbox
        .borrow_mut()
        .mark_skipped("SKIP-BOOTSTRAP: temporary UID switching is no longer supported");
}

#[cfg(not(feature = "privileged-tests"))]
fn run_bootstrap_with_temp_drop(sandbox: &RefCell<BootstrapSandbox>) {
    sandbox
        .borrow_mut()
        .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires the privileged-tests feature");
}

type BootstrapSandboxFixture = Result<RefCell<BootstrapSandbox>>;

fn borrow_sandbox(sandbox: &BootstrapSandboxFixture) -> Result<&RefCell<BootstrapSandbox>> {
    sandbox
        .as_ref()
        .map_err(|err| eyre!(format!("bootstrap sandbox fixture failed: {err}")))
}

#[fixture]
fn sandbox() -> BootstrapSandboxFixture {
    Ok(RefCell::new(BootstrapSandbox::new()?))
}

#[given("a fresh bootstrap sandbox")]
fn given_fresh_sandbox(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    borrow_sandbox(sandbox)?.borrow_mut().reset()
}

#[when("the bootstrap runs as an unprivileged user")]
fn when_bootstrap_runs_unprivileged(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    if geteuid().is_root() {
        run_bootstrap_with_temp_drop(sandbox_cell);
        return Ok(());
    }

    let uid = geteuid();
    {
        let mut state = sandbox_cell.borrow_mut();
        state.set_expected_owner(uid);
        state.record_privileges(detect_execution_privileges());
    }
    let outcome = {
        let sandbox_ref = sandbox_cell.borrow();
        sandbox_ref.run_bootstrap()
    };
    sandbox_cell.borrow_mut().handle_outcome(outcome)
}

#[when("the bootstrap runs twice as root")]
fn when_bootstrap_runs_twice_as_root(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    if !geteuid().is_root() {
        sandbox_cell
            .borrow_mut()
            .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires root access");
        return Ok(());
    }

    {
        let mut state = sandbox_cell.borrow_mut();
        state.set_expected_owner(nobody_uid());
        state.record_privileges(detect_execution_privileges());
    }

    {
        let outcome = {
            let sandbox_ref = sandbox_cell.borrow();
            sandbox_ref.run_bootstrap()
        };
        let mut state = sandbox_cell.borrow_mut();
        state.handle_outcome(outcome)?;
        if state.is_skipped() {
            return Ok(());
        }
    }

    let outcome = {
        let sandbox_ref = sandbox_cell.borrow();
        sandbox_ref.run_bootstrap()
    };
    let mut state = sandbox_cell.borrow_mut();
    if state.is_skipped() {
        return Ok(());
    }
    state.handle_outcome(outcome)
}

#[when("the bootstrap runs as root without a worker")]
fn when_bootstrap_runs_as_root_without_worker(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    if !geteuid().is_root() {
        sandbox_cell
            .borrow_mut()
            .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires root access");
        return Ok(());
    }

    sandbox_cell
        .borrow_mut()
        .record_privileges(detect_execution_privileges());

    let outcome = {
        let sandbox_ref = sandbox_cell.borrow();
        sandbox_ref.run_bootstrap_without_worker()
    };

    match outcome {
        Ok(()) => Err(eyre!(
            "expected bootstrap to fail without PG_EMBEDDED_WORKER"
        )),
        Err(err) => {
            sandbox_cell.borrow_mut().record_error(err.to_string());
            Ok(())
        }
    }
}

#[then("the sandbox directories are owned by the target uid")]
fn then_directories_owned(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    borrow_sandbox(sandbox)?
        .borrow_mut()
        .assert_owned_by_expected_user()
}

#[then("the detected privileges were unprivileged")]
fn then_detected_unprivileged(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    sandbox_cell
        .borrow()
        .assert_detected(ExecutionPrivileges::Unprivileged)
}

#[then("the detected privileges were root")]
fn then_detected_root(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    borrow_sandbox(sandbox)?
        .borrow()
        .assert_detected(ExecutionPrivileges::Root)
}

#[then("the bootstrap reports the missing worker")]
fn then_bootstrap_reports_missing_worker(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    let sandbox_ref = sandbox_cell.borrow();
    if sandbox_ref.is_skipped() {
        return Ok(());
    }
    let message = sandbox_ref
        .last_error
        .as_deref()
        .ok_or_else(|| eyre!("missing bootstrap error message"))?;
    ensure!(
        message.contains("PG_EMBEDDED_WORKER must be set"),
        "unexpected missing-worker error: {message}",
    );
    Ok(())
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 0)]
fn bootstrap_as_unprivileged(serial_guard: ScenarioSerialGuard, sandbox: BootstrapSandboxFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(sandbox, "bootstrap privileges sandbox");
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 1)]
fn bootstrap_as_root(serial_guard: ScenarioSerialGuard, sandbox: BootstrapSandboxFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(sandbox, "bootstrap privileges sandbox");
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 2)]
fn bootstrap_as_root_without_worker(
    serial_guard: ScenarioSerialGuard,
    sandbox: BootstrapSandboxFixture,
) {
    let _guard = serial_guard;
    let _ = expect_fixture(sandbox, "bootstrap privileges sandbox");
}
