use camino::{Utf8Path, Utf8PathBuf};
#[cfg(feature = "diesel-support")]
use color_eyre::eyre::WrapErr;

use crate::TestBootstrapSettings;
#[cfg(feature = "diesel-support")]
use crate::error::BootstrapResult;

/// Provides ergonomic accessors for connection-oriented cluster metadata.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = TestCluster::new()?;
/// let metadata = cluster.connection().metadata();
/// assert_eq!(metadata.host(), "localhost");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionMetadata {
    host: String,
    port: u16,
    superuser: String,
    password: String,
    pgpass_file: Utf8PathBuf,
}

impl ConnectionMetadata {
    pub(crate) fn from_settings(settings: &TestBootstrapSettings) -> Self {
        Self {
            host: settings.settings.host.clone(),
            port: settings.settings.port,
            superuser: settings.settings.username.clone(),
            password: settings.settings.password.clone(),
            pgpass_file: settings.environment.pgpass_file.clone(),
        }
    }

    /// Returns the configured database host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the configured port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Returns the configured superuser name.
    #[must_use]
    pub fn superuser(&self) -> &str {
        &self.superuser
    }

    /// Returns the generated superuser password.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }

    /// Returns the prepared `.pgpass` file path.
    #[must_use]
    pub fn pgpass_file(&self) -> &Utf8Path {
        self.pgpass_file.as_ref()
    }

    /// Constructs a libpq-compatible URL for `database`.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String {
        format!(
            "postgresql://{}:{}@{}:{}/{}",
            self.superuser(),
            self.password(),
            self.host(),
            self.port(),
            database,
        )
    }
}

/// Accessor for connection helpers derived from a
/// [`TestCluster`](crate::TestCluster).
///
/// Enable the `diesel-support` feature to call the Diesel connection helper.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = TestCluster::new()?;
/// let url = cluster.connection().database_url("postgres");
/// assert!(url.contains("postgresql://"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug)]
pub struct TestClusterConnection<'cluster> {
    settings: &'cluster TestBootstrapSettings,
}

impl<'cluster> TestClusterConnection<'cluster> {
    pub(crate) const fn new(settings: &'cluster TestBootstrapSettings) -> Self {
        Self { settings }
    }

    /// Returns host metadata without cloning the underlying settings.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.settings.settings.host
    }

    /// Returns the configured port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.settings.settings.port
    }

    /// Returns the configured superuser account name.
    #[must_use]
    pub fn superuser(&self) -> &str {
        &self.settings.settings.username
    }

    /// Returns the generated password for the superuser.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.settings.settings.password
    }

    /// Returns the `.pgpass` file prepared during bootstrap.
    #[must_use]
    pub fn pgpass_file(&self) -> &Utf8Path {
        self.settings.environment.pgpass_file.as_ref()
    }

    /// Provides an owned snapshot of the connection metadata.
    #[must_use]
    pub fn metadata(&self) -> ConnectionMetadata {
        ConnectionMetadata::from_settings(self.settings)
    }

    /// Builds a libpq-compatible database URL for `database`.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String {
        self.settings.settings.url(database)
    }

    /// Establishes a Diesel connection for the target `database`.
    ///
    /// # Errors
    /// Returns a [`crate::error::BootstrapError`] when Diesel cannot connect.
    #[cfg(feature = "diesel-support")]
    pub fn diesel_connection(&self, database: &str) -> BootstrapResult<diesel::PgConnection> {
        use diesel::Connection;

        let url = self.database_url(database);
        diesel::PgConnection::establish(&url)
            .wrap_err(format!("failed to connect to {database} via Diesel"))
            .map_err(crate::error::BootstrapError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TestBootstrapSettings;
    use crate::bootstrap::{ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment};
    use postgresql_embedded::Settings;
    use std::time::Duration;

    fn sample_settings() -> TestBootstrapSettings {
        let settings = Settings {
            host: "127.0.0.1".into(),
            port: 55_321,
            username: "fixture_user".into(),
            password: "fixture_pass".into(),
            data_dir: "/tmp/cluster-data".into(),
            installation_dir: "/tmp/cluster-install".into(),
            ..Settings::default()
        };

        TestBootstrapSettings {
            privileges: ExecutionPrivileges::Unprivileged,
            execution_mode: ExecutionMode::InProcess,
            settings,
            environment: TestBootstrapEnvironment {
                home: Utf8PathBuf::from("/tmp/home"),
                xdg_cache_home: Utf8PathBuf::from("/tmp/home/cache"),
                xdg_runtime_dir: Utf8PathBuf::from("/tmp/home/run"),
                pgpass_file: Utf8PathBuf::from("/tmp/home/.pgpass"),
                tz_dir: Some(Utf8PathBuf::from("/usr/share/zoneinfo")),
                timezone: "UTC".into(),
            },
            worker_binary: None,
            setup_timeout: Duration::from_secs(1),
            start_timeout: Duration::from_secs(1),
            shutdown_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn metadata_reflects_underlying_settings() {
        let settings = sample_settings();
        let connection = TestClusterConnection::new(&settings);
        let metadata = connection.metadata();

        assert_eq!(metadata.host(), "127.0.0.1");
        assert_eq!(metadata.port(), 55_321);
        assert_eq!(metadata.superuser(), "fixture_user");
        assert_eq!(metadata.password(), "fixture_pass");
        assert_eq!(metadata.pgpass_file(), Utf8Path::new("/tmp/home/.pgpass"));
    }

    #[test]
    fn database_url_matches_postgresql_embedded() {
        let settings = sample_settings();
        let connection = TestClusterConnection::new(&settings);
        let expected = settings.settings.url("postgres");

        assert_eq!(connection.database_url("postgres"), expected);
    }
}
