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
