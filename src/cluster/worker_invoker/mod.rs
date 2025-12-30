//! Dispatches `PostgreSQL` lifecycle operations either in-process or via the privileged worker binary.
use std::future::Future;

use color_eyre::eyre::{Context, eyre};
use tokio::runtime::Runtime;

use crate::error::{BootstrapError, BootstrapResult};
use crate::observability::LOG_TARGET;
use crate::worker_process::{self, WorkerRequest};
use crate::{ExecutionMode, ExecutionPrivileges, TestBootstrapSettings};

use super::worker_operation::WorkerOperation;
use tracing::{error, info, info_span};

/// Executes worker operations whilst respecting configured privileges.
#[derive(Debug)]
#[doc(hidden)]
pub struct WorkerInvoker<'a> {
    runtime: &'a Runtime,
    bootstrap: &'a TestBootstrapSettings,
    env_vars: &'a [(String, Option<String>)],
}

impl<'a> WorkerInvoker<'a> {
    /// Creates an invoker bound to a runtime, bootstrap configuration, and
    /// derived environment variables.
    ///
    /// # Examples
    /// ```ignore
    /// use pg_embedded_setup_unpriv::{ExecutionPrivileges, WorkerInvoker};
    /// use pg_embedded_setup_unpriv::test_support::{dummy_settings, test_runtime};
    ///
    /// # fn demo() -> color_eyre::eyre::Result<()> {
    /// let runtime = test_runtime()?;
    /// let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    /// let env = bootstrap.environment.to_env();
    /// let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env);
    /// # let _ = invoker;
    /// # Ok(())
    /// # }
    /// ```
    pub const fn new(
        runtime: &'a Runtime,
        bootstrap: &'a TestBootstrapSettings,
        env_vars: &'a [(String, Option<String>)],
    ) -> Self {
        Self {
            runtime,
            bootstrap,
            env_vars,
        }
    }

    /// Executes an operation either in-process or via the privileged worker,
    /// depending on the configured privilege level.
    ///
    /// # Errors
    ///
    /// Returns a [`BootstrapError`] when the worker invocation fails or when
    /// the in-process operation surfaces an error.
    ///
    /// # Examples
    /// ```ignore
    /// use pg_embedded_setup_unpriv::{ExecutionPrivileges, WorkerInvoker, WorkerOperation};
    /// use pg_embedded_setup_unpriv::test_support::{dummy_settings, test_runtime};
    ///
    /// # fn demo() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let runtime = test_runtime()?;
    /// let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    /// let env = bootstrap.environment.to_env();
    /// let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env);
    /// invoker.invoke(WorkerOperation::Setup, async {
    ///     Ok::<(), postgresql_embedded::Error>(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn invoke<Fut>(&self, operation: WorkerOperation, in_process_op: Fut) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        let span = self.lifecycle_span(operation);
        let _entered = span.enter();

        let result = self.dispatch_operation(operation, in_process_op);
        Self::log_outcome(operation, &result);
        result
    }

    fn dispatch_operation<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match self.bootstrap.privileges {
            ExecutionPrivileges::Unprivileged => self.run_unprivileged(operation, in_process_op),
            ExecutionPrivileges::Root => self.run_root(operation),
        }
    }

    fn run_unprivileged<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        info!(
            target: LOG_TARGET,
            operation = operation.as_str(),
            "running lifecycle operation in-process"
        );
        self.invoke_unprivileged(in_process_op, operation.error_context())
    }

    fn run_root(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        info!(
            target: LOG_TARGET,
            operation = operation.as_str(),
            worker = self
                .bootstrap
                .worker_binary
                .as_ref()
                .map(|path| path.as_str()),
            "dispatching lifecycle operation via worker"
        );
        self.invoke_as_root(operation)
    }

    fn log_outcome(operation: WorkerOperation, result: &BootstrapResult<()>) {
        if let Err(err) = result {
            log_failure(operation, err);
        } else {
            log_success(operation);
        }
    }

    fn invoke_unprivileged<Fut>(&self, future: Fut, ctx: &'static str) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        self.runtime
            .block_on(future)
            .context(ctx)
            .map_err(BootstrapError::from)
    }

    fn lifecycle_span(&self, operation: WorkerOperation) -> tracing::Span {
        info_span!(
            target: LOG_TARGET,
            "lifecycle_operation",
            operation = operation.as_str(),
            privileges = ?self.bootstrap.privileges,
            mode = ?self.bootstrap.execution_mode
        )
    }

    pub(super) fn invoke_as_root(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        #[cfg(any(test, feature = "cluster-unit-tests"))]
        {
            let hook_slot = crate::test_support::run_root_operation_hook()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if let Some(hook) = hook_slot {
                return hook(self.bootstrap, self.env_vars, operation);
            }
        }

        match self.bootstrap.execution_mode {
            ExecutionMode::InProcess => Err(BootstrapError::from(eyre!(concat!(
                "ExecutionMode::InProcess is unsafe for root because process-wide ",
                "UID/GID changes race in multi-threaded tests; switch to ",
                "ExecutionMode::Subprocess"
            )))),
            ExecutionMode::Subprocess => {
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
                    return Err(BootstrapError::from(eyre!(
                        "privilege drop not supported on this target; refusing to run as root: {}",
                        operation.error_context()
                    )));
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
                {
                    return self.spawn_worker(operation);
                }

                #[expect(unreachable_code, reason = "cfg guard ensures all targets handled")]
                Err(BootstrapError::from(eyre!(
                    "privilege drop support unexpectedly unavailable"
                )))
            }
        }
    }

    fn spawn_worker(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        let worker = self.bootstrap.worker_binary.as_ref().ok_or_else(|| {
            BootstrapError::from(eyre!(
                "PG_EMBEDDED_WORKER must be set when using ExecutionMode::Subprocess"
            ))
        })?;

        let request = WorkerRequest::new(
            worker,
            &self.bootstrap.settings,
            self.env_vars,
            operation,
            operation.timeout(self.bootstrap),
        );

        worker_process::run(&request)
    }
}

fn log_failure(operation: WorkerOperation, err: &BootstrapError) {
    error!(
        target: LOG_TARGET,
        operation = operation.as_str(),
        error = %err,
        "lifecycle operation failed"
    );
}

fn log_success(operation: WorkerOperation) {
    info!(
        target: LOG_TARGET,
        operation = operation.as_str(),
        "lifecycle operation completed"
    );
}

#[cfg(test)]
mod tests;
