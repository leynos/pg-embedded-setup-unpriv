//! Internal helpers re-exported for integration tests.
//!
//! Besides filesystem convenience wrappers, this module exposes the
//! `RUN_ROOT_OPERATION_HOOK` plumbing so behavioural tests can intercept and
//! inspect privileged worker operations. The `install_run_root_operation_hook`
//! helper registers a closure for the duration of a `HookGuard`, ensuring
//! `TestCluster` calls are observable without leaking state across suites.

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod errors;
mod filesystem;
mod fixtures;
mod hash;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod hook;
#[cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))]
mod logging;
mod scoped_env;

#[cfg(test)]
mod worker_env;

#[cfg(doc)]
mod fixtures_docs;

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use errors::{bootstrap_error, privilege_error};
pub use filesystem::ambient_dir_and_path;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use filesystem::{ensure_dir_exists, metadata, set_permissions, CapabilityTempDir};
pub use fixtures::{
    dummy_environment, dummy_settings, ensure_worker_env, shared_cluster, test_runtime,
};
#[cfg(not(doc))]
pub use fixtures::{shared_test_cluster, test_cluster};
#[cfg(doc)]
pub use fixtures_docs::{shared_test_cluster, test_cluster};
pub use hash::hash_directory;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use hook::{
    drain_hook_install_logs, install_run_root_operation_hook, invoke_with_privileges,
    run_root_operation_hook, HookGuard, RunRootOperationHook, RunRootOperationHookInstallError,
};
#[cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use logging::{
    capture_debug_logs, capture_info_logs, capture_info_logs_with_spans, capture_warn_logs,
};
pub use scoped_env::scoped_env;
pub use worker_env::worker_binary_for_tests;
