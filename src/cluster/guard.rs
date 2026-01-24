//! Lifecycle guard for a running `PostgreSQL` cluster.
//!
//! [`ClusterGuard`] manages the non-`Send` components of a cluster's lifecycle:
//! environment variable restoration and cluster shutdown. It is intentionally
//! `!Send` to ensure environment guards are dropped on the thread that created
//! them.
//!
//! # Architecture
//!
//! The guard holds:
//! - **Environment guards**: `ScopedEnv` instances that restore environment
//!   variables when dropped
//! - **Shutdown resources**: Runtime, `PostgreSQL` instance, and configuration
//!   needed to cleanly stop the cluster
//! - **Tracing span**: Keeps the cluster's observability span alive
//!
//! # Drop Behaviour
//!
//! When dropped, the guard:
//! 1. Stops the `PostgreSQL` cluster (gracefully if possible)
//! 2. Restores environment variables to their pre-cluster state
//!
//! # Thread Safety
//!
//! `ClusterGuard` is intentionally `!Send` because `ScopedEnv` uses thread-local
//! storage to track environment changes. Dropping on a different thread would
//! corrupt the environment restoration logic.

use super::runtime_mode::ClusterRuntime;
use super::shutdown;
use crate::TestBootstrapSettings;
use crate::env::ScopedEnv;
use crate::observability::LOG_TARGET;
use postgresql_embedded::PostgreSQL;
use tracing::{info, warn};

/// Lifecycle guard for a running `PostgreSQL` cluster.
///
/// This guard manages cluster shutdown and environment restoration. It is
/// intentionally `!Send` to ensure thread-local environment state is handled
/// correctly.
///
/// # Obtaining a Guard
///
/// Use [`TestCluster::new_split()`](super::TestCluster::new_split) to obtain
/// a handle and guard pair:
///
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// let (handle, guard) = TestCluster::new_split()?;
/// // handle: ClusterHandle (Send + Sync)
/// // guard: ClusterGuard (!Send, manages lifecycle)
///
/// // When guard drops, cluster shuts down and environment is restored
/// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
/// ```
///
/// # Shared Cluster Pattern
///
/// For shared clusters that should run for the entire process lifetime,
/// the guard must be explicitly forgotten to prevent shutdown on drop.
/// Use [`std::mem::forget`] to keep the cluster running:
///
/// ```no_run
/// use std::sync::OnceLock;
/// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
///
/// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
///
/// fn shared_handle() -> &'static ClusterHandle {
///     SHARED.get_or_init(|| {
///         let (handle, guard) = TestCluster::new_split()
///             .expect("cluster bootstrap failed");
///         // Forget the guard to prevent shutdown - cluster runs for process lifetime
///         std::mem::forget(guard);
///         handle
///     })
/// }
/// ```
///
/// **Warning**: Dropping the guard shuts down the cluster. Do not use the
/// handle after the guard has been dropped unless the guard was forgotten.
#[derive(Debug)]
pub struct ClusterGuard {
    /// Runtime mode: either owns a runtime (sync) or runs on caller's runtime (async).
    pub(super) runtime: ClusterRuntime,
    /// The `PostgreSQL` instance, taken during shutdown.
    pub(super) postgres: Option<PostgreSQL>,
    /// Bootstrap settings needed for shutdown operations.
    pub(super) bootstrap: TestBootstrapSettings,
    /// Whether the cluster is managed via the worker subprocess.
    pub(super) is_managed_via_worker: bool,
    /// Environment variables applied to the cluster.
    pub(super) env_vars: Vec<(String, Option<String>)>,
    /// Optional worker environment guard.
    pub(super) worker_guard: Option<ScopedEnv>,
    /// Main environment guard (must drop last among env guards).
    pub(super) _env_guard: ScopedEnv,
    /// Keeps the cluster span alive for the lifetime of the guard.
    pub(super) _cluster_span: tracing::Span,
}

// Note: ClusterGuard is !Send because it contains ScopedEnv which has
// PhantomData<Rc<()>>. This is verified by the test in tests/test_cluster.rs
// which uses a compile_fail doctest to ensure the type cannot be sent across
// threads.

impl ClusterGuard {
    /// Extends the guard to cover an additional scoped environment.
    ///
    /// Primarily used by fixtures that need to ensure `PG_EMBEDDED_WORKER`
    /// remains set for the duration of the cluster lifetime.
    #[must_use]
    pub fn with_worker_guard(mut self, worker_guard: Option<ScopedEnv>) -> Self {
        self.worker_guard = worker_guard;
        self
    }
}

impl Drop for ClusterGuard {
    fn drop(&mut self) {
        if self.should_skip_shutdown() {
            return;
        }
        self.perform_shutdown();
        // Environment guards drop after this block, restoring the process state.
    }
}

impl ClusterGuard {
    /// Returns true if shutdown should be skipped.
    ///
    /// Shutdown is skipped if the cluster was already stopped (e.g., via
    /// `stop_async()`) or if the postgres handle was never initialised.
    const fn should_skip_shutdown(&self) -> bool {
        self.postgres.is_none() && !self.is_managed_via_worker
    }

    /// Performs cluster shutdown, logging and delegating to the appropriate path.
    fn perform_shutdown(&mut self) {
        let context = shutdown::stop_context(&self.bootstrap.settings);
        let is_async = self.runtime.is_async();
        info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = self.is_managed_via_worker,
            async_mode = is_async,
            "stopping embedded postgres cluster"
        );

        if is_async {
            self.drop_async_cluster(&context);
        } else {
            self.drop_sync_cluster(&context);
        }
    }

    /// Asynchronous drop path: best-effort cleanup for async clusters.
    fn drop_async_cluster(&mut self, context: &str) {
        shutdown::drop_async_cluster(shutdown::DropContext {
            is_managed_via_worker: self.is_managed_via_worker,
            postgres: &mut self.postgres,
            bootstrap: &self.bootstrap,
            env_vars: &self.env_vars,
            context,
        });
    }

    /// Synchronous drop path: stops the cluster using the owned runtime.
    fn drop_sync_cluster(&mut self, context: &str) {
        let ClusterRuntime::Sync(ref runtime) = self.runtime else {
            // Should never happen: drop_sync_cluster is only called for sync mode.
            warn!(
                target: LOG_TARGET,
                "drop_sync_cluster called with non-sync runtime mode; skipping shutdown"
            );
            return;
        };

        shutdown::drop_sync_cluster(
            runtime,
            shutdown::DropContext {
                is_managed_via_worker: self.is_managed_via_worker,
                postgres: &mut self.postgres,
                bootstrap: &self.bootstrap,
                env_vars: &self.env_vars,
                context,
            },
        );
    }
}
