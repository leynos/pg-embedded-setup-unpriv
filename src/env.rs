//! Guards process environment mutations for deterministic orchestration.
//!
//! Note: This guard is not re-entrant. Calling `ScopedEnv::apply` whilst a
//! `ScopedEnv` is already active in the same process will deadlock on
//! `ENV_LOCK`. Keep environment mutations flat and scoped.
//!
//! # Example
//! ```no_run
//! use pg_embedded_setup_unpriv::env::ScopedEnv;
//!
//! # fn main() {
//! let guard = ScopedEnv::apply(&[(
//!     "PGUSER".into(),
//!     Some("postgres".into()),
//! )]);
//! // Perform work that relies on the scoped environment here.
//! drop(guard); // Restores the original environment once the scope ends.
//! # }
//! ```
//!
//! Avoid nesting guards; the mutex serialises mutations and will deadlock when
//! re-acquired by the same thread.

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
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut saved = Vec::with_capacity(vars.len());
        for (key, current_value) in vars {
            debug_assert!(
                !key.is_empty() && !key.contains('='),
                "invalid env var name"
            );
            let previous = env::var_os(key);
            match current_value {
                Some(new_value) => {
                    debug_assert!(
                        !new_value.contains('\0'),
                        "NUL bytes are not allowed in env values"
                    );
                    unsafe {
                        // SAFETY: `ENV_LOCK` serialises changes. Drop restores
                        // recorded values before releasing the lock.
                        env::set_var(key, new_value);
                    }
                }
                None => unsafe {
                    // SAFETY: `ENV_LOCK` serialises changes. Drop restores
                    // recorded values before releasing the lock.
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
                    // SAFETY: restoration still holds `ENV_LOCK`, so no other
                    // mutations can observe intermediate states.
                    env::set_var(&key, previous);
                },
                None => unsafe {
                    // SAFETY: restoration still holds `ENV_LOCK`, so no other
                    // mutations can observe intermediate states.
                    env::remove_var(&key);
                },
            }
        }
        // `lock` drops here, releasing the mutex once restoration completes.
    }
}

#[cfg(test)]
mod tests {
    use super::ScopedEnv;

    use std::env;
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
}
