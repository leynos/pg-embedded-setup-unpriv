//! Shared sandbox for behavioural tests that need isolated `PostgreSQL` directories.

use std::ffi::OsString;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, eyre};

use pg_embedded_setup_unpriv::test_support::CapabilityTempDir;
use pg_embedded_setup_unpriv::{ExecutionPrivileges, detect_execution_privileges};

use super::cap_fs::{remove_tree, set_permissions};
use super::env::{ScopedEnvVars, build_env, with_scoped_env};

/// Provides a capability-backed directory tree for behavioural `PostgreSQL`
/// tests. Each sandbox supplies dedicated installation and data directories so
/// scenarios remain isolated.
///
/// # Examples
///
/// ```rust
/// # use color_eyre::Result;
/// # use tests::support::sandbox::TestSandbox;
/// # fn docs() -> Result<()> {
/// let sandbox = TestSandbox::new("docs-sandbox")?;
/// assert!(sandbox.install_dir().ends_with("install"));
/// sandbox.reset()?;
/// # Ok(())
/// # }
/// # docs().expect("sandbox example should succeed");
/// ```
#[derive(Debug)]
pub struct TestSandbox {
    _guard: CapabilityTempDir,
    base_dir: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
}

impl TestSandbox {
    /// Creates a new sandbox rooted under the capability-aware temporary
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the sandbox directory or its capability handles
    /// cannot be created.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-new")?;
    /// assert!(sandbox.data_dir().ends_with("data"));
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("sandbox::new example should succeed");
    /// ```
    pub fn new(prefix: &str) -> Result<Self> {
        let guard = CapabilityTempDir::new(prefix).context("create sandbox tempdir")?;
        let base_dir = guard.path().to_owned();
        // Allow the postgres child processes to traverse and modify the tree
        // whilst keeping the directory mode aligned with privilege mode.
        set_permissions(&base_dir, base_dir_mode())?;
        let install_dir = base_dir.join("install");
        let data_dir = base_dir.join("data");

        Ok(Self {
            _guard: guard,
            base_dir,
            install_dir,
            data_dir,
        })
    }

    /// Returns the installation directory assigned to the sandbox.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-install")?;
    /// let install_dir = sandbox.install_dir();
    /// assert!(install_dir.ends_with("install"));
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("install_dir example should succeed");
    /// ```
    pub fn install_dir(&self) -> &Utf8Path {
        &self.install_dir
    }

    /// Returns the `PostgreSQL` data directory assigned to the sandbox.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-data")?;
    /// let data_dir = sandbox.data_dir();
    /// assert!(data_dir.ends_with("data"));
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("data_dir example should succeed");
    /// ```
    pub fn data_dir(&self) -> &Utf8Path {
        &self.data_dir
    }

    /// Provides the base environment variables required for `PostgreSQL` to run
    /// within the sandbox.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # use std::ffi::OsStr;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-base-env")?;
    /// let vars = sandbox.base_env();
    /// let has_runtime = vars.iter().any(|(key, _)| key == OsStr::new("PG_RUNTIME_DIR"));
    /// assert!(has_runtime, "runtime directory should be present");
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("base_env example should succeed");
    /// ```
    pub fn base_env(&self) -> ScopedEnvVars {
        build_env([
            ("PG_RUNTIME_DIR", self.install_dir.as_str()),
            ("PG_DATA_DIR", self.data_dir.as_str()),
            ("PG_SUPERUSER", "postgres"),
            ("PG_PASSWORD", "postgres"),
        ])
    }

    /// Derives the base environment with `TZ` and `TZDIR` removed so tests can
    /// exercise missing time zone data scenarios.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # use std::ffi::OsStr;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-without-tz")?;
    /// let vars = sandbox.env_without_timezone();
    /// let tz_missing = vars.iter().any(|(key, value)| key == OsStr::new("TZ") && value.is_none());
    /// let tzdir_missing = vars.iter().any(|(key, value)| key == OsStr::new("TZDIR") && value.is_none());
    /// assert!(tz_missing && tzdir_missing, "time zone variables should be cleared");
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("env_without_timezone example should succeed");
    /// ```
    pub fn env_without_timezone(&self) -> ScopedEnvVars {
        let mut vars = self.base_env();
        vars.push((OsString::from("TZDIR"), None));
        vars.push((OsString::from("TZ"), None));
        vars
    }

