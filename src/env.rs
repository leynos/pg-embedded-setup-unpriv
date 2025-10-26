//! Guards process environment mutations for deterministic orchestration.
//!
//! The guard is re-entrant within a thread. Nested scopes reuse the same global
//! mutex whilst keeping track of the outer state so environment restoration
//! still occurs in reverse order.
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
//! Nested guards on the same thread reuse the held mutex whilst tracking the
//! depth so callers can compose helpers without deadlocking. Different threads
//! are still serialised.

use std::cell::Cell;
use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static RECURSION_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Restores the process environment when dropped, reverting to prior values.
#[derive(Debug)]
#[must_use = "Hold the guard until the end of the environment scope"]
pub struct ScopedEnv {
    saved: Vec<(OsString, Option<OsString>)>,
    lock: Option<MutexGuard<'static, ()>>,
}

fn acquire_env_lock() -> Option<MutexGuard<'static, ()>> {
    RECURSION_DEPTH.with(|depth| {
        let current = depth.get();
        depth.set(current + 1);
        if current == 0 {
            Some(
                ENV_LOCK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        } else {
            None
        }
    })
}

fn release_env_lock(lock: &mut Option<MutexGuard<'static, ()>>) {
    RECURSION_DEPTH.with(|depth| {
        let current = depth.get();
        debug_assert!(current > 0, "ScopedEnv lock released without acquisition");
        let next = current - 1;
        depth.set(next);
        if next == 0 {
            lock.take();
        } else {
            debug_assert!(lock.is_none());
        }
    });
}

impl ScopedEnv {
    /// Applies the supplied environment variables and returns a guard that
    /// restores the previous values when dropped.
    pub(crate) fn apply(vars: &[(String, Option<String>)]) -> Self {
        let owned: Vec<(OsString, Option<OsString>)> = vars
            .iter()
            .map(|(key, value)| {
                debug_assert!(
                    !key.is_empty() && !key.contains('='),
                    "invalid env var name"
                );
                let owned_value = value.as_ref().map(OsString::from);
                (OsString::from(key), owned_value)
            })
            .collect();
        Self::apply_os(&owned)
    }

    /// Applies environment variables provided as `OsString` pairs.
    pub(crate) fn apply_os(vars: &[(OsString, Option<OsString>)]) -> Self {
        let lock = acquire_env_lock();
        let mut saved = Vec::with_capacity(vars.len());
        for (key, current_value) in vars {
            let previous = env::var_os(key);
            match current_value {
                Some(new_value) => unsafe {
                    // SAFETY: `ENV_LOCK` serialises changes. Drop restores
                    // recorded values before releasing the lock.
                    env::set_var(key, new_value);
                },
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

    /// Applies environment variables provided by any owned iterator.
    pub(crate) fn apply_os_iter<I>(vars: I) -> Self
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        let owned: Vec<(OsString, Option<OsString>)> = vars.into_iter().collect();
        Self::apply_os(&owned)
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
        release_env_lock(&mut self.lock);
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

    #[test]
    fn allows_reentrant_scopes() {
        let outer = ScopedEnv::apply(&[(String::from("NESTED_TEST"), Some(String::from("outer")))]);
        assert_eq!(env::var("NESTED_TEST").as_deref(), Ok("outer"));

        {
            let inner =
                ScopedEnv::apply(&[(String::from("NESTED_TEST"), Some(String::from("inner")))]);
            assert_eq!(env::var("NESTED_TEST").as_deref(), Ok("inner"));
            drop(inner);
        }

        assert_eq!(env::var("NESTED_TEST").as_deref(), Ok("outer"));
        drop(outer);
        assert!(env::var("NESTED_TEST").is_err());
    }
}
