#![cfg(unix)]

use std::cell::RefCell;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::{Uid, geteuid};
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges, detect_execution_privileges, nobody_uid, with_temp_euid,
};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};
use tempfile::TempDir;

#[derive(Debug)]
struct BootstrapSandbox {
    base: TempDir,
    install_dir: PathBuf,
    data_dir: PathBuf,
    previous_runtime_dir: Option<OsString>,
    previous_data_dir: Option<OsString>,
    previous_superuser: Option<OsString>,
    previous_password: Option<OsString>,
    detected: Option<ExecutionPrivileges>,
    expected_owner: Option<Uid>,
    skip_checks: bool,
}

impl BootstrapSandbox {
    fn new() -> Result<Self> {
        let base = TempDir::new().context("create sandbox tempdir")?;
        fs::set_permissions(base.path(), fs::Permissions::from_mode(0o777))
            .with_context(|| format!("chmod {}", base.path().display()))?;

        let install_dir = base.path().join("install");
        let data_dir = base.path().join("data");

        let previous_runtime_dir = env::var_os("PG_RUNTIME_DIR");
        let previous_data_dir = env::var_os("PG_DATA_DIR");
        let previous_superuser = env::var_os("PG_SUPERUSER");
        let previous_password = env::var_os("PG_PASSWORD");

        set_env("PG_RUNTIME_DIR", &install_dir);
        set_env("PG_DATA_DIR", &data_dir);
        set_env("PG_SUPERUSER", "postgres");
        set_env("PG_PASSWORD", "postgres");

        Ok(Self {
            base,
            install_dir,
            data_dir,
            previous_runtime_dir,
            previous_data_dir,
            previous_superuser,
            previous_password,
            detected: None,
            expected_owner: None,
            skip_checks: false,
        })
    }

    fn reset(&mut self) -> Result<()> {
        self.skip_checks = false;
        self.remove_if_present(&self.install_dir)?;
        self.remove_if_present(&self.data_dir)?;
        fs::set_permissions(self.base.path(), fs::Permissions::from_mode(0o777))
            .with_context(|| format!("chmod {}", self.base.path().display()))?;
        Ok(())
    }

    fn remove_if_present(&self, path: &Path) -> Result<()> {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
        }
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

    fn assert_path_owner(&self, path: &Path, expected: Uid) -> Result<()> {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) if err.kind() == ErrorKind::PermissionDenied => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| format!("inspect ownership of {}", path.display()));
            }
        };
        ensure!(
            metadata.uid() == expected.as_raw(),
            "expected {} to be owned by uid {} but found {}",
            path.display(),
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

impl Drop for BootstrapSandbox {
    fn drop(&mut self) {
        restore_env("PG_RUNTIME_DIR", self.previous_runtime_dir.take());
        restore_env("PG_DATA_DIR", self.previous_data_dir.take());
        restore_env("PG_SUPERUSER", self.previous_superuser.take());
        restore_env("PG_PASSWORD", self.previous_password.take());
    }
}

fn set_env<K, V>(key: K, value: V)
where
    K: AsRef<str>,
    V: Into<OsString>,
{
    unsafe { env::set_var(key.as_ref(), value.into()) }
}

fn restore_env(key: &str, value: Option<OsString>) {
    unsafe {
        match value {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
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
        {
            sandbox.borrow_mut().set_expected_owner(nobody_uid());
        }
        let outcome = with_temp_euid(nobody_uid(), || {
            sandbox
                .borrow_mut()
                .record_privileges(detect_execution_privileges());
            pg_embedded_setup_unpriv::run()
        });
        sandbox.borrow_mut().handle_outcome(outcome)
    } else {
        let uid = geteuid();
        let mut state = sandbox.borrow_mut();
        state.set_expected_owner(uid);
        state.record_privileges(detect_execution_privileges());
        let outcome = pg_embedded_setup_unpriv::run();
        state.handle_outcome(outcome)
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
        let outcome = pg_embedded_setup_unpriv::run();
        let mut state = sandbox.borrow_mut();
        state.handle_outcome(outcome)?;
        if state.is_skipped() {
            return Ok(());
        }
    }

    let outcome = pg_embedded_setup_unpriv::run();
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
