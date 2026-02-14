//! Shared cluster singleton implementations.
//!
//! This module provides thread-safe singleton patterns for shared test clusters.
//! The clusters are initialised lazily on first access and persist for the entire
//! process lifetime.

use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{BootstrapError, BootstrapResult};
use crate::{ClusterHandle, TestCluster};

use super::fixtures::ensure_worker_env;

// ============================================================================
// Shared cluster handle singleton
// ============================================================================

/// Global state for the shared cluster handle singleton.
///
/// Uses `OnceLock<Mutex<...>>` to support fallible initialisation whilst
/// maintaining thread-safe singleton semantics. The `Mutex` protects
/// initialisation; once complete, the pointer is stable.
///
/// The handle is leaked to obtain a `'static` reference for the entire
/// process lifetime.
static SHARED_CLUSTER_HANDLE: OnceLock<Mutex<SharedHandleState>> = OnceLock::new();

/// State machine for lazy cluster handle initialisation.
enum SharedHandleState {
    /// Not yet initialised.
    Uninitialised,
    /// Successfully initialised with a leaked handle reference.
    Initialised(&'static ClusterHandle),
    /// Initialisation failed; stores the original error for reconstruction.
    Failed(Arc<BootstrapError>),
}

/// Returns a reference to the shared cluster handle.
///
/// The cluster is initialised lazily on first access using [`OnceLock`] for
/// thread-safe singleton semantics. Subsequent calls return the same handle
/// instance, eliminating per-test bootstrap overhead.
///
/// This function returns a [`ClusterHandle`] which is `Send + Sync`, making it
/// suitable for use in contexts requiring thread safety (e.g., rstest fixtures
/// with timeouts).
///
/// # Errors
///
/// Returns a [`BootstrapError`] if the cluster cannot be started. Once
/// initialisation fails, subsequent calls return an error with the same
/// [`BootstrapErrorKind`](crate::error::BootstrapErrorKind) and a message
/// indicating the previous failure.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads concurrently. The first
/// caller to reach the initialisation path will bootstrap the cluster while
/// other callers wait.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::shared_cluster_handle;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let handle = shared_cluster_handle()?;
/// assert!(handle.database_exists("postgres")?);
///
/// // Second call returns the same instance
/// let handle2 = shared_cluster_handle()?;
/// assert!(std::ptr::eq(handle, handle2));
/// # Ok(())
/// # }
/// ```
pub fn shared_cluster_handle() -> BootstrapResult<&'static ClusterHandle> {
    let mutex = SHARED_CLUSTER_HANDLE.get_or_init(|| Mutex::new(SharedHandleState::Uninitialised));
    let mut guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    match &*guard {
        SharedHandleState::Initialised(handle) => Ok(*handle),
        SharedHandleState::Failed(original_err) => {
            let report = color_eyre::eyre::eyre!(
                "shared cluster initialisation previously failed: {:?}",
                original_err
            );
            Err(BootstrapError::new(original_err.kind(), report))
        }
        SharedHandleState::Uninitialised => {
            let worker_guard = ensure_worker_env();
            match TestCluster::new_split() {
                Ok((handle, cluster_guard)) => {
                    // Attach worker guard to cluster guard, then leak it.
                    // The guard manages shutdown; leaking it means the cluster
                    // runs for the process lifetime.
                    let guarded = cluster_guard.with_worker_guard(worker_guard);

                    // Best-effort atexit registration. On Unix this sends
                    // SIGTERM to the postmaster on process exit. On other
                    // platforms it is a silent no-op. Failure is non-fatal:
                    // the cluster remains usable, but the postmaster may be
                    // orphaned when the process terminates.
                    best_effort_register_shutdown_hook_for_handle(&handle);

                    // Leak the guard so the cluster keeps running.
                    // This is intentional: shared clusters live for the entire
                    // process lifetime.
                    std::mem::forget(guarded);

                    // Leak the handle to get a 'static reference.
                    let leaked: &'static ClusterHandle = Box::leak(Box::new(handle));
                    *guard = SharedHandleState::Initialised(leaked);
                    Ok(leaked)
                }
                Err(err) => {
                    // Store error info for subsequent callers to retrieve.
                    let stored = Arc::new(BootstrapError::new(
                        err.kind(),
                        color_eyre::eyre::eyre!("bootstrap failed: {:?}", err),
                    ));
                    *guard = SharedHandleState::Failed(stored);
                    // Return the original error with full diagnostics.
                    Err(err)
                }
            }
        }
    }
}

// ============================================================================
// Shared shutdown hook helper
// ============================================================================

/// Best-effort atexit registration for a [`ClusterHandle`].
///
/// Failure is non-fatal so that shared-handle initialisation succeeds on
/// platforms where the hook cannot be registered (non-Unix) and on the
/// rare occasion that `libc::atexit` fails on Unix.
fn best_effort_register_shutdown_hook_for_handle(handle: &ClusterHandle) {
    if let Err(err) = handle.register_shutdown_on_exit() {
        tracing::debug!(
            target: crate::observability::LOG_TARGET,
            error = %err,
            "shutdown hook registration failed; postmaster may be orphaned on exit"
        );
    }
}

