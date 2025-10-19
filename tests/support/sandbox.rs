//! Shared sandbox for behavioural tests that need isolated PostgreSQL directories.

use std::ffi::OsString;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result};

use pg_embedded_setup_unpriv::test_support::CapabilityTempDir;

use super::cap_fs::{remove_tree, set_permissions};
use super::env::{ScopedEnvVars, build_env, with_scoped_env};

#[derive(Debug)]
pub struct TestSandbox {
    _guard: CapabilityTempDir,
    base_dir: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
}

impl TestSandbox {
    pub fn new(prefix: &str) -> Result<Self> {
        let guard = CapabilityTempDir::new(prefix).context("create sandbox tempdir")?;
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

    pub fn install_dir(&self) -> &Utf8Path {
        &self.install_dir
    }

    pub fn data_dir(&self) -> &Utf8Path {
        &self.data_dir
    }

    pub fn base_env(&self) -> ScopedEnvVars {
        build_env([
            ("PG_RUNTIME_DIR", self.install_dir.as_str()),
            ("PG_DATA_DIR", self.data_dir.as_str()),
            ("PG_SUPERUSER", "postgres"),
            ("PG_PASSWORD", "postgres"),
        ])
    }

    pub fn env_without_timezone(&self) -> ScopedEnvVars {
        let mut vars = self.base_env();
        vars.push((OsString::from("TZDIR"), None));
        vars.push((OsString::from("TZ"), None));
        vars
    }

    pub fn env_with_timezone_override(&self, tz_dir: &Utf8Path) -> ScopedEnvVars {
        let mut vars = self.base_env();
        vars.push((
            OsString::from("TZDIR"),
            Some(OsString::from(tz_dir.as_str())),
        ));
        vars
    }

    pub fn with_env<R>(&self, vars: ScopedEnvVars, body: impl FnOnce() -> R) -> R {
        with_scoped_env(vars, body)
    }

    pub fn reset(&self) -> Result<()> {
        remove_tree(self.install_dir())?;
        remove_tree(self.data_dir())?;
        set_permissions(&self.base_dir, 0o777)?;
        Ok(())
    }
}
