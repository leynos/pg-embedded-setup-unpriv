//! Helper binary that deliberately stalls to exercise worker timeouts.
#![cfg(unix)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use color_eyre::eyre::{Context, Report, Result};
use pg_embedded_setup_unpriv::worker::WorkerPayload;

fn main() -> Result<()> {
    color_eyre::install()?;
    let mut args = env::args_os();
    let _program = args.next();
    let _operation = args
        .next()
        .ok_or_else(|| Report::msg("missing operation argument"))?;
    let config_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| Report::msg("missing config path argument"))?;

    let config_bytes = fs::read(&config_path).wrap_err("failed to read worker config")?;
    let _: WorkerPayload =
        serde_json::from_slice(&config_bytes).wrap_err("failed to parse worker config")?;

    thread::sleep(Duration::from_secs(120));
    Ok(())
}