/// Best-effort atexit registration for a leaked `TestCluster`.
///
/// The hook may already be registered by `shared_cluster_handle()` in the
/// same process, so any error is swallowed with a debug log.
fn best_effort_register_shutdown_hook(cluster: &TestCluster) {
    if let Err(err) = cluster.register_shutdown_on_exit() {
        tracing::debug!(
            target: crate::observability::LOG_TARGET,
            error = %err,
            "failed to register shutdown hook (may already be registered)"
        );
    }
}

// ============================================================================
// Legacy shared cluster singleton (for backward compatibility)
// ============================================================================

/// Global state for the legacy shared cluster singleton.
///
/// Uses `OnceLock<Mutex<...>>` to support fallible initialisation whilst
/// maintaining thread-safe singleton semantics.
static SHARED_CLUSTER: OnceLock<Mutex<SharedClusterState>> = OnceLock::new();

/// State machine for lazy cluster initialisation (legacy API).
enum SharedClusterState {
    /// Not yet initialised.
    Uninitialised,
    /// Successfully initialised with a pointer wrapper.
    Initialised(SharedClusterPtr),
    /// Initialisation failed; stores the original error for reconstruction.
    Failed(Arc<BootstrapError>),
}

/// Wrapper around raw pointer to `TestCluster` that implements `Send + Sync`.
///
/// Required because `TestCluster` is `!Send` (contains `ScopedEnv` with
/// `PhantomData<Rc<()>>`). The pointer is safe to share across threads because:
/// 1. The cluster is only initialised once and never moved.
/// 2. All access goes through immutable references.
/// 3. The cluster's public API is thread-safe (database operations use
///    independent connections).
struct SharedClusterPtr(*const TestCluster);

// SAFETY: SharedClusterPtr upholds the following invariants:
// 1. The pointer targets a `Box::leak`ed allocation that outlives all references.
// 2. No mutable access occurs through this pointer; all usage is via `&TestCluster`.
// 3. `TestCluster` methods internally handle synchronisation (each database
//    operation creates an independent connection).
unsafe impl Send for SharedClusterPtr {}
unsafe impl Sync for SharedClusterPtr {}

/// Returns a reference to the shared test cluster.
///
/// The cluster is initialised lazily on first access using [`OnceLock`] for
/// thread-safe singleton semantics. Subsequent calls return the same cluster
/// instance, eliminating per-test bootstrap overhead.
///
/// # Recommendation
///
/// Prefer [`shared_cluster_handle()`] for new code. It returns a `Send + Sync`
/// handle suitable for rstest fixtures with timeouts and other thread-safe
/// contexts. This function is retained for backward compatibility with
/// existing tests that depend on the `TestCluster` type.
///
/// # Errors
///
/// Returns a [`BootstrapError`] if the cluster cannot be started. Once
/// initialisation fails, subsequent calls return an error with the same
/// [`BootstrapErrorKind`](crate::error::BootstrapErrorKind) and a message
/// indicating the previous failure.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads concurrently. The first
/// caller to reach the initialisation path will bootstrap the cluster while
/// other callers wait. All callers receive the same cluster reference.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::shared_cluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = shared_cluster()?;
/// assert!(cluster.database_exists("postgres")?);
///
/// // Second call returns the same instance
/// let cluster2 = shared_cluster()?;
/// assert!(std::ptr::eq(cluster, cluster2));
/// # Ok(())
/// # }
/// ```
pub fn shared_cluster() -> BootstrapResult<&'static TestCluster> {
    let mutex = SHARED_CLUSTER.get_or_init(|| Mutex::new(SharedClusterState::Uninitialised));
    let mut guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    match &*guard {
        SharedClusterState::Initialised(ptr) => {
            // SAFETY: The pointer was created from Box::leak and is valid forever.
            Ok(unsafe { &*ptr.0 })
        }
        SharedClusterState::Failed(original_err) => {
            let report = color_eyre::eyre::eyre!(
                "shared cluster initialisation previously failed: {:?}",
                original_err
            );
            Err(BootstrapError::new(original_err.kind(), report))
        }
        SharedClusterState::Uninitialised => {
            let worker_guard = ensure_worker_env();
            match TestCluster::new() {
                Ok(new_cluster) => {
                    let guarded_cluster = new_cluster.with_worker_guard(worker_guard);
                    // Leak the cluster to get a stable pointer.
                    // This is intentional: the shared cluster lives for the
                    // entire process lifetime and is never dropped.
                    let leaked: &'static TestCluster = Box::leak(Box::new(guarded_cluster));

                    best_effort_register_shutdown_hook(leaked);

                    let ptr = SharedClusterPtr(std::ptr::from_ref::<TestCluster>(leaked));
                    *guard = SharedClusterState::Initialised(ptr);
                    Ok(leaked)
                }
                Err(err) => {
                    // Store error info for subsequent callers to retrieve.
                    let stored = Arc::new(BootstrapError::new(
                        err.kind(),
                        color_eyre::eyre::eyre!("bootstrap failed: {:?}", err),
                    ));
                    *guard = SharedClusterState::Failed(stored);
                    // Return the original error with full diagnostics.
                    Err(err)
                }
            }
        }
    }
}
