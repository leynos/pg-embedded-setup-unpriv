//! Delegation methods that forward `TestCluster` calls to `TestClusterConnection`.

use super::TestCluster;
use super::temporary_database::TemporaryDatabase;
use crate::error::BootstrapResult;

/// Generates delegation methods on `TestCluster` that forward to `TestClusterConnection`.
///
/// Each invocation generates a method that calls `self.connection().$method(...)`.
macro_rules! delegate_to_connection {
    // Single argument, unit return
    (
        $(#[$meta:meta])*
        fn $name:ident(&self, $arg:ident: $arg_ty:ty) -> BootstrapResult<()>
    ) => {
        $(#[$meta])*
        pub fn $name(&self, $arg: $arg_ty) -> BootstrapResult<()> {
            self.connection().$name($arg)
        }
    };

    // Two arguments, unit return
    (
        $(#[$meta:meta])*
        fn $name:ident(&self, $arg1:ident: $arg1_ty:ty, $arg2:ident: $arg2_ty:ty) -> BootstrapResult<()>
    ) => {
        $(#[$meta])*
        pub fn $name(&self, $arg1: $arg1_ty, $arg2: $arg2_ty) -> BootstrapResult<()> {
            self.connection().$name($arg1, $arg2)
        }
    };

    // Single argument, custom return type
    (
        $(#[$meta:meta])*
        fn $name:ident(&self, $arg:ident: $arg_ty:ty) -> BootstrapResult<$ret:ty>
    ) => {
        $(#[$meta])*
        pub fn $name(&self, $arg: $arg_ty) -> BootstrapResult<$ret> {
            self.connection().$name($arg)
        }
    };

    // Two arguments, custom return type
    (
        $(#[$meta:meta])*
        fn $name:ident(&self, $arg1:ident: $arg1_ty:ty, $arg2:ident: $arg2_ty:ty) -> BootstrapResult<$ret:ty>
    ) => {
        $(#[$meta])*
        pub fn $name(&self, $arg1: $arg1_ty, $arg2: $arg2_ty) -> BootstrapResult<$ret> {
            self.connection().$name($arg1, $arg2)
        }
    };
}

impl TestCluster {
    delegate_to_connection! {
        /// Creates a new database with the given name.
        ///
        /// Delegates to [`crate::TestClusterConnection::create_database`].
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
        /// cluster.create_database("my_test_db")?;
        /// # Ok(())
        /// # }
        /// ```
        fn create_database(&self, name: &str) -> BootstrapResult<()>
    }

    delegate_to_connection! {
        /// Creates a new database by cloning an existing template.
        ///
        /// Delegates to [`crate::TestClusterConnection::create_database_from_template`].
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
        /// cluster.create_database("my_template")?;
        /// // ... run migrations on my_template ...
        /// cluster.create_database_from_template("test_db", "my_template")?;
        /// # Ok(())
        /// # }
        /// ```
        fn create_database_from_template(&self, name: &str, template: &str) -> BootstrapResult<()>
    }

    delegate_to_connection! {
        /// Drops an existing database.
        ///
        /// Delegates to [`crate::TestClusterConnection::drop_database`].
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
        /// cluster.create_database("temp_db")?;
        /// cluster.drop_database("temp_db")?;
        /// # Ok(())
        /// # }
        /// ```
        fn drop_database(&self, name: &str) -> BootstrapResult<()>
    }

    delegate_to_connection! {
        /// Checks whether a database with the given name exists.
        ///
        /// Delegates to [`crate::TestClusterConnection::database_exists`].
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
        /// assert!(cluster.database_exists("postgres")?);
        /// assert!(!cluster.database_exists("nonexistent")?);
        /// # Ok(())
        /// # }
        /// ```
        fn database_exists(&self, name: &str) -> BootstrapResult<bool>
    }

    /// Ensures a template database exists, creating it if necessary.
    ///
    /// Delegates to [`crate::TestClusterConnection::ensure_template_exists`].
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
    /// cluster.ensure_template_exists("my_template", |db_name| {
    ///     // Run migrations on the newly created template database
    ///     Ok(())
    /// })?;
    ///
    /// // Clone the template for each test
    /// cluster.create_database_from_template("test_db_1", "my_template")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn ensure_template_exists<F>(&self, name: &str, setup_fn: F) -> BootstrapResult<()>
    where
        F: FnOnce(&str) -> BootstrapResult<()>,
    {
        self.connection().ensure_template_exists(name, setup_fn)
    }

    delegate_to_connection! {
        /// Creates a temporary database that is dropped when the guard is dropped.
        ///
        /// Delegates to [`crate::TestClusterConnection::temporary_database`].
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
        /// let temp_db = cluster.temporary_database("my_temp_db")?;
        ///
        /// // Database is dropped automatically when temp_db goes out of scope
        /// # Ok(())
        /// # }
        /// ```
        fn temporary_database(&self, name: &str) -> BootstrapResult<TemporaryDatabase>
    }

    delegate_to_connection! {
        /// Creates a temporary database from a template.
        ///
        /// Delegates to [`crate::TestClusterConnection::temporary_database_from_template`].
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
        /// cluster.ensure_template_exists("migrated_template", |_| Ok(()))?;
        ///
        /// let temp_db = cluster.temporary_database_from_template("test_db", "migrated_template")?;
        ///
        /// // Database is dropped automatically when temp_db goes out of scope
        /// # Ok(())
        /// # }
        /// ```
        fn temporary_database_from_template(&self, name: &str, template: &str) -> BootstrapResult<TemporaryDatabase>
    }
}
