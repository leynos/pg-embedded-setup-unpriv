//! Internal helpers re-exported for integration tests.
//!
//! Besides filesystem convenience wrappers, this module exposes the
//! `RUN_ROOT_OPERATION_HOOK` plumbing so behavioural tests can intercept and
//! inspect privileged worker operations. The `install_run_root_operation_hook`
//! helper registers a closure for the duration of a `HookGuard`, ensuring
//! `TestCluster` calls are observable without leaking state across suites.

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod errors;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod filesystem;
#[cfg(any(
    doc,
    test,
    feature = "cluster-unit-tests",
    feature = "dev-worker",
    feature = "rstest-fixtures"
))]
mod fixtures;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod hook;
#[cfg(feature = "cluster-unit-tests")]
mod logging;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod scoped_env;

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use errors::{bootstrap_error, privilege_error};
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use filesystem::{
    CapabilityTempDir, ambient_dir_and_path, ensure_dir_exists, metadata, set_permissions,
};
#[cfg(any(
    doc,
    test,
    feature = "cluster-unit-tests",
    feature = "dev-worker",
    feature = "rstest-fixtures"
))]
pub use fixtures::{dummy_environment, dummy_settings, test_runtime};
#[cfg(feature = "rstest-fixtures")]
pub use fixtures::{test_cluster, try_test_cluster};
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use hook::{
    HookGuard, RunRootOperationHook, RunRootOperationHookInstallError, drain_hook_install_logs,
    install_run_root_operation_hook, invoke_with_privileges, run_root_operation_hook,
};
#[cfg(feature = "cluster-unit-tests")]
pub use logging::capture_warn_logs;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use scoped_env::scoped_env;
