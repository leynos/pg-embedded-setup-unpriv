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

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;

use color_eyre::eyre::{Context, Report, Result};
use pg_embedded_setup_unpriv::worker::WorkerPayload;
use postgresql_embedded::PostgreSQL;
use secrecy::ExposeSecret;
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

    for (key, value) in &payload.environment {
        match value {
            Some(env_value) => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::set_var(key, env_value.expose_secret());
            },
            None => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::remove_var(key);
            },
        }
    }
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
            Operation::Stop => pg
                .stop()
                .await
                .wrap_err("postgresql_embedded::stop() failed"),
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
