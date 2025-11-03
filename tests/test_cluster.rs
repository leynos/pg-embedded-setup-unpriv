#![cfg(all(unix, feature = "cluster-unit-tests"))]
//! Unit tests covering `TestCluster` privilege dispatch behaviour.

use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, ensure, eyre};
#[cfg(feature = "privileged-tests")]
use nix::unistd::geteuid;
use pg_embedded_setup_unpriv::BootstrapError;
use pg_embedded_setup_unpriv::test_support::{
    RunRootOperationHookInstallError, capture_warn_logs, drain_hook_install_logs,
    install_run_root_operation_hook, invoke_with_privileges,
};
use pg_embedded_setup_unpriv::{
    ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    WorkerInvoker, WorkerOperation,
};
use postgresql_embedded::Settings;
use tokio::runtime::{Builder, Runtime};

fn test_runtime() -> Result<Runtime> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| eyre!(err))
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

fn serialise_hook_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
fn unprivileged_operations_run_in_process() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let setup_calls = AtomicUsize::new(0);

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let call_counter = &setup_calls;
        invoke_with_privileges(&invoker, operation, async move {
            call_counter.fetch_add(1, Ordering::Relaxed);
            Ok::<(), postgresql_embedded::Error>(())
        })
        .map_err(|err| eyre!(err))?;
    }

    ensure!(
        setup_calls.load(Ordering::Relaxed) == 2,
        "expected setup and start operations to run in-process",
    );
    Ok(())
}

#[test]
fn unprivileged_operation_errors_propagate() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);

    let result = invoke_with_privileges(&invoker, WorkerOperation::Setup, async {
        Err::<(), postgresql_embedded::Error>(postgresql_embedded::Error::DatabaseStartError(
            "boom".into(),
        ))
    });

    let Err(err) = result else {
        return Err(eyre!("expected in-process failure to propagate"));
    };
    ensure!(
        err.to_string()
            .contains("postgresql_embedded::setup() failed"),
        "expected context-rich error, got {err:?}",
    );
    Ok(())
}

#[test]
fn root_operation_errors_surface_worker_failure() -> Result<()> {
    let _serial = serialise_hook_tests();
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);

    let _guard = install_run_root_operation_hook(|_, _, _| {
        Err(BootstrapError::from(eyre!("worker exploded")))
    })
    .map_err(|err| eyre!(err))?;

    let result = invoke_with_privileges(&invoker, WorkerOperation::Start, async {
        Ok::<(), postgresql_embedded::Error>(())
    });

    let Err(err) = result else {
        return Err(eyre!("expected worker failure to propagate"));
    };
    ensure!(
        err.to_string().contains("worker exploded"),
        "expected worker failure details, got {err:?}",
    );
    Ok(())
}

#[test]
fn root_operations_delegate_to_worker() -> Result<()> {
    let _serial = serialise_hook_tests();
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let worker_calls = Arc::new(AtomicUsize::new(0));
    let in_process_invoked = Arc::new(AtomicBool::new(false));

    let hook_calls = Arc::clone(&worker_calls);
    let _guard = install_run_root_operation_hook(move |_, _, _| {
        hook_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .map_err(|err| eyre!(err))?;

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let flag = Arc::clone(&in_process_invoked);
        invoke_with_privileges(&invoker, operation, async move {
            flag.store(true, Ordering::Relaxed);
            Ok::<(), postgresql_embedded::Error>(())
        })
        .map_err(|err| eyre!(err))?;
    }

    ensure!(
        worker_calls.load(Ordering::Relaxed) == 2,
        "expected both worker operations to execute",
    );
    ensure!(
        !in_process_invoked.load(Ordering::Relaxed),
        "in-process path should not run when privileges drop to the worker",
    );
    Ok(())
}

#[test]
fn installing_hook_twice_returns_error() -> Result<()> {
    let _serial = serialise_hook_tests();
    drop(drain_hook_install_logs());
    let initial_guard = install_run_root_operation_hook(|_, _, _| Ok(()))
        .map_err(|install_err| eyre!(install_err))?;

    let Err(err) = install_run_root_operation_hook(|_, _, _| Ok(())) else {
        return Err(eyre!("second installation should fail"));
    };
    ensure!(
        err == RunRootOperationHookInstallError::AlreadyInstalled,
        "unexpected install outcome: {err:?}",
    );
    let logs = drain_hook_install_logs();
    ensure!(
        logs.iter()
            .any(|entry| entry.contains("run_root_operation_hook already installed")),
        "expected contention log entry, got {logs:?}",
    );

    drop(initial_guard);

    let reinstalled_guard = install_run_root_operation_hook(|_, _, _| Ok(()))
        .map_err(|install_err| eyre!(install_err))?;
    drop(reinstalled_guard);
    drop(drain_hook_install_logs());
    Ok(())
}

#[cfg(feature = "privileged-tests")]
#[test]
fn worker_setup_times_out_when_helper_hangs() -> Result<()> {
    let Some(worker_path) = hanging_worker_binary() else {
        return Ok(());
    };

    if !geteuid().is_root() {
        tracing::warn!("skipping worker timeout test: requires root privileges");
        return Ok(());
    }

    run_hanging_worker_timeout_test(Utf8PathBuf::from(worker_path))
}

#[cfg(feature = "privileged-tests")]
fn hanging_worker_binary() -> Option<&'static str> {
    let path = option_env!("CARGO_BIN_EXE_pg_worker_hang");
    if path.is_none() {
        tracing::warn!("skipping worker timeout test: hanging worker binary not available");
    }
    path
}

#[cfg(feature = "privileged-tests")]
fn run_hanging_worker_timeout_test(worker_path: Utf8PathBuf) -> Result<()> {
    let runtime = test_runtime()?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    bootstrap.worker_binary = Some(worker_path);
    bootstrap.setup_timeout = Duration::from_secs(1);
    bootstrap.start_timeout = Duration::from_secs(1);

    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let (logs, result) = capture_warn_logs(|| {
        invoke_with_privileges(&invoker, WorkerOperation::Setup, async {
            Ok::<(), postgresql_embedded::Error>(())
        })
    });
    let Err(err) = result else {
        return Err(eyre!("expected hanging worker to time out"));
    };
    let message = err.to_string();
    ensure!(
        message.contains("timed out"),
        "expected timeout error, got: {message}",
    );
    ensure!(
        logs.iter()
            .any(|entry| entry.contains("worker setup timed out after 1s")),
        "expected timeout warning log, got {logs:?}",
    );
    Ok(())
}
