//! Thread coordination helpers for cross-thread environment tests.
//!
//! Provides the drop guards and spawn routines used by
//! `serialises_env_across_threads` to exercise cross-thread ordering.

use super::{ENV_LOCK, ScopedEnv, remove_env_var_unlocked, set_env_var_unlocked};
use std::env;
use std::ffi::{OsStr, OsString};
use std::sync::mpsc;
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

/// Spawn the outer-guard thread that acquires the mutex, signals readiness, and
/// waits for release before dropping the guard and signalling completion.
pub(super) fn spawn_outer_guard_thread(
    key: String,
    ready_tx: mpsc::Sender<()>,
    release_rx: mpsc::Receiver<()>,
    done_tx: mpsc::Sender<()>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let guard = ScopedEnv::apply(&[(key, Some(String::from("one")))]);

        ready_tx.send(()).expect("ready signal must be sent");
        release_rx.recv().expect("release signal must be sent");
        drop(guard);
        done_tx.send(()).expect("completion signal must be sent");
    })
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
        start_rx.recv().expect("start signal must be received");
        attempt_tx.send(()).expect("attempt signal must be sent");
        let guard = ScopedEnv::apply(&[(key.clone(), Some(String::from("two")))]);

        let value = env::var(&key).ok();
        acquired_tx
            .send(value)
            .expect("acquired value must be sent");
        drop(guard);
        done_tx.send(()).expect("completion signal must be sent");
    })
}
