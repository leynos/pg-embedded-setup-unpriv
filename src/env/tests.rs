//! Tests for environment scoping and logging.

use super::ScopedEnv;
use super::THREAD_STATE;
use super::state::{ENV_LOCK, ThreadState};
#[cfg(feature = "cluster-unit-tests")]
use crate::test_support::capture_info_logs;
use rstest::rstest;
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

#[test]
fn thread_state_recovers_from_invalid_index() {
    let key = OsString::from("THREAD_STATE_INVALID_INDEX");
    let original = env::var_os(&key);
    let mut state = ThreadState::new();
    let index = state.enter_scope(vec![(key.clone(), Some(OsString::from("value")))]);

    assert_eq!(env::var_os(&key), Some(OsString::from("value")));

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        state.exit_scope(index + 1);
    }));
    assert!(result.is_ok(), "invalid scope exit should not panic");

    assert_eq!(env::var_os(&key), original);
    assert_eq!(state.depth, 0);
    assert!(state.stack.is_empty());
    assert!(state.lock.is_none());
}

#[rstest]
#[case::corrupt_exit(
    "CORRUPT_EXIT",
    setup_single_guard,
    "dropping guard after corruption should not panic"
)]
#[case::invalid_index_nested(
    "INVALID_INDEX_NESTED",
    setup_nested_guards,
    "dropping guards after invalid scope exit should not panic"
)]
fn scoped_env_recovers_from_corrupt_exit(
    #[case] test_name: &str,
    #[case] setup_guards: fn(&OsString) -> GuardSet,
    #[case] drop_message: &str,
) {
    assert_scoped_env_recovers_from_corrupt_exit(test_name, |key, _original_value| {
        let original = env::var_os(key);
        let guards = setup_guards(key);

        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            THREAD_STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                state.exit_scope(usize::MAX);
            });
        }));
        assert!(result.is_ok(), "invalid scope exit should not panic");

        assert_eq!(env::var_os(key), original);
        let drop_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            guards.drop_in_order();
        }));
        assert!(drop_result.is_ok(), "{}", drop_message);
        assert_eq!(env::var_os(key), original);
    });
}

#[test]
fn scoped_env_recovers_from_out_of_order_drop() {
    assert_scoped_env_recovers_from_corrupt_exit("OUT_OF_ORDER_DROP", |key, _original_value| {
        let original = env::var_os(key);
        let outer = ScopedEnv::apply_os(vec![(key.clone(), Some(OsString::from("outer")))]);
        let inner = ScopedEnv::apply_os(vec![(key.clone(), Some(OsString::from("inner")))]);

        let drop_outer = panic::catch_unwind(panic::AssertUnwindSafe(|| drop(outer)));
        assert!(
            drop_outer.is_ok(),
            "dropping outer guard out of order should not panic"
        );
        assert_eq!(env::var_os(key), Some(OsString::from("inner")));

        let drop_inner = panic::catch_unwind(panic::AssertUnwindSafe(|| drop(inner)));
        assert!(
            drop_inner.is_ok(),
            "dropping inner guard after out-of-order drop should not panic"
        );
        assert_eq!(env::var_os(key), original);
    });
}

fn assert_scoped_env_recovers_from_corrupt_exit<F>(test_name: &str, setup_and_corrupt: F)
where
    F: FnOnce(&OsString, &OsString),
{
    let key = OsString::from(format!("SCOPED_ENV_{test_name}"));
    let original = env::var_os(&key);
    let fallback = OsString::new();
    let original_value = original.as_ref().unwrap_or(&fallback);

    setup_and_corrupt(&key, original_value);

    assert_eq!(env::var_os(&key), original);
}

enum GuardSet {
    Single(ScopedEnv),
    Nested { outer: ScopedEnv, inner: ScopedEnv },
}

impl GuardSet {
    fn drop_in_order(self) {
        match self {
            Self::Single(guard) => drop(guard),
            Self::Nested { outer, inner } => {
                drop(inner);
                drop(outer);
            }
        }
    }
}

fn setup_single_guard(key: &OsString) -> GuardSet {
    GuardSet::Single(ScopedEnv::apply_os(vec![(
        key.clone(),
        Some(OsString::from("value")),
    )]))
}

fn setup_nested_guards(key: &OsString) -> GuardSet {
    let outer = ScopedEnv::apply_os(vec![(key.clone(), Some(OsString::from("outer")))]);
    let inner = ScopedEnv::apply_os(vec![(key.clone(), Some(OsString::from("inner")))]);
    GuardSet::Nested { outer, inner }
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
