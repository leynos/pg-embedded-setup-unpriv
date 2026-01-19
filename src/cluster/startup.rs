//! Startup and lifecycle helpers for `TestCluster`.

use super::{ClusterRuntime, StartupOutcome, TestCluster};
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};
use postgresql_embedded::PostgreSQL;
use tokio::runtime::Runtime;
use tracing::{info, info_span};

use super::{refresh_worker_installation_dir, refresh_worker_port};
use super::runtime::build_runtime;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::worker_operation;

impl TestCluster {
    /// Boots a `PostgreSQL` instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    ///
    /// # Errors
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting the
    /// embedded cluster fails.
    pub fn new() -> crate::error::BootstrapResult<Self> {
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
    ) -> crate::error::BootstrapResult<StartupOutcome> {
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
    ) -> crate::error::BootstrapResult<()> {
        let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        setup_invoker.invoke_as_root(worker_operation::WorkerOperation::Setup)?;
        refresh_worker_installation_dir(bootstrap);
        let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        start_invoker.invoke_as_root(worker_operation::WorkerOperation::Start)?;
        refresh_worker_port(bootstrap)
    }

    fn invoke_lifecycle(
        runtime: &Runtime,
        bootstrap: &mut TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        embedded: &mut PostgreSQL,
    ) -> crate::error::BootstrapResult<()> {
        // Scope ensures the setup invoker releases its borrows before we refresh the settings.
        {
            let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
            invoker.invoke(worker_operation::WorkerOperation::Setup, async {
                embedded.setup().await
            })?;
        }
        refresh_worker_installation_dir(bootstrap);
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        })?;
        refresh_worker_port(bootstrap)
    }
}
