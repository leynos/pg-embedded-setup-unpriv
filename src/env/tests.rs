//! Tests for environment scoping and logging.

use super::ScopedEnv;
use super::THREAD_STATE;
use super::state::{ENV_LOCK, ThreadState};
#[cfg(feature = "cluster-unit-tests")]
use crate::test_support::capture_info_logs;
use rstest::rstest;
use serial_test::serial;
use std::env;
use std::ffi::{OsStr, OsString};
use std::panic;
use std::sync::{Arc, Barrier, TryLockError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Helper to send a signal when dropped, used for test coordination.
struct ReleaseOnDrop {
    sender: Option<mpsc::Sender<()>>,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            // Ignore send errors - receiver may have dropped after a test failure.
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Receiver may have dropped after a test failure."
            )]
            let _ = sender.send(());
        }
    }
}

/// Helper to restore environment variable state after a test.
struct RestoreEnv {
    key: String,
    original: Option<OsString>,
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => {
                set_env_var_locked(OsStr::new(&self.key), value.as_os_str());
            }
            None => remove_env_var_locked(OsStr::new(&self.key)),
        }
    }
}

#[test]
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
fn serialises_env_across_threads() {
    let key = "THREAD_SCOPE_TEST";
    let restore_env = RestoreEnv {
        key: String::from(key),
        original: env::var_os(key),
    };
    set_env_var_locked(OsStr::new(key), OsStr::new("pre-existing"));

    let barrier = Arc::new(Barrier::new(2));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (start_tx, start_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();

    let barrier_for_a = Arc::clone(&barrier);
    let key_a = String::from(key);
    let thread_a = thread::spawn(move || {
        let guard = ScopedEnv::apply(&[(key_a.clone(), Some(String::from("one")))]);

        ready_tx.send(()).expect("ready signal must be sent");
        barrier_for_a.wait();
        release_rx.recv().expect("release signal must be sent");
        drop(guard);
    });

    let barrier_for_b = Arc::clone(&barrier);
    let key_b = String::from(key);
    let thread_b = thread::spawn(move || {
        barrier_for_b.wait();
        start_rx.recv().expect("start signal must be received");
        attempt_tx.send(()).expect("attempt signal must be sent");
        let guard = ScopedEnv::apply(&[(key_b.clone(), Some(String::from("two")))]);

        let value = env::var(&key_b).ok();
        acquired_tx
            .send(value)
            .expect("acquired value must be sent");
        drop(guard);
    });

    ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("outer guard should be ready");

    let release_guard = ReleaseOnDrop {
        sender: Some(release_tx),
    };

    start_tx.send(()).expect("start signal must be sent");
    attempt_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("second thread should attempt to acquire the guard");

    assert!(
        acquired_rx.recv_timeout(Duration::from_secs(2)).is_err(),
        "second thread must block while the outer guard holds the lock"
    );

    drop(release_guard);

    let value = acquired_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("second thread should acquire after release");
    assert_eq!(value.as_deref(), Some("two"));

    thread_a.join().expect("thread A should exit cleanly");
    thread_b.join().expect("thread B should exit cleanly");

    assert_eq!(env::var(key).as_deref(), Ok("pre-existing"));
    assert_env_lock_released();
    drop(restore_env);
}

#[test]
#[serial]
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
#[serial]
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
    assert_eq!(state.depth(), 0);
    assert!(state.is_stack_empty());
    assert!(!state.has_lock());
    assert_env_lock_released();
}

#[rstest]
#[case::corrupt_exit(CorruptionCase {
    test_name: "CORRUPT_EXIT",
    setup_guards: setup_single_guard,
    corrupt_state: apply_invalid_scope_exit,
    drop_guards: drop_guards_in_order,
    drop_message: "dropping guard after corruption should not panic",
})]
#[case::invalid_index_nested(CorruptionCase {
    test_name: "INVALID_INDEX_NESTED",
    setup_guards: setup_nested_guards,
    corrupt_state: apply_invalid_scope_exit,
    drop_guards: drop_guards_in_order,
    drop_message: "dropping guards after invalid scope exit should not panic",
})]
#[case::out_of_order_drop(CorruptionCase {
    test_name: "OUT_OF_ORDER_DROP",
    setup_guards: setup_nested_guards,
    corrupt_state: no_corruption,
    drop_guards: drop_guards_out_of_order,
    drop_message: "dropping outer guard out of order should not panic",
})]
#[serial]
fn scoped_env_recovers_from_corrupt_exit(#[case] case: CorruptionCase) {
    assert_scoped_env_recovers_from_corrupt_exit(case.test_name, |key| {
        let original = env::var_os(key);
        let guards = (case.setup_guards)(key);
        let restored = (case.corrupt_state)();

        if restored {
            assert_eq!(env::var_os(key), original);
            assert_thread_state_reset();
        }

        let drop_result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            (case.drop_guards)(guards);
        }));
        assert!(drop_result.is_ok(), "{}", case.drop_message);
        assert_eq!(env::var_os(key), original);
        assert_thread_state_reset();
        assert_env_lock_released();
    });
}

fn assert_scoped_env_recovers_from_corrupt_exit<F>(test_name: &str, setup_and_corrupt: F)
where
    F: FnOnce(&OsString),
{
    let key = OsString::from(format!("SCOPED_ENV_{test_name}"));
    setup_and_corrupt(&key);
}

enum GuardSet {
    Single(ScopedEnv),
    Nested { outer: ScopedEnv, inner: ScopedEnv },
}

#[derive(Clone, Copy)]
struct CorruptionCase {
    test_name: &'static str,
    setup_guards: fn(&OsString) -> GuardSet,
    corrupt_state: fn() -> bool,
    drop_guards: fn(GuardSet),
    drop_message: &'static str,
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

    fn drop_out_of_order(self) {
        match self {
            Self::Single(guard) => drop(guard),
            Self::Nested { outer, inner } => {
                drop(outer);
                drop(inner);
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

fn apply_invalid_scope_exit() -> bool {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        THREAD_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            state.exit_scope(usize::MAX);
        });
    }));
    assert!(result.is_ok(), "invalid scope exit should not panic");
    true
}

fn no_corruption() -> bool {
    false
}

fn drop_guards_in_order(guards: GuardSet) {
    guards.drop_in_order();
}

fn drop_guards_out_of_order(guards: GuardSet) {
    guards.drop_out_of_order();
}

fn set_env_var_locked(key: &OsStr, value: &OsStr) {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe {
        // SAFETY: Tests serialise process env access via ENV_LOCK and restore
        // original values on drop.
        env::set_var(key, value);
    }
}

fn remove_env_var_locked(key: &OsStr) {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe {
        // SAFETY: Tests serialise process env access via ENV_LOCK and restore
        // original values on drop.
        env::remove_var(key);
    }
}

fn assert_thread_state_reset() {
    THREAD_STATE.with(|cell| {
        let state = cell.borrow();
        assert_eq!(state.depth(), 0);
        assert!(state.is_stack_empty());
        assert!(!state.has_lock());
    });
}

fn assert_env_lock_released() {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match ENV_LOCK.try_lock() {
            Ok(guard) => {
                drop(guard);
                return;
            }
            Err(TryLockError::Poisoned(guard)) => {
                drop(guard);
                ENV_LOCK.clear_poison();
                return;
            }
            Err(TryLockError::WouldBlock) => {}
        }
        assert!(Instant::now() < deadline, "ENV_LOCK should be released");
        thread::yield_now();
    }
}

#[cfg(feature = "cluster-unit-tests")]
#[test]
#[serial]
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
