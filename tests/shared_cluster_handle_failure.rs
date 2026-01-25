//! Tests that `shared_cluster_handle()` caches failed initialisation.
//!
//! This test file runs in its own process (Cargo compiles each `tests/*.rs`
//! file as a separate binary), ensuring the global `OnceLock` state is isolated
//! from other tests.
//!
//! **Important**: This file must contain only this one test. Adding other tests
//! that call `shared_cluster_handle()` would interfere with the failure caching
//! verification.
#![cfg(unix)]

use std::ffi::OsStr;

use pg_embedded_setup_unpriv::test_support::shared_cluster_handle;
use pg_embedded_setup_unpriv::{BootstrapError, BootstrapErrorKind, ClusterHandle};
use tracing::warn;

/// Sets an environment variable using the established project pattern.
///
/// # Safety
///
/// Callers must ensure no other threads are reading environment variables
/// concurrently. This test runs in isolation (separate test binary).
unsafe fn set_env_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    // SAFETY: Caller guarantees thread isolation.
    unsafe { std::env::set_var(key, value) };
}

/// Removes an environment variable using the established project pattern.
///
/// # Safety
///
/// Callers must ensure no other threads are reading environment variables
/// concurrently. This test runs in isolation (separate test binary).
unsafe fn remove_env_var<K>(key: K)
where
    K: AsRef<OsStr>,
{
    // SAFETY: Caller guarantees thread isolation.
    unsafe { std::env::remove_var(key) };
}

/// Sets up the environment to force bootstrap failure.
///
/// # Safety
///
/// This function modifies environment variables, which is unsafe in
/// multi-threaded contexts. This test runs in its own process (separate
/// test binary) and is the only test in this file, so there are no other
/// threads that could be reading environment variables concurrently.
unsafe fn setup_failing_environment() {
    // SAFETY: This test runs in isolation (separate test binary with only
    // one test), so no concurrent threads are reading environment variables.
    unsafe {
        set_env_var(
            "TZDIR",
            "/nonexistent/timezone/directory/that/does/not/exist",
        );
        // Also clear TZ to ensure the bootstrap tries to read from TZDIR
        remove_env_var("TZ");
    }
}

/// Extracts the error from a result, or returns None if it succeeded.
///
/// Logs a skip message if the call unexpectedly succeeded.
fn extract_error(
    result: Result<&'static ClusterHandle, BootstrapError>,
    context: &str,
) -> Option<BootstrapError> {
    if let Err(e) = result {
        return Some(e);
    }
    warn!(
        "SKIP: shared_cluster_handle succeeded despite invalid TZDIR \
         ({}); system may have fallback timezone handling",
        context
    );
    None
}

/// Verifies that the cached error preserves the original error kind.
fn verify_error_kind_preserved(first: BootstrapErrorKind, second: BootstrapErrorKind) {
    assert_eq!(
        first, second,
        "cached error should preserve BootstrapErrorKind"
    );
}

/// Verifies that the error message indicates this is a cached failure.
fn verify_cached_error_message(error: &BootstrapError) {
    let message = format!("{error}");
    assert!(
        message.contains("previously failed"),
        "cached error message should indicate previous failure; got: {message}"
    );
}

/// Verifies that `shared_cluster_handle()` caches the error from a failed
/// initialisation attempt and returns the same error on subsequent calls.
///
/// This test forces bootstrap failure by setting TZDIR to a non-existent
/// directory, which causes timezone validation to fail.
#[test]
fn caches_failed_initialisation() {
    // Force bootstrap failure by pointing TZDIR to a non-existent directory.
    // SAFETY: See `setup_failing_environment` documentation.
    unsafe {
        setup_failing_environment();
    }

    let err1 = shared_cluster_handle();
    let Some(first_error) = extract_error(err1, "first call") else {
        return;
    };
    let first_kind = first_error.kind();

    // Second call should return cached error
    let err2 = shared_cluster_handle();
    let second_error = err2.expect_err(
        "second call to shared_cluster_handle succeeded after first call failed; caching is broken",
    );

    verify_error_kind_preserved(first_kind, second_error.kind());
    verify_cached_error_message(&second_error);

    // Third call for good measure
    let err3 = shared_cluster_handle();
    let third_error = err3.expect_err("third call succeeded unexpectedly");

    verify_error_kind_preserved(second_error.kind(), third_error.kind());
}
