//! Tests for environment scoping and logging.

use super::ScopedEnv;
use super::state::ENV_LOCK;
#[cfg(feature = "cluster-unit-tests")]
use crate::test_support::capture_info_logs;
use std::env;
use std::ffi::OsString;
use std::panic;

#[test]
fn recovers_from_poisoned_lock() {
    assert!(
        panic::catch_unwind(|| {
            let _guard =
                ScopedEnv::apply(&[(String::from("POISON_TEST"), Some(String::from("one")))]);
            panic!("intentional panic to poison the mutex");
        })
        .is_err()
    );

    let guard = ScopedEnv::apply(&[(String::from("POISON_TEST"), Some(String::from("two")))]);
    assert_eq!(env::var("POISON_TEST").as_deref(), Ok("two"));
    drop(guard);
    assert!(env::var("POISON_TEST").is_err());
}

#[test]
fn allows_reentrant_scopes() {
    let outer = ScopedEnv::apply(&[(String::from("NESTED_TEST"), Some(String::from("outer")))]);
    assert_eq!(env::var("NESTED_TEST").as_deref(), Ok("outer"));

    {
        let inner = ScopedEnv::apply(&[(String::from("NESTED_TEST"), Some(String::from("inner")))]);
        assert_eq!(env::var("NESTED_TEST").as_deref(), Ok("inner"));
        drop(inner);
    }

    assert_eq!(env::var("NESTED_TEST").as_deref(), Ok("outer"));
    drop(outer);
    assert!(env::var("NESTED_TEST").is_err());
}

#[test]
fn keeps_lock_until_last_scope_drops() {
    let outer = ScopedEnv::apply(&[(String::from("SCOPE_TEST"), Some(String::from("outer")))]);
    let inner = ScopedEnv::apply(&[(String::from("SCOPE_TEST"), Some(String::from("inner")))]);

    drop(outer);
    assert_eq!(env::var("SCOPE_TEST").as_deref(), Ok("inner"));
    assert!(
        ENV_LOCK.try_lock().is_err(),
        "mutex must remain held by inner guard"
    );

    let third = ScopedEnv::apply(&[(String::from("SCOPE_TEST"), Some(String::from("third")))]);
    assert_eq!(env::var("SCOPE_TEST").as_deref(), Ok("third"));
    drop(third);
    assert_eq!(env::var("SCOPE_TEST").as_deref(), Ok("inner"));

    drop(inner);
    let free = ENV_LOCK
        .try_lock()
        .expect("mutex should release after final scope drops");
    drop(free);
    assert!(env::var("SCOPE_TEST").is_err());
}

#[test]
fn apply_os_rejects_invalid_keys() {
    let result = panic::catch_unwind(|| {
        let invalid = vec![(OsString::from("INVALID=KEY"), Some(OsString::from("value")))];
        let _guard = ScopedEnv::apply_os(invalid);
    });

    assert!(
        result.is_err(),
        "apply_os must reject environment names containing '='"
    );
}

#[cfg(feature = "cluster-unit-tests")]
#[test]
fn logs_application_and_restoration() {
    let (logs, ()) = capture_info_logs(|| {
        let guard = ScopedEnv::apply(&[
            (String::from("OBS_ENV_APPLY"), Some(String::from("one"))),
            (String::from("OBS_ENV_CLEAR"), None),
        ]);
        drop(guard);
    });

    assert!(
        logs.iter()
            .any(|line| line.contains("applied scoped environment variables")),
        "expected application log entry, got {logs:?}"
    );
    assert!(
        logs.iter().any(|line| line.contains("OBS_ENV_APPLY=set")),
        "expected set entry, got {logs:?}"
    );
    assert!(
        logs.iter().any(|line| line.contains("OBS_ENV_CLEAR=unset")),
        "expected unset entry, got {logs:?}"
    );
    assert!(
        logs.iter()
            .any(|line| line.contains("restoring scoped environment variables")),
        "expected restoration log, got {logs:?}"
    );
}
