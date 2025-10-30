//! Internal helpers re-exported for integration tests.
//!
//! Besides filesystem convenience wrappers, this module exposes the
//! `RUN_ROOT_OPERATION_HOOK` plumbing so behavioural tests can intercept and
//! inspect privileged worker operations. The `install_run_root_operation_hook`
//! helper registers a closure for the duration of a `HookGuard`, ensuring
//! `TestCluster` calls are observable without leaking state across suites.

mod errors;
mod fs;
#[cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod hook;
#[cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod scoped_env;

pub use errors::{bootstrap_error, privilege_error};
pub use fs::{
    CapabilityTempDir, ambient_dir_and_path, ensure_dir_exists, metadata, set_permissions,
};
#[cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use hook::{
    HookGuard, RunRootOperationHook, RunRootOperationHookInstallError, drain_hook_install_logs,
    install_run_root_operation_hook, invoke_with_privileges, run_root_operation_hook,
};
#[cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use scoped_env::scoped_env;
