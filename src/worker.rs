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
