//! Unit tests covering `TestCluster` privilege dispatch behaviour.
#![cfg(unix)]

use serial_test::serial;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[cfg(feature = "privileged-tests")]
use std::time::Duration;

#[cfg(not(feature = "cluster-unit-tests"))]
use crate as cluster_crate;
#[cfg(feature = "privileged-tests")]
use camino::Utf8Path;
#[cfg(feature = "privileged-tests")]
use camino::Utf8PathBuf;
use cluster_crate::BootstrapError;
#[cfg(feature = "privileged-tests")]
use cluster_crate::test_support::capture_warn_logs;
use cluster_crate::test_support::{
    RunRootOperationHookInstallError, drain_hook_install_logs, dummy_settings,
    install_run_root_operation_hook, test_runtime,
};
use cluster_crate::{ExecutionPrivileges, WorkerInvoker, WorkerOperation};
#[cfg(feature = "privileged-tests")]
use color_eyre::eyre::Context;
use color_eyre::eyre::{Result, ensure, eyre};
#[cfg(feature = "privileged-tests")]
use nix::unistd::geteuid;
#[cfg(feature = "cluster-unit-tests")]
use pg_embedded_setup_unpriv as cluster_crate;
#[cfg(feature = "privileged-tests")]
use std::fs;
#[cfg(feature = "privileged-tests")]
use std::os::unix::fs::PermissionsExt;
#[cfg(feature = "privileged-tests")]
use tempfile::tempdir;

#[test]
fn unprivileged_operations_run_in_process() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let setup_calls = AtomicUsize::new(0);

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let call_counter = &setup_calls;
        invoker
            .invoke(operation, async move {
                call_counter.fetch_add(1, Ordering::Relaxed);
                Ok::<(), postgresql_embedded::Error>(())
            })
            .map_err(|err: BootstrapError| eyre!(err))?;
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

    let result = invoker.invoke(WorkerOperation::Setup, async {
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
#[serial(worker_hook)]
fn root_operation_errors_surface_worker_failure() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);

    let _guard = install_run_root_operation_hook(|_, _, _| {
        Err(BootstrapError::from(eyre!("worker exploded")))
    })
    .map_err(|err: RunRootOperationHookInstallError| eyre!(err))?;

    let result = invoker.invoke(WorkerOperation::Start, async {
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
#[serial(worker_hook)]
fn root_operations_delegate_to_worker() -> Result<()> {
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
    .map_err(|err: RunRootOperationHookInstallError| eyre!(err))?;

    for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
        let flag = Arc::clone(&in_process_invoked);
        invoker
            .invoke(operation, async move {
                flag.store(true, Ordering::Relaxed);
                Ok::<(), postgresql_embedded::Error>(())
            })
            .map_err(|err: BootstrapError| eyre!(err))?;
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
#[serial(worker_hook)]
fn installing_hook_twice_returns_error() -> Result<()> {
    drop(drain_hook_install_logs());
    let initial_guard = install_run_root_operation_hook(|_, _, _| Ok(()))
        .map_err(|install_err: RunRootOperationHookInstallError| eyre!(install_err))?;

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
        .map_err(|install_err: RunRootOperationHookInstallError| eyre!(install_err))?;
    drop(reinstalled_guard);
    drop(drain_hook_install_logs());
    Ok(())
}

#[cfg(feature = "privileged-tests")]
#[test]
fn worker_setup_times_out_when_helper_hangs() -> Result<()> {
    let Some(worker_path_str) = hanging_worker_binary() else {
        return Ok(());
    };

    if !geteuid().is_root() {
        tracing::warn!("skipping worker timeout test: requires root privileges");
        return Ok(());
    }

    let worker_path = Utf8Path::new(worker_path_str);
    run_hanging_worker_timeout_test(worker_path)
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
fn run_hanging_worker_timeout_test(worker_path: &Utf8Path) -> Result<()> {
    let runtime = test_runtime()?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let (_staging_dir, staged_worker) = stage_worker_for_nobody(worker_path)?;
    bootstrap.worker_binary = Some(staged_worker);
    bootstrap.setup_timeout = Duration::from_secs(1);
    bootstrap.start_timeout = Duration::from_secs(1);

    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let (logs, result) = capture_warn_logs(|| {
        invoker.invoke(WorkerOperation::Setup, async {
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

#[cfg(feature = "privileged-tests")]
fn stage_worker_for_nobody(worker_path: &Utf8Path) -> Result<(tempfile::TempDir, Utf8PathBuf)> {
    let staging_dir = tempdir().context("create worker staging directory")?;
    let filename = worker_path
        .file_name()
        .ok_or_else(|| eyre!("worker path must include a filename"))?;
    let staged_path = staging_dir.path().join(filename);
    fs::copy(worker_path, &staged_path).context("copy worker binary for staging")?;
    let mut permissions = fs::metadata(&staged_path)
        .context("read staged worker metadata")?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&staged_path, permissions).context("update staged worker permissions")?;
    let staged_utf8 =
        Utf8PathBuf::from_path_buf(staged_path).map_err(|_| eyre!("worker path not UTF-8"))?;
    Ok((staging_dir, staged_utf8))
}
