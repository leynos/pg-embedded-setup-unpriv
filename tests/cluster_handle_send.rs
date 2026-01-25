//! Tests verifying `ClusterHandle` Send/Sync traits and thread-safety patterns.
//!
//! These tests validate Issue #84: enabling `TestCluster` usage in `Send`-bound
//! contexts through the `ClusterHandle` type.
#![cfg(unix)]

use std::sync::OnceLock;
use std::thread;

use pg_embedded_setup_unpriv::test_support::dummy_settings;
use pg_embedded_setup_unpriv::{ClusterGuard, ClusterHandle, ExecutionPrivileges};
use rstest::{fixture, rstest};

// ============================================================================
// Compile-time trait assertions
// ============================================================================

/// Compile-time assertion that `ClusterHandle` implements `Send`.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<ClusterHandle>();
};

/// Compile-time assertion that `ClusterHandle` implements `Sync`.
const _: () = {
    const fn assert_sync<T: Sync>() {}
    assert_sync::<ClusterHandle>();
};

/// Compile-time assertion that `ClusterHandle` can be stored in `OnceLock`.
///
/// This is the primary use case from Issue #84: shared cluster fixtures.
const _: () = {
    const fn assert_oncelock_compatible<T: Send + Sync>() {}
    assert_oncelock_compatible::<ClusterHandle>();
};

// ============================================================================
// Compile-time assertion that ClusterGuard is !Send
// ============================================================================

// Note: ClusterGuard being !Send is enforced by containing ScopedEnv which has
// PhantomData<Rc<()>>. The following test documents this constraint - if
// ClusterGuard becomes Send, this comment should be updated or removed.
//
// To verify !Send manually, uncomment this block - it should fail to compile:
// ```
// const _: () = {
//     const fn assert_send<T: Send>() {}
//     assert_send::<ClusterGuard>();
// };
// ```

// ============================================================================
// Test fixtures
// ============================================================================

/// Creates a dummy `ClusterHandle` for testing thread-safety patterns.
///
/// This fixture provides a handle that doesn't start a real cluster,
/// suitable for verifying Send/Sync behaviour.
#[fixture]
fn dummy_handle() -> ClusterHandle {
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    ClusterHandle::from(bootstrap)
}

// ============================================================================
// Runtime tests for thread-safety patterns
// ============================================================================

/// Verifies that `ClusterHandle` can be stored in a static `OnceLock`.
///
/// This pattern is essential for shared cluster fixtures that avoid
/// per-test bootstrap overhead.
///
/// Note: This test intentionally creates its own handle inside the closure
/// rather than using the `dummy_handle` fixture. Using a fixture would pass
/// a stack-allocated handle that could be invalidated if this test runs
/// multiple times (rstest parameterization) or in parallel, leading to
/// undefined behaviour when the `OnceLock` outlives the fixture's scope.
#[test]
fn cluster_handle_works_with_oncelock() {
    static SHARED: OnceLock<ClusterHandle> = OnceLock::new();

    let handle = SHARED.get_or_init(|| {
        let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        ClusterHandle::from(bootstrap)
    });

    // Second access returns the same instance
    let handle2 = SHARED.get().expect("should be initialised");
    assert!(
        std::ptr::eq(handle, handle2),
        "OnceLock should return the same handle instance"
    );
}

/// Verifies that `ClusterHandle` can be moved across thread boundaries.
///
/// This is required for rstest fixtures with timeouts, which spawn the test
/// body in a separate thread.
#[rstest]
fn cluster_handle_can_be_sent_to_thread(dummy_handle: ClusterHandle) {
    // Move handle to another thread
    let join_handle = thread::spawn(move || {
        // Access handle methods from the spawned thread
        let settings = dummy_handle.settings();
        settings.port // Return something to prove access worked
    });

    let _port = join_handle
        .join()
        .expect("thread should complete successfully");
}

/// Verifies that `ClusterHandle` can be shared across threads via `Arc`.
#[rstest]
fn cluster_handle_can_be_shared_via_arc(dummy_handle: ClusterHandle) {
    use std::sync::Arc;

    let handle = Arc::new(dummy_handle);

    let handle_clone = Arc::clone(&handle);
    let join_handle = thread::spawn(move || {
        // Access the shared handle from another thread
        handle_clone.settings().port
    });

    // Access from main thread simultaneously
    let main_port = handle.settings().port;
    let spawned_port = join_handle
        .join()
        .expect("thread should complete successfully");

    assert_eq!(main_port, spawned_port, "both threads see the same port");
}

// ============================================================================
// Guard !Send verification (runtime check)
// ============================================================================

/// Documents that `ClusterGuard` cannot be sent across threads.
///
/// This test exists to document the intentional `!Send` constraint rather than
/// to verify it at runtime (which isn't possible without `compile_fail`).
#[test]
fn cluster_guard_is_not_send_documented() {
    // ClusterGuard contains ScopedEnv which has PhantomData<Rc<()>>,
    // making it !Send. This is intentional - environment guards must be
    // dropped on the thread that created them.
    //
    // The following would fail to compile:
    // ```
    // let guard: ClusterGuard = ...;
    // std::thread::spawn(move || drop(guard));
    // ```
    //
    // This constraint ensures thread-local environment state is handled correctly.

    // Type-level assertion that ClusterGuard exists (proves the type is accessible)
    fn _type_exists(_: Option<ClusterGuard>) {}
}

// ============================================================================
// Shared cluster handle caching behaviour
// ============================================================================

// Note: Testing `shared_cluster_handle()` caching directly is problematic because:
//
// 1. **Success caching**: The function uses a global `OnceLock`, so once
//    initialised, the state cannot be reset. Calling `shared_cluster_handle()`
//    in a test would interfere with other tests that use the shared cluster.
//
// 2. **Failure caching**: Similarly, simulating a failure would poison the
//    global state, preventing other tests from using the shared cluster.
//
// The caching behaviour is implicitly tested by integration tests that call
// `shared_cluster_handle()` multiple times and verify they receive the same
// instance (via pointer equality checks in fixtures).
//
// For explicit caching tests, a separate test binary with isolation would be
// required. The current test suite validates the core functionality through:
// - `cluster_handle_works_with_oncelock`: Verifies OnceLock compatibility
// - Integration tests using `shared_test_cluster_handle` fixture
