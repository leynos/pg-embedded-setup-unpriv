//! Facilitates preparing an embedded PostgreSQL instance while dropping root
//! privileges.
//!
//! The library owns the lifecycle for configuring paths, permissions, and
//! process identity so the bundled PostgreSQL binaries can initialise safely
//! under an unprivileged account.

mod bootstrap;
mod cluster;
mod env;
mod error;
mod fs;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
mod privileges;
#[doc(hidden)]
pub mod test_support;
#[doc(hidden)]
pub mod worker;

#[doc(hidden)]
pub use crate::env::ScopedEnv;
pub use bootstrap::{
    ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    bootstrap_for_tests, detect_execution_privileges, run,
};
pub use cluster::TestCluster;
#[doc(hidden)]
pub use cluster::WorkerOperation;
#[doc(hidden)]
pub use error::BootstrapResult;
pub use error::{Error, Result};
#[cfg(feature = "privileged-tests")]
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
pub use privileges::with_temp_euid;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
pub use privileges::{default_paths_for, make_data_dir_private, make_dir_accessible, nobody_uid};

use color_eyre::eyre::{Context, eyre};
use ortho_config::OrthoConfig;
use postgresql_embedded::{Settings, VersionReq};
use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ConfigResult};
use camino::Utf8PathBuf;
use std::ffi::OsString;

/// Captures PostgreSQL settings supplied via environment variables.
#[derive(Debug, Clone, Serialize, Deserialize, OrthoConfig, Default)]
#[ortho_config(prefix = "PG")]
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::PgEnvCfg;
///
/// let cfg = PgEnvCfg::default();
/// assert!(cfg.port.is_none());
/// ```
pub struct PgEnvCfg {
    /// Optional semver requirement that constrains the PostgreSQL version.
    pub version_req: Option<String>,
    /// Port assigned to the embedded PostgreSQL server.
    pub port: Option<u16>,
    /// Name of the administrative user created for the cluster.
    pub superuser: Option<String>,
    /// Password provisioned for the administrative user.
    pub password: Option<String>,
    /// Directory used for PostgreSQL data files when provided.
    pub data_dir: Option<Utf8PathBuf>,
    /// Directory containing the PostgreSQL binaries when provided.
    pub runtime_dir: Option<Utf8PathBuf>,
    /// Locale applied to `initdb` when specified.
    pub locale: Option<String>,
    /// Encoding applied to `initdb` when specified.
    pub encoding: Option<String>,
}

impl PgEnvCfg {
    /// Loads configuration from environment variables without parsing CLI arguments.
    pub fn load() -> ConfigResult<Self> {
        let args = [OsString::from("pg-embedded-setup-unpriv")];
        Self::load_from_iter(args).map_err(|err| ConfigError::from(eyre!(err)))
    }

    /// Converts the configuration into a complete `postgresql_embedded::Settings` object.
    ///
    /// Applies version, connection, path, and locale settings from the current configuration.
    /// Returns an error if the version requirement is invalid.
    ///
    /// # Returns
    /// A fully configured `Settings` instance on success, or an error if configuration fails.
    pub fn to_settings(&self) -> Result<Settings> {
        let mut s = Settings::default();

        self.apply_version(&mut s)?;
        self.apply_connection(&mut s);
        self.apply_paths(&mut s);
        self.apply_locale(&mut s);

        Ok(s)
    }

    fn apply_version(&self, settings: &mut Settings) -> ConfigResult<()> {
        if let Some(ref vr) = self.version_req {
            settings.version =
                VersionReq::parse(vr).context("PG_VERSION_REQ invalid semver spec")?;
        }
        Ok(())
    }

    fn apply_connection(&self, settings: &mut Settings) {
        if let Some(p) = self.port {
            settings.port = p;
        }
        if let Some(ref u) = self.superuser {
            settings.username = u.clone();
        }
        if let Some(ref pw) = self.password {
            settings.password = pw.clone();
        }
    }

    fn apply_paths(&self, settings: &mut Settings) {
        if let Some(ref dir) = self.data_dir {
            settings.data_dir = dir.clone().into_std_path_buf();
        }
        if let Some(ref dir) = self.runtime_dir {
            settings.installation_dir = dir.clone().into_std_path_buf();
        }
    }

    /// Applies locale and encoding settings to the PostgreSQL configuration if specified
    /// in the environment.
    ///
    /// Inserts the `locale` and `encoding` values into the settings configuration map when
    /// present in the environment configuration.
    fn apply_locale(&self, settings: &mut Settings) {
        if let Some(ref loc) = self.locale {
            settings.configuration.insert("locale".into(), loc.clone());
        }
        if let Some(ref enc) = self.encoding {
            settings
                .configuration
                .insert("encoding".into(), enc.clone());
        }
    }
}
