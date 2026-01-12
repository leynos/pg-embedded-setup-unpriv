//! Thread coordination helpers for cross-thread environment tests.
//!
//! Provides the drop guards and spawn routines used by
//! `serialises_env_across_threads` to exercise cross-thread ordering.

use super::{ENV_LOCK, ScopedEnv, remove_env_var_unlocked, set_env_var_unlocked};
use std::env;
use std::ffi::{OsStr, OsString};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

/// Sends a unit on drop via `mpsc::Sender` and ignores send errors.
pub(super) struct ReleaseOnDrop {
    pub(super) sender: Option<mpsc::Sender<()>>,
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

/// Restores or removes a named env var while holding `ENV_LOCK`, delegating to
/// the unlocked helpers that perform the underlying mutations.
///
/// # Panic safety
///
/// `RestoreEnv::drop` acquires `ENV_LOCK`, so callers must ensure the lock is
/// not held when a `RestoreEnv` is dropped. In `serialises_env_across_threads`
/// this is enforced by calling `assert_env_lock_released()` before letting the
/// `RestoreEnv` go out of scope. Alternatively, if spawned threads panic due to
/// channel closure, the panic unwinds and drops their guards, releasing
/// `ENV_LOCK` before `RestoreEnv::drop` runs.
pub(super) struct RestoreEnv {
    pub(super) key: String,
    pub(super) original: Option<OsString>,
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &self.original {
            Some(value) => set_env_var_unlocked(OsStr::new(&self.key), value.as_os_str()),
            None => remove_env_var_unlocked(OsStr::new(&self.key)),
        }
    }
}

/// Channels and synchronisation primitives for the outer guard thread.
pub(super) struct ThreadAChannels {
    /// Barrier used to co-ordinate with other threads.
    pub(super) barrier: Arc<Barrier>,
    /// Sender used to signal readiness after applying the scoped env.
    pub(super) ready_tx: mpsc::Sender<()>,
    /// Receiver used to wait for release before dropping the guard.
    pub(super) release_rx: mpsc::Receiver<()>,
    /// Sender used to signal completion after the guard drops.
    pub(super) done_tx: mpsc::Sender<()>,
}

/// Channels used by thread B to coordinate acquisition, report state, and
/// signal completion.
pub(super) struct ThreadBChannels {
    /// Signals when thread A has instructed thread B to begin.
    pub(super) start_rx: mpsc::Receiver<()>,
    /// Notifies the main thread that thread B is attempting to acquire the lock.
    pub(super) attempt_tx: mpsc::Sender<()>,
    /// Reports the environment value observed after acquiring the guard.
    pub(super) acquired_tx: mpsc::Sender<Option<String>>,
    /// Signals that thread B has completed its work.
    pub(super) done_tx: mpsc::Sender<()>,
}

/// Spawn the inner-guard thread that blocks on the mutex, reports the value,
/// and signals completion.
pub(super) fn spawn_inner_guard_thread(
    key: String,
    channels: ThreadBChannels,
) -> thread::JoinHandle<()> {
    let ThreadBChannels {
        start_rx,
        attempt_tx,
        acquired_tx,
        done_tx,
    } = channels;
    thread::spawn(move || {
        start_rx
            .recv()
            .unwrap_or_else(|_| panic!("start signal must be received"));
        attempt_tx
            .send(())
            .unwrap_or_else(|_| panic!("attempt signal must be sent"));
        let guard = ScopedEnv::apply(&[(key.clone(), Some(String::from("two")))]);

        let value = env::var(&key).ok();
        acquired_tx
            .send(value)
            .unwrap_or_else(|_| panic!("acquired value must be sent"));
        drop(guard);
        done_tx
            .send(())
            .unwrap_or_else(|_| panic!("completion signal must be sent"));
    })
}

/// Spawn a thread that applies a scoped environment variable and waits on
/// synchronisation primitives.
///
/// # Parameters
///
/// - `key`: Environment key string to set while the scoped guard is held.
/// - `channels`: `ThreadAChannels` containing the coordination primitives:
///   - `barrier`: `Arc<Barrier>` used to co-ordinate with other threads.
///   - `ready_tx`: `mpsc::Sender<()>` used to signal readiness after applying.
///   - `release_rx`: `mpsc::Receiver<()>` used to wait for release before
///     dropping the guard.
///   - `done_tx`: `mpsc::Sender<()>` used to signal completion after the guard
///     is dropped.
///
/// # Behaviour
///
/// Calls `ScopedEnv::apply` to set the env var to "one", sends the ready
/// signal, waits on the barrier, blocks on `release_rx`, then drops the guard
/// to restore the environment and signals completion.
///
/// # Panics
///
/// Panics if the ready signal cannot be sent, if the release signal is not
/// received, or if the completion signal cannot be sent.
///
/// # Returns
///
/// Returns a `thread::JoinHandle<()>` for the spawned thread.
///
/// # Examples
///
/// ```ignore
/// let barrier = Arc::new(Barrier::new(2));
/// let (ready_tx, _ready_rx) = mpsc::channel();
/// let (_release_tx, release_rx) = mpsc::channel();
/// let (done_tx, _done_rx) = mpsc::channel();
///
/// let handle = spawn_outer_guard_thread(
///     String::from("THREAD_SCOPE_TEST"),
///     ThreadAChannels {
///         barrier: Arc::clone(&barrier),
///         ready_tx,
///         release_rx,
///         done_tx,
///     },
/// );
///
/// barrier.wait();
/// handle.join().expect("thread should exit cleanly");
/// ```
pub(super) fn spawn_outer_guard_thread(
    key: String,
    channels: ThreadAChannels,
) -> thread::JoinHandle<()> {
    let ThreadAChannels {
        barrier,
        ready_tx,
        release_rx,
        done_tx,
    } = channels;
    thread::spawn(move || {
        let guard = ScopedEnv::apply(&[(key, Some(String::from("one")))]);

        ready_tx
            .send(())
            .unwrap_or_else(|_| panic!("ready signal must be sent"));
        barrier.wait();
        release_rx
            .recv()
            .unwrap_or_else(|_| panic!("release signal must be sent"));
        drop(guard);
        done_tx
            .send(())
            .unwrap_or_else(|_| panic!("completion signal must be sent"));
    })
}
