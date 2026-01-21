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

use pg_embedded_setup_unpriv::worker::{PlainSecret, WorkerPayload};
use postgresql_embedded::PostgreSQL;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::runtime::Builder;

/// Boxed error type for the main result.
type BoxError = Box<dyn std::error::Error>;

/// Errors that can occur during worker operations.
#[derive(Debug)]
enum WorkerError {
    /// Invalid command-line arguments.
    InvalidArgs(String),
    /// Failed to read configuration file.
    ConfigRead(std::io::Error),
    /// Failed to parse configuration JSON.
    ConfigParse(serde_json::Error),
    /// Failed to convert settings snapshot to `PostgreSQL` settings.
    SettingsConversion(String),
    /// Failed to initialise the Tokio runtime.
    RuntimeInit(std::io::Error),
    /// `PostgreSQL` operation failed.
    PostgresOperation(String),
    /// Data directory recovery issue (e.g., missing PID file during stop).
    #[expect(
        dead_code,
        reason = "variant reserved for future data directory recovery errors"
    )]
    DataDirRecovery(String),
}

impl fmt::Display for WorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgs(msg) => write!(f, "invalid arguments: {msg}"),
            Self::ConfigRead(err) => write!(f, "config read failed: {err}"),
            Self::ConfigParse(err) => write!(f, "config parse failed: {err}"),
            Self::SettingsConversion(msg) => write!(f, "settings conversion failed: {msg}"),
            Self::RuntimeInit(err) => write!(f, "runtime init failed: {err}"),
            Self::PostgresOperation(msg) => write!(f, "postgres operation failed: {msg}"),
            Self::DataDirRecovery(msg) => write!(f, "data dir recovery: {msg}"),
        }
    }
}

impl std::error::Error for WorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigRead(err) | Self::RuntimeInit(err) => Some(err),
            Self::ConfigParse(err) => Some(err),
            Self::InvalidArgs(_)
            | Self::SettingsConversion(_)
            | Self::PostgresOperation(_)
            | Self::DataDirRecovery(_) => None,
        }
    }
}

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

    let runtime = build_runtime()?;
    apply_worker_environment(&payload.environment);
    let mut pg = Some(PostgreSQL::new(settings));
    runtime.block_on(async {
        match operation {
            Operation::Setup => {
                let pg_handle = pg.as_mut().ok_or_else(|| {
                    WorkerError::PostgresOperation("pg handle missing during setup".into())
                })?;
                execute_setup(pg_handle).await
            }
            Operation::Start => execute_start(&mut pg).await,
            Operation::Stop => execute_stop(&mut pg).await,
        }
    })?;
    Ok(())
}

fn parse_args(
    mut args: impl Iterator<Item = OsString>,
) -> Result<(Operation, PathBuf), WorkerError> {
    let _program = args.next();
    let operation = args
        .next()
        .ok_or_else(|| WorkerError::InvalidArgs("missing operation argument".into()))
        .and_then(|arg| Operation::parse(&arg))?;
    let config_path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| WorkerError::InvalidArgs("missing config path argument".into()))?;
    if let Some(extra) = args.next() {
        let extra_arg = extra.to_string_lossy();
        return Err(WorkerError::InvalidArgs(format!(
            "unexpected extra argument: {extra_arg}"
        )));
    }
    Ok((operation, config_path))
}

fn load_payload(config_path: &Path) -> Result<WorkerPayload, WorkerError> {
    let config_bytes = fs::read(config_path).map_err(WorkerError::ConfigRead)?;
    serde_json::from_slice(&config_bytes).map_err(WorkerError::ConfigParse)
}

fn build_runtime() -> Result<tokio::runtime::Runtime, WorkerError> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(WorkerError::RuntimeInit)
}

async fn execute_setup(pg: &mut PostgreSQL) -> Result<(), WorkerError> {
    pg.setup()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("setup failed: {e}")))
}

async fn execute_start(pg: &mut Option<PostgreSQL>) -> Result<(), WorkerError> {
    let mut handle = pg
        .take()
        .ok_or_else(|| WorkerError::PostgresOperation("pg handle missing during start".into()))?;
    handle
        .start()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("start failed: {e}")))?;
    // Prevent `Drop` from stopping the just-started server.
    std::mem::forget(handle);
    Ok(())
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
    use postgresql_embedded::{Settings, VersionReq};
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::{TempDir, tempdir};

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    const PG_CTL_STUB: &str = r#"#!/bin/sh
set -eu
data_dir=""
expect_data=0
for arg in "$@"; do
  if [ "$expect_data" -eq 1 ]; then
    data_dir="$arg"
    break
  fi
  case "$arg" in
    -D)
      expect_data=1
      ;;
    -D*)
      data_dir="${arg#-D}"
      break
      ;;
    --pgdata)
      expect_data=1
      ;;
    --pgdata=*)
      data_dir="${arg#--pgdata=}"
      break
      ;;
  esac
done
if [ -z "$data_dir" ]; then
  echo "missing -D argument" >&2
  exit 1
fi
mkdir -p "$data_dir"
echo "12345" > "$data_dir/postmaster.pid"
"#;

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

    #[test]
    fn start_operation_does_not_stop_postgres() {
        let temp_root = tempdir().expect("create temp root");
        let install_dir = temp_root.path().join("install");
        let data_dir = temp_root.path().join("data");
        write_pg_ctl_stub(&install_dir.join("bin")).expect("write pg_ctl stub");
        fs::create_dir_all(&data_dir).expect("create data dir");

        let settings =
            build_settings(&temp_root, install_dir, data_dir.clone()).expect("build settings");
        let config_path = write_worker_config(&temp_root, &settings).expect("write worker config");

        let args = vec![
            OsString::from("pg_worker"),
            OsString::from("start"),
            config_path.into_os_string(),
        ];
        run_worker(args.into_iter()).expect("run start operation");

        let pid_path = data_dir.join("postmaster.pid");
        assert!(
            pid_path.is_file(),
            "expected pid file to persist at {pid_path:?}"
        );
    }

    fn write_pg_ctl_stub(bin_dir: &Path) -> Result<(), std::io::Error> {
        fs::create_dir_all(bin_dir)?;
        let pg_ctl_path = bin_dir.join("pg_ctl");
        fs::write(&pg_ctl_path, PG_CTL_STUB)?;
        let mut permissions = fs::metadata(&pg_ctl_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&pg_ctl_path, permissions)?;
        Ok(())
    }

    fn build_settings(
        temp_root: &TempDir,
        install_dir: PathBuf,
        data_dir: PathBuf,
    ) -> Result<Settings, BoxError> {
        Ok(Settings {
            releases_url: "https://example.invalid/releases".into(),
            version: VersionReq::parse("=16.4.0")?,
            installation_dir: install_dir,
            password_file: temp_root.path().join("pgpass"),
            data_dir,
            host: "127.0.0.1".into(),
            port: 54_321,
            username: "postgres".into(),
            password: "postgres".into(),
            temporary: false,
            timeout: Some(Duration::from_secs(5)),
            configuration: HashMap::new(),
            trust_installation_dir: true,
        })
    }

    fn write_worker_config(temp_root: &TempDir, settings: &Settings) -> Result<PathBuf, BoxError> {
        let payload = WorkerPayload::new(settings, Vec::new())?;
        let config_path = temp_root.path().join("config.json");
        let config_bytes = serde_json::to_vec(&payload)?;
        fs::write(&config_path, config_bytes)?;
        Ok(config_path)
    }
}
