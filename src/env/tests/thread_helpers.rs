//! Thread coordination helpers for cross-thread environment tests.
//!
//! Provides the drop guards and spawn routines used by
//! `serialises_env_across_threads` to exercise cross-thread ordering.

use super::{ScopedEnv, remove_env_var_locked, set_env_var_locked};
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

/// Restores or removes a named env var using `set_env_var_locked` or
/// `remove_env_var_locked`.
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
        match &self.original {
            Some(value) => {
                set_env_var_locked(OsStr::new(&self.key), value.as_os_str());
            }
            None => remove_env_var_locked(OsStr::new(&self.key)),
        }
    }
}

pub(super) struct ThreadBChannels {
    pub(super) start_rx: mpsc::Receiver<()>,
    pub(super) attempt_tx: mpsc::Sender<()>,
    pub(super) acquired_tx: mpsc::Sender<Option<String>>,
}

pub(super) fn spawn_outer_guard_thread(
    key: String,
    barrier: Arc<Barrier>,
    ready_tx: mpsc::Sender<()>,
    release_rx: mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let guard = ScopedEnv::apply(&[(key, Some(String::from("one")))]);

        ready_tx.send(()).expect("ready signal must be sent");
        barrier.wait();
        release_rx.recv().expect("release signal must be sent");
        drop(guard);
    })
}

pub(super) fn spawn_inner_guard_thread(
    key: String,
    barrier: Arc<Barrier>,
    channels: ThreadBChannels,
) -> thread::JoinHandle<()> {
    let ThreadBChannels {
        start_rx,
        attempt_tx,
        acquired_tx,
    } = channels;
    thread::spawn(move || {
        barrier.wait();
        start_rx.recv().expect("start signal must be received");
        attempt_tx.send(()).expect("attempt signal must be sent");
        let guard = ScopedEnv::apply(&[(key.clone(), Some(String::from("two")))]);

        let value = env::var(&key).ok();
        acquired_tx
            .send(value)
            .expect("acquired value must be sent");
        drop(guard);
    })
}
