//! Environment helpers for integration tests.
#![allow(dead_code)] // Some integration suites reuse this module without exercising all helpers.

use std::ffi::OsString;

use temp_env::with_vars;

/// Runs `body` with the provided environment variables temporarily set.
///
/// The guard restores any pre-existing values when `body` returns, ensuring tests do
/// not leak environment configuration across scenarios.
pub fn with_scoped_env<R>(
    vars: impl IntoIterator<Item = (OsString, Option<OsString>)>,
    body: impl FnOnce() -> R,
) -> R {
    let pairs: Vec<_> = vars.into_iter().collect();
    with_vars(&pairs, body)
}
