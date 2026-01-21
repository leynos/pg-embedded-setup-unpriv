//! Behavioural tests covering privilege-aware bootstrap flows.
#![cfg(unix)]

use color_eyre::eyre::{Result, ensure, eyre};
use nix::unistd::geteuid;
use pg_embedded_setup_unpriv::{ExecutionPrivileges, detect_execution_privileges, nobody_uid};
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/bootstrap_sandbox.rs"]
mod bootstrap_sandbox;
#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs_bootstrap;
#[path = "support/env.rs"]
mod env;
#[path = "support/scenario.rs"]
mod scenario;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use bootstrap_sandbox::{
    BootstrapSandboxFixture, borrow_sandbox, run_bootstrap_with_temp_drop, sandbox,
};
use scenario::expect_fixture;
use serial::{ScenarioSerialGuard, serial_guard};

#[given("a fresh bootstrap sandbox")]
fn given_fresh_sandbox(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    borrow_sandbox(sandbox)?.borrow_mut().reset()
}

#[when("the bootstrap runs as an unprivileged user")]
fn when_bootstrap_runs_unprivileged(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    if geteuid().is_root() {
        run_bootstrap_with_temp_drop(sandbox_cell);
        return Ok(());
    }

    let uid = geteuid();
    {
        let mut state = sandbox_cell.borrow_mut();
        state.set_expected_owner(uid);
        state.record_privileges(detect_execution_privileges());
    }
    let outcome = {
        let sandbox_ref = sandbox_cell.borrow();
        sandbox_ref.run_bootstrap()
    };
    sandbox_cell.borrow_mut().handle_outcome(outcome)
}

#[when("the bootstrap runs twice as root")]
fn when_bootstrap_runs_twice_as_root(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    if !geteuid().is_root() {
        sandbox_cell
            .borrow_mut()
            .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires root access");
        return Ok(());
    }

    {
        let mut state = sandbox_cell.borrow_mut();
        state.set_expected_owner(nobody_uid());
        state.record_privileges(detect_execution_privileges());
    }

    {
        let outcome = {
            let sandbox_ref = sandbox_cell.borrow();
            sandbox_ref.run_bootstrap()
        };
        let mut state = sandbox_cell.borrow_mut();
        state.handle_outcome(outcome)?;
        if state.is_skipped() {
            return Ok(());
        }
    }

    let outcome = {
        let sandbox_ref = sandbox_cell.borrow();
        sandbox_ref.run_bootstrap()
    };
    let mut state = sandbox_cell.borrow_mut();
    if state.is_skipped() {
        return Ok(());
    }
    state.handle_outcome(outcome)
}

#[when("the bootstrap runs as root without a worker")]
fn when_bootstrap_runs_as_root_without_worker(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    if !geteuid().is_root() {
        sandbox_cell
            .borrow_mut()
            .mark_skipped("SKIP-BOOTSTRAP: privileged scenario requires root access");
        return Ok(());
    }

    sandbox_cell
        .borrow_mut()
        .record_privileges(detect_execution_privileges());

    let outcome = {
        let sandbox_ref = sandbox_cell.borrow();
        sandbox_ref.run_bootstrap_without_worker()
    };

    match outcome {
        Ok(()) => Err(eyre!(
            "expected bootstrap to fail without PG_EMBEDDED_WORKER"
        )),
        Err(err) => {
            sandbox_cell.borrow_mut().record_error(err.to_string());
            Ok(())
        }
    }
}

#[then("the sandbox directories are owned by the target uid")]
fn then_directories_owned(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    borrow_sandbox(sandbox)?
        .borrow_mut()
        .assert_owned_by_expected_user()
}

#[then("the detected privileges were unprivileged")]
fn then_detected_unprivileged(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    sandbox_cell
        .borrow()
        .assert_detected(ExecutionPrivileges::Unprivileged)
}

#[then("the detected privileges were root")]
fn then_detected_root(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    borrow_sandbox(sandbox)?
        .borrow()
        .assert_detected(ExecutionPrivileges::Root)
}

#[then("the bootstrap reports the missing worker")]
fn then_bootstrap_reports_missing_worker(sandbox: &BootstrapSandboxFixture) -> Result<()> {
    let sandbox_cell = borrow_sandbox(sandbox)?;
    let sandbox_ref = sandbox_cell.borrow();
    if sandbox_ref.is_skipped() {
        return Ok(());
    }
    let message = sandbox_ref
        .last_error()
        .ok_or_else(|| eyre!("missing bootstrap error message"))?;
    ensure!(
        message.contains("pg_worker binary not found"),
        "unexpected missing-worker error: {message}",
    );
    Ok(())
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 0)]
fn bootstrap_as_unprivileged(serial_guard: ScenarioSerialGuard, sandbox: BootstrapSandboxFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(sandbox, "bootstrap privileges sandbox");
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 1)]
fn bootstrap_as_root(serial_guard: ScenarioSerialGuard, sandbox: BootstrapSandboxFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(sandbox, "bootstrap privileges sandbox");
}

#[scenario(path = "tests/features/bootstrap_privileges.feature", index = 2)]
fn bootstrap_as_root_without_worker(
    serial_guard: ScenarioSerialGuard,
    sandbox: BootstrapSandboxFixture,
) {
    let _guard = serial_guard;
    let _ = expect_fixture(sandbox, "bootstrap privileges sandbox");
}
