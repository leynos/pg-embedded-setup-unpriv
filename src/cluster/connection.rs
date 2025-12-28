//! Connection helpers for `TestCluster`, including metadata accessors and optional Diesel support.
use std::sync::{Mutex, OnceLock};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::WrapErr;
use dashmap::DashMap;
use postgres::{Client, NoTls};
use postgresql_embedded::Settings;
use tracing::info_span;

use super::temporary_database::TemporaryDatabase;
use crate::TestBootstrapSettings;
use crate::error::BootstrapResult;

/// Escapes a SQL identifier by doubling embedded double quotes.
///
/// `PostgreSQL` identifiers are quoted with double quotes. Any embedded
/// double quote must be escaped by doubling it.
pub(crate) fn escape_identifier(name: &str) -> String {
    name.replace('"', "\"\"")
}

/// Creates a new `PostgreSQL` client connection from the given URL.
///
/// This is a shared helper for admin database connections used by both
/// `TestClusterConnection` and `TemporaryDatabase`.
pub(crate) fn connect_admin(url: &str) -> BootstrapResult<Client> {
    Client::connect(url, NoTls)
        .wrap_err("failed to connect to admin database")
        .map_err(crate::error::BootstrapError::from)
}

/// Global per-template locks to prevent concurrent template creation.
///
/// Uses a `DashMap` to allow lock-free reads and concurrent access to
/// different templates while serialising access to the same template.
static TEMPLATE_LOCKS: OnceLock<DashMap<String, Mutex<()>>> = OnceLock::new();

fn template_locks() -> &'static DashMap<String, Mutex<()>> {
    TEMPLATE_LOCKS.get_or_init(DashMap::new)
}

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
#[derive(Debug, Clone)]
pub struct ConnectionMetadata {
    settings: Settings,
    pgpass_file: Utf8PathBuf,
}

impl ConnectionMetadata {
    pub(crate) fn from_settings(settings: &TestBootstrapSettings) -> Self {
        Self {
            settings: settings.settings.clone(),
            pgpass_file: settings.environment.pgpass_file.clone(),
        }
    }

    /// Returns the configured database host.
    #[must_use]
    pub fn host(&self) -> &str {
        self.settings.host.as_str()
    }

    /// Returns the configured port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.settings.port
    }

    /// Returns the configured superuser name.
    #[must_use]
    pub fn superuser(&self) -> &str {
        self.settings.username.as_str()
    }

    /// Returns the generated superuser password.
    #[must_use]
    pub fn password(&self) -> &str {
        self.settings.password.as_str()
    }

    /// Returns the prepared `.pgpass` file path.
    #[must_use]
    pub fn pgpass_file(&self) -> &Utf8Path {
        self.pgpass_file.as_ref()
    }

    /// Constructs a libpq-compatible URL for `database` using the underlying
    /// `postgresql_embedded` helper.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String {
        self.settings.url(database)
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
#[derive(Debug, Clone)]
pub struct TestClusterConnection {
    metadata: ConnectionMetadata,
}

impl TestClusterConnection {
    pub(crate) fn new(settings: &TestBootstrapSettings) -> Self {
        Self {
            metadata: ConnectionMetadata::from_settings(settings),
        }
    }

    /// Returns host metadata without exposing internal storage.
    #[must_use]
    pub fn host(&self) -> &str {
        self.metadata.host()
    }

