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

use std::cell::{Cell, RefCell};
use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static RECURSION_DEPTH: Cell<usize> = const { Cell::new(0) };
    static HELD_LOCK: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
}

/// Restores the process environment when dropped, reverting to prior values.
#[derive(Debug)]
#[must_use = "Hold the guard until the end of the environment scope"]
pub struct ScopedEnv {
    index: usize,
}

#[derive(Debug)]
struct GuardState {
    saved: Vec<(OsString, Option<OsString>)>,
    finished: bool,
}

thread_local! {
    static SCOPE_STACK: RefCell<Vec<GuardState>> = const { RefCell::new(Vec::new()) };
}

fn acquire_env_lock() {
    RECURSION_DEPTH.with(|depth| {
        let current = depth.get();
        if current == 0 {
            HELD_LOCK.with(|held| {
                debug_assert!(held.borrow().is_none(), "ScopedEnv depth desynchronised");
                let guard = ENV_LOCK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                held.replace(Some(guard));
            });
        }
        depth.set(current + 1);
        SCOPE_STACK.with(|cells| {
            let stack = cells.borrow();
            let active = stack.iter().filter(|state| !state.finished).count();
            debug_assert_eq!(
                active, current,
                "active scope count must track recursion depth"
            );
        });
    });
}

fn release_env_lock() {
    RECURSION_DEPTH.with(|depth| {
        let current = depth.get();
        debug_assert!(current > 0, "ScopedEnv lock released without acquisition");
        let next = current - 1;
        depth.set(next);
        if next == 0 {
            SCOPE_STACK.with(|stack| {
                debug_assert!(
                    stack.borrow().is_empty(),
                    "scope stack must be empty when recursion depth resets"
                );
            });
            HELD_LOCK.with(|held| {
                let previous = held.replace(None);
                debug_assert!(previous.is_some(), "ScopedEnv lock missing at depth zero");
                drop(previous);
            });
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
        acquire_env_lock();
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
        let index = SCOPE_STACK.with(|cells| {
            let mut stack = cells.borrow_mut();
            let index = stack.len();
            stack.push(GuardState {
                saved,
                finished: false,
            });
            index
        });

        Self { index }
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
        restore_finished_scopes(self.index);
        release_env_lock();
    }
}

fn restore_finished_scopes(index: usize) {
    SCOPE_STACK.with(|cells| {
        let mut stack = cells.borrow_mut();
        let Some(state) = stack.get_mut(index) else {
            debug_assert!(false, "scope index should exist when finishing");
            return;
        };
        debug_assert!(!state.finished, "scope finished twice");
        state.finished = true;

        while stack.last().is_some_and(|candidate| candidate.finished) {
            if let Some(finished) = stack.pop() {
                restore_saved(finished.saved);
            } else {
                debug_assert!(false, "finished scope disappeared");
                break;
            }
        }
    });
}

fn restore_saved(saved: Vec<(OsString, Option<OsString>)>) {
    for (key, value) in saved.into_iter().rev() {
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
}

#[cfg(test)]
mod tests {
    use super::{ENV_LOCK, ScopedEnv};

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
}
