#![cfg(unix)]

use std::ffi::{OsStr, OsString};

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
fn bootstrap_fails_when_worker_binary_missing() -> Result<()> {
    let sandbox = TestSandbox::new("missing-worker-binary")?;
    let missing_worker = sandbox.install_dir().join("nonexistent-worker");
    assert!(
        !missing_worker.as_std_path().exists(),
        "expected test sandbox to start without a worker binary",
    );

    let tz_free_env = sandbox.env_without_timezone();
    assert!(
        tz_free_env
            .iter()
            .any(|(key, value)| key == OsStr::new("TZ") && value.is_none()),
        "expected time zone helper to remove the TZ variable",
    );

    let mut env_vars = sandbox.base_env();
    env_vars.push((
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(OsString::from(missing_worker.as_str())),
    ));

    let outcome = sandbox.with_env(env_vars, bootstrap_for_tests);
    let error = outcome
        .expect_err("bootstrap_for_tests should fail fast when the worker binary is missing");
    let message = error.to_string();
    assert!(
        message.contains("must reference an existing file"),
        "expected missing-worker error, got: {message}",
    );

    sandbox.reset()?;

    Ok(())
}
