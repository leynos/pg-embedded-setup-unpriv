//! Environment helpers for integration tests.

use once_cell::sync::Lazy;
use std::ffi::OsString;
use std::sync::Mutex;

use temp_env::with_vars;

#[allow(dead_code)] // Some integration tests only use the type alias helpers.
static ENV_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Collection type for guarded environment variables.
pub type ScopedEnvVars = Vec<(OsString, Option<OsString>)>;

/// Builds guarded environment variables from any iterable of key/value pairs.
pub fn build_env<I, K, V>(vars: I) -> ScopedEnvVars
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    vars.into_iter()
        .map(|(key, value)| (key.into(), Some(value.into())))
        .collect()
}

/// Runs `body` with the provided environment variables temporarily set.
///
/// The guard restores any pre-existing values when `body` returns, ensuring tests do
/// not leak environment configuration across scenarios. A global mutex serialises
/// access so concurrent tests cannot interleave environment mutations.
///
/// Important: this guard is not re-entrant. Do not nest `with_scoped_env` calls, as
/// the inner invocation will deadlock waiting for the mutex held by the outer scope.
#[allow(dead_code)] // Other integration tests rely on this guard; some modules do not.
pub fn with_scoped_env<R>(
    vars: impl IntoIterator<Item = (OsString, Option<OsString>)>,
    body: impl FnOnce() -> R,
) -> R {
    let pairs: Vec<_> = vars.into_iter().collect();
    let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");
    with_vars(&pairs, body)
}
