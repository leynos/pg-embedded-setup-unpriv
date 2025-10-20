//! Environment helpers for integration tests.

use once_cell::sync::Lazy;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

static ENV_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Collection type for guarded environment variables.
pub type ScopedEnvVars = Vec<(OsString, Option<OsString>)>;

/// Guard that keeps the supplied environment variables active until dropped.
#[derive(Debug)]
pub struct ScopedEnvGuard {
    saved: Vec<(OsString, Option<OsString>)>,
    #[expect(dead_code, reason = "Mutex guard keeps the lock held until drop")]
    lock: MutexGuard<'static, ()>,
}

impl Drop for ScopedEnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(previous) => unsafe {
                    // SAFETY: serialised by `ENV_MUTEX` and restored before releasing it.
                    std::env::set_var(&key, previous);
                },
                None => unsafe {
                    std::env::remove_var(&key);
                },
            }
        }
        // `lock` drops here, releasing the mutex once restoration completes.
    }
}

/// Builds guarded environment variables from any iterable of key/value pairs.
pub fn build_env<I, K, V>(vars: I) -> ScopedEnvVars
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut env: ScopedEnvVars = vars
        .into_iter()
        .map(|(key, value)| (key.into(), Some(value.into())))
        .collect();

    if env
        .iter()
        .any(|(key, _)| key.as_os_str() == OsStr::new("PG_EMBEDDED_WORKER"))
    {
        return env;
    }

    if let Some(worker) = option_env!("CARGO_BIN_EXE_pg_worker") {
        env.push((OsString::from("PG_EMBEDDED_WORKER"), Some(worker.into())));
    }

    env
}

/// Applies `vars` and returns a guard that keeps them active until dropped.
pub fn apply_env(vars: ScopedEnvVars) -> ScopedEnvGuard {
    let lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let mut saved = Vec::with_capacity(vars.len());
    for (key, value) in vars.into_iter() {
        let previous = std::env::var_os(&key);
        match value {
            Some(ref new_value) => unsafe {
                // SAFETY: serialised by `ENV_MUTEX` and restored before releasing it.
                std::env::set_var(&key, new_value);
            },
            None => unsafe {
                std::env::remove_var(&key);
            },
        }
        saved.push((key, previous));
    }
    ScopedEnvGuard { saved, lock }
}

/// Runs `body` with the provided environment variables temporarily set.
///
/// The guard restores any pre-existing values when `body` returns, ensuring tests do
/// not leak environment configuration across scenarios. A global mutex serialises
/// access so concurrent tests cannot interleave environment mutations.
///
/// Important: this guard is not re-entrant. Do not nest `with_scoped_env` calls, as
/// the inner invocation will deadlock waiting for the mutex held by the outer scope.
pub fn with_scoped_env<R>(
    vars: impl IntoIterator<Item = (OsString, Option<OsString>)>,
    body: impl FnOnce() -> R,
) -> R {
    let guard = apply_env(vars.into_iter().collect());
    let result = body();
    drop(guard);
    result
}
