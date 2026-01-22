//! Exercises failure paths when the worker binary is misconfigured.
//!
//! These checks ensure the bootstrapper validates helper paths eagerly so
//! privileged orchestration does not defer errors to runtime.
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;

use color_eyre::eyre::{Result, ensure, eyre};
use nix::unistd::geteuid;
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
    ensure!(
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
    ensure!(
        error.kind() == BootstrapErrorKind::WorkerBinaryMissing,
        "expected structured worker-missing error but observed {:?}",
        error.kind()
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
    ensure!(
        message.contains("must reference a regular file"),
        eyre!("expected regular-file error, got: {message}")
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
    ensure!(
        message.contains("must be executable"),
        eyre!("expected non-executable error, got: {message}")
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn env_without_timezone_removes_tz_variable() -> Result<()> {
    let sandbox = TestSandbox::new("env-without-timezone")?;
    let env_vars = sandbox.env_without_timezone();
    let tz_removed = env_vars
        .iter()
        .any(|(key, value)| key == OsStr::new("TZ") && value.is_none());
    let tzdir_removed = env_vars
        .iter()
        .any(|(key, value)| key == OsStr::new("TZDIR") && value.is_none());
    ensure!(
        tz_removed,
        "expected time zone helper to remove the TZ variable"
    );
    ensure!(
        tzdir_removed,
        "expected time zone helper to remove the TZDIR variable"
    );

    sandbox.reset()?;

    Ok(())
}

#[test]
fn bootstrap_discovers_worker_from_path() -> Result<()> {
    let sandbox = TestSandbox::new("discover-worker-from-path")?;
    let bin_dir = sandbox.install_dir().join("bin");
    fs::create_dir_all(bin_dir.as_std_path())?;

    // Create a minimal pg_worker stub that satisfies discovery checks
    let worker_path = bin_dir.join("pg_worker");
    fs::write(worker_path.as_std_path(), b"#!/bin/sh\nexit 0\n")?;
    let mut perms = fs::metadata(worker_path.as_std_path())?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(worker_path.as_std_path(), perms)?;

    // Set up environment with custom PATH containing our bin directory
    // and explicitly unset PG_EMBEDDED_WORKER so discovery falls through to PATH
    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((OsString::from("PG_EMBEDDED_WORKER"), None));
    env_vars.push((
        OsString::from("PATH"),
        Some(OsString::from(bin_dir.as_str())),
    ));

    // Bootstrap should discover the worker from PATH. It will fail later
    // during actual setup (since our stub doesn't do real work), but the
    // discovery phase should succeed.
    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);

    // The bootstrap will fail because our stub doesn't actually work,
    // but the error should NOT be about missing worker binary.
    match outcome {
        Ok(_) => {
            // Bootstrap succeeded - worker was discovered and used
            sandbox.reset()?;
            Ok(())
        }
        Err(err) => {
            let message = err.to_string();
            ensure!(
                !message.contains("pg_worker binary not found"),
                "PATH discovery should have found the worker, but got: {message}"
            );
            ensure!(
                err.kind() != BootstrapErrorKind::WorkerBinaryMissing,
                "PATH discovery should have found the worker, but got WorkerBinaryMissing"
            );
            sandbox.reset()?;
            Ok(())
        }
    }
}

#[test]
fn bootstrap_fails_when_worker_not_in_path_or_env() -> Result<()> {
    let sandbox = TestSandbox::new("no-worker-anywhere")?;

    // Create environment with:
    // - PG_EMBEDDED_WORKER explicitly unset
    // - PATH pointing to an empty directory (no pg_worker)
    let empty_bin_dir = sandbox.install_dir().join("empty-bin");
    fs::create_dir_all(empty_bin_dir.as_std_path())?;

    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((OsString::from("PG_EMBEDDED_WORKER"), None));
    env_vars.push((
        OsString::from("PATH"),
        Some(OsString::from(empty_bin_dir.as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);

    // When running as root, the bootstrap should fail with missing worker error
    // When running as unprivileged, the bootstrap succeeds without needing a worker
    if geteuid().is_root() {
        let err = outcome.expect_err("bootstrap should fail when worker is not found as root");
        ensure!(
            err.kind() == BootstrapErrorKind::WorkerBinaryMissing,
            "expected WorkerBinaryMissing error when running as root, got: {:?}",
            err.kind()
        );
    } else {
        // Unprivileged execution doesn't require a worker, so bootstrap may succeed
        // or fail for other reasons (e.g., network issues), but not for missing worker
        if let Err(err) = outcome {
            let message = err.to_string();
            ensure!(
                !message.contains("pg_worker binary not found"),
                "unprivileged bootstrap should not require worker, but got: {message}"
            );
        }
    }

    sandbox.reset()?;

    Ok(())
}
