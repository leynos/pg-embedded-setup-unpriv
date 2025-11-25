#![expect(
    dead_code,
    reason = "utility helpers kept for optional env isolation scenarios"
)]
//! Environment helpers for isolating test scenarios.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use camino::Utf8Path;

use super::env::ScopedEnvVars;

/// Captures the process environment and restores it on drop.
pub struct EnvIsolationGuard {
    snapshot: Vec<(OsString, OsString)>,
}

impl EnvIsolationGuard {
    /// Records the current process environment for later restoration.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            snapshot: std::env::vars_os().collect(),
        }
    }
}

impl Drop for EnvIsolationGuard {
    fn drop(&mut self) {
        let saved_keys: HashSet<OsString> =
            self.snapshot.iter().map(|(key, _)| key.clone()).collect();
        let current_keys: Vec<OsString> = std::env::vars_os().map(|(key, _)| key).collect();
        for key in current_keys {
            if !saved_keys.contains(&key) {
                unsafe { remove_env_var(&key) };
            }
        }
        for (key, value) in &self.snapshot {
            unsafe { set_env_var(key, value) };
        }
    }
}

/// Sets an environment variable whilst bypassing nightly's lint.
pub unsafe fn set_env_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    // SAFETY: callers must serialise environment mutations; enforced at call sites.
    unsafe { std::env::set_var(key, value) };
}

/// Removes an environment variable whilst bypassing nightly's lint.
pub unsafe fn remove_env_var<K>(key: K)
where
    K: AsRef<OsStr>,
{
    // SAFETY: callers must serialise environment mutations; enforced at call sites.
    unsafe { std::env::remove_var(key) };
}

/// Overrides an environment variable entry in `vars`.
pub fn override_env_os(vars: &mut ScopedEnvVars, key: impl AsRef<OsStr>, value: Option<OsString>) {
    let key_ref = key.as_ref();
    if let Some((_, existing_value)) = vars
        .iter_mut()
        .find(|(candidate, _)| candidate.as_os_str() == key_ref)
    {
        *existing_value = value;
    } else {
        vars.push((key_ref.to_os_string(), value));
    }
}

/// Overrides a UTF-8 environment variable entry in `vars`.
pub fn override_env_path(vars: &mut ScopedEnvVars, key: impl AsRef<OsStr>, value: &Utf8Path) {
    override_env_os(vars, key, Some(OsString::from(value.as_str())));
}
