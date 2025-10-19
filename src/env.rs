//! Guards process environment mutations for deterministic orchestration.
//!
//! Note: This guard is not re-entrant. Calling `ScopedEnv::apply` whilst a
//! `ScopedEnv` is already active in the same process will deadlock on
//! `ENV_LOCK`. Keep environment mutations flat and scoped.

use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores the process environment when dropped, reverting to prior values.
#[derive(Debug)]
#[must_use = "Hold the guard until the end of the environment scope"]
pub struct ScopedEnv {
    saved: Vec<(String, Option<OsString>)>,
    #[expect(dead_code, reason = "Mutex guard keeps the lock held until drop")]
    lock: MutexGuard<'static, ()>,
}

impl ScopedEnv {
    /// Applies the supplied environment variables and returns a guard that
    /// restores the previous values when dropped.
    pub(crate) fn apply(vars: &[(String, Option<String>)]) -> Self {
        let lock = ENV_LOCK.lock().expect("environment lock poisoned");
        let mut saved = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            debug_assert!(
                !key.is_empty() && !key.contains('='),
                "invalid env var name"
            );
            let previous = env::var_os(key);
            match value {
                Some(value) => {
                    debug_assert!(
                        !value.contains('\0'),
                        "NUL bytes are not allowed in env values"
                    );
                    unsafe {
                        env::set_var(key, value);
                    }
                }
                None => unsafe {
                    env::remove_var(key);
                },
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
