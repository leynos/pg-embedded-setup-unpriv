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
use postgresql_embedded::PostgreSQL;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::PathBuf;
use thiserror::Error;
use tokio::runtime::Builder;

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
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::{TempDir, tempdir};

    const PG_CTL_STUB: &str = include_str!("fixtures/pg_ctl_stub.sh");

    /// Trait for environment variable operations, allowing mock implementations in tests.
    trait EnvironmentOperations {
        fn set_var(&self, key: &str, value: &str);
        fn remove_var(&self, key: &str);
    }

    /// Mock environment implementation for tests that tracks environment operations
    /// without actually mutating the process environment.
    #[derive(Debug, Default)]
    struct MockEnvironment {
        vars: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    }

    impl MockEnvironment {
        fn get(&self, key: &str) -> Option<String> {
            self.vars.lock().ok()?.get(key).cloned()
        }
    }

    impl EnvironmentOperations for MockEnvironment {
        fn set_var(&self, key: &str, value: &str) {
            let mut vars = self.vars.lock().expect("mock env mutex poisoned");
            vars.insert(key.to_owned(), value.to_owned());
        }

        fn remove_var(&self, key: &str) {
            let mut vars = self.vars.lock().expect("mock env mutex poisoned");
            vars.remove(key);
        }
    }

    /// Applies environment overrides using the provided operations implementation.
    fn apply_worker_environment_with<E>(env_ops: &E, environment: &[(String, Option<PlainSecret>)])
    where
        E: EnvironmentOperations,
    {
        for (key, value) in environment {
            match value {
                Some(env_value) => env_ops.set_var(key, env_value.expose()),
                None => env_ops.remove_var(key),
            }
        }
    }

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
        let secret = PlainSecret::from("super-secret-value".to_owned());
        let env_pairs = vec![
            ("PGWORKER_SECRET_KEY".to_owned(), Some(secret)),
            ("PGWORKER_NONE_KEY".to_owned(), None),
        ];

        let mock = MockEnvironment::default();
        apply_worker_environment_with::<MockEnvironment>(&mock, &env_pairs);

        assert_eq!(
            mock.get("PGWORKER_SECRET_KEY"),
            Some("super-secret-value".to_owned())
        );
        assert!(mock.get("PGWORKER_NONE_KEY").is_none());
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

    #[test]
    fn parse_args_rejects_non_utf8_config_path() {
        let program = OsString::from("pg_worker");
        let operation = OsString::from("setup");
        let non_utf8 = OsStr::from_bytes(&[0x80]).to_os_string();

        let args = vec![program, operation, non_utf8].into_iter();

        let result = parse_args(args);

        match result {
            Err(WorkerError::InvalidArgs(msg)) => {
                let msg_lc = msg.to_lowercase();
                assert!(
                    msg_lc.contains("utf-8"),
                    "error message should mention UTF-8, got: {msg}"
                );
                assert!(
                    msg_lc.contains("config"),
                    "error message should mention config path, got: {msg}"
                );
            }
            other => panic!(
                "expected WorkerError::InvalidArgs for non-UTF-8 config path, got: {other:?}"
            ),
        }
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
