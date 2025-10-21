use std::sync::{Arc, Mutex, OnceLock};

use super::{BootstrapResult, TestBootstrapSettings, WorkerOperation};

pub(super) type RunRootOperationHook = Arc<
    dyn Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync,
>;

static RUN_ROOT_OPERATION_HOOK: OnceLock<Mutex<Option<RunRootOperationHook>>> = OnceLock::new();

pub(super) fn run_root_operation_hook() -> &'static Mutex<Option<RunRootOperationHook>> {
    RUN_ROOT_OPERATION_HOOK.get_or_init(|| Mutex::new(None))
}

pub(super) struct HookGuard;

pub(super) fn install_run_root_operation_hook<F>(hook: F) -> HookGuard
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
    {
        let mut guard = slot.lock().expect("run_root_operation_hook lock poisoned");
        assert!(guard.is_none(), "run_root_operation_hook already installed");
        *guard = Some(Arc::new(hook));
    }
    HookGuard
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        let slot = run_root_operation_hook();
        let mut guard = slot.lock().expect("run_root_operation_hook lock poisoned");
        guard.take();
    }
}
