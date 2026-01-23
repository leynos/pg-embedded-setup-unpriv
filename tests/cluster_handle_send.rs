//! Tests verifying `ClusterHandle` Send/Sync traits and thread-safety patterns.
//!
//! These tests validate Issue #84: enabling `TestCluster` usage in `Send`-bounded
//! contexts through the `ClusterHandle` type.
#![cfg(unix)]

use std::sync::OnceLock;
use std::thread;

use pg_embedded_setup_unpriv::{ClusterGuard, ClusterHandle};

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

// Note: We cannot use a compile_fail doctest here because ClusterGuard being
// !Send is enforced by containing ScopedEnv which has PhantomData<Rc<()>>.
// The following test documents this constraint - if ClusterGuard becomes Send,
// this comment should be updated or removed.
//
// To verify !Send manually, uncomment this block - it should fail to compile:
// ```
// const _: () = {
//     const fn assert_send<T: Send>() {}
//     assert_send::<ClusterGuard>();
// };
// ```

// ============================================================================
// Runtime tests for thread-safety patterns
// ============================================================================

/// Verifies that `ClusterHandle` can be stored in a static `OnceLock`.
///
/// This pattern is essential for shared cluster fixtures that avoid
/// per-test bootstrap overhead.
#[test]
fn cluster_handle_works_with_oncelock() {
    use pg_embedded_setup_unpriv::ExecutionPrivileges;
    use pg_embedded_setup_unpriv::test_support::dummy_settings;

    static SHARED: OnceLock<ClusterHandle> = OnceLock::new();

    let handle = SHARED.get_or_init(|| {
        // Create a dummy handle for testing (doesn't start a real cluster)
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
#[test]
fn cluster_handle_can_be_sent_to_thread() {
    use pg_embedded_setup_unpriv::ExecutionPrivileges;
    use pg_embedded_setup_unpriv::test_support::dummy_settings;

    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let handle = ClusterHandle::from(bootstrap);

    // Move handle to another thread
    let join_handle = thread::spawn(move || {
        // Access handle methods from the spawned thread
        let settings = handle.settings();
        settings.port // Return something to prove we accessed it
    });

    let _port = join_handle
        .join()
        .expect("thread should complete successfully");
}

/// Verifies that `ClusterHandle` can be shared across threads via `Arc`.
#[test]
fn cluster_handle_can_be_shared_via_arc() {
    use pg_embedded_setup_unpriv::ExecutionPrivileges;
    use pg_embedded_setup_unpriv::test_support::dummy_settings;
    use std::sync::Arc;

    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let handle = Arc::new(ClusterHandle::from(bootstrap));

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
