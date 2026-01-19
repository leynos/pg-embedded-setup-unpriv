//! Synchronous lifecycle management for embedded clusters.

use postgresql_embedded::{PostgreSQL, Settings};
use tokio::runtime::Runtime;
use tracing::{info, info_span};

use super::connection::TestClusterConnection;
use super::installation::refresh_worker_installation_dir;
use super::port_refresh::refresh_worker_port;
use super::runtime::build_runtime;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::{ClusterRuntime, StartupOutcome, TestCluster};
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};

impl TestCluster {
    /// Boots a `PostgreSQL` instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    ///
    /// # Errors
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting the
    /// embedded cluster fails.
    pub fn new() -> BootstrapResult<Self> {
        let span = info_span!(target: LOG_TARGET, "test_cluster");
        let (runtime, env_vars, env_guard, outcome) = {
            let _entered = span.enter();
            let initial_bootstrap = bootstrap_for_tests()?;
            let runtime = build_runtime()?;
            let env_vars = initial_bootstrap.environment.to_env();
            let env_guard = ScopedEnv::apply(&env_vars);
            let outcome = Self::start_postgres(&runtime, initial_bootstrap, &env_vars)?;
            (runtime, env_vars, env_guard, outcome)
        };

        Ok(Self {
            runtime: ClusterRuntime::Sync(runtime),
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        })
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "privilege-aware lifecycle setup requires explicit branching for observability"
    )]
    fn start_postgres(
        runtime: &Runtime,
        mut bootstrap: TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<StartupOutcome> {
        let privileges = bootstrap.privileges;
        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            mode = ?bootstrap.execution_mode,
            "starting embedded postgres lifecycle"
        );

        let (is_managed_via_worker, postgres) = if privileges == ExecutionPrivileges::Root {
            Self::invoke_lifecycle_root(runtime, &mut bootstrap, env_vars)?;
            (true, None)
        } else {
            let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
            Self::invoke_lifecycle(runtime, &mut bootstrap, env_vars, &mut embedded)?;
            (
                false,
                Self::prepare_postgres_handle(false, &mut bootstrap, embedded),
            )
        };

        info!(
            target: LOG_TARGET,
            privileges = ?privileges,
            worker_managed = is_managed_via_worker,
            "embedded postgres started"
        );
        Ok(StartupOutcome {
            bootstrap,
            postgres,
            is_managed_via_worker,
        })
    }

    pub(super) fn prepare_postgres_handle(
        is_managed_via_worker: bool,
        bootstrap: &mut TestBootstrapSettings,
        embedded: PostgreSQL,
    ) -> Option<PostgreSQL> {
        if is_managed_via_worker {
            None
        } else {
            bootstrap.settings = embedded.settings().clone();
            Some(embedded)
        }
    }

    fn invoke_lifecycle_root(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<()> {
        let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        setup_invoker.invoke_as_root(super::worker_operation::WorkerOperation::Setup)?;
        refresh_worker_installation_dir(bootstrap);
        let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        start_invoker.invoke_as_root(super::worker_operation::WorkerOperation::Start)?;
        refresh_worker_port(bootstrap)
    }

    fn invoke_lifecycle(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> BootstrapResult<()> {
        // Scope ensures the setup invoker releases its borrows before we refresh the settings.
        {
            let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
            invoker.invoke(super::worker_operation::WorkerOperation::Setup, async {
                embedded.setup().await
            })?;
        }
        refresh_worker_installation_dir(bootstrap);
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoker.invoke(super::worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        })?;
        refresh_worker_port(bootstrap)
    }

    /// Extends the cluster lifetime to cover additional scoped environment guards.
    ///
    /// Primarily used by fixtures that need to ensure `PG_EMBEDDED_WORKER` remains set for the
    /// duration of the cluster lifetime.
    #[doc(hidden)]
    #[must_use]
    pub fn with_worker_guard(mut self, worker_guard: Option<ScopedEnv>) -> Self {
        self.worker_guard = worker_guard;
        self
    }

    /// Returns the prepared `PostgreSQL` settings for the running cluster.
    pub const fn settings(&self) -> &Settings {
        &self.bootstrap.settings
    }

    /// Returns the environment required for clients to interact with the cluster.
    pub const fn environment(&self) -> &TestBootstrapEnvironment {
        &self.bootstrap.environment
    }

    /// Returns the bootstrap metadata captured when the cluster was started.
    pub const fn bootstrap(&self) -> &TestBootstrapSettings {
        &self.bootstrap
    }

    /// Returns helper methods for constructing connection artefacts.
    ///
    /// # Examples
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let cluster = TestCluster::new()?;
    /// let metadata = cluster.connection().metadata();
    /// println!(
    ///     "postgresql://{}:***@{}:{}/postgres",
    ///     metadata.superuser(),
    ///     metadata.host(),
    ///     metadata.port(),
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn connection(&self) -> TestClusterConnection {
        TestClusterConnection::new(&self.bootstrap)
    }

    pub(super) fn stop_context(settings: &Settings) -> String {
        let data_dir = settings.data_dir.display();
        let version = settings.version.to_string();
        format!("version {version}, data_dir {data_dir}")
    }
}
