#![cfg(unix)]
//! Validates configuration of the shutdown timeout environment variable.

use std::ffi::OsString;
use std::time::Duration;

use color_eyre::eyre::Result;
use pg_embedded_setup_unpriv::bootstrap_for_tests;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;

use sandbox::TestSandbox;

#[test]
fn shutdown_timeout_defaults_to_15s() -> Result<()> {
    let sandbox = TestSandbox::new("shutdown-timeout-default")?;
    let env = sandbox.env_without_timezone();
    let outcome = sandbox.with_env(env, bootstrap_for_tests);
    let settings = outcome?;
    assert_eq!(settings.shutdown_timeout, Duration::from_secs(15));
    sandbox.reset()?;
    Ok(())
}

#[test]
fn shutdown_timeout_honours_override() -> Result<()> {
    let sandbox = TestSandbox::new("shutdown-timeout-override")?;
    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_SHUTDOWN_TIMEOUT_SECS"),
        Some(OsString::from("42")),
    ));
    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let settings = outcome?;
    assert_eq!(settings.shutdown_timeout, Duration::from_secs(42));
    sandbox.reset()?;
    Ok(())
}

#[test]
fn shutdown_timeout_rejects_non_numeric_values() -> Result<()> {
    let sandbox = TestSandbox::new("shutdown-timeout-invalid")?;
    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_SHUTDOWN_TIMEOUT_SECS"),
        Some(OsString::from("forty-two")),
    ));
    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("expected invalid timeout value to fail");
    assert!(
        err.to_string()
            .contains("failed to parse PG_SHUTDOWN_TIMEOUT_SECS"),
        "unexpected error message: {err}"
    );
    sandbox.reset()?;
    Ok(())
}

#[test]
fn shutdown_timeout_rejects_excessive_values() -> Result<()> {
    let sandbox = TestSandbox::new("shutdown-timeout-excessive")?;
    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_SHUTDOWN_TIMEOUT_SECS"),
        Some(OsString::from("601")),
    ));
    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("expected excessive timeout to fail");
    assert!(
        err.to_string().contains("must be 600 seconds or less"),
        "unexpected error message: {err}"
    );
    sandbox.reset()?;
    Ok(())
}

#[test]
fn shutdown_timeout_rejects_zero() -> Result<()> {
    let sandbox = TestSandbox::new("shutdown-timeout-zero")?;
    let mut env_vars = sandbox.env_without_timezone();
    env_vars.push((
        OsString::from("PG_SHUTDOWN_TIMEOUT_SECS"),
        Some(OsString::from("0")),
    ));
    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let err = outcome.expect_err("expected zero timeout to fail");
    assert!(
        err.to_string().contains("must be at least 1 second"),
        "unexpected error message: {err}"
    );
    sandbox.reset()?;
    Ok(())
}
