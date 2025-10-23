//! Environment helpers for integration tests.

use once_cell::sync::Lazy;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

static ENV_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Collection type for guarded environment variables.
pub type ScopedEnvVars = Vec<(OsString, Option<OsString>)>;

/// Guard that keeps the supplied environment variables active until dropped.
#[derive(Debug)]
#[must_use = "Hold the guard until the environment scope completes"]
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
                    // SAFETY: serialised by `ENV_MUTEX` and restored before releasing it.
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

    if let Some(worker) = std::env::var_os("CARGO_BIN_EXE_pg_worker").or_else(locate_worker_binary)
    {
        env.push((OsString::from("PG_EMBEDDED_WORKER"), Some(worker)));
    }

    env
}

fn locate_worker_binary() -> Option<OsString> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_dir = deps_dir.parent()?;
    let worker_path = target_dir.join("pg_worker");
    worker_path.exists().then(|| worker_path.into_os_string())
}

/// Applies `vars` and returns a guard that keeps them active until dropped.
///
/// The guard acquires a global, non-re-entrant mutex. Nesting [`apply_env`] or
/// mixing it with [`with_scoped_env`] within the same thread will deadlock
/// because the outer guard retains the mutex until it is dropped.
pub fn apply_env(vars: ScopedEnvVars) -> ScopedEnvGuard {
    let lock = ENV_MUTEX
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut saved = Vec::with_capacity(vars.len());
    for (key, value) in vars.into_iter() {
        let previous = std::env::var_os(&key);
        match value {
            Some(ref new_value) => unsafe {
                // SAFETY: serialised by `ENV_MUTEX` and restored before releasing it.
                std::env::set_var(&key, new_value);
            },
            None => unsafe {
                // SAFETY: serialised by `ENV_MUTEX` and restored before releasing it.
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
    let _guard = apply_env(vars.into_iter().collect());
    body()
}
