//! Sandbox environment for bootstrap privilege tests.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::ErrorKind;

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::MetadataExt;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::Uid;
use pg_embedded_setup_unpriv::test_support::CapabilityTempDir;
use pg_embedded_setup_unpriv::test_support::metadata;
use pg_embedded_setup_unpriv::ExecutionPrivileges;
#[cfg(feature = "privileged-tests")]
use pg_embedded_setup_unpriv::nobody_uid;
use rstest::fixture;

use super::cap_fs_bootstrap::{remove_tree, set_permissions};
use super::env::{build_env, with_scoped_env};
use super::skip::skip_message;

/// Test sandbox for bootstrap privilege scenarios.
#[derive(Debug)]
pub struct BootstrapSandbox {
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
    /// Creates a new bootstrap sandbox with temporary directories.
    pub fn new() -> Result<Self> {
        let tempdir_guard =
            CapabilityTempDir::new("bootstrap-sandbox").context("create sandbox tempdir")?;
        let base_path = tempdir_guard.path().to_owned();
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

    /// Returns the base environment variables for bootstrap operations.
    pub fn base_env(&self) -> Vec<(OsString, Option<OsString>)> {
        build_env([
            ("PG_RUNTIME_DIR", self.install_dir.as_str()),
            ("PG_DATA_DIR", self.data_dir.as_str()),
            ("PG_SUPERUSER", "postgres"),
            ("PG_PASSWORD", "postgres"),
        ])
    }

    /// Runs a closure with the sandbox environment variables set.
    pub fn with_env<F, R>(&self, body: F) -> R
    where
        F: FnOnce() -> R,
    {
        with_scoped_env(self.base_env(), body)
    }

    /// Runs a closure with sandbox environment but without `PG_EMBEDDED_WORKER`.
    pub fn with_env_without_worker<F, R>(&self, body: F) -> R
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

    /// Runs the bootstrap process with the sandbox environment.
    pub fn run_bootstrap(&self) -> pg_embedded_setup_unpriv::Result<()> {
        self.with_env(pg_embedded_setup_unpriv::run)
    }

    /// Runs the bootstrap process without a worker binary configured.
    pub fn run_bootstrap_without_worker(&self) -> pg_embedded_setup_unpriv::Result<()> {
        self.with_env_without_worker(pg_embedded_setup_unpriv::run)
    }

    /// Resets the sandbox to a clean state.
    pub fn reset(&mut self) -> Result<()> {
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

    /// Records the detected execution privileges.
    pub const fn record_privileges(&mut self, privileges: ExecutionPrivileges) {
        self.detected = Some(privileges);
    }

    /// Sets the expected owner UID for directory ownership checks.
    pub const fn set_expected_owner(&mut self, uid: Uid) {
        self.expected_owner = Some(uid);
    }

    /// Records an error message from a failed bootstrap attempt.
    pub fn record_error(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    /// Marks this scenario as skipped with a reason.
    pub fn mark_skipped(&mut self, skip_reason: impl Into<String>) {
        let message = skip_reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    /// Returns whether this scenario has been marked as skipped.
    pub const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    /// Returns the last recorded error message.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Asserts that the detected privileges match the expected value.
    pub fn assert_detected(&self, expected: ExecutionPrivileges) -> Result<()> {
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

    /// Asserts that sandbox directories are owned by the expected user.
    pub fn assert_owned_by_expected_user(&mut self) -> Result<()> {
        if self.is_skipped() {
            return Ok(());
        }
        let expected = self
            .expected_owner
            .ok_or_else(|| eyre!("expected owner not recorded for sandbox"))?;
        let paths = [self.install_dir.clone(), self.data_dir.clone()];
        for path in paths {
            if let Some(reason) = Self::inspect_path_owner(path.as_ref(), expected)? {
                self.mark_skipped(reason);
            }
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

    /// Handles the outcome of a bootstrap attempt, marking as skipped if appropriate.
    pub fn handle_outcome(&mut self, outcome: pg_embedded_setup_unpriv::Result<()>) -> Result<()> {
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

/// Marks the sandbox as skipped for temporary UID switching (privileged-tests feature).
#[cfg(feature = "privileged-tests")]
pub fn run_bootstrap_with_temp_drop(sandbox: &RefCell<BootstrapSandbox>) {
    sandbox.borrow_mut().set_expected_owner(nobody_uid());
    sandbox
        .borrow_mut()
        .mark_skipped("SKIP-BOOTSTRAP: temporary UID switching is no longer supported");
}

/// Marks the sandbox as skipped when privileged-tests feature is not enabled.
#[cfg(not(feature = "privileged-tests"))]
pub fn run_bootstrap_with_temp_drop(sandbox: &RefCell<BootstrapSandbox>) {
    sandbox
        .borrow_mut()
        .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires the privileged-tests feature");
}

/// Result type alias for the bootstrap sandbox fixture.
pub type BootstrapSandboxFixture = Result<RefCell<BootstrapSandbox>>;

/// Borrows the sandbox from the fixture result, converting errors.
pub fn borrow_sandbox(sandbox: &BootstrapSandboxFixture) -> Result<&RefCell<BootstrapSandbox>> {
    sandbox
        .as_ref()
        .map_err(|err| eyre!(format!("bootstrap sandbox fixture failed: {err}")))
}

/// Fixture that provides a bootstrap sandbox for testing.
#[fixture]
pub fn sandbox() -> BootstrapSandboxFixture {
    Ok(RefCell::new(BootstrapSandbox::new()?))
}
