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
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tokio::time;

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use crate::privileges::drop_process_privileges;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use nix::unistd::User;

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

        match bootstrap.privileges {
            crate::ExecutionPrivileges::Unprivileged => {
                let setup = runtime
                    .block_on(async { postgres.setup().await })
                    .context("postgresql_embedded::setup() failed")
                    .map_err(BootstrapError::from);
                if let Err(err) = setup {
                    drop(env_guard);
                    return Err(err);
                }
            }
            crate::ExecutionPrivileges::Root => {
                #[cfg(all(
                    unix,
                    any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "freebsd",
                        target_os = "openbsd",
                        target_os = "dragonfly",
                    ),
                ))]
                {
                    let nobody_user = User::from_name("nobody")
                        .context("failed to resolve user 'nobody'")
                        .map_err(BootstrapError::from)?
                        .ok_or_else(|| color_eyre::eyre::eyre!("user 'nobody' not found"))
                        .map_err(BootstrapError::from)?;
                    let guard =
                        drop_process_privileges(&nobody_user).map_err(BootstrapError::from)?;
                    let setup = runtime
                        .block_on(async { postgres.setup().await })
                        .context("postgresql_embedded::setup() failed")
                        .map_err(BootstrapError::from);
                    drop(guard);
                    if let Err(err) = setup {
                        drop(env_guard);
                        return Err(err);
                    }
                }
                #[cfg(not(all(
                    unix,
                    any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "freebsd",
                        target_os = "openbsd",
                        target_os = "dragonfly",
                    ),
                )))]
                {
                    let setup = runtime
                        .block_on(async { postgres.setup().await })
                        .context("postgresql_embedded::setup() failed")
                        .map_err(BootstrapError::from);
                    if let Err(err) = setup {
                        drop(env_guard);
                        return Err(err);
                    }
                }
            }
        }

        let start = match bootstrap.privileges {
            crate::ExecutionPrivileges::Unprivileged => runtime
                .block_on(async { postgres.start().await })
                .context("postgresql_embedded::start() failed")
                .map_err(BootstrapError::from),
            crate::ExecutionPrivileges::Root => {
                #[cfg(all(
                    unix,
                    any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "freebsd",
                        target_os = "openbsd",
                        target_os = "dragonfly",
                    ),
                ))]
                {
                    let nobody_user = User::from_name("nobody")
                        .context("failed to resolve user 'nobody'")
                        .map_err(BootstrapError::from)?
                        .ok_or_else(|| color_eyre::eyre::eyre!("user 'nobody' not found"))
                        .map_err(BootstrapError::from)?;
                    let guard =
                        drop_process_privileges(&nobody_user).map_err(BootstrapError::from)?;
                    let start = runtime
                        .block_on(async { postgres.start().await })
                        .context("postgresql_embedded::start() failed")
                        .map_err(BootstrapError::from);
                    drop(guard);
                    start
                }
                #[cfg(not(all(
                    unix,
                    any(
                        target_os = "linux",
                        target_os = "android",
                        target_os = "freebsd",
                        target_os = "openbsd",
                        target_os = "dragonfly",
                    ),
                )))]
                {
                    runtime
                        .block_on(async { postgres.start().await })
                        .context("postgresql_embedded::start() failed")
                        .map_err(BootstrapError::from)
                }
            }
        };
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
        if let Some(postgres) = self.postgres.take() {
            let outcome = self
                .runtime
                .block_on(async { time::timeout(Duration::from_secs(15), postgres.stop()).await });
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    eprintln!(
                        "SKIP-TEST-CLUSTER: failed to stop embedded postgres instance: {err}"
                    );
                }
                Err(_) => {
                    eprintln!(
                        "SKIP-TEST-CLUSTER: stop() timed out after 15s; proceeding with drop"
                    );
                }
            }
        }
        // `env_guard` drops after this block, restoring the environment.
    }
}
