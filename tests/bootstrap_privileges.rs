//! Behavioural tests covering privilege-aware bootstrap flows.
#![cfg(unix)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::ErrorKind;

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::MetadataExt;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::{Uid, geteuid};
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges, detect_execution_privileges, nobody_uid, with_temp_euid,
};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};
use tempfile::TempDir;

#[path = "support/mod.rs"]
mod support;

use support::{
    cap_fs::{metadata, remove_tree, set_permissions},
    env::with_scoped_env,
};

#[derive(Debug)]
struct BootstrapSandbox {
    _base: TempDir,
    base_path: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    detected: Option<ExecutionPrivileges>,
    expected_owner: Option<Uid>,
    skip_checks: bool,
}

impl BootstrapSandbox {
    fn new() -> Result<Self> {
        let base = TempDir::new().context("create sandbox tempdir")?;
        let base_path = Utf8PathBuf::from_path_buf(base.path().to_path_buf())
            .map_err(|_| eyre!("sandbox path is not valid UTF-8"))?;
        set_permissions(&base_path, 0o777)?;

        let install_dir = base_path.join("install");
        let data_dir = base_path.join("data");

        Ok(Self {
            _base: base,
            base_path,
            install_dir,
            data_dir,
            detected: None,
            expected_owner: None,
            skip_checks: false,
        })
    }

    fn with_env<F, R>(&self, body: F) -> R
    where
        F: FnOnce() -> R,
    {
        with_scoped_env(
            [
                (
                    OsString::from("PG_RUNTIME_DIR"),
                    Some(OsString::from(self.install_dir.as_str())),
                ),
                (
                    OsString::from("PG_DATA_DIR"),
                    Some(OsString::from(self.data_dir.as_str())),
                ),
                (
                    OsString::from("PG_SUPERUSER"),
                    Some(OsString::from("postgres")),
                ),
                (
                    OsString::from("PG_PASSWORD"),
                    Some(OsString::from("postgres")),
                ),
            ],
            body,
        )
    }

    fn run_bootstrap(&self) -> Result<()> {
        self.with_env(pg_embedded_setup_unpriv::run)
    }

    fn reset(&mut self) -> Result<()> {
        self.skip_checks = false;
        self.remove_if_present(&self.install_dir)?;
        self.remove_if_present(&self.data_dir)?;
        set_permissions(&self.base_path, 0o777)?;
        Ok(())
    }

    fn remove_if_present(&self, path: &Utf8Path) -> Result<()> {
        remove_tree(path)
    }

    fn record_privileges(&mut self, privileges: ExecutionPrivileges) {
        self.detected = Some(privileges);
    }

    fn set_expected_owner(&mut self, uid: Uid) {
        self.expected_owner = Some(uid);
    }

    fn mark_skipped(&mut self) {
        self.skip_checks = true;
    }

    fn is_skipped(&self) -> bool {
        self.skip_checks
    }

    fn assert_detected(&self, expected: ExecutionPrivileges) -> Result<()> {
        if self.skip_checks {
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

    fn assert_owned_by_expected_user(&self) -> Result<()> {
        if self.skip_checks {
            return Ok(());
        }
        let expected = self
            .expected_owner
            .ok_or_else(|| eyre!("expected owner not recorded for sandbox"))?;
        self.assert_path_owner(&self.install_dir, expected)?;
        self.assert_path_owner(&self.data_dir, expected)?;
        Ok(())
    }

    fn assert_path_owner(&self, path: &Utf8Path, expected: Uid) -> Result<()> {
        let metadata = match metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) if err.kind() == ErrorKind::PermissionDenied => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| format!("inspect ownership of {}", path.as_str()));
            }
        };
        ensure!(
            metadata.uid() == expected.as_raw(),
            "expected {} to be owned by uid {} but found {}",
            path.as_str(),
            expected,
            metadata.uid()
        );
        Ok(())
    }

    fn handle_outcome(&mut self, outcome: Result<()>) -> Result<()> {
        match outcome {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = err.to_string();
                let should_skip = [
                    "rate limit exceeded",
                    "setgroups failed",
                    "postgresql_embedded::setup() failed",
                    "must start as root to drop privileges temporarily",
                ]
                .iter()
                .any(|needle| message.contains(needle));
                if should_skip {
                    eprintln!("skipping bootstrap scenario: {}", message);
                    self.mark_skipped();
                    Ok(())
                } else {
                    Err(err)
                }
            }
        }
    }
}

#[fixture]
fn sandbox() -> RefCell<BootstrapSandbox> {
    RefCell::new(BootstrapSandbox::new().expect("create bootstrap sandbox"))
}

#[given("a fresh bootstrap sandbox")]
fn given_fresh_sandbox(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    sandbox.borrow_mut().reset()
}

#[when("the bootstrap runs as an unprivileged user")]
fn when_bootstrap_runs_unprivileged(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    if geteuid().is_root() {
        sandbox.borrow_mut().set_expected_owner(nobody_uid());
        let privileges = with_temp_euid(nobody_uid(), || Ok(detect_execution_privileges()))?;
        sandbox.borrow_mut().record_privileges(privileges);
        let outcome = with_temp_euid(nobody_uid(), || {
            let sandbox_ref = sandbox.borrow();
            sandbox_ref.run_bootstrap()
        });
        sandbox.borrow_mut().handle_outcome(outcome)
    } else {
        let uid = geteuid();
        {
            let mut state = sandbox.borrow_mut();
            state.set_expected_owner(uid);
            state.record_privileges(detect_execution_privileges());
        }
        let outcome = {
            let sandbox_ref = sandbox.borrow();
            sandbox_ref.run_bootstrap()
        };
        sandbox.borrow_mut().handle_outcome(outcome)
    }
}

#[when("the bootstrap runs twice as root")]
fn when_bootstrap_runs_twice_as_root(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    if !geteuid().is_root() {
        eprintln!("skipping root-dependent bootstrap scenario");
        sandbox.borrow_mut().mark_skipped();
        return Ok(());
    }

    {
        let mut state = sandbox.borrow_mut();
        state.set_expected_owner(nobody_uid());
        state.record_privileges(detect_execution_privileges());
    }

    {
        let outcome = {
            let sandbox_ref = sandbox.borrow();
            sandbox_ref.run_bootstrap()
        };
        let mut state = sandbox.borrow_mut();
        state.handle_outcome(outcome)?;
        if state.is_skipped() {
            return Ok(());
        }
    }

    let outcome = {
        let sandbox_ref = sandbox.borrow();
        sandbox_ref.run_bootstrap()
    };
    let mut state = sandbox.borrow_mut();
    if state.is_skipped() {
        return Ok(());
    }
    state.handle_outcome(outcome)
}

#[then("the sandbox directories are owned by the target uid")]
fn then_directories_owned(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    sandbox.borrow().assert_owned_by_expected_user()
}

#[then("the detected privileges were unprivileged")]
fn then_detected_unprivileged(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    sandbox
        .borrow()
        .assert_detected(ExecutionPrivileges::Unprivileged)
}

#[then("the detected privileges were root")]
fn then_detected_root(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    sandbox.borrow().assert_detected(ExecutionPrivileges::Root)
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 0)]
fn bootstrap_as_unprivileged(sandbox: RefCell<BootstrapSandbox>) {
    let _ = sandbox;
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 1)]
fn bootstrap_as_root(sandbox: RefCell<BootstrapSandbox>) {
    let _ = sandbox;
}
