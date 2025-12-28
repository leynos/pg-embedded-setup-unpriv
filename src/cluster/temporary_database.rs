//! RAII guard for automatic database cleanup.
//!
//! `TemporaryDatabase` drops its associated database when the guard goes out of
//! scope, mirroring the `TestCluster` lifecycle semantics.

use color_eyre::eyre::WrapErr;
use postgres::{Client, NoTls};
use tracing::info_span;

use super::connection::escape_identifier;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;

/// RAII guard that drops a database when it goes out of scope.
///
/// The guard stores the database name and connection URL rather than borrowing
/// a connection, avoiding lifetime issues and allowing reconnection in `Drop`.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = TestCluster::new()?;
///
/// // Create a temporary database that is dropped when the guard is dropped
/// let temp_db = cluster.connection().temporary_database("my_temp_db")?;
///
/// // Use the database
/// let url = temp_db.url();
/// // ... run queries ...
///
/// // Database is dropped automatically when temp_db goes out of scope
/// drop(temp_db);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct TemporaryDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TemporaryDatabase {
    /// Creates a new `TemporaryDatabase` guard.
    ///
    /// This constructor is intended for internal use. Prefer using
    /// [`TestClusterConnection::temporary_database`] or
    /// [`TestClusterConnection::temporary_database_from_template`].
    pub(crate) const fn new(name: String, admin_url: String, database_url: String) -> Self {
        Self {
            name,
            admin_url,
            database_url,
        }
    }

    /// Returns the database name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the connection URL for this database.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.database_url
    }

    /// Drops the database, failing if connections exist.
    ///
    /// This mirrors `PostgreSQL`'s native behaviour where `DROP DATABASE` fails
    /// if there are active connections to the database.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The database has active connections
    /// - The database does not exist
    /// - The connection to the admin database fails
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
    /// // Explicitly drop (consumes the guard)
    /// temp_db.drop_database()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn drop_database(self) -> BootstrapResult<()> {
        self.try_drop()
    }

    /// Drops the database, terminating any active connections first.
    ///
    /// This is useful when you need to ensure the database is dropped even if
    /// there are lingering connections (e.g., from connection pools that
    /// haven't been drained).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The database does not exist
    /// - The connection to the admin database fails
    /// - Terminating connections fails
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
    /// // Force drop even if connections exist
    /// temp_db.force_drop()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn force_drop(self) -> BootstrapResult<()> {
        let _span = info_span!("force_drop_database", db = %self.name).entered();
        let mut client = Client::connect(&self.admin_url, NoTls)
            .wrap_err("failed to connect to admin database")
            .map_err(crate::error::BootstrapError::from)?;

        // Terminate active connections using parameterized query
        client
            .execute(
                "SELECT pg_terminate_backend(pid) \
                 FROM pg_stat_activity \
                 WHERE datname = $1 AND pid <> pg_backend_pid()",
                &[&self.name],
            )
            .wrap_err(format!(
                "failed to terminate connections to database '{}'",
                self.name
            ))
            .map_err(crate::error::BootstrapError::from)?;

        // Drop the database with escaped identifier
        let escaped = escape_identifier(&self.name);
        let drop_sql = format!("DROP DATABASE \"{escaped}\"");
        client
            .batch_execute(&drop_sql)
            .wrap_err(format!("failed to drop database '{}'", self.name))
            .map_err(crate::error::BootstrapError::from)?;

        Ok(())
    }

    /// Attempts to drop the database without consuming self.
    ///
    /// Used by the `Drop` implementation for best-effort cleanup.
    fn try_drop(&self) -> BootstrapResult<()> {
        let _span = info_span!("drop_database", db = %self.name).entered();
        let mut client = Client::connect(&self.admin_url, NoTls)
            .wrap_err("failed to connect to admin database")
            .map_err(crate::error::BootstrapError::from)?;

        let escaped = escape_identifier(&self.name);
        let sql = format!("DROP DATABASE \"{escaped}\"");
        client
            .batch_execute(&sql)
            .wrap_err(format!("failed to drop database '{}'", self.name))
            .map_err(crate::error::BootstrapError::from)?;

        Ok(())
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        if let Err(e) = self.try_drop() {
            tracing::warn!(
                target: LOG_TARGET,
                db = %self.name,
                error = ?e,
                "failed to drop temporary database"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_database_accessors() {
        let temp = TemporaryDatabase::new(
            "test_db".to_owned(),
            "postgresql://user:pass@localhost:5432/postgres".to_owned(),
            "postgresql://user:pass@localhost:5432/test_db".to_owned(),
        );

        assert_eq!(temp.name(), "test_db");
        assert!(temp.url().contains("test_db"));
    }
}
