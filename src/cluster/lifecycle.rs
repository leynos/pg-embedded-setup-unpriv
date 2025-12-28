//! Database lifecycle operations for `TestClusterConnection`.
//!
//! This module provides methods for creating, dropping, and managing databases
//! on a running `PostgreSQL` cluster.

use std::sync::{Mutex, OnceLock};

use color_eyre::eyre::WrapErr;
use dashmap::DashMap;
use tracing::info_span;

use super::connection::{escape_identifier, TestClusterConnection};
use super::temporary_database::TemporaryDatabase;
use crate::error::BootstrapResult;

/// Global per-template locks to prevent concurrent template creation.
///
/// Uses a `DashMap` to allow lock-free reads and concurrent access to
/// different templates while serialising access to the same template.
static TEMPLATE_LOCKS: OnceLock<DashMap<String, Mutex<()>>> = OnceLock::new();

fn template_locks() -> &'static DashMap<String, Mutex<()>> {
    TEMPLATE_LOCKS.get_or_init(DashMap::new)
}

impl TestClusterConnection {
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
}
