//! Serialization helpers for subprocess workers.
//!
//! Provides UTF-8 safe snapshots of [`postgresql_embedded::Settings`] so the
//! worker binary can restore settings and environment state received via IPC.
//!
//! # Examples
//! ```no_run
//! use pg_embedded_setup_unpriv::worker::{SettingsSnapshot, WorkerPayload};
//! use postgresql_embedded::Settings;
//! use std::error::Error;
//! use std::time::Duration;
//!
//! fn main() -> Result<(), Box<dyn Error>> {
//!     let mut settings = Settings::default();
//!     settings.releases_url = "https://example.invalid/releases".into();
//!     settings.installation_dir = "/var/lib/postgres/install".into();
//!     settings.password_file = "/var/lib/postgres/.pgpass".into();
//!     settings.data_dir = "/var/lib/postgres/data".into();
//!     settings.host = "127.0.0.1".into();
//!     settings.port = 54_321;
//!     settings.username = "postgres".into();
//!     settings.password = "secret".into();
//!     settings.temporary = false;
//!     settings.timeout = Some(Duration::from_secs(30));
//!     settings.configuration.insert("log_min_messages".into(), "debug".into());
//!     settings.trust_installation_dir = true;
//!
//!     let snapshot = SettingsSnapshot::try_from(&settings)?;
//!     let restored_from_snapshot = snapshot.into_settings()?;
//!     assert_eq!(restored_from_snapshot.host, settings.host);
//!
//!     let env = vec![("RUST_LOG".to_string(), Some("debug".to_string()))];
//!     let payload = WorkerPayload::new(&settings, env)?;
//!     let encoded = serde_json::to_string(&payload)?;
//!     let decoded: WorkerPayload = serde_json::from_str(&encoded)?;
//!     let restored = decoded.settings.into_settings()?;
//!
//!     assert_eq!(restored.host, settings.host);
//!     assert_eq!(restored.port, settings.port);
//!     Ok(())
//! }
//! ```
use crate::error::BootstrapError;
use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::Settings;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serialised representation of [`Settings`] for subprocess helpers.
#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    releases_url: String,
    version: String,
    installation_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    host: String,
    port: u16,
    username: String,
    password: String,
    temporary: bool,
    timeout_secs: Option<u64>,
    configuration: HashMap<String, String>,
    trust_installation_dir: bool,
}

impl SettingsSnapshot {
    /// Converts the snapshot back into [`Settings`].
    pub fn into_settings(self) -> Result<Settings, BootstrapError> {
        let version = postgresql_embedded::VersionReq::parse(&self.version)
            .wrap_err("failed to parse version requirement from snapshot")?;
        let installation_dir = self.installation_dir.into_std_path_buf();
        let password_file = self.password_file.into_std_path_buf();
        let data_dir = self.data_dir.into_std_path_buf();
        let timeout = self.timeout_secs.map(std::time::Duration::from_secs);

        Ok(Settings {
            releases_url: self.releases_url,
            version,
            installation_dir,
            password_file,
            data_dir,
            host: self.host,
            port: self.port,
            username: self.username,
            password: self.password,
            temporary: self.temporary,
            timeout,
            configuration: self.configuration,
            trust_installation_dir: self.trust_installation_dir,
        })
    }
}

impl TryFrom<&Settings> for SettingsSnapshot {
    type Error = BootstrapError;

    fn try_from(settings: &Settings) -> Result<Self, Self::Error> {
        let installation_dir = Utf8PathBuf::from_path_buf(settings.installation_dir.clone())
            .map_err(|_| eyre!("installation_dir must be valid UTF-8"))?;
        let password_file = Utf8PathBuf::from_path_buf(settings.password_file.clone())
            .map_err(|_| eyre!("password_file must be valid UTF-8"))?;
        let data_dir = Utf8PathBuf::from_path_buf(settings.data_dir.clone())
            .map_err(|_| eyre!("data_dir must be valid UTF-8"))?;

        Ok(Self {
            releases_url: settings.releases_url.clone(),
            version: settings.version.to_string(),
            installation_dir,
            password_file,
            data_dir,
            host: settings.host.clone(),
            port: settings.port,
            username: settings.username.clone(),
            password: settings.password.clone(),
            temporary: settings.temporary,
            timeout_secs: settings.timeout.map(|duration| duration.as_secs()),
            configuration: settings.configuration.clone(),
            trust_installation_dir: settings.trust_installation_dir,
        })
    }
}

/// Payload exchanged with the worker subprocess.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerPayload {
    pub settings: SettingsSnapshot,
    pub environment: Vec<(String, Option<String>)>,
}

impl WorkerPayload {
    pub fn new(
        settings: &Settings,
        environment: Vec<(String, Option<String>)>,
    ) -> Result<Self, BootstrapError> {
        Ok(Self {
            settings: SettingsSnapshot::try_from(settings)?,
            environment,
        })
    }
}
