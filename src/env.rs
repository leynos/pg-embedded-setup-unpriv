//! Guards process environment mutations for deterministic orchestration.

use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores the process environment when dropped, reverting to prior values.
#[derive(Debug)]
pub(crate) struct ScopedEnv {
    saved: Vec<(String, Option<OsString>)>,
    #[expect(dead_code, reason = "Mutex guard keeps the lock held until drop")]
    lock: MutexGuard<'static, ()>,
}

impl ScopedEnv {
    /// Applies the supplied environment variables and returns a guard that
    /// restores the previous values when dropped.
    pub(crate) fn apply(vars: &[(String, String)]) -> Self {
        let lock = ENV_LOCK.lock().expect("environment lock poisoned");
        let mut saved = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            saved.push((key.clone(), previous));
        }
        Self { saved, lock }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(previous) => unsafe {
                    env::set_var(&key, previous);
                },
                None => unsafe {
                    env::remove_var(&key);
                },
            }
        }
        // `lock` drops here, releasing the mutex once restoration completes.
    }
}
