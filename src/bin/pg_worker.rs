//! Privileged worker for `PostgreSQL` bootstrap: `pg_worker <setup|start|stop> <config.json>`
//!
//! Deserializes [`WorkerPayload`] from `config.json` and invokes `postgresql_embedded` lifecycle
//! calls, allowing the caller to demote credentials before spawning the child process.

#[cfg(unix)]
use {
    camino::{Utf8Path, Utf8PathBuf},
    pg_embedded_setup_unpriv::{
        ambient_dir_and_path,
        worker::{PlainSecret, WorkerPayload},
    },
    postgresql_embedded::{PostgreSQL, Status},
    std::{
        env,
        ffi::{OsStr, OsString},
        io::{ErrorKind, Read},
        path::PathBuf,
    },
    thiserror::Error,
    tokio::runtime::Builder,
    tracing::info,
};

#[cfg(unix)]
type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(unix)]
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
    #[error("data dir recovery: {0}")]
    DataDirRecovery(String),
}

#[cfg(unix)]
#[derive(Debug)]
enum Operation {
    Setup,
    Start,
    Stop,
}

#[cfg(unix)]
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

#[cfg(unix)]
fn main() -> Result<(), BoxError> {
    run_worker(env::args_os()).map_err(Into::into)
}

#[cfg(unix)]
fn run_worker(args: impl Iterator<Item = OsString>) -> Result<(), WorkerError> {
    let (op, cfg_path) = parse_args(args)?;
    let payload = load_payload(&cfg_path)?;
    let settings = payload
        .settings
        .into_settings()
        .map_err(|e| WorkerError::SettingsConversion(e.to_string()))?;
    let data_dir = extract_data_dir(&settings)?;
    let runtime = build_runtime()?;
    apply_worker_environment(&payload.environment);
    let mut pg = Some(PostgreSQL::new(settings));
    runtime.block_on(async {
        match op {
            Operation::Setup => {
                let h = pg
                    .as_mut()
                    .ok_or_else(|| WorkerError::PostgresOperation("no pg".into()))?;
                run_postgres_setup(h, &data_dir).await
            }
            Operation::Start => {
                let h = pg
                    .as_mut()
                    .ok_or_else(|| WorkerError::PostgresOperation("no pg".into()))?;
                ensure_postgres_started(h, &data_dir).await?;
                // Intentionally leak PostgreSQL to keep it running after worker exit.
                if let Some(i) = pg.take() {
                    std::mem::forget(i);
                }
                Ok(())
            }
            Operation::Stop => execute_stop(&mut pg).await,
        }
    })
}

#[cfg(unix)]
fn parse_args(
    mut args: impl Iterator<Item = OsString>,
) -> Result<(Operation, Utf8PathBuf), WorkerError> {
    let _ = args.next();
    let op = args
        .next()
        .ok_or_else(|| WorkerError::InvalidArgs("missing operation".into()))
        .and_then(|a| Operation::parse(&a))?;
    let path = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| WorkerError::InvalidArgs("missing config path".into()))?;
    let cfg = Utf8PathBuf::from_path_buf(path)
        .map_err(|p| WorkerError::InvalidArgs(format!("config path not UTF-8: {}", p.display())))?;
    if let Some(e) = args.next() {
        return Err(WorkerError::InvalidArgs(format!(
            "unexpected extra argument: {}",
            e.to_string_lossy()
        )));
    }
    Ok((op, cfg))
}

#[cfg(unix)]
fn load_payload(path: &Utf8Path) -> Result<WorkerPayload, WorkerError> {
    let (dir, rel) = ambient_dir_and_path(path).map_err(|e| WorkerError::ConfigRead(e.into()))?;
    let mut f = dir
        .open(rel.as_std_path())
        .map_err(|e| WorkerError::ConfigRead(e.into()))?;
    let mut b = Vec::new();
    f.read_to_end(&mut b)
        .map_err(|e| WorkerError::ConfigRead(e.into()))?;
    serde_json::from_slice(&b).map_err(WorkerError::ConfigParse)
}

#[cfg(unix)]
fn build_runtime() -> Result<tokio::runtime::Runtime, WorkerError> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(WorkerError::RuntimeInit)
}

