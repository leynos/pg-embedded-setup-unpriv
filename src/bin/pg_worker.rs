//! Privileged `PostgreSQL` bootstrap worker: deserializes [`WorkerPayload`] from `config.json` and
//! invokes lifecycle calls, allowing the caller to demote credentials before spawning the child.

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

/// Marker file that indicates a valid `PostgreSQL` data directory.
///
/// This path is created by `initdb` during successful initialization and is used
/// to distinguish complete setups from partial or interrupted ones. The stub at
/// `tests/support/fixtures/pg_ctl_stub.sh` must create this file to match.
#[cfg(unix)]
const PG_FILENODE_MAP_MARKER: &str = "global/pg_filenode.map";

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
    #[error("cleanup failed: {0}")]
    CleanupFailed(String),
    #[error("data dir recovery: {0}")]
    DataDirRecovery(String),
}

#[cfg(unix)]
#[derive(Debug)]
enum Operation {
    Setup,
    Start,
    Stop,
    Cleanup,
    CleanupFull,
}

#[cfg(unix)]
impl Operation {
    fn parse(arg: &OsStr) -> Result<Self, WorkerError> {
        match arg.to_string_lossy().as_ref() {
            "setup" => Ok(Self::Setup),
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "cleanup" => Ok(Self::Cleanup),
            "cleanup-full" => Ok(Self::CleanupFull),
            other => Err(WorkerError::InvalidArgs(format!(
                "unknown operation '{other}'; expected setup, start, stop, cleanup, or cleanup-full"
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
    apply_worker_environment(&payload.environment);
    match op {
        Operation::Cleanup => execute_cleanup(&data_dir, None, None),
        Operation::CleanupFull => {
            let install_dir = extract_install_dir(&settings)?;
            let install_root = extract_install_root(&settings)?;
            execute_cleanup(&data_dir, Some(&install_dir), install_root.as_deref())
        }
        Operation::Setup | Operation::Start | Operation::Stop => {
            let runtime = build_runtime()?;
            let mut pg = Some(PostgreSQL::new(settings));
            runtime.block_on(async {
                let pg_err = || WorkerError::PostgresOperation("no pg".into());
                match op {
                    Operation::Setup => {
                        run_postgres_setup(pg.as_mut().ok_or_else(pg_err)?, &data_dir).await
                    }
                    Operation::Start => {
                        ensure_postgres_started(pg.as_mut().ok_or_else(pg_err)?, &data_dir).await?;
                        if let Some(i) = pg.take() {
                            std::mem::forget(i); // Leak to keep PostgreSQL running after worker exit.
                        }
                        Ok(())
                    }
                    Operation::Stop => execute_stop(&mut pg).await,
                    Operation::Cleanup | Operation::CleanupFull => Ok(()),
                }
            })
        }
    }
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
    let cfg_err = |e: BoxError| WorkerError::ConfigRead(e);
    let (dir, rel) = ambient_dir_and_path(path).map_err(|e| cfg_err(e.into()))?;
    let mut f = dir.open(rel.as_std_path()).map_err(|e| cfg_err(e.into()))?;
    let mut b = Vec::new();
    f.read_to_end(&mut b).map_err(|e| cfg_err(e.into()))?;
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
fn extract_install_dir(
    settings: &postgresql_embedded::Settings,
) -> Result<Utf8PathBuf, WorkerError> {
    Utf8PathBuf::from_path_buf(settings.installation_dir.clone()).map_err(|_| {
        WorkerError::SettingsConversion("installation_dir must be valid UTF-8".into())
    })
}

#[cfg(unix)]
fn extract_install_root(
    settings: &postgresql_embedded::Settings,
) -> Result<Option<Utf8PathBuf>, WorkerError> {
    let pgpass = Utf8PathBuf::from_path_buf(settings.password_file.clone())
        .map_err(|_| WorkerError::SettingsConversion("password_file must be valid UTF-8".into()))?;
    Ok(pgpass.parent().map(ToOwned::to_owned))
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
mod log {
    //! Logging helpers for recovery flow; extracted to avoid cognitive complexity inflation.
    use super::{Utf8Path, info};
    pub fn check(p: &Utf8Path, exists: bool) {
        info!("Check: path={p}, exists={exists}");
    }
    pub fn valid(p: &Utf8Path, v: bool) {
        info!("Validation: path={p}, valid={v}");
    }
}

#[cfg(unix)]
fn perform_data_dir_reset(path: &Utf8Path) -> Result<(), WorkerError> {
    info!("Reset: path={path}");
    reset_data_dir(path).map_err(|e| WorkerError::DataDirRecovery(format!("reset: {e}")))
}

#[cfg(unix)]
fn is_dir_empty(path: &Utf8Path) -> Result<bool, BoxError> {
    let (dir, rel) = ambient_dir_and_path(path)?;
    Ok(dir.read_dir(rel.as_std_path())?.next().is_none())
}

#[cfg(unix)]
fn recover_invalid_data_dir(data_dir: &Utf8Path) -> Result<(), WorkerError> {
    let exists = data_dir.exists();
    log::check(data_dir, exists);
    if !exists {
        return Ok(());
    }
    let is_valid = has_valid_data_dir(data_dir)
        .map_err(|e| WorkerError::DataDirRecovery(format!("validation: {e}")))?;
    log::valid(data_dir, is_valid);
    let is_empty = is_dir_empty(data_dir)
        .map_err(|e| WorkerError::DataDirRecovery(format!("empty check: {e}")))?;
    if !is_valid && !is_empty {
        perform_data_dir_reset(data_dir)?;
    }
    Ok(())
}

#[cfg(unix)]
#[expect(
    clippy::cognitive_complexity,
    reason = "lint triggers (16/9) despite simple 6-line body; caused by async desugaring"
)]
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
        info!("PostgreSQL already started");
        return Ok(());
    }
    pg.start()
        .await
        .map_err(|e| WorkerError::PostgresOperation(format!("start failed: {e}")))
}

#[cfg(unix)]
async fn execute_stop(pg: &mut Option<PostgreSQL>) -> Result<(), WorkerError> {
    let h = pg
        .as_mut()
        .ok_or_else(|| WorkerError::PostgresOperation("pg handle missing".into()))?;
    match h.stop().await {
        Ok(()) => Ok(()),
        Err(e) if stop_missing_pid_is_ok(&e) => Ok(()),
        Err(e) => Err(WorkerError::PostgresOperation(format!("stop failed: {e}"))),
    }
}

#[cfg(unix)]
fn collect_removal_error(failures: &mut Vec<String>, path: &Utf8Path, label: &str) {
    if let Err(err) = remove_dir_all_if_exists(path, label) {
        failures.push(err);
    }
}

#[cfg(unix)]
fn execute_cleanup(
    data_dir: &Utf8Path,
    install_dir: Option<&Utf8Path>,
    install_root: Option<&Utf8Path>,
) -> Result<(), WorkerError> {
    let mut failures = Vec::new();
    collect_removal_error(&mut failures, data_dir, "data");
    if let Some(path) = install_dir {
        collect_removal_error(&mut failures, path, "installation");
    }
    if let Some(path) = install_root {
        let already_removed = install_dir.is_some_and(|install_path| install_path == path);
        if !already_removed {
            collect_removal_error(&mut failures, path, "installation-root");
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorkerError::CleanupFailed(failures.join("; ")))
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
    use postgresql_embedded::Error::{DatabaseStopError, IoError};
    matches!(err, DatabaseStopError(m) | IoError(m) if m.contains("postmaster.pid") && m.contains("does not exist"))
}

#[cfg(unix)]
fn has_valid_data_dir(data_dir: &Utf8Path) -> Result<bool, BoxError> {
    let (dir, rel) = ambient_dir_and_path(data_dir)?;
    Ok(dir.exists(rel.join(PG_FILENODE_MAP_MARKER).as_std_path()))
}

#[cfg(unix)]
fn reset_data_dir(data_dir: &Utf8Path) -> Result<(), BoxError> {
    let (dir, rel) = ambient_dir_and_path(data_dir)?;
    if rel.as_str().is_empty() {
        return Err("cannot reset root directory".into());
    }
    match dir.remove_dir_all(rel.as_std_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum RemovalOutcome {
    Removed,
    Missing,
}

#[cfg(unix)]
fn remove_dir_all_if_exists(path: &Utf8Path, label: &str) -> Result<(), String> {
    match try_remove_dir_all(path) {
        Ok(outcome) => {
            log_removal_outcome(outcome, path, label);
            Ok(())
        }
        Err(err) => Err(format!(
            "failed to remove {label} directory {}: {err}",
            path.as_str()
        )),
    }
}

#[cfg(unix)]
fn log_removal_outcome(outcome: RemovalOutcome, path: &Utf8Path, label: &str) {
    match outcome {
        RemovalOutcome::Removed => log_dir_removed(path, label),
        RemovalOutcome::Missing => log_dir_missing(path, label),
    }
}

#[cfg(unix)]
fn log_dir_removed(path: &Utf8Path, label: &str) {
    info!(path = %path, label, "removed postgres directory");
}

#[cfg(unix)]
fn log_dir_missing(path: &Utf8Path, label: &str) {
    info!(path = %path, label, "postgres directory already removed");
}

#[cfg(unix)]
fn try_remove_dir_all(path: &Utf8Path) -> Result<RemovalOutcome, std::io::Error> {
    match std::fs::remove_dir_all(path.as_std_path()) {
        Ok(()) => Ok(RemovalOutcome::Removed),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(RemovalOutcome::Missing),
        Err(err) => Err(err),
    }
}

/// Stub main for non-Unix platforms (returns runtime error).
#[cfg(not(unix))]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Err("pg_worker is not supported on non-Unix platforms".into())
}

#[cfg(all(test, unix))]
mod tests {
    //! Unit tests for `pg_worker` data directory recovery and argument parsing.

    use super::*;
    use rstest::{fixture, rstest};
    use std::{
        ffi::{OsStr, OsString},
        fs,
        os::unix::ffi::OsStrExt,
    };
    use tempfile::{TempDir, tempdir};
    type R<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
    type TempDir2 = R<(TempDir, Utf8PathBuf)>;
    fn ensure(cond: bool, msg: &str) -> R {
        if cond { Ok(()) } else { Err(msg.into()) }
    }

    #[fixture]
    fn temp_data_dir() -> TempDir2 {
        let temp = tempdir()?;
        let p = Utf8PathBuf::from_path_buf(temp.path().join("data"))
            .map_err(|p| format!("not UTF-8: {}", p.display()))?;
        Ok((temp, p))
    }

    #[test]
    fn rejects_extra_argument() -> R {
        let args = ["pg_worker", "setup", "/tmp/config.json", "unexpected"].map(OsString::from);
        let err = run_worker(args.into_iter()).err().ok_or("expected error")?;
        ensure(
            err.to_string().contains("unexpected extra argument"),
            "wrong err",
        )
    }

    #[test]
    fn parse_args_rejects_non_utf8_config_path() -> R {
        let args = [
            OsString::from("pg_worker"),
            OsString::from("setup"),
            OsStr::from_bytes(&[0x80]).to_os_string(),
        ];
        match parse_args(args.into_iter()) {
            Err(WorkerError::InvalidArgs(m)) => ensure(
                m.to_lowercase().contains("utf-8") && m.contains("config"),
                "bad msg",
            ),
            o => Err(format!("expected InvalidArgs: {o:?}").into()),
        }
    }

    #[rstest]
    fn valid_data_dir_detected(temp_data_dir: TempDir2) -> R {
        let (_, p) = temp_data_dir?;
        fs::create_dir_all(p.join("global"))?;
        fs::write(p.join(PG_FILENODE_MAP_MARKER), "")?;
        ensure(has_valid_data_dir(&p)?, "should be valid")
    }

    #[rstest]
    fn missing_dir_is_invalid(temp_data_dir: TempDir2) -> R {
        ensure(!has_valid_data_dir(&temp_data_dir?.1)?, "should be invalid")
    }

    #[rstest]
    fn dir_without_marker_is_invalid(temp_data_dir: TempDir2) -> R {
        let (_, p) = temp_data_dir?;
        fs::create_dir_all(&p)?;
        ensure(!has_valid_data_dir(&p)?, "should be invalid")
    }

    #[rstest]
    fn reset_removes_partial(temp_data_dir: TempDir2) -> R {
        let (_, p) = temp_data_dir?;
        fs::create_dir_all(p.join("x"))?;
        reset_data_dir(&p)?;
        ensure(!p.exists(), "should be removed")
    }

    #[rstest]
    fn reset_ok_for_missing(temp_data_dir: TempDir2) -> R {
        reset_data_dir(&temp_data_dir?.1)
    }

    #[test]
    fn reset_errors_on_root() -> R {
        let e = reset_data_dir(&Utf8PathBuf::from("/"))
            .err()
            .ok_or("expected err")?;
        ensure(
            e.to_string().to_lowercase().contains("root"),
            "should mention root",
        )
    }

    #[rstest]
    fn recover_skips_nonexistent(temp_data_dir: TempDir2) -> R {
        let (_, p) = temp_data_dir?;
        recover_invalid_data_dir(&p)?;
        ensure(!p.exists(), "should not exist")
    }

    #[rstest]
    fn recover_skips_empty_dir(temp_data_dir: TempDir2) -> R {
        let (_, p) = temp_data_dir?;
        fs::create_dir_all(&p)?;
        recover_invalid_data_dir(&p)?;
        ensure(p.exists(), "empty dir should remain")
    }
}
