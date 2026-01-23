//! Tests for the `pg_worker` binary.

use super::*;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use tempfile::tempdir;

use pg_worker_helpers::{
    MockEnvironmentOperations, apply_worker_environment_with, build_settings, write_pg_ctl_stub,
    write_worker_config,
};

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

    let mut mock = MockEnvironmentOperations::new();
    mock.expect_set_var()
        .times(1)
        .withf(|key, value| key == "PGWORKER_SECRET_KEY" && value == "super-secret-value")
        .return_const(());
    mock.expect_remove_var()
        .times(1)
        .withf(|key| key == "PGWORKER_NONE_KEY")
        .return_const(());

    apply_worker_environment_with::<MockEnvironmentOperations>(&mock, &env_pairs);
}

#[test]
fn start_operation_does_not_stop_postgres() {
    let temp_root = tempdir().expect("create temp root");
    let install_dir = temp_root.path().join("install");
    let data_dir = temp_root.path().join("data");
    write_pg_ctl_stub(&install_dir.join("bin")).expect("write pg_ctl stub");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::write(data_dir.join("PG_VERSION"), "16\n").expect("write PG_VERSION");

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

#[test]
fn test_env_store_test_impl_get_set_and_remove() {
    let mut store = TestEnvStore::new();

    store.set("KEY", "value");
    store.remove("OTHER_KEY");

    assert_eq!(store.get("KEY"), Some("value"));
    assert_eq!(store.get("OTHER_KEY"), None);
    assert_eq!(store.get("UNSET"), None);
}