#[cfg(unix)]
fn extract_data_dir(settings: &postgresql_embedded::Settings) -> Result<Utf8PathBuf, WorkerError> {
    Utf8PathBuf::from_path_buf(settings.data_dir.clone())
        .map_err(|_| WorkerError::SettingsConversion("data_dir must be valid UTF-8".into()))
}

#[cfg(unix)]
fn is_setup_complete(pg: &PostgreSQL, data_dir: &Utf8Path) -> bool {
    data_dir.is_dir() && data_dir.join("PG_VERSION").exists() && pg.status() != Status::NotInstalled
}

#[cfg(unix)]
async fn run_setup(pg: &mut PostgreSQL) -> Result<(), WorkerError> {
    pg.setup()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("setup failed: {e}")))
}

#[cfg(unix)]
fn validate_data_dir(path: &Utf8Path) -> Result<bool, WorkerError> {
    has_valid_data_dir(path).map_err(|e| WorkerError::DataDirRecovery(format!("validation: {e}")))
}

#[cfg(unix)]
#[expect(clippy::cognitive_complexity, reason = "info! macro inflates score")]
fn perform_data_dir_reset(path: &Utf8Path) -> Result<(), WorkerError> {
    info!("Resetting: path={path}");
    reset_data_dir(path).map_err(|e| WorkerError::DataDirRecovery(format!("reset: {e}")))?;
    info!("Reset complete");
    Ok(())
}

#[cfg(unix)]
#[expect(clippy::cognitive_complexity, reason = "info! macro inflates score")]
fn recover_invalid_data_dir(data_dir: &Utf8Path) -> Result<(), WorkerError> {
    let exists = data_dir.exists();
    info!("Check: path={data_dir}, exists={exists}");
    if !exists {
        return Ok(());
    }
    let is_valid = validate_data_dir(data_dir)?;
    info!("Validation: path={data_dir}, valid={is_valid}");
    if !is_valid {
        perform_data_dir_reset(data_dir)?;
    }
    Ok(())
}

#[cfg(unix)]
#[expect(clippy::cognitive_complexity, reason = "info! macro inflates score")]
async fn run_postgres_setup(pg: &mut PostgreSQL, data_dir: &Utf8Path) -> Result<(), WorkerError> {
    if is_setup_complete(pg, data_dir) {
        info!("Setup complete");
        return Ok(());
    }
    recover_invalid_data_dir(data_dir)?;
    info!("Running setup");
    run_setup(pg).await
}

#[cfg(unix)]
async fn ensure_postgres_started(pg: &mut PostgreSQL, d: &Utf8Path) -> Result<(), WorkerError> {
    run_postgres_setup(pg, d).await?;
    start_if_not_started(pg).await
}

#[cfg(unix)]
async fn start_if_not_started(pg: &mut PostgreSQL) -> Result<(), WorkerError> {
    if pg.status() == Status::Started {
        info!("PostgreSQL already started, skipping redundant start");
        return Ok(());
    }

    pg.start()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("start failed: {e}")))
}

#[cfg(unix)]
async fn execute_stop(pg: &mut Option<PostgreSQL>) -> Result<(), WorkerError> {
    let pg_handle = pg
        .as_mut()
        .ok_or_else(|| WorkerError::PostgresOperation("pg handle missing during stop".into()))?;
    handle_stop_result(pg_handle.stop().await)
}

#[cfg(unix)]
fn handle_stop_result(result: Result<(), postgresql_embedded::Error>) -> Result<(), WorkerError> {
    match result {
        Ok(()) => Ok(()),
        Err(err) if stop_missing_pid_is_ok(&err) => Ok(()),
        Err(err) => Err(WorkerError::PostgresOperation(format!(
            "stop failed: {err}"
        ))),
    }
}

#[cfg(unix)]
fn apply_worker_environment(environment: &[(String, Option<PlainSecret>)]) {
    for (key, value) in environment {
        // SAFETY: worker is single-threaded; environment updates cannot race.
        match value {
            Some(v) => unsafe { env::set_var(key, v.expose()) },
            None => unsafe { env::remove_var(key) },
        }
    }
}

#[cfg(unix)]
fn stop_missing_pid_is_ok(err: &postgresql_embedded::Error) -> bool {
    match err {
        postgresql_embedded::Error::DatabaseStopError(msg)
        | postgresql_embedded::Error::IoError(msg) => {
            msg.contains("postmaster.pid") && msg.contains("does not exist")
        }
        _ => false,
    }
}

