//! Thread-local state and mutex management for scoped environment guards.

use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub(crate) struct GuardState {
    pub(crate) saved: Vec<(OsString, Option<OsString>)>,
    pub(crate) finished: bool,
}

#[derive(Debug)]
pub(crate) struct ThreadState {
    pub(crate) depth: usize,
    pub(crate) lock: Option<MutexGuard<'static, ()>>,
    pub(crate) stack: Vec<GuardState>,
}

impl ThreadState {
    pub const fn new() -> Self {
        Self {
            depth: 0,
            lock: None,
            stack: Vec::new(),
        }
    }

    pub fn enter_scope<I>(&mut self, vars: I) -> usize
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

    pub fn exit_scope(&mut self, index: usize) {
        debug_assert!(self.depth > 0, "ScopedEnv drop without matching apply");
        self.depth -= 1;

        self.finish_scope(index);

        if self.depth == 0 {
            self.release_outermost_lock();
        }
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
        assert!(
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
        debug_assert!(
            !key.is_empty() && !Self::contains_equals(key),
            "invalid env var name: {key:?}"
        );
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

    fn finish_scope(&mut self, index: usize) {
        {
            let state = self
                .stack
                .get_mut(index)
                .unwrap_or_else(|| panic!("ScopedEnv finished out of order: index {index}"));
            debug_assert!(
                !state.finished,
                "ScopedEnv finished twice for index {index}"
            );
            state.finished = true;
        }

        self.restore_finished_scopes();
    }

    fn restore_finished_scopes(&mut self) {
        while self
            .stack
            .last()
            .is_some_and(|candidate| candidate.finished)
        {
            if let Some(finished) = self.stack.pop() {
                restore_saved(finished.saved);
            } else {
                debug_assert!(
                    false,
                    "Finished scope missing from stack during restoration"
                );
                break;
            }
        }
    }

    fn release_outermost_lock(&mut self) {
        debug_assert!(
            self.stack.is_empty(),
            "ScopedEnv stack must be empty once recursion depth reaches zero",
        );
        if let Some(guard) = self.lock.take() {
            drop(guard);
        } else {
            debug_assert!(false, "ScopedEnv mutex guard missing at depth zero");
        }
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
