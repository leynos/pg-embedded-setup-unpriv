//! Invokes PostgreSQL bootstrap operations inside a privileged worker process.
//!
//! The helper mirrors `postgresql_embedded` lifecycle calls while allowing the
//! caller to demote credentials before spawning the child process.
#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use color_eyre::eyre::{Context, Report, Result};
use pg_embedded_setup_unpriv::worker::WorkerPayload;
use postgresql_embedded::PostgreSQL;
use tokio::runtime::Builder;

enum Operation {
    Setup,
    Start,
    Stop,
}

impl Operation {
    fn parse(arg: OsString) -> Result<Self> {
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
    let mut args = env::args_os();
    let _program = args.next();
    let op = args
        .next()
        .ok_or_else(|| Report::msg("missing operation argument"))
        .and_then(Operation::parse)?;
    let config_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| Report::msg("missing config path argument"))?;

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
            Some(value) => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::set_var(key, value);
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