    /// Returns the configured port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.metadata.port()
    }

    /// Returns the configured superuser account name.
    #[must_use]
    pub fn superuser(&self) -> &str {
        self.metadata.superuser()
    }

    /// Returns the generated password for the superuser.
    #[must_use]
    pub fn password(&self) -> &str {
        self.metadata.password()
    }

    /// Returns the `.pgpass` file prepared during bootstrap.
    #[must_use]
    pub fn pgpass_file(&self) -> &Utf8Path {
        self.metadata.pgpass_file()
    }

    /// Provides an owned snapshot of the connection metadata.
    #[must_use]
    pub fn metadata(&self) -> ConnectionMetadata {
        self.metadata.clone()
    }

    /// Builds a libpq-compatible database URL for `database`.
    #[must_use]
    pub fn database_url(&self, database: &str) -> String {
        self.metadata.database_url(database)
    }

    /// Establishes a Diesel connection for the target `database`.
    ///
    /// # Errors
    /// Returns a [`crate::error::BootstrapError`] when Diesel cannot connect.
    #[cfg(feature = "diesel-support")]
    pub fn diesel_connection(&self, database: &str) -> BootstrapResult<diesel::PgConnection> {
        use diesel::Connection;

        diesel::PgConnection::establish(&self.database_url(database))
            .wrap_err(format!("failed to connect to {database} via Diesel"))
            .map_err(crate::error::BootstrapError::from)
    }

    /// Creates a new database with the given name.
    ///
    /// Connects to the `postgres` database as superuser and executes
    /// `CREATE DATABASE`.
    ///
    /// # Errors
    ///
    /// Returns an error if the database already exists or if the connection
    /// fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// cluster.connection().create_database("my_test_db")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_database(&self, name: &str) -> BootstrapResult<()> {
        let _span = info_span!("create_database", db = %name).entered();
        let mut client = self.admin_client()?;
        let escaped = escape_identifier(name);
        let sql = format!("CREATE DATABASE \"{escaped}\"");
        client
            .batch_execute(&sql)
            .wrap_err(format!("failed to create database '{name}'"))
            .map_err(crate::error::BootstrapError::from)
    }

    /// Creates a new database by cloning an existing template.
    ///
    /// Connects to the `postgres` database as superuser and executes
    /// `CREATE DATABASE ... TEMPLATE`. This is significantly faster than
    /// creating an empty database and running migrations, as `PostgreSQL`
    /// performs a filesystem-level copy.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The target database already exists
    /// - The template database does not exist
    /// - The template database has active connections
    /// - The connection fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    ///
    /// // Create and set up a template database
    /// cluster.connection().create_database("my_template")?;
    /// // ... run migrations on my_template ...
    ///
    /// // Clone the template for a test
    /// cluster.connection().create_database_from_template("test_db", "my_template")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_database_from_template(&self, name: &str, template: &str) -> BootstrapResult<()> {
        let _span =
            info_span!("create_database_from_template", db = %name, template = %template).entered();
        let mut client = self.admin_client()?;
        let escaped_name = escape_identifier(name);
        let escaped_template = escape_identifier(template);
        let sql = format!("CREATE DATABASE \"{escaped_name}\" TEMPLATE \"{escaped_template}\"");
        client
            .batch_execute(&sql)
            .wrap_err(format!(
                "failed to create database '{name}' from template '{template}'"
            ))
            .map_err(crate::error::BootstrapError::from)
    }

    /// Drops an existing database.
    ///
    /// Connects to the `postgres` database as superuser and executes
    /// `DROP DATABASE`.
    ///
    /// # Errors
    ///
    /// Returns an error if the database does not exist, has active connections,
    /// or if the connection fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// cluster.connection().create_database("temp_db")?;
    /// cluster.connection().drop_database("temp_db")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn drop_database(&self, name: &str) -> BootstrapResult<()> {
        let _span = info_span!("drop_database", db = %name).entered();
        let mut client = self.admin_client()?;
        let escaped = escape_identifier(name);
        let sql = format!("DROP DATABASE \"{escaped}\"");
        client
            .batch_execute(&sql)
            .wrap_err(format!("failed to drop database '{name}'"))
            .map_err(crate::error::BootstrapError::from)
    }

    /// Checks whether a database with the given name exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// assert!(cluster.connection().database_exists("postgres")?);
    /// assert!(!cluster.connection().database_exists("nonexistent")?);
    /// # Ok(())
    /// # }
    /// ```
    pub fn database_exists(&self, name: &str) -> BootstrapResult<bool> {
        let mut client = self.admin_client()?;
        let row = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
                &[&name],
            )
            .wrap_err("failed to query pg_database")
            .map_err(crate::error::BootstrapError::from)?;
        Ok(row.get(0))
    }

    /// Ensures a template database exists, creating it if necessary.
    ///
    /// Uses per-template locking to prevent concurrent creation attempts when
    /// multiple tests race to initialise the same template. The `setup_fn` is
    /// called only if the template does not already exist.
    ///
    /// # Errors
    ///
    /// Returns an error if database creation fails or if `setup_fn` returns
    /// an error.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    ///
    /// // Ensure template exists, running migrations if needed
    /// cluster.connection().ensure_template_exists("my_template", |db_name| {
    ///     // Run migrations on the newly created template database
    ///     // e.g., diesel::migration::run(&mut conn)?;
    ///     Ok(())
    /// })?;
    ///
    /// // Clone the template for each test
    /// cluster.connection().create_database_from_template("test_db_1", "my_template")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn ensure_template_exists<F>(&self, name: &str, setup_fn: F) -> BootstrapResult<()>
    where
        F: FnOnce(&str) -> BootstrapResult<()>,
    {
        let _span = info_span!("ensure_template_exists", template = %name).entered();
        let locks = template_locks();
        let lock = locks
            .entry(name.to_owned())
            .or_insert_with(|| Mutex::new(()));
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if !self.database_exists(name)? {
            self.create_database(name)?;
            setup_fn(name)?;
        }
        Ok(())
    }

    /// Creates a temporary database that is dropped when the guard is dropped.
    ///
    /// This is useful for test isolation where each test creates its own
    /// database and the database is automatically cleaned up when the test
    /// completes.
    ///
    /// # Errors
    ///
    /// Returns an error if the database already exists or if the connection
    /// fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// let temp_db = cluster.connection().temporary_database("my_temp_db")?;
    ///
    /// // Database is dropped automatically when temp_db goes out of scope
    /// let url = temp_db.url();
    /// # Ok(())
    /// # }
    /// ```
    pub fn temporary_database(&self, name: &str) -> BootstrapResult<TemporaryDatabase> {
        let _span = info_span!("temporary_database", db = %name).entered();
        self.create_database(name)?;
        Ok(TemporaryDatabase::new(
            name.to_owned(),
            self.database_url("postgres"),
            self.database_url(name),
        ))
    }

    /// Creates a temporary database from a template.
    ///
    /// Combines template cloning with RAII cleanup. The database is created
    /// by cloning the template and is automatically dropped when the guard
    /// goes out of scope.
    ///
    /// # Errors
    ///
    /// Returns an error if the target database already exists, the template
    /// does not exist, the template has active connections, or if the
    /// connection fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    ///
    /// // Create and migrate a template once
    /// cluster.ensure_template_exists("migrated_template", |_| Ok(()))?;
    ///
    /// // Each test gets its own database cloned from the template
    /// let temp_db = cluster.connection()
    ///     .temporary_database_from_template("test_db", "migrated_template")?;
    ///
    /// // Database is dropped automatically when temp_db goes out of scope
    /// # Ok(())
    /// # }
    /// ```
    pub fn temporary_database_from_template(
        &self,
        name: &str,
        template: &str,
    ) -> BootstrapResult<TemporaryDatabase> {
        let _span =
            info_span!("temporary_database_from_template", db = %name, template = %template)
                .entered();
        self.create_database_from_template(name, template)?;
        Ok(TemporaryDatabase::new(
            name.to_owned(),
            self.database_url("postgres"),
            self.database_url(name),
        ))
    }

    /// Connects to the `postgres` administration database.
    fn admin_client(&self) -> BootstrapResult<Client> {
        connect_admin(&self.database_url("postgres"))
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
