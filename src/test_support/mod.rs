//! Internal helpers re-exported for integration tests.
//!
//! Besides filesystem convenience wrappers, this module exposes `RUN_ROOT_OPERATION_HOOK`
//! plumbing so behavioural tests can intercept and inspect privileged worker
//! operations. The `install_run_root_operation_hook` helper registers a closure
//! for duration of a `HookGuard`, ensuring `TestCluster` calls are
//! observable without leaking state across suites.

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use errors::{bootstrap_error, privilege_error};

#[cfg(not(doc))]
pub use filesystem::ambient_dir_and_path;
#[cfg(not(doc))]
pub use filesystem::{CapabilityTempDir, ensure_dir_exists, metadata, set_permissions};
#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use fixtures::{
    dummy_environment, dummy_settings, ensure_worker_env, shared_cluster, test_runtime,
};
#[cfg(not(doc))]
pub use fixtures_docs::{shared_test_cluster, test_cluster};
#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use hash::hash_directory;

#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use hook::{
    HookGuard, RunRootOperationHook, RunRootOperationHookInstallError, drain_hook_install_logs,
    install_run_root_operation_hook, invoke_with_privileges, run_root_operation_hook,
};
#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use logging::{
    capture_debug_logs, capture_info_logs, capture_info_logs_with_spans, capture_warn_logs,
};
#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use scoped_env::scoped_env;
#[allow(unused_imports)]
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use worker_env::worker_binary_for_tests;
