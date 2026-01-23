//! Send-safe handle for accessing a running `PostgreSQL` cluster.
//!
//! [`ClusterHandle`] provides thread-safe access to cluster metadata and
//! connection helpers. Unlike [`TestCluster`](super::TestCluster), handles
//! implement [`Send`] and [`Sync`], enabling patterns such as:
//!
//! - Shared cluster fixtures using [`OnceLock`](std::sync::OnceLock)
//! - rstest fixtures with timeouts (which require `Send + 'static`)
//! - Cross-thread sharing in async test patterns
//!
//! # Architecture
//!
//! The handle/guard split separates concerns:
//!
//! - **`ClusterHandle`**: Read-only access to cluster metadata. `Send + Sync`.
//! - **`ClusterGuard`**: Manages environment and shutdown. `!Send`.
//!
//! This separation preserves the safety of thread-local environment management
//! whilst enabling the most common shared cluster use cases.
//!
//! # Examples
//!
//! ```no_run
//! use std::sync::OnceLock;
//! use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
//!
//! static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
//!
//! fn shared_handle() -> &'static ClusterHandle {
//!     SHARED.get_or_init(|| {
//!         let (handle, _guard) = TestCluster::new_split()
//!             .expect("cluster bootstrap failed");
//!         // Guard drops here, but cluster keeps running
//!         handle
//!     })
//! }
//! ```

use super::connection::TestClusterConnection;
use super::lifecycle::DatabaseName;
use super::temporary_database::TemporaryDatabase;
use crate::error::BootstrapResult;
use crate::{TestBootstrapEnvironment, TestBootstrapSettings};
use postgresql_embedded::Settings;

/// Send-safe handle providing read-only access to a running `PostgreSQL` cluster.
///
/// Handles are lightweight and cloneable. They contain only the bootstrap
/// metadata needed to construct connections and query cluster state.
///
/// # Thread Safety
///
/// `ClusterHandle` implements [`Send`] and [`Sync`], making it safe to share
/// across threads. The underlying `PostgreSQL` process is an external OS process
/// that handles concurrent connections safely.
///
/// # Obtaining a Handle
///
/// Use [`TestCluster::new_split()`](super::TestCluster::new_split) to obtain
/// a handle and guard pair:
///
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// let (handle, guard) = TestCluster::new_split()?;
/// // handle: ClusterHandle (Send + Sync)
/// // guard: ClusterGuard (!Send, manages lifecycle)
/// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ClusterHandle {
    bootstrap: TestBootstrapSettings,
}

// Compile-time assertions that ClusterHandle is Send + Sync.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<ClusterHandle>();
    assert_sync::<ClusterHandle>();
};

impl ClusterHandle {
    /// Creates a new handle from bootstrap settings.
    pub(super) const fn new(bootstrap: TestBootstrapSettings) -> Self {
        Self { bootstrap }
    }

    /// Returns the prepared `PostgreSQL` settings for the running cluster.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// let (handle, _guard) = TestCluster::new_split()?;
    /// let url = handle.settings().url("my_database");
    /// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
    /// ```
    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.bootstrap.settings
    }

    /// Returns the environment required for clients to interact with the cluster.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// let (handle, _guard) = TestCluster::new_split()?;
    /// let env = handle.environment();
    /// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
    /// ```
    #[must_use]
    pub const fn environment(&self) -> &TestBootstrapEnvironment {
        &self.bootstrap.environment
    }

    /// Returns the bootstrap metadata captured when the cluster was started.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// let (handle, _guard) = TestCluster::new_split()?;
    /// let bootstrap = handle.bootstrap();
    /// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
    /// ```
    #[must_use]
    pub const fn bootstrap(&self) -> &TestBootstrapSettings {
        &self.bootstrap
    }

    /// Returns helper methods for constructing connection artefacts.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// let (handle, _guard) = TestCluster::new_split()?;
    /// let metadata = handle.connection().metadata();
    /// println!(
    ///     "postgresql://{}:***@{}:{}/postgres",
    ///     metadata.superuser(),
    ///     metadata.host(),
    ///     metadata.port(),
    /// );
    /// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
    /// ```
    #[must_use]
    pub fn connection(&self) -> TestClusterConnection {
        TestClusterConnection::new(&self.bootstrap)
    }
}

// Delegation methods that forward to TestClusterConnection.
impl ClusterHandle {
    /// Creates a new database with the given name.
    ///
    /// See [`TestClusterConnection::create_database`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the database already exists or if the connection fails.
    pub fn create_database(&self, name: impl Into<DatabaseName>) -> BootstrapResult<()> {
        self.connection().create_database(name)
    }

    /// Creates a new database by cloning an existing template.
    ///
    /// See [`TestClusterConnection::create_database_from_template`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the target database already exists, the template does
    /// not exist, or the connection fails.
    pub fn create_database_from_template(
        &self,
        name: impl Into<DatabaseName>,
        template: impl Into<DatabaseName>,
    ) -> BootstrapResult<()> {
        self.connection()
            .create_database_from_template(name, template)
    }

    /// Drops an existing database.
    ///
    /// See [`TestClusterConnection::drop_database`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the database does not exist or the connection fails.
    pub fn drop_database(&self, name: impl Into<DatabaseName>) -> BootstrapResult<()> {
        self.connection().drop_database(name)
    }

    /// Checks whether a database with the given name exists.
    ///
    /// See [`TestClusterConnection::database_exists`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    pub fn database_exists(&self, name: impl Into<DatabaseName>) -> BootstrapResult<bool> {
        self.connection().database_exists(name)
    }

    /// Ensures a template database exists, creating it if necessary.
    ///
    /// See [`TestClusterConnection::ensure_template_exists`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if database creation fails or if `setup_fn` returns an error.
    pub fn ensure_template_exists<F>(
        &self,
        name: impl Into<DatabaseName>,
        setup_fn: F,
    ) -> BootstrapResult<()>
    where
        F: FnOnce(&str) -> BootstrapResult<()>,
    {
        self.connection().ensure_template_exists(name, setup_fn)
    }

    /// Creates a temporary database that is dropped when the guard is dropped.
    ///
    /// See [`TestClusterConnection::temporary_database`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the database already exists or the connection fails.
    pub fn temporary_database(
        &self,
        name: impl Into<DatabaseName>,
    ) -> BootstrapResult<TemporaryDatabase> {
        self.connection().temporary_database(name)
    }

    /// Creates a temporary database from a template.
    ///
    /// See [`TestClusterConnection::temporary_database_from_template`] for details.
    ///
    /// # Errors
    ///
    /// Returns an error if the target database already exists, the template does
    /// not exist, or the connection fails.
    pub fn temporary_database_from_template(
        &self,
        name: impl Into<DatabaseName>,
        template: impl Into<DatabaseName>,
    ) -> BootstrapResult<TemporaryDatabase> {
        self.connection()
            .temporary_database_from_template(name, template)
    }
}
