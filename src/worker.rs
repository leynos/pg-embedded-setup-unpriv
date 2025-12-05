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
use color_eyre::eyre::eyre;
use postgresql_embedded::{Settings, VersionReq};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, DurationSeconds, serde_as};
use std::collections::HashMap;
use std::time::Duration;

/// Serialised representation of [`Settings`] for subprocess helpers.
#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub struct SettingsSnapshot {
    releases_url: String,
    #[serde_as(as = "DisplayFromStr")]
    version: VersionReq,
    installation_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    host: String,
    port: u16,
    username: String,
    #[serde(with = "secret_string")]
    password: SecretString,
    temporary: bool,
    #[serde_as(as = "Option<DurationSeconds<u64>>")]
    timeout_secs: Option<Duration>,
    configuration: HashMap<String, String>,
    trust_installation_dir: bool,
}

impl SettingsSnapshot {
    /// Converts the snapshot back into [`Settings`].
    pub fn into_settings(self) -> Result<Settings, BootstrapError> {
        Ok(self.into())
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
            version: settings.version.clone(),
            installation_dir,
            password_file,
            data_dir,
            host: settings.host.clone(),
            port: settings.port,
            username: settings.username.clone(),
            password: SecretString::from(settings.password.clone()),
            temporary: settings.temporary,
            timeout_secs: settings.timeout,
            configuration: settings.configuration.clone(),
            trust_installation_dir: settings.trust_installation_dir,
        })
    }
}

/// Payload exchanged with the worker subprocess.
#[derive(Serialize, Deserialize, Debug)]
pub struct WorkerPayload {
    pub settings: SettingsSnapshot,
    #[serde(with = "secret_string_option")]
    pub environment: Vec<(String, Option<SecretString>)>,
}

impl WorkerPayload {
    pub fn new(
        settings: &Settings,
        environment: Vec<(String, Option<String>)>,
    ) -> Result<Self, BootstrapError> {
        Ok(Self {
            settings: SettingsSnapshot::try_from(settings)?,
            environment: environment
                .into_iter()
                .map(|(key, value)| (key, value.map(SecretString::from)))
                .collect(),
        })
    }
}

impl From<SettingsSnapshot> for Settings {
    fn from(snapshot: SettingsSnapshot) -> Self {
        Self {
            releases_url: snapshot.releases_url,
            version: snapshot.version,
            installation_dir: snapshot.installation_dir.into(),
            password_file: snapshot.password_file.into(),
            data_dir: snapshot.data_dir.into(),
            host: snapshot.host,
            port: snapshot.port,
            username: snapshot.username,
            password: snapshot.password.expose_secret().to_owned(),
            temporary: snapshot.temporary,
            timeout: snapshot.timeout_secs,
            configuration: snapshot.configuration,
            trust_installation_dir: snapshot.trust_installation_dir,
        }
    }
}

mod secret_string {
    use secrecy::{ExposeSecret, SecretString};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &SecretString, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value.expose_secret())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SecretString, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Into::into)
    }
}

mod secret_string_option {
    use secrecy::{ExposeSecret, SecretString};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(
        entries: &[(String, Option<SecretString>)],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mapped: Vec<(String, Option<String>)> = entries
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_ref()
                        .map(|secret| secret.expose_secret().to_owned()),
                )
            })
            .collect();
        mapped.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Vec<(String, Option<SecretString>)>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<(String, Option<String>)>::deserialize(deserializer).map(|entries| {
            entries
                .into_iter()
                .map(|(key, value)| (key, value.map(Into::into)))
                .collect()
        })
    }
}
