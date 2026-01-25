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

use camino::{Utf8Path, Utf8PathBuf};
use pg_embedded_setup_unpriv::test_support::ambient_dir_and_path;
use pg_embedded_setup_unpriv::worker::{PlainSecret, WorkerPayload};
use postgresql_embedded::{PostgreSQL, Status};
use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::PathBuf;
use thiserror::Error;
use tokio::runtime::Builder;
use tracing::info;

/// Boxed error type for the main result.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Errors that can occur during worker operations.
#[derive(Debug, Error)]
enum WorkerError {
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("failed to read worker config: {0}")]
    ConfigRead(#[source] BoxError),
    #[error("failed to parse worker config: {0}")]
    ConfigParse(#[source] serde_json::Error),
    #[error("settings conversion failed: {0}")]
    SettingsConversion(String),
    #[error("runtime init failed: {0}")]
    RuntimeInit(#[source] std::io::Error),
    #[error("postgres operation failed: {0}")]
    PostgresOperation(String),
    #[expect(
        dead_code,
        reason = "variant reserved for future data directory recovery errors"
    )]
    #[error("data dir recovery: {0}")]
    DataDirRecovery(String),
}

/// Abstracts environment variable mutation for testability.
pub trait EnvStore {
    /// Sets an environment variable to the given value.
    fn set(&mut self, key: &str, value: &str);

    /// Removes an environment variable.
    fn remove(&mut self, key: &str);
}

/// Wraps the real process environment for production use.
pub struct ProcessEnvStore;

impl EnvStore for ProcessEnvStore {
    fn set(&mut self, key: &str, value: &str) {
        unsafe {
            // SAFETY: the worker is single-threaded; environment updates
            // cannot race.
            env::set_var(key, value);
        }
    }

    fn remove(&mut self, key: &str) {
        unsafe {
            // SAFETY: the worker is single-threaded; environment updates
            // cannot race.
            env::remove_var(key);
        }
    }
}

/// In-memory environment store for deterministic testing.
pub struct TestEnvStore {
    env: HashMap<String, Option<String>>,
}

impl TestEnvStore {
    /// Creates a new in-memory environment store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            env: HashMap::new(),
        }
    }

    /// Returns the value for a key, or `None` if unset or removed.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.env.get(key).and_then(|v| v.as_deref())
    }
}

impl Default for TestEnvStore {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvStore for TestEnvStore {
    fn set(&mut self, key: &str, value: &str) {
        self.env.insert(key.to_owned(), Some(value.to_owned()));
    }

    fn remove(&mut self, key: &str) {
        self.env.insert(key.to_owned(), None);
    }
}

#[derive(Debug)]
enum Operation {
    Setup,
    Start,
    Stop,
}

impl Operation {
    fn parse(arg: &OsStr) -> Result<Self, WorkerError> {
        match arg.to_string_lossy().as_ref() {
            "setup" => Ok(Self::Setup),
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            other => Err(WorkerError::InvalidArgs(format!(
                "unknown operation '{other}'; expected setup, start, or stop"
            ))),
        }
    }
}

fn main() -> Result<(), BoxError> {
    run_worker(env::args_os()).map_err(Into::into)
}

fn run_worker(args: impl Iterator<Item = OsString>) -> Result<(), WorkerError> {
    let (operation, config_path) = parse_args(args)?;
    let payload = load_payload(&config_path)?;
    let settings = payload
        .settings
        .into_settings()
        .map_err(|e| WorkerError::SettingsConversion(e.to_string()))?;
    let data_dir = extract_data_dir(&settings)?;

    let runtime = build_runtime()?;
    let mut env_store = ProcessEnvStore;
    apply_worker_environment(&mut env_store, &payload.environment);
    let mut pg = Some(PostgreSQL::new(settings));
    runtime.block_on(async {
        match operation {
            Operation::Setup => {
                let pg_handle = pg.as_mut().ok_or_else(|| {
                    WorkerError::PostgresOperation("pg handle missing during setup".into())
                })?;
                ensure_postgres_setup(pg_handle, &data_dir).await
            }
            Operation::Start => {
                let pg_handle = pg.as_mut().ok_or_else(|| {
                    WorkerError::PostgresOperation("pg handle missing during start".into())
                })?;
                ensure_postgres_started(pg_handle, &data_dir).await?;
                let handle = pg.take();
                std::mem::forget(handle);
                Ok(())
            }
            Operation::Stop => execute_stop(&mut pg).await,
        }
    })?;
    Ok(())
}

fn parse_args(
    mut args: impl Iterator<Item = OsString>,
) -> Result<(Operation, Utf8PathBuf), WorkerError> {
    let _program = args.next();
    let operation = args
        .next()
        .ok_or_else(|| WorkerError::InvalidArgs("missing operation argument".into()))
        .and_then(|arg| Operation::parse(&arg))?;
    let config_path_buf = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| WorkerError::InvalidArgs("missing config path argument".into()))?;
    let config_path = Utf8PathBuf::from_path_buf(config_path_buf).map_err(|p| {
        WorkerError::InvalidArgs(format!("config path is not valid UTF-8: {}", p.display()))
    })?;
    if let Some(extra) = args.next() {
        let extra_arg = extra.to_string_lossy();
        return Err(WorkerError::InvalidArgs(format!(
            "unexpected extra argument: {extra_arg}"
        )));
    }
    Ok((operation, config_path))
}

