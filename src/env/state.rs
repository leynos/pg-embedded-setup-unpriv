//! Thread-local state and mutex management for scoped environment guards.

use crate::observability::LOG_TARGET;
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
        if self.depth == 0 {
            self.force_restore_and_reset("ScopedEnv drop without matching apply");
            return;
        }
        self.depth -= 1;

        if !self.finish_scope(index) {
            return;
        }

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
            tracing::warn!(
                target: LOG_TARGET,
                "ENV_LOCK was poisoned; clearing poison and proceeding"
            );
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

    fn finish_scope(&mut self, index: usize) -> bool {
        {
            let Some(state) = self.stack.get_mut(index) else {
                self.force_restore_and_reset("ScopedEnv finished out of order");
                return false;
            };
            if state.finished {
                self.force_restore_and_reset("ScopedEnv finished twice");
                return false;
            }
            state.finished = true;
        }

        self.restore_finished_scopes()
    }

    fn restore_finished_scopes(&mut self) -> bool {
        while let Some(finished) = self.stack.pop() {
            if !finished.finished {
                self.stack.push(finished);
                break;
            }
            restore_saved(finished.saved);
        }
        true
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

    fn force_restore_and_reset(&mut self, reason: &str) {
        Self::log_corruption(reason);
        self.ensure_lock_for_restore();
        self.restore_all_scopes();
        self.reset_depth_and_unlock();
    }

    fn log_corruption(reason: &str) {
        tracing::error!(
            target: LOG_TARGET,
            "{reason}; restoring environment and resetting state"
        );
    }

    fn ensure_lock_for_restore(&mut self) {
        if self.lock.is_none() {
            Self::ensure_lock_is_clean();
            self.lock = Some(Self::lock_env_mutex());
        }
    }

    fn restore_all_scopes(&mut self) {
        while let Some(state) = self.stack.pop() {
            restore_saved(state.saved);
        }
    }

    fn reset_depth_and_unlock(&mut self) {
        self.depth = 0;
        if let Some(guard) = self.lock.take() {
            drop(guard);
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
