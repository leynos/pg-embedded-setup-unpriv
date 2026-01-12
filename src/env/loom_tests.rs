//! Loom-backed concurrency checks for `ScopedEnv`.

use super::state::{EnvLockOps, ThreadStateInner};
use super::{ScopedEnvCore, ThreadStateAccess};
use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;
use std::cell::RefCell;

loom::lazy_static! {
    static ref LOOM_ENV_LOCK: loom::sync::Mutex<()> = loom::sync::Mutex::new(());
}

struct LoomEnvLock;

impl EnvLockOps for LoomEnvLock {
    type Guard = loom::sync::MutexGuard<'static, ()>;

    fn lock_env_mutex() -> Self::Guard {
        LOOM_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ensure_lock_is_clean() {}
}

loom::thread_local! {
    static LOOM_THREAD_STATE: RefCell<ThreadStateInner<LoomEnvLock>> =
        RefCell::new(ThreadStateInner::new());
}

struct LoomThreadStateAccess;

impl ThreadStateAccess for LoomThreadStateAccess {
    type Lock = LoomEnvLock;

    fn with_state<F, R>(f: F) -> R
    where
        F: FnOnce(&mut ThreadStateInner<Self::Lock>) -> R,
    {
        LOOM_THREAD_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            f(&mut state)
        })
    }
}

type LoomScopedEnv = ScopedEnvCore<LoomThreadStateAccess>;

fn run_loom_model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut builder = loom::model::Builder::new();
    builder.max_threads = 3;
    builder.max_branches = 64;
    builder.preemption_bound = Some(3);
    builder.check(f);
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_serialises_concurrent_scopes() {
    run_loom_model(|| {
        let active_counter = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..2 {
            let active_clone = Arc::clone(&active_counter);
            handles.push(thread::spawn(move || {
                let empty: &[(String, Option<String>)] = &[];
                let _guard = LoomScopedEnv::apply(empty);

                let previous = active_clone.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    previous, 0,
                    "ScopedEnv must serialise concurrent environment scopes"
                );
                let current = active_clone.fetch_sub(1, Ordering::SeqCst);
                assert_eq!(current, 1, "ScopedEnv must release the scope cleanly");
            }));
        }

        for handle in handles {
            handle.join().expect("thread should join cleanly");
        }

        assert_eq!(active_counter.load(Ordering::SeqCst), 0);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_allows_reentrant_scopes_on_one_thread() {
    run_loom_model(|| {
        let active_counter = Arc::new(AtomicUsize::new(0));
        let active_thread = Arc::clone(&active_counter);

        let handle = thread::spawn(move || {
            let empty: &[(String, Option<String>)] = &[];
            let outer = LoomScopedEnv::apply(empty);
            let inner = LoomScopedEnv::apply(empty);

            let previous = active_thread.fetch_add(1, Ordering::SeqCst);
            assert_eq!(previous, 0, "outer scope should hold the lock");
            let current = active_thread.fetch_sub(1, Ordering::SeqCst);
            assert_eq!(current, 1, "inner scope should not release the lock");

            drop(inner);
            drop(outer);
        });

        handle.join().expect("thread should join cleanly");
        assert_eq!(active_counter.load(Ordering::SeqCst), 0);
    });
}
