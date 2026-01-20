//! Drop-time cleanup helpers for embedded clusters.

use std::fmt::Display;

use postgresql_embedded::PostgreSQL;
use tokio::time;

use super::runtime::build_runtime;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::{ClusterRuntime, TestCluster};
use crate::TestBootstrapSettings;
use crate::observability::LOG_TARGET;

impl TestCluster {
    /// Best-effort cleanup for async clusters dropped without `stop_async()`.
    ///
    /// Attempts to spawn cleanup on the current runtime handle if available.
    /// For worker-managed clusters, attempts to invoke the worker stop operation.
    pub(super) fn drop_async_cluster(&mut self, context: &str) {
        Self::warn_async_drop_without_stop(context);

        if self.is_managed_via_worker {
            self.drop_async_worker_managed(context);
        } else if let Some(postgres) = self.postgres.take() {
            self.drop_async_in_process(context, postgres);
        }
        // If neither worker-managed nor has postgres handle, already cleaned up via stop_async().
    }

    /// Best-effort worker stop for async clusters dropped without `stop_async()`.
    fn drop_async_worker_managed(&self, context: &str) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            Self::error_no_runtime_for_cleanup(context);
            return;
        };

        let bootstrap = self.bootstrap.clone();
        let env_vars = self.env_vars.clone();
        let owned_context = context.to_owned();

        drop(handle.spawn(spawn_worker_stop_task(bootstrap, env_vars, owned_context)));
    }

    /// Best-effort in-process stop for async clusters dropped without `stop_async()`.
    fn drop_async_in_process(&self, context: &str, postgres: PostgreSQL) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            Self::error_no_runtime_for_cleanup(context);
            return;
        };

        spawn_async_cleanup(&handle, postgres, self.bootstrap.shutdown_timeout);
    }

    fn warn_async_drop_without_stop(context: &str) {
        tracing::warn!(
            target: LOG_TARGET,
            context = %context,
            concat!(
                "async TestCluster dropped without calling stop_async(); ",
                "attempting best-effort cleanup"
            )
        );
    }

    fn error_no_runtime_for_cleanup(context: &str) {
        tracing::error!(
            target: LOG_TARGET,
            context = %context,
            "no async runtime available for cleanup; resources may leak"
        );
    }

    pub(super) fn warn_stop_failure(context: &str, err: &impl Display) {
        tracing::warn!(
            target: LOG_TARGET,
            context = %context,
            error = %err,
            "failed to stop embedded postgres instance"
        );
    }

    pub(super) fn warn_stop_timeout(timeout_secs: u64, context: &str) {
        tracing::warn!(
            target: LOG_TARGET,
            context = %context,
            timeout_secs = timeout_secs,
            "stop() timed out; proceeding with drop"
        );
    }

    /// Synchronous drop path: stops the cluster using the owned runtime.
    pub(super) fn drop_sync_cluster(&mut self, context: &str) {
        let ClusterRuntime::Sync(ref runtime) = self.runtime else {
            // Should never happen: drop_sync_cluster is only called for sync mode.
            return;
        };

        if self.is_managed_via_worker {
            let invoker = ClusterWorkerInvoker::new(runtime, &self.bootstrap, &self.env_vars);
            if let Err(err) = invoker.invoke_as_root(super::worker_operation::WorkerOperation::Stop)
            {
                Self::warn_stop_failure(context, &err);
            }
            return;
        }

        let Some(postgres) = self.postgres.take() else {
            return;
        };

        let timeout = self.bootstrap.shutdown_timeout;
        let timeout_secs = timeout.as_secs();
        let outcome = runtime.block_on(async { time::timeout(timeout, postgres.stop()).await });

        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(err)) => Self::warn_stop_failure(context, &err),
            Err(_) => Self::warn_stop_timeout(timeout_secs, context),
        }
    }
}

/// Spawns async cleanup of a `PostgreSQL` instance on the provided runtime handle.
///
/// The task is fire-and-forget; errors during shutdown are logged at debug level.
fn spawn_async_cleanup(
    handle: &tokio::runtime::Handle,
    postgres: PostgreSQL,
    timeout: std::time::Duration,
) {
    drop(handle.spawn(async move {
        match time::timeout(timeout, postgres.stop()).await {
            Ok(Ok(())) => {
                tracing::debug!(target: LOG_TARGET, "async cleanup completed successfully");
            }
            Ok(Err(err)) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    error = %err,
                    "async cleanup failed during postgres stop"
                );
            }
            Err(_) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    timeout_secs = timeout.as_secs(),
                    "async cleanup timed out"
                );
            }
        }
    }));
}

/// Spawns a blocking task to stop a worker-managed cluster.
///
/// Used by the async drop path to invoke the worker stop operation without
/// blocking the current async context.
#[expect(
    clippy::cognitive_complexity,
    reason = "reported complexity stems from tracing::error! macro expansion, not control flow"
)]
async fn spawn_worker_stop_task(
    bootstrap: TestBootstrapSettings,
    env_vars: Vec<(String, Option<String>)>,
    context: String,
) {
    let result =
        tokio::task::spawn_blocking(move || worker_stop_sync(&bootstrap, &env_vars, &context))
            .await;

    if let Err(err) = result {
        tracing::error!(
            target: LOG_TARGET,
            error = %err,
            "worker stop task panicked during async drop"
        );
    }
}

/// Synchronous worker stop for async drop cleanup.
///
/// Builds a temporary runtime to invoke the worker stop operation.
fn worker_stop_sync(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    let Ok(runtime) = build_runtime() else {
        tracing::error!(
            target: LOG_TARGET,
            "failed to build runtime for worker stop during async drop"
        );
        return;
    };

    let invoker = ClusterWorkerInvoker::new(&runtime, bootstrap, env_vars);
    if let Err(err) = invoker.invoke_as_root(super::worker_operation::WorkerOperation::Stop) {
        TestCluster::warn_stop_failure(context, &err);
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        let context = Self::stop_context(&self.bootstrap.settings);
        let is_async = self.runtime.is_async();
        tracing::info!(
            target: LOG_TARGET,
            context = %context,
            worker_managed = self.is_managed_via_worker,
            async_mode = is_async,
            "stopping embedded postgres cluster"
        );

        if is_async {
            // Async clusters should use stop_async() explicitly; attempt best-effort cleanup.
            self.drop_async_cluster(&context);
        } else {
            self.drop_sync_cluster(&context);
        }
        // Environment guards drop after this block, restoring the process state.
    }
}