    /// Returns the base environment augmented with a custom `TZDIR` override.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-with-tz")?;
    /// let vars = sandbox.env_with_timezone_override(sandbox.install_dir());
    /// let has_override = vars.iter().any(|(key, value)| {
    ///     key == std::ffi::OsStr::new("TZDIR")
    ///         && value.as_ref().map(|os| os.as_os_str()) == Some(sandbox.install_dir().as_os_str())
    /// });
    /// assert!(has_override, "custom TZDIR should be present");
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("env_with_timezone_override example should succeed");
    /// ```
    pub fn env_with_timezone_override(&self, tz_dir: &Utf8Path) -> ScopedEnvVars {
        let mut vars = self.base_env();
        vars.push((
            OsString::from("TZDIR"),
            Some(OsString::from(tz_dir.as_str())),
        ));
        vars
    }

    /// Runs `body` with the supplied environment scoped to the sandbox.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-with-env")?;
    /// let vars = sandbox.base_env();
    /// let captured = sandbox.with_env(vars, || std::env::var("PG_SUPERUSER"));
    /// assert!(matches!(captured.as_deref(), Ok("postgres")));
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("with_env example should succeed");
    /// ```
    pub fn with_env<R>(&self, vars: ScopedEnvVars, body: impl FnOnce() -> R) -> R {
        debug_assert!(
            vars.iter().any(|(key, value)| {
                if key != "PG_RUNTIME_DIR" {
                    return false;
                }
                let Some(runtime_value) = value.as_deref().and_then(|runtime| runtime.to_str())
                else {
                    return false;
                };
                let runtime_path = Utf8Path::new(runtime_value);
                let Ok(remainder) = runtime_path.strip_prefix(self.install_dir()) else {
                    return false;
                };
                !remainder
                    .components()
                    .any(|component| matches!(component, camino::Utf8Component::ParentDir))
            }),
            "sandbox environment missing PG_RUNTIME_DIR for {}",
            self.install_dir
        );
        with_scoped_env(vars, body)
    }

    /// Deletes sandbox directories and re-applies restrictive permissions so
    /// subsequent scenarios start from a clean state.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use color_eyre::Result;
    /// # use tests::support::sandbox::TestSandbox;
    /// # fn docs() -> Result<()> {
    /// let sandbox = TestSandbox::new("example-reset")?;
    /// sandbox.reset()?;
    /// # Ok(())
    /// # }
    /// # docs().expect("reset example should succeed");
    /// ```
    pub fn reset(&self) -> Result<()> {
        remove_tree(self.install_dir())?;
        remove_tree(self.data_dir())?;
        set_permissions(&self.base_dir, base_dir_mode())?;
        Ok(())
    }
}

/// Selects base directory permissions based on privilege mode.
///
/// Root runs need world access so the `nobody` worker can traverse and write
/// to sandbox directories; unprivileged runs can stay tighter.
fn base_dir_mode() -> u32 {
    match detect_execution_privileges() {
        ExecutionPrivileges::Root => 0o777,
        ExecutionPrivileges::Unprivileged => 0o755,
    }
}

#[cfg(test)]
mod tests {
    //! Tests for sandbox environment helpers.

    use super::*;

    use color_eyre::eyre::Result;

    #[test]
    fn env_with_timezone_override_sets_tzdir() -> Result<()> {
        let sandbox = TestSandbox::new("sandbox-tz-override")?;
        let vars = sandbox.env_with_timezone_override(sandbox.install_dir());
        let has_override = vars.iter().any(|(key, value)| {
            key == "TZDIR" && value == &Some(OsString::from(sandbox.install_dir().as_str()))
        });
        if !has_override {
            return Err(eyre!("expected TZDIR override to be present"));
        }
        Ok(())
    }
}
