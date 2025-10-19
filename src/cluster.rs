//! RAII wrapper that boots an embedded PostgreSQL instance for tests.
//!
//! The cluster starts during [`TestCluster::new`] and shuts down automatically when the
//! value drops out of scope.
//!
//! # Examples
//! ```no_run
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//! let cluster = TestCluster::new()?;
//! let url = cluster.settings().url("my_database");
//! // Perform test database work here.
//! drop(cluster); // PostgreSQL stops automatically.
//! # Ok(())
//! # }
//! ```

use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::{BootstrapError, BootstrapResult};
use crate::{TestBootstrapEnvironment, TestBootstrapSettings};
use color_eyre::eyre::Context;
use postgresql_embedded::{PostgreSQL, Settings};
use tokio::runtime::{Builder, Runtime};

/// Embedded PostgreSQL instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    runtime: Runtime,
    postgres: Option<PostgreSQL>,
    bootstrap: TestBootstrapSettings,
    _env_guard: ScopedEnv,
}

impl TestCluster {
    /// Boots a PostgreSQL instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    pub fn new() -> BootstrapResult<Self> {
        let bootstrap = bootstrap_for_tests()?;
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create Tokio runtime for TestCluster")
            .map_err(BootstrapError::from)?;

        let mut postgres = PostgreSQL::new(bootstrap.settings.clone());
        let env_vars = bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);

        let start = runtime
            .block_on(async { postgres.start().await })
            .context("postgresql_embedded::start() failed")
            .map_err(BootstrapError::from);
        if let Err(err) = start {
            drop(env_guard);
            return Err(err);
        }

        Ok(Self {
            runtime,
            postgres: Some(postgres),
            bootstrap,
            _env_guard: env_guard,
        })
    }

    /// Returns the prepared PostgreSQL settings for the running cluster.
    pub fn settings(&self) -> &Settings {
        &self.bootstrap.settings
    }

    /// Returns the environment required for clients to interact with the cluster.
    pub fn environment(&self) -> &TestBootstrapEnvironment {
        &self.bootstrap.environment
    }

    /// Returns the bootstrap metadata captured when the cluster was started.
    pub fn bootstrap(&self) -> &TestBootstrapSettings {
        &self.bootstrap
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        if let Some(postgres) = self.postgres.take()
            && let Err(err) = self.runtime.block_on(postgres.stop())
        {
            eprintln!("SKIP-TEST-CLUSTER: failed to stop embedded postgres instance: {err}");
        }
        // `env_guard` drops after this block, restoring the environment.
    }
}
