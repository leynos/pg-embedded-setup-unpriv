#[cfg(feature = "cluster-unit-tests")]
fn main() {}

#[cfg(not(feature = "cluster-unit-tests"))]
use super::cluster_test_utils::install_run_root_operation_hook;
#[cfg(not(feature = "cluster-unit-tests"))]
use super::*;
#[cfg(not(feature = "cluster-unit-tests"))]
use camino::Utf8PathBuf;
#[cfg(not(feature = "cluster-unit-tests"))]
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[cfg(not(feature = "cluster-unit-tests"))]
use std::time::Duration;

#[cfg(not(feature = "cluster-unit-tests"))]
fn test_runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create test runtime")
}

#[cfg(not(feature = "cluster-unit-tests"))]
fn dummy_environment() -> TestBootstrapEnvironment {
    TestBootstrapEnvironment {
        home: Utf8PathBuf::from("/tmp/pg-home"),
        xdg_cache_home: Utf8PathBuf::from("/tmp/pg-cache"),
        xdg_runtime_dir: Utf8PathBuf::from("/tmp/pg-run"),
        pgpass_file: Utf8PathBuf::from("/tmp/.pgpass"),
        tz_dir: Some(Utf8PathBuf::from("/usr/share/zoneinfo")),
        timezone: "UTC".into(),
    }
}

#[cfg(not(feature = "cluster-unit-tests"))]
fn dummy_settings(privileges: crate::ExecutionPrivileges) -> TestBootstrapSettings {
    TestBootstrapSettings {
        privileges,
        execution_mode: crate::ExecutionMode::Subprocess,
        settings: Settings::default(),
        environment: dummy_environment(),
        worker_binary: None,
        shutdown_timeout: Duration::from_secs(15),
    }
}

#[cfg(not(feature = "cluster-unit-tests"))]
#[test]
fn unprivileged_operations_run_in_process() {
    let runtime = test_runtime();
    let bootstrap = dummy_settings(crate::ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let setup_calls = AtomicUsize::new(0);

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let call_counter = &setup_calls;
        TestCluster::with_privileges(
            &runtime,
            crate::ExecutionPrivileges::Unprivileged,
            &bootstrap,
            &env_vars,
            operation,
            async move {
                call_counter.fetch_add(1, Ordering::SeqCst);
                Ok::<(), postgresql_embedded::Error>(())
            },
        )
        .expect("in-process operation should succeed");
    }

    assert_eq!(setup_calls.load(Ordering::SeqCst), 2);
}

#[cfg(not(feature = "cluster-unit-tests"))]
#[test]
fn root_operations_delegate_to_worker() {
    let runtime = test_runtime();
    let bootstrap = dummy_settings(crate::ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let in_process_invoked = Arc::new(AtomicBool::new(false));

    let hook_calls = Arc::clone(&worker_calls);
    let _guard = install_run_root_operation_hook(move |_, _, _| {
        hook_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let flag = Arc::clone(&in_process_invoked);
        TestCluster::with_privileges(
            &runtime,
            crate::ExecutionPrivileges::Root,
            &bootstrap,
            &env_vars,
            operation,
            async move {
                flag.store(true, Ordering::SeqCst);
                Ok::<(), postgresql_embedded::Error>(())
            },
        )
        .expect("worker operation should succeed");
    }

    assert_eq!(worker_calls.load(Ordering::SeqCst), 2);
    assert!(!in_process_invoked.load(Ordering::SeqCst));
}
