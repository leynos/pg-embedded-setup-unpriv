#![cfg(unix)]
//! Exercises failure paths when the worker binary is misconfigured.
//!
//! These checks ensure the bootstrapper validates helper paths eagerly so
//! privileged orchestration does not defer errors to runtime.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;

use color_eyre::eyre::Result;
use pg_embedded_setup_unpriv::{BootstrapErrorKind, bootstrap_for_tests};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;

use sandbox::TestSandbox;

#[test]
fn bootstrap_fails_when_worker_binary_missing() -> Result<()> {
    let sandbox = TestSandbox::new("missing-worker-binary")?;
    let missing_worker = sandbox.install_dir().join("nonexistent-worker");
    assert!(
        !missing_worker.as_std_path().exists(),
        "expected test sandbox to start without a worker binary",
    );

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(missing_worker.as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let error = outcome
        .expect_err("bootstrap_for_tests should fail fast when the worker binary is missing");
    assert_eq!(
        error.kind(),
        BootstrapErrorKind::WorkerBinaryMissing,
        "expected structured worker-missing error",
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn bootstrap_fails_when_worker_path_is_directory() -> Result<()> {
    let sandbox = TestSandbox::new("worker-path-directory")?;
    fs::create_dir_all(sandbox.install_dir().as_std_path())?;

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(sandbox.install_dir().as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("bootstrap_for_tests should reject directory worker paths");
    let message = err.to_string();
    assert!(
        message.contains("must reference a regular file"),
        "expected regular-file error, got: {message}",
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn bootstrap_fails_when_worker_binary_not_executable() -> Result<()> {
    let sandbox = TestSandbox::new("worker-path-non-executable")?;
    fs::create_dir_all(sandbox.install_dir().as_std_path())?;
    let worker_path = sandbox.install_dir().join("pg_worker_stub");
    fs::write(worker_path.as_std_path(), b"#!/bin/sh\nexit 0\n")?;
    let mut perms = fs::metadata(worker_path.as_std_path())?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(worker_path.as_std_path(), perms)?;

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(worker_path.as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("bootstrap_for_tests should reject non-executable workers");
    let message = err.to_string();
    assert!(
        message.contains("must be executable"),
        "expected non-executable error, got: {message}",
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn env_without_timezone_removes_tz_variable() -> Result<()> {
    let sandbox = TestSandbox::new("env-without-timezone")?;
    let env_vars = sandbox.env_without_timezone();
    assert!(
        env_vars
            .iter()
            .any(|(key, value)| key == OsStr::new("TZ") && value.is_none()),
        "expected time zone helper to remove the TZ variable",
    );

    sandbox.reset()?;

    Ok(())
}
