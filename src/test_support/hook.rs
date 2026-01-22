//! Hook infrastructure that intercepts privileged worker operations during
//! tests to assert behaviour and control cluster bootstrapping.

#[allow(unused_imports)]
use std::future::Future;
use std::mem;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use crate::cluster::{WorkerInvoker, WorkerOperation};
use crate::error::BootstrapResult;
use crate::TestBootstrapSettings;
use tracing::debug_span;

#[doc(hidden)]
/// Signature for intercepting privileged worker operations triggered by `TestCluster`.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::RunRootOperationHook;
///
/// fn installs_hook(hook: RunRootOperationHook) {
///     let _ = hook;
/// }
/// ```
pub type RunRootOperationHook = Arc<
    dyn Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[doc(hidden)]
pub enum RunRootOperationHookInstallError {
    #[error("run_root_operation_hook already installed")]
    AlreadyInstalled,
}

static RUN_ROOT_OPERATION_HOOK: OnceLock<Mutex<Option<RunRootOperationHook>>> = OnceLock::new();
static RUN_ROOT_OPERATION_HOOK_LOGS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

#[doc(hidden)]
#[must_use]
pub fn drain_hook_install_logs() -> Vec<String> {
    let mut guard = RUN_ROOT_OPERATION_HOOK_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    mem::take(&mut *guard)
}

#[doc(hidden)]
/// Retrieves the optional run-root-operation hook for inspection or mutation.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::{
///     install_run_root_operation_hook,
///     run_root_operation_hook,
/// };
///
/// let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
///     .expect("hook should install");
/// assert!(
///     run_root_operation_hook()
///         .lock()
///         .expect("hook mutex poisoned")
///         .is_some()
/// );
/// drop(guard);
/// ```
pub fn run_root_operation_hook() -> &'static Mutex<Option<RunRootOperationHook>> {
    RUN_ROOT_OPERATION_HOOK.get_or_init(|| Mutex::new(None))
}

/// Guard that removes the installed run-root-operation hook when dropped.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::install_run_root_operation_hook;
///
/// let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
///     .expect("hook should install");
/// drop(guard); // hook removed automatically
/// ```
pub struct HookGuard;

struct ThreadScope {
    label: String,
    span: tracing::Span,
}

fn capture_thread_scope() -> ThreadScope {
    let current_thread = thread::current();
    let thread_name = current_thread.name().unwrap_or("unnamed").to_owned();
    let thread_id = format!("{:?}", current_thread.id());
    let label = format!("{thread_name} ({thread_id})");
    let span = debug_span!(
        "install_run_root_operation_hook",
        thread.name = %thread_name,
        thread.id = %thread_id
    );
    ThreadScope { label, span }
}

fn log_duplicate_install(thread_label: &str) {
    let message = format!("run_root_operation_hook already installed by thread {thread_label}");
    record_duplicate_install(&message);
}

fn record_duplicate_install(message: &str) {
    RUN_ROOT_OPERATION_HOOK_LOGS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(message.to_owned());
    #[cfg(test)]
    {
        log_duplicate_hook_to_stderr(message);
        tracing::warn!("{message}");
    }
}

#[cfg(test)]
pub(crate) fn log_duplicate_hook_to_stderr(message: &str) {
    use std::io::Write as _;

    let mut stderr = std::io::stderr();
    if let Err(err) = writeln!(stderr, "{message}") {
        tracing::warn!(?err, "failed to log duplicate hook install");
    }
}

fn register_hook<F>(hook: F, thread_label: &str) -> Result<(), RunRootOperationHookInstallError>
where
    F: Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync
        + 'static,
{
    let slot = run_root_operation_hook();
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_some() {
        log_duplicate_install(thread_label);
        return Err(RunRootOperationHookInstallError::AlreadyInstalled);
    }
    *guard = Some(Arc::new(hook));
    Ok(())
}

/// Installs a hook that observes privileged worker operations triggered by `TestCluster`.
///
/// The hook remains active until the returned [`HookGuard`] is dropped.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::install_run_root_operation_hook;
///
/// let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
///     .expect("hook should install");
/// drop(guard);
/// ```
pub fn install_run_root_operation_hook<F>(
    hook: F,
) -> Result<HookGuard, RunRootOperationHookInstallError>
where
    F: Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync
        + 'static,
{
    let scope = capture_thread_scope();
    let _entered = scope.span.enter();
    register_hook(hook, &scope.label)?;
    Ok(HookGuard)
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        let slot = run_root_operation_hook();
        let mut guard = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.take();
    }
}

#[doc(hidden)]
pub fn invoke_with_privileges<Fut>(
    invoker: &WorkerInvoker<'_>,
    operation: WorkerOperation,
    in_process_op: Fut,
) -> BootstrapResult<()>
where
    Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
{
    invoker.invoke(operation, in_process_op)
}