fn load_payload(config_path: &Utf8Path) -> Result<WorkerPayload, WorkerError> {
    let config_bytes = read_config_file(config_path).map_err(WorkerError::ConfigRead)?;
    serde_json::from_slice(&config_bytes).map_err(WorkerError::ConfigParse)
}

fn read_config_file(path: &Utf8Path) -> Result<Vec<u8>, BoxError> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    let mut file = dir.open(relative.as_std_path())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn build_runtime() -> Result<tokio::runtime::Runtime, WorkerError> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(WorkerError::RuntimeInit)
}

fn extract_data_dir(settings: &postgresql_embedded::Settings) -> Result<Utf8PathBuf, WorkerError> {
    Utf8PathBuf::from_path_buf(settings.data_dir.clone())
        .map_err(|_| WorkerError::SettingsConversion("data_dir must be valid UTF-8".into()))
}

fn is_setup_complete(pg: &PostgreSQL, data_dir: &Utf8Path) -> bool {
    data_dir.is_dir() && data_dir.join("PG_VERSION").exists() && pg.status() != Status::NotInstalled
}

#[expect(
    clippy::cognitive_complexity,
    reason = "function has simple conditional with early return and one async call"
)]
async fn ensure_postgres_setup(
    pg: &mut PostgreSQL,
    data_dir: &Utf8Path,
) -> Result<(), WorkerError> {
    if is_setup_complete(pg, data_dir) {
        info!("PostgreSQL setup already complete, skipping redundant setup");
        return Ok(());
    }

    info!("PostgreSQL data directory not initialized, running setup");
    pg.setup()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("setup failed: {e}")))
}

async fn ensure_postgres_started(
    pg: &mut PostgreSQL,
    data_dir: &Utf8Path,
) -> Result<(), WorkerError> {
    ensure_postgres_setup(pg, data_dir).await?;
    start_if_not_started(pg).await
}

async fn start_if_not_started(pg: &mut PostgreSQL) -> Result<(), WorkerError> {
    if pg.status() == Status::Started {
        info!("PostgreSQL already started, skipping redundant start");
        return Ok(());
    }

    pg.start()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("start failed: {e}")))
}

async fn execute_stop(pg: &mut Option<PostgreSQL>) -> Result<(), WorkerError> {
    let pg_handle = pg
        .as_mut()
        .ok_or_else(|| WorkerError::PostgresOperation("pg handle missing during stop".into()))?;
    handle_stop_result(pg_handle.stop().await)
}

fn handle_stop_result(result: Result<(), postgresql_embedded::Error>) -> Result<(), WorkerError> {
    match result {
        Ok(()) => Ok(()),
        Err(err) if stop_missing_pid_is_ok(&err) => Ok(()),
        Err(err) => Err(WorkerError::PostgresOperation(format!(
            "stop failed: {err}"
        ))),
    }
}

/// Applies the worker environment overrides to the current process.
fn apply_worker_environment(
    store: &mut dyn EnvStore,
    environment: &[(String, Option<PlainSecret>)],
) {
    for (key, value) in environment {
        match value {
            Some(env_value) => store.set(key, env_value.expose()),
            None => store.remove(key),
        }
    }
}

fn stop_missing_pid_is_ok(err: &postgresql_embedded::Error) -> bool {
    let message = err.to_string();
    message.contains("postmaster.pid") && message.contains("does not exist")
}

#[cfg(test)]
#[path = "pg_worker_helpers.rs"]
mod pg_worker_helpers;

#[cfg(test)]
#[path = "pg_worker_tests.rs"]
mod tests;
