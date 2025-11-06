use super::*;
use crate::test_support::{
    RunRootOperationHookInstallError, drain_hook_install_logs, install_run_root_operation_hook,
};
use crate::{ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, ensure, eyre};
use postgresql_embedded::Settings;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
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
fn unprivileged_operations_execute_in_process() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let calls = AtomicUsize::new(0);

    invoker
        .invoke(WorkerOperation::Setup, async {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<(), postgresql_embedded::Error>(())
        })
        .map_err(|err| eyre!(err))?;

    ensure!(
        calls.load(Ordering::Relaxed) == 1,
        "expected in-process call"
    );
    Ok(())
}

#[test]
fn root_operations_delegate_to_hook() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let worker_calls = Arc::new(AtomicUsize::new(0));

    let hook_calls = Arc::clone(&worker_calls);
    let _guard = install_run_root_operation_hook(move |_, _, _| {
        hook_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .map_err(|err| eyre!(err))?;

    invoker
        .invoke(WorkerOperation::Setup, async {
            Ok::<(), postgresql_embedded::Error>(())
        })
        .map_err(|err| eyre!(err))?;

    ensure!(
        worker_calls.load(Ordering::Relaxed) == 1,
        "expected worker hook to run"
    );
    Ok(())
}

#[test]
fn installing_hook_twice_errors() -> Result<()> {
    let _guard = install_run_root_operation_hook(|_, _, _| Ok(())).map_err(|err| eyre!(err))?;

    let Err(err) = install_run_root_operation_hook(|_, _, _| Ok(())) else {
        return Err(eyre!("expected second installation to fail"));
    };
    ensure!(
        err == RunRootOperationHookInstallError::AlreadyInstalled,
        "unexpected install error: {err:?}"
    );

    drop(drain_hook_install_logs());
    Ok(())
}
