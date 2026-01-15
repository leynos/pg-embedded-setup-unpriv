//! Unit tests for the [`WorkerInvoker`] component, verifying both in-process
//! execution for unprivileged operations and hook delegation for root operations.

use super::*;
use crate::ExecutionPrivileges;
use crate::test_support::{
    RunRootOperationHookInstallError, drain_hook_install_logs, dummy_settings,
    install_run_root_operation_hook, test_runtime,
};
use color_eyre::eyre::{Result, ensure, eyre};
use serial_test::serial;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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
#[serial]
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
#[serial]
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
