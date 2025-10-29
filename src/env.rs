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

use std::cell::RefCell;
use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

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

#[derive(Debug)]
struct ThreadState {
    depth: usize,
    lock: Option<MutexGuard<'static, ()>>,
    stack: Vec<GuardState>,
}

impl ThreadState {
    const fn new() -> Self {
        Self {
            depth: 0,
            lock: None,
            stack: Vec::new(),
        }
    }

    fn enter_scope<I>(&mut self, vars: I) -> usize
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        self.acquire_lock_if_needed();

        self.depth += 1;

        let saved = self.apply_env_vars(vars);

        let index = self.stack.len();
        self.stack.push(GuardState {
            saved,
            finished: false,
        });
        index
    }

    fn acquire_lock_if_needed(&mut self) {
        if self.depth > 0 {
            return;
        }

        assert!(
            self.lock.is_none(),
            "ScopedEnv depth desynchronised: mutex still held",
        );
        Self::ensure_lock_is_clean();
        let guard = Self::lock_env_mutex();
        self.lock = Some(guard);
    }

    fn ensure_lock_is_clean() {
        if ENV_LOCK.is_poisoned() {
            tracing::warn!("ENV_LOCK was poisoned; clearing poison and proceeding");
            ENV_LOCK.clear_poison();
        }
    }

    fn lock_env_mutex() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn apply_env_vars<I>(&self, vars: I) -> Vec<(OsString, Option<OsString>)>
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        debug_assert!(
            self.lock.is_some(),
            "ScopedEnv must hold the mutex before mutating the environment",
        );
        let mut saved = Vec::new();
        for (key, new_value) in vars {
            Self::validate_env_key(&key);
            let previous = Self::apply_single_var(&key, new_value);
            saved.push((key, previous));
        }
        saved
    }

    fn validate_env_key(key: &OsString) {
        assert!(
            !key.is_empty(),
            "ScopedEnv received an empty environment variable name"
        );
        assert!(
            !Self::contains_equals(key),
            "ScopedEnv received an environment variable name containing '='"
        );
    }

    #[cfg(unix)]
    fn contains_equals(key: &OsString) -> bool {
        use std::os::unix::ffi::OsStrExt;

        key.as_os_str().as_bytes().contains(&b'=')
    }

    #[cfg(windows)]
    fn contains_equals(key: &OsString) -> bool {
        use std::os::windows::ffi::OsStrExt;

        key.as_os_str()
            .encode_wide()
            .any(|value| value == u16::from(b'='))
    }

    #[cfg(not(any(unix, windows)))]
    fn contains_equals(key: &OsString) -> bool {
        key.to_string_lossy().contains('=')
    }

    fn apply_single_var(key: &OsString, new_value: Option<OsString>) -> Option<OsString> {
        let previous = env::var_os(key);
        match new_value {
            Some(value) => unsafe {
                // SAFETY: `ENV_LOCK` serialises changes. Drop restores
                // recorded values before releasing the lock.
                env::set_var(key, value);
            },
            None => unsafe {
                // SAFETY: `ENV_LOCK` serialises changes. Drop restores
                // recorded values before releasing the lock.
                env::remove_var(key);
            },
        }
        previous
    }

    fn exit_scope(&mut self, index: usize) {
        assert!(self.depth > 0, "ScopedEnv drop without matching apply");
        self.depth -= 1;

        let state = self
            .stack
            .get_mut(index)
            .unwrap_or_else(|| panic!("ScopedEnv finished out of order: index {index}"));
        assert!(
            !state.finished,
            "ScopedEnv finished twice for index {index}"
        );
        state.finished = true;

        while self
            .stack
            .last()
            .is_some_and(|candidate| candidate.finished)
        {
            let Some(finished) = self.stack.pop() else {
                panic!("Finished scope missing from stack during restoration");
            };
            restore_saved(finished.saved);
        }

        if self.depth == 0 {
            assert!(
                self.stack.is_empty(),
                "ScopedEnv stack must be empty once recursion depth reaches zero",
            );
            let guard = self
                .lock
                .take()
                .unwrap_or_else(|| panic!("ScopedEnv mutex guard missing at depth zero"));
            drop(guard);
        }
    }
}

thread_local! {
    static THREAD_STATE: RefCell<ThreadState> = const { RefCell::new(ThreadState::new()) };
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
        Self::apply_os(owned)
    }

    /// Applies environment variables provided as `OsString` pairs by any owned iterator.
    pub(crate) fn apply_os<I>(vars: I) -> Self
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        let index = THREAD_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            state.enter_scope(vars)
        });
        Self { index }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        THREAD_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            state.exit_scope(self.index);
        });
    }
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
    use std::ffi::OsString;
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

    #[test]
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
}