#[cfg(unix)]
fn has_valid_data_dir(data_dir: &Utf8Path) -> Result<bool, BoxError> {
    let (dir, relative) = ambient_dir_and_path(data_dir)?;
    let marker_path = relative.join("global/pg_filenode.map");
    Ok(cap_std::fs::Dir::exists(&dir, marker_path.as_std_path()))
}

#[cfg(unix)]
fn reset_data_dir(data_dir: &Utf8Path) -> Result<(), BoxError> {
    let (dir, relative) = ambient_dir_and_path(data_dir)?;
    if relative.as_str().is_empty() {
        return Err("cannot reset root directory".into());
    }

    match cap_std::fs::Dir::remove_dir_all(&dir, relative.as_std_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Stub main for non-Unix platforms (returns runtime error).
#[cfg(not(unix))]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Err("pg_worker is not supported on non-Unix platforms".into())
}

#[cfg(all(test, unix))]
#[expect(clippy::expect_used, reason = "tests may panic on setup failure")]
mod tests {
    use super::*;
    use rstest::{fixture, rstest};
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use tempfile::{TempDir, tempdir};

    #[fixture]
    fn temp_data_dir() -> (TempDir, Utf8PathBuf) {
        let temp = tempdir().expect("create temp dir");
        let data_dir = temp.path().join("data");
        let utf8 = Utf8PathBuf::from_path_buf(data_dir).expect("valid UTF-8 path");
        (temp, utf8)
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
    fn parse_args_rejects_non_utf8_config_path() {
        let args = vec![
            OsString::from("pg_worker"),
            OsString::from("setup"),
            OsStr::from_bytes(&[0x80]).to_os_string(),
        ];
        match parse_args(args.into_iter()) {
            Err(WorkerError::InvalidArgs(msg)) => {
                let lc = msg.to_lowercase();
                assert!(lc.contains("utf-8"), "should mention UTF-8, got: {msg}");
                assert!(lc.contains("config"), "should mention config, got: {msg}");
            }
            other => panic!("expected InvalidArgs, got: {other:?}"),
        }
    }

    #[rstest]
    fn valid_data_dir_detected(temp_data_dir: (TempDir, Utf8PathBuf)) {
        let (_, p) = temp_data_dir;
        fs::create_dir_all(p.join("global")).expect("mkdir");
        fs::write(p.join("global/pg_filenode.map"), "").expect("marker");
        assert!(has_valid_data_dir(&p).expect("check"), "valid dir");
    }

    #[rstest]
    fn missing_dir_is_invalid(temp_data_dir: (TempDir, Utf8PathBuf)) {
        let (_, p) = temp_data_dir;
        assert!(!has_valid_data_dir(&p).expect("check"), "missing invalid");
    }

    #[rstest]
    fn dir_without_marker_is_invalid(temp_data_dir: (TempDir, Utf8PathBuf)) {
        let (_, p) = temp_data_dir;
        fs::create_dir_all(&p).expect("mkdir");
        assert!(!has_valid_data_dir(&p).expect("check"), "no marker");
    }

    #[rstest]
    fn reset_removes_partial(temp_data_dir: (TempDir, Utf8PathBuf)) {
        let (_, p) = temp_data_dir;
        fs::create_dir_all(p.join("x")).expect("partial");
        reset_data_dir(&p).expect("reset");
        assert!(!p.exists(), "removed");
    }

    #[rstest]
    fn reset_ok_for_missing(temp_data_dir: (TempDir, Utf8PathBuf)) {
        let (_, p) = temp_data_dir;
        assert!(reset_data_dir(&p).is_ok(), "ok");
    }

    #[test]
    fn reset_errors_on_root() {
        let err = reset_data_dir(&Utf8PathBuf::from("/")).expect_err("root");
        assert!(err.to_string().to_lowercase().contains("root"), "{err}");
    }

    #[rstest]
    fn recover_skips_nonexistent(temp_data_dir: (TempDir, Utf8PathBuf)) {
        let (_, p) = temp_data_dir;
        assert!(
            recover_invalid_data_dir(&p).is_ok() && !p.exists(),
            "skip nonexistent"
        );
    }
}
