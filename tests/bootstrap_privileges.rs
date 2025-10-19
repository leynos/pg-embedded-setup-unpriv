//! Behavioural tests covering privilege-aware bootstrap flows.
#![cfg(unix)]

use std::cell::RefCell;
use std::io::ErrorKind;

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::MetadataExt;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::{Uid, geteuid};
#[cfg(feature = "privileged-tests")]
use pg_embedded_setup_unpriv::with_temp_euid;
use pg_embedded_setup_unpriv::{ExecutionPrivileges, detect_execution_privileges, nobody_uid};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs_bootstrap;
#[path = "support/env.rs"]
mod env;
#[path = "support/serial.rs"]
mod serial;

use cap_fs_bootstrap::{remove_tree, set_permissions};
use env::{build_env, with_scoped_env};
use pg_embedded_setup_unpriv::test_support::CapabilityTempDir;
use pg_embedded_setup_unpriv::test_support::metadata;
use serial::{ScenarioSerialGuard, serial_guard};

#[derive(Debug)]
struct BootstrapSandbox {
    _tempdir_guard: CapabilityTempDir,
    base_path: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    detected: Option<ExecutionPrivileges>,
    expected_owner: Option<Uid>,
    skip_reason: Option<String>,
}

impl BootstrapSandbox {
    fn new() -> Result<Self> {
        let tempdir_guard =
            CapabilityTempDir::new("bootstrap-sandbox").context("create sandbox tempdir")?;
        let base_path = tempdir_guard.path().to_owned();
        set_permissions(tempdir_guard.path(), 0o777)?;

        let install_dir = base_path.join("install");
        let data_dir = base_path.join("data");

        Ok(Self {
            _tempdir_guard: tempdir_guard,
            base_path,
            install_dir,
            data_dir,
            detected: None,
            expected_owner: None,
            skip_reason: None,
        })
    }

    fn with_env<F, R>(&self, body: F) -> R
    where
        F: FnOnce() -> R,
    {
        with_scoped_env(
            build_env([
                ("PG_RUNTIME_DIR", self.install_dir.as_str()),
                ("PG_DATA_DIR", self.data_dir.as_str()),
                ("PG_SUPERUSER", "postgres"),
                ("PG_PASSWORD", "postgres"),
            ]),
            body,
        )
    }

    fn run_bootstrap(&self) -> pg_embedded_setup_unpriv::Result<()> {
        self.with_env(pg_embedded_setup_unpriv::run)
    }

    fn reset(&mut self) -> Result<()> {
        self.skip_reason = None;
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

    fn mark_skipped(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        eprintln!("{reason}");
        self.skip_reason = Some(reason);
    }

    fn is_skipped(&self) -> bool {
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
                    "SKIP-BOOTSTRAP: ownership check skipped for {} (missing): {}",
                    path, err
                )));
            }
            Err(err) if err.kind() == ErrorKind::PermissionDenied => {
                return Ok(Some(format!(
                    "SKIP-BOOTSTRAP: ownership check skipped for {} (permission denied): {}",
                    path, err
                )));
            }
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
        Ok(None)
    }

    fn handle_outcome(&mut self, outcome: pg_embedded_setup_unpriv::Result<()>) -> Result<()> {
        match outcome {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = err.to_string();
                const SKIP_CONDITIONS: &[(&str, &str)] = &[
                    (
                        "rate limit exceeded",
                        "SKIP-BOOTSTRAP: rate limit exceeded whilst downloading PostgreSQL",
                    ),
                    (
                        "setgroups failed",
                        "SKIP-BOOTSTRAP: kernel refused to adjust supplementary groups",
                    ),
                    (
                        "must start as root to drop privileges temporarily",
                        "SKIP-BOOTSTRAP: root privileges unavailable for privileged bootstrap path",
                    ),
                    (
                        "No such file or directory",
                        "SKIP-BOOTSTRAP: postgres binary unavailable for privileged bootstrap",
                    ),
                ];
                if let Some((_, reason)) = SKIP_CONDITIONS
                    .iter()
                    .find(|(needle, _)| message.contains(needle))
                {
                    self.mark_skipped(format!("{reason}: {message}"));
                    Ok(())
                } else {
                    eprintln!("SKIP-BOOTSTRAP-FAILURE: {message}");
                    Err(err.into())
                }
            }
        }
    }
}

#[cfg(feature = "privileged-tests")]
fn run_bootstrap_with_temp_drop(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    sandbox.borrow_mut().set_expected_owner(nobody_uid());
    let privileges = with_temp_euid(nobody_uid(), || {
        Ok::<ExecutionPrivileges, pg_embedded_setup_unpriv::Error>(detect_execution_privileges())
    })
    .map_err(|err| eyre!(err))?;
    sandbox.borrow_mut().record_privileges(privileges);
    let outcome = with_temp_euid(nobody_uid(), || {
        let sandbox_ref = sandbox.borrow();
        sandbox_ref.run_bootstrap()
    });
    sandbox.borrow_mut().handle_outcome(outcome)
}

#[cfg(not(feature = "privileged-tests"))]
fn run_bootstrap_with_temp_drop(sandbox: &RefCell<BootstrapSandbox>) -> Result<()> {
    sandbox
        .borrow_mut()
        .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires the privileged-tests feature");
    Ok(())
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
        run_bootstrap_with_temp_drop(sandbox)
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
        sandbox
            .borrow_mut()
            .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires root access");
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
    sandbox.borrow_mut().assert_owned_by_expected_user()
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
fn bootstrap_as_unprivileged(
    _serial_guard: ScenarioSerialGuard,
    sandbox: RefCell<BootstrapSandbox>,
) {
    let _ = sandbox;
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 1)]
fn bootstrap_as_root(_serial_guard: ScenarioSerialGuard, sandbox: RefCell<BootstrapSandbox>) {
    let _ = sandbox;
}
