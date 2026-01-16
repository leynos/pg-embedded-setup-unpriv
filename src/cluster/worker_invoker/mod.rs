//! Dispatches `PostgreSQL` lifecycle operations either in-process or via the privileged worker binary.
use std::future::Future;

use color_eyre::eyre::{Context, eyre};
use tokio::runtime::Runtime;

use crate::error::{BootstrapError, BootstrapResult};
use crate::observability::LOG_TARGET;
use crate::worker_process::{self, WorkerRequest, WorkerRequestArgs};
use crate::{ExecutionMode, ExecutionPrivileges, TestBootstrapSettings};

use super::WorkerOperation;
use tracing::{error, info, info_span};

// ============================================================================
// Shared helper functions
// ============================================================================

/// Creates a tracing span for lifecycle operations.
///
/// Used by both sync and async invokers to maintain consistent observability.
/// The `async_mode` field is only recorded when `true` to maintain backward
/// compatibility with existing sync span format.
fn create_lifecycle_span(
    operation: WorkerOperation,
    bootstrap: &TestBootstrapSettings,
    async_mode: bool,
) -> tracing::Span {
    let span = info_span!(
        target: LOG_TARGET,
        "lifecycle_operation",
        operation = operation.as_str(),
        privileges = ?bootstrap.privileges,
        mode = ?bootstrap.execution_mode,
        async_mode = tracing::field::Empty
    );
    if async_mode {
        span.record("async_mode", true);
    }
    span
}

/// Executes a root operation, handling test hooks, execution mode validation,
/// and platform-specific constraints.
///
/// This is the shared implementation used by both sync (`WorkerInvoker::invoke_as_root`)
/// and async (`invoke_as_root_sync`) code paths.
fn execute_root_operation(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    operation: WorkerOperation,
) -> BootstrapResult<()> {
    #[cfg(any(test, feature = "cluster-unit-tests"))]
    {
        let hook_slot = crate::test_support::run_root_operation_hook()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(hook) = hook_slot {
            return hook(bootstrap, env_vars, operation);
        }
    }

    match bootstrap.execution_mode {
        ExecutionMode::InProcess => Err(BootstrapError::from(eyre!(concat!(
            "ExecutionMode::InProcess is unsafe for root because process-wide ",
            "UID/GID changes race in multi-threaded tests; switch to ",
            "ExecutionMode::Subprocess"
        )))),
        ExecutionMode::Subprocess => spawn_worker_inner(bootstrap, env_vars, operation),
    }
}

/// Spawns the worker subprocess to execute a privileged operation.
///
/// This is the shared implementation for worker spawning, used by both sync and
/// async code paths. Contains platform-specific guards for privilege dropping.
fn spawn_worker_inner(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    operation: WorkerOperation,
) -> BootstrapResult<()> {
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
        let worker = bootstrap.worker_binary.as_ref().ok_or_else(|| {
            BootstrapError::from(eyre!(
                "PG_EMBEDDED_WORKER must be set when using ExecutionMode::Subprocess"
            ))
        })?;

        let args = WorkerRequestArgs {
            worker,
            settings: &bootstrap.settings,
            env_vars,
            operation,
            timeout: operation.timeout(bootstrap),
        };
        let request = WorkerRequest::new(args);
        return worker_process::run(&request);
    }

    #[expect(unreachable_code, reason = "cfg guard ensures all targets handled")]
    Err(BootstrapError::from(eyre!(
        "privilege drop support unexpectedly unavailable"
    )))
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

// ============================================================================
// Synchronous invoker
// ============================================================================

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
        create_lifecycle_span(operation, self.bootstrap, false)
    }

    pub(super) fn invoke_as_root(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        execute_root_operation(self.bootstrap, self.env_vars, operation)
    }
}

/// Async variant of [`WorkerInvoker`] that operates on the caller's runtime.
///
/// Use this invoker when running within an existing async context (e.g., inside
/// `#[tokio::test]`). Unlike `WorkerInvoker`, this does not require an owned
/// runtime reference since it directly `.await`s futures.
#[cfg(feature = "async-api")]
#[derive(Debug)]
pub(crate) struct AsyncInvoker<'a> {
    bootstrap: &'a TestBootstrapSettings,
    env_vars: &'a [(String, Option<String>)],
}

#[cfg(feature = "async-api")]
impl<'a> AsyncInvoker<'a> {
    /// Creates an async invoker bound to bootstrap configuration and environment.
    pub(crate) const fn new(
        bootstrap: &'a TestBootstrapSettings,
        env_vars: &'a [(String, Option<String>)],
    ) -> Self {
        Self {
            bootstrap,
            env_vars,
        }
    }

    /// Executes an operation asynchronously, either in-process or via the worker.
    ///
    /// For unprivileged operations, the future is awaited directly.
    /// For root operations, the synchronous worker is spawned via `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns a [`BootstrapError`] when the operation fails.
    pub(crate) async fn invoke<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        let span = self.lifecycle_span(operation);
        let _entered = span.enter();

        let result = self
            .dispatch_operation_async(operation, in_process_op)
            .await;
        WorkerInvoker::log_outcome(operation, &result);
        result
    }

    async fn dispatch_operation_async<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match self.bootstrap.privileges {
            ExecutionPrivileges::Unprivileged => {
                self.run_unprivileged_async(operation, in_process_op).await
            }
            ExecutionPrivileges::Root => self.run_root_async(operation).await,
        }
    }

    async fn run_unprivileged_async<Fut>(
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
            "running lifecycle operation in-process (async)"
        );
        invoke_unprivileged_async(in_process_op, operation.error_context()).await
    }

    async fn run_root_async(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        info!(
            target: LOG_TARGET,
            operation = operation.as_str(),
            worker = self
                .bootstrap
                .worker_binary
                .as_ref()
                .map(|path| path.as_str()),
            "dispatching lifecycle operation via worker (async)"
        );
        // Worker subprocess spawning is inherently blocking; use spawn_blocking.
        let bootstrap = self.bootstrap.clone();
        let env_vars = self.env_vars.to_vec();
        tokio::task::spawn_blocking(move || {
            execute_root_operation(&bootstrap, &env_vars, operation)
        })
        .await
        .map_err(|err| BootstrapError::from(eyre!("worker task panicked: {err}")))?
    }

    fn lifecycle_span(&self, operation: WorkerOperation) -> tracing::Span {
        create_lifecycle_span(operation, self.bootstrap, true)
    }
}

/// Directly awaits an unprivileged operation's future.
#[cfg(feature = "async-api")]
async fn invoke_unprivileged_async<Fut>(future: Fut, ctx: &'static str) -> BootstrapResult<()>
where
    Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
{
    future.await.context(ctx).map_err(BootstrapError::from)
}

#[cfg(test)]
mod tests;
