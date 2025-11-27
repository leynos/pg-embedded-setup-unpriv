//! Environment helpers for integration tests.

use std::ffi::{OsStr, OsString};

use pg_embedded_setup_unpriv::ScopedEnv;
use pg_embedded_setup_unpriv::test_support;

/// Collection type for guarded environment variables.
pub type ScopedEnvVars = Vec<(OsString, Option<OsString>)>;

/// Guard that keeps the supplied environment variables active until dropped.
pub type ScopedEnvGuard = ScopedEnv;

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

/// Locates the `pg_worker` binary for integration tests.
///
/// Resolution proceeds in two steps:
/// 1. Checks for an un-suffixed binary at `target/{profile}/pg_worker` (e.g., from `cargo run`).
/// 2. Falls back to scanning `target/{profile}/deps` for hashed binaries (e.g., `pg_worker-<hash>`),
///    excluding `.d` dependency metadata files.
///
/// Returns `None` if the binary cannot be located.
fn locate_worker_binary() -> Option<OsString> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_dir = deps_dir.parent()?;

    // Prefer an un-suffixed binary if one exists (for example when built via `cargo run`).
    let direct = target_dir.join("pg_worker");
    if direct.is_file() {
        return Some(direct.into_os_string());
    }

    // Fallback: search hashed binaries under target/{profile}/deps.
    let entries = std::fs::read_dir(deps_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_worker_binary(&path) {
            return Some(path.into_os_string());
        }
    }

    None
}

/// Determines whether a path points to a valid `pg_worker` binary.
///
/// A valid binary:
/// - has a filename starting with `pg_worker`,
/// - is not a `.d` dependency metadata file, and
/// - is a regular file.
fn is_worker_binary(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    if !name.starts_with("pg_worker") {
        return false;
    }

    // Exclude .d files (dependency metadata).
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("d"))
    {
        return false;
    }

    path.is_file()
}

/// Applies `vars` and returns a guard that keeps them active until dropped.
///
/// The guard relies on the library's re-entrant [`ScopedEnv`] implementation, so
/// nested scopes on the same thread share the mutex whilst recording the outer
/// state. Scopes on different threads still serialise to avoid interleaving
/// process-level environment mutations.
pub fn apply_env(vars: ScopedEnvVars) -> ScopedEnvGuard {
    test_support::scoped_env(vars)
}

/// Runs `body` with the provided environment variables temporarily set.
///
/// The guard restores any pre-existing values when `body` returns, ensuring tests do
/// not leak environment configuration across scenarios. A global mutex serialises
/// access so concurrent tests cannot interleave environment mutations. Calls on the
/// same thread are re-entrant, enabling helpers to compose without risking
/// deadlocks.
///
pub fn with_scoped_env<R>(
    vars: impl IntoIterator<Item = (OsString, Option<OsString>)>,
    body: impl FnOnce() -> R,
) -> R {
    let _guard = apply_env(vars.into_iter().collect());
    body()
}
