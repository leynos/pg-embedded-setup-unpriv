//! Facilitates preparing an embedded PostgreSQL instance while dropping root
//! privileges.
//!
//! The library owns the lifecycle for configuring paths, permissions, and
//! process identity so the bundled PostgreSQL binaries can initialise safely
//! under an unprivileged account.
#![allow(non_snake_case)]

mod bootstrap;
mod error;
mod fs;
mod privileges;
#[doc(hidden)]
pub mod test_support;

pub use bootstrap::{ExecutionPrivileges, detect_execution_privileges, run};
pub use error::{Error, Result};
#[cfg(feature = "privileged-tests")]
pub use privileges::with_temp_euid;
pub use privileges::{default_paths_for, make_data_dir_private, make_dir_accessible, nobody_uid};

use color_eyre::eyre::Context;
use ortho_config::OrthoConfig;
use postgresql_embedded::{Settings, VersionReq};
use serde::{Deserialize, Serialize};

use crate::error::ConfigResult;

#[allow(non_snake_case)]
#[derive(Debug, Clone, Serialize, Deserialize, OrthoConfig, Default)]
#[ortho_config(prefix = "PG")]
pub struct PgEnvCfg {
    /// e.g. "=16.4.0" or "^17"
    pub version_req: Option<String>,
    pub port: Option<u16>,
    pub superuser: Option<String>,
    pub password: Option<String>,
    pub data_dir: Option<std::path::PathBuf>,
    pub runtime_dir: Option<std::path::PathBuf>,
    pub locale: Option<String>,
    pub encoding: Option<String>,
}

impl PgEnvCfg {
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
            settings.data_dir = dir.clone();
        }
        if let Some(ref dir) = self.runtime_dir {
            settings.installation_dir = dir.clone();
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
