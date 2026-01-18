//! Invokes `PostgreSQL` bootstrap operations inside a privileged worker process.
//!
//! Usage:
//!
//! ```text
//! pg_worker <operation> <config-path>
//! ```
//!
//! The `operation` must be `setup`, `start`, or `stop`. The JSON payload at
//! `config-path` must serialise a [`WorkerPayload`] containing `PostgreSQL`
//! settings and environment overrides. A representative payload is:
//!
//! ```json
//! {
//!   "environment": {
//!     "PG_SUPERUSER": "postgres",
//!     "TZ": null
//!   },
//!   "settings": {
//!     "version": "=16.4.0",
//!     "port": 15433,
//!     "username": "postgres",
//!     "password": "postgres",
//!     "data_dir": "/tmp/data",
//!     "installation_dir": "/tmp/install",
//!     "temporary": false,
//!     "timeout_secs": 30,
//!     "configuration": {
//!       "lc_messages": "C"
//!     },
//!     "trust_installation_dir": true
//!   }
//! }
//! ```
//!
//! The helper mirrors `postgresql_embedded` lifecycle calls while allowing the
//! caller to demote credentials before spawning the child process.
#![cfg(unix)]

use color_eyre::eyre::{Context, Report, Result};
use pg_embedded_setup_unpriv::worker::{PlainSecret, WorkerPayload};
use postgresql_embedded::PostgreSQL;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;
use tokio::runtime::Builder;

enum Operation {
    Setup,
    Start,
    Stop,
}

impl Operation {
    fn parse(arg: &OsStr) -> Result<Self> {
        match arg.to_string_lossy().as_ref() {
            "setup" => Ok(Self::Setup),
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            other => Err(Report::msg(format!(
                "unknown pg_worker operation '{other}'; valid operations are setup, start, and stop"
            ))),
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    run_worker(env::args_os())
}

fn run_worker(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let _program = args.next();
    let op = args
        .next()
        .ok_or_else(|| Report::msg("missing operation argument"))
        .and_then(|arg| Operation::parse(&arg))?;
    let config_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| Report::msg("missing config path argument"))?;
    if let Some(extra) = args.next() {
        let extra_arg = extra.to_string_lossy();
        return Err(Report::msg(format!(
            "unexpected extra argument: {extra_arg}; expected only operation and config path"
        )));
    }

    let config_bytes = fs::read(&config_path).wrap_err("failed to read worker config")?;
    let payload: WorkerPayload =
        serde_json::from_slice(&config_bytes).wrap_err("failed to parse worker config")?;
    let settings = payload
        .settings
        .into_settings()
        .wrap_err("failed to rebuild PostgreSQL settings from snapshot")?;

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .wrap_err("failed to build runtime for worker")?;

    apply_worker_environment(&payload.environment);
    let mut pg = PostgreSQL::new(settings);

    runtime.block_on(async {
        match op {
            Operation::Setup => pg
                .setup()
                .await
                .wrap_err("postgresql_embedded::setup() failed"),
            Operation::Start => pg
                .start()
                .await
                .wrap_err("postgresql_embedded::start() failed"),
            Operation::Stop => match pg.stop().await {
                Ok(()) => Ok(()),
                Err(err) if stop_missing_pid_is_ok(&err) => Ok(()),
                Err(err) => Err(err).wrap_err("postgresql_embedded::stop() failed"),
            },
        }
    })?;
    Ok(())
}

/// Applies the worker environment overrides to the current process.
fn apply_worker_environment(environment: &[(String, Option<PlainSecret>)]) {
    for (key, value) in environment {
        match value {
            Some(env_value) => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::set_var(key, env_value.expose());
            },
            None => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::remove_var(key);
            },
        }
    }
}

fn stop_missing_pid_is_ok(err: &postgresql_embedded::Error) -> bool {
    let message = err.to_string();
    message.contains("postmaster.pid") && message.contains("does not exist")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn rejects_extra_argument() {
        let args = vec![
            OsString::from("pg_worker"),
            OsString::from("setup"),
            OsString::from("/tmp/config.json"),
            OsString::from("unexpected"),
        ];
        let err = run_worker(args.into_iter()).expect_err("extra argument must fail");
        assert!(
            err.to_string().contains("unexpected extra argument"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_worker_environment_uses_plaintext_and_unsets() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let secret_key = unique_env_key("PGWORKER_SECRET_KEY");
        let none_key = unique_env_key("PGWORKER_NONE_KEY");

        unsafe {
            // SAFETY: scoped test cleanup; single-threaded in this process.
            env::remove_var(&secret_key);
            env::remove_var(&none_key);
        }

        let secret = PlainSecret::from("super-secret-value".to_owned());
        let env_pairs = vec![(secret_key.clone(), Some(secret)), (none_key.clone(), None)];

        apply_worker_environment(&env_pairs);

        assert_eq!(env::var(&secret_key).as_deref(), Ok("super-secret-value"));
        assert!(matches!(
            env::var(&none_key),
            Err(env::VarError::NotPresent)
        ));

        unsafe {
            // SAFETY: scoped test cleanup; single-threaded in this process.
            env::remove_var(secret_key);
            env::remove_var(none_key);
        }
    }

    fn unique_env_key(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        format!("{prefix}_{pid}_{nanos}", pid = std::process::id())
    }
}
