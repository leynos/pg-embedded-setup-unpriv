#![cfg(all(unix, feature = "cluster-unit-tests"))]
//! Unit tests covering TestCluster privilege dispatch behaviour.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use camino::Utf8PathBuf;
use pg_embedded_setup_unpriv::test_support::{
    RunRootOperationHookInstallError, install_run_root_operation_hook, invoke_with_privileges,
};
use pg_embedded_setup_unpriv::{
    ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    WorkerOperation,
};
use postgresql_embedded::Settings;
use tokio::runtime::{Builder, Runtime};

#[cfg(feature = "privileged-tests")]
use nix::unistd::geteuid;

fn test_runtime() -> Runtime {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create test runtime")
}

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

fn dummy_settings(privileges: ExecutionPrivileges) -> TestBootstrapSettings {
    TestBootstrapSettings {
        privileges,
        execution_mode: match privileges {
            ExecutionPrivileges::Unprivileged => ExecutionMode::InProcess,
            ExecutionPrivileges::Root => ExecutionMode::Subprocess,
        },
        settings: Settings::default(),
        environment: dummy_environment(),
        worker_binary: None,
        setup_timeout: Duration::from_secs(180),
        start_timeout: Duration::from_secs(60),
        shutdown_timeout: Duration::from_secs(15),
    }
}

#[test]
fn unprivileged_operations_run_in_process() {
    let runtime = test_runtime();
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let setup_calls = AtomicUsize::new(0);

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let call_counter = &setup_calls;
        invoke_with_privileges(
            &runtime,
            ExecutionPrivileges::Unprivileged,
            &bootstrap,
            &env_vars,
            operation,
            async move {
                call_counter.fetch_add(1, Ordering::Relaxed);
                Ok::<(), postgresql_embedded::Error>(())
            },
        )
        .expect("in-process operation should succeed");
    }

    assert_eq!(setup_calls.load(Ordering::Relaxed), 2);
}

#[test]
fn root_operations_delegate_to_worker() {
    let runtime = test_runtime();
    let bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let in_process_invoked = Arc::new(AtomicBool::new(false));

    let hook_calls = Arc::clone(&worker_calls);
    let _guard = install_run_root_operation_hook(move |_, _, _| {
        hook_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .expect("install run_root_operation_hook");

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let flag = Arc::clone(&in_process_invoked);
        invoke_with_privileges(
            &runtime,
            ExecutionPrivileges::Root,
            &bootstrap,
            &env_vars,
            operation,
            async move {
                flag.store(true, Ordering::Relaxed);
                Ok::<(), postgresql_embedded::Error>(())
            },
        )
        .expect("worker operation should succeed");
    }

    assert_eq!(worker_calls.load(Ordering::Relaxed), 2);
    assert!(!in_process_invoked.load(Ordering::Relaxed));
}

#[test]
fn installing_hook_twice_returns_error() {
    let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
        .expect("initial hook installation succeeds");

    let err = match install_run_root_operation_hook(|_, _, _| Ok(())) {
        Ok(_) => panic!("second installation should fail"),
        Err(err) => err,
    };
    assert_eq!(err, RunRootOperationHookInstallError::AlreadyInstalled);

    drop(guard);

    let guard =
        install_run_root_operation_hook(|_, _, _| Ok(())).expect("hook installs after guard drop");
    drop(guard);
}

#[cfg(feature = "privileged-tests")]
#[test]
fn worker_setup_times_out_when_helper_hangs() {
    let Some(worker_path) = option_env!("CARGO_BIN_EXE_pg_worker_hang") else {
        eprintln!("skipping worker timeout test: hanging worker binary not available");
        return;
    };

    if !geteuid().is_root() {
        eprintln!("skipping worker timeout test: requires root privileges");
        return;
    }

    let runtime = test_runtime();
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    bootstrap.worker_binary = Some(Utf8PathBuf::from(worker_path));
    bootstrap.setup_timeout = Duration::from_secs(1);
    bootstrap.start_timeout = Duration::from_secs(1);

    let env_vars = bootstrap.environment.to_env();
    let result = invoke_with_privileges(
        &runtime,
        ExecutionPrivileges::Root,
        &bootstrap,
        &env_vars,
        WorkerOperation::Setup,
        async { Ok::<(), postgresql_embedded::Error>(()) },
    );
    let err = result.expect_err("expected hanging worker to time out");
    let message = err.to_string();
    assert!(
        message.contains("timed out"),
        "expected timeout error, got: {message}",
    );
}
