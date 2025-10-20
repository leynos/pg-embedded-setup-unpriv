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
use crate::worker::WorkerPayload;
use crate::{TestBootstrapEnvironment, TestBootstrapSettings};
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::{PostgreSQL, Settings};
use serde_json::to_writer;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::process::Command;
use std::time::Duration;
use tempfile::NamedTempFile;
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
use nix::unistd::User;
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
use std::os::unix::process::CommandExt;

/// Embedded PostgreSQL instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    runtime: Runtime,
    postgres: Option<PostgreSQL>,
    bootstrap: TestBootstrapSettings,
    managed_via_worker: bool,
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

        let postgres = PostgreSQL::new(bootstrap.settings.clone());
        let env_vars = bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);
        let privileges = bootstrap.privileges;

        let setup_bootstrap = bootstrap.clone();
        let setup_env = env_vars.clone();
        let postgres = match Self::with_privileges(
            &runtime,
            privileges,
            move || {
                let bootstrap = setup_bootstrap.clone();
                let env_vars = setup_env.clone();
                let mut postgres = postgres;
                Box::pin(async move {
                    match privileges {
                        crate::ExecutionPrivileges::Unprivileged => {
                            postgres
                                .setup()
                                .await
                                .context("postgresql_embedded::setup() failed")
                                .map_err(BootstrapError::from)?;
                            Ok(postgres)
                        }
                        crate::ExecutionPrivileges::Root => {
                            Self::run_root_operation(
                                &bootstrap,
                                &env_vars,
                                WorkerOperation::Setup,
                            )?;
                            Ok(postgres)
                        }
                    }
                })
            },
            "postgresql_embedded::setup() failed",
        ) {
            Ok(postgres) => postgres,
            Err(err) => {
                drop(env_guard);
                return Err(err);
            }
        };

        let start_bootstrap = bootstrap.clone();
        let start_env = env_vars.clone();
        let postgres = match Self::with_privileges(
            &runtime,
            privileges,
            move || {
                let bootstrap = start_bootstrap.clone();
                let env_vars = start_env.clone();
                let mut postgres = postgres;
                Box::pin(async move {
                    match privileges {
                        crate::ExecutionPrivileges::Unprivileged => {
                            postgres
                                .start()
                                .await
                                .context("postgresql_embedded::start() failed")
                                .map_err(BootstrapError::from)?;
                            Ok(postgres)
                        }
                        crate::ExecutionPrivileges::Root => {
                            Self::run_root_operation(
                                &bootstrap,
                                &env_vars,
                                WorkerOperation::Start,
                            )?;
                            Ok(postgres)
                        }
                    }
                })
            },
            "postgresql_embedded::start() failed",
        ) {
            Ok(postgres) => postgres,
            Err(err) => {
                drop(env_guard);
                return Err(err);
            }
        };

        let managed_via_worker = matches!(privileges, crate::ExecutionPrivileges::Root);
        let postgres = if managed_via_worker {
            None
        } else {
            Some(postgres)
        };

        Ok(Self {
            runtime,
            postgres,
            bootstrap,
            managed_via_worker,
            _env_guard: env_guard,
        })
    }

    fn with_privileges<R, F>(
        runtime: &Runtime,
        mode: crate::ExecutionPrivileges,
        operation: F,
        ctx: &'static str,
    ) -> BootstrapResult<R>
    where
        F: FnOnce() -> Pin<Box<dyn Future<Output = BootstrapResult<R>> + Send>>,
    {
        match mode {
            crate::ExecutionPrivileges::Unprivileged => runtime.block_on(operation()),
            crate::ExecutionPrivileges::Root => {
                Self::with_dropped_privileges(runtime, operation, ctx)
            }
        }
    }

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
    fn with_dropped_privileges<R, F>(
        runtime: &Runtime,
        operation: F,
        _ctx: &'static str,
    ) -> BootstrapResult<R>
    where
        F: FnOnce() -> Pin<Box<dyn Future<Output = BootstrapResult<R>> + Send>>,
    {
        runtime.block_on(operation())
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
    fn with_dropped_privileges<R, F>(
        runtime: &Runtime,
        operation: F,
        _ctx: &'static str,
    ) -> BootstrapResult<R>
    where
        F: FnOnce() -> Pin<Box<dyn Future<Output = BootstrapResult<R>> + Send>>,
    {
        runtime.block_on(operation())
    }

    fn run_root_operation(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
    ) -> BootstrapResult<()> {
        match bootstrap.execution_mode {
            crate::ExecutionMode::InProcess => Err(BootstrapError::from(eyre!(
                "ExecutionMode::InProcess cannot be used when running as root"
            ))),
            crate::ExecutionMode::Subprocess => Self::spawn_worker(bootstrap, env_vars, operation),
        }
    }

    fn spawn_worker(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
    ) -> BootstrapResult<()> {
        let worker = bootstrap.worker_binary.as_ref().ok_or_else(|| {
            BootstrapError::from(eyre!(
                "PG_EMBEDDED_WORKER must be set when using ExecutionMode::Subprocess"
            ))
        })?;

        let payload = WorkerPayload::new(&bootstrap.settings, env_vars.to_vec())?;
        let mut file = NamedTempFile::new()
            .context("failed to create worker payload file")
            .map_err(BootstrapError::from)?;
        to_writer(&mut file, &payload)
            .context("failed to serialise worker payload")
            .map_err(BootstrapError::from)?;
        file.flush()
            .context("failed to flush worker payload")
            .map_err(BootstrapError::from)?;
        let temp_path = file.into_temp_path();
        let path_buf = temp_path.to_path_buf();

        let mut command = Command::new(worker.as_std_path());
        command.arg(operation.as_str());
        command.arg(&path_buf);

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
            let user = User::from_name("nobody")
                .context("failed to resolve user 'nobody'")
                .map_err(BootstrapError::from)?
                .ok_or_else(|| BootstrapError::from(eyre!("user 'nobody' not found")))?;
            command.uid(user.uid.as_raw());
            command.gid(user.gid.as_raw());
        }

        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        let output = command
            .output()
            .context("failed to execute pg_worker")
            .map_err(BootstrapError::from)?;

        temp_path
            .close()
            .context("failed to clean up worker payload file")
            .map_err(BootstrapError::from)?;

        if output.status.success() {
            Ok(())
        } else {
            Err(BootstrapError::from(eyre!(
                "{}\nstdout: {}\nstderr: {}",
                operation.error_context(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
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

#[derive(Clone, Copy)]
enum WorkerOperation {
    Setup,
    Start,
    Stop,
}

impl WorkerOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }

    fn error_context(self) -> &'static str {
        match self {
            Self::Setup => "postgresql_embedded::setup() failed",
            Self::Start => "postgresql_embedded::start() failed",
            Self::Stop => "postgresql_embedded::stop() failed",
        }
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        if self.managed_via_worker {
            let env_vars = self.bootstrap.environment.to_env();
            if let Err(err) =
                Self::run_root_operation(&self.bootstrap, &env_vars, WorkerOperation::Stop)
            {
                eprintln!("SKIP-TEST-CLUSTER: failed to stop embedded postgres instance: {err}");
            }
            return;
        }

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
