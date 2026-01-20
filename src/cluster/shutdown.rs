//! Shutdown and cleanup logic for `TestCluster`.
//!
//! Provides synchronous and asynchronous shutdown methods, as well as
//! drop-time cleanup for both worker-managed and in-process clusters.

use crate::TestBootstrapSettings;
#[cfg(feature = "async-api")]
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use postgresql_embedded::{PostgreSQL, Settings};
use std::fmt::Display;
use std::time::Duration;
use tokio::time;

use super::runtime::build_runtime;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::worker_operation;

/// Builds a context string for logging shutdown operations.
pub(super) fn stop_context(settings: &Settings) -> String {
    let data_dir = settings.data_dir.display();
    let version = settings.version.to_string();
    format!("version {version}, data_dir {data_dir}")
}

/// Logs the start of an async stop operation.
#[cfg(feature = "async-api")]
pub(super) fn log_async_stop(context: &str, is_managed_via_worker: bool) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        worker_managed = is_managed_via_worker,
        async_mode = true,
        "stopping embedded postgres cluster"
    );
}

/// Stops a worker-managed cluster asynchronously.
#[cfg(feature = "async-api")]
pub(super) async fn stop_worker_managed_async(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) -> BootstrapResult<()> {
    let owned_bootstrap = bootstrap.clone();
    let owned_env_vars = env_vars.to_vec();
    let owned_context = context.to_owned();
    tokio::task::spawn_blocking(move || {
        stop_via_worker_sync(&owned_bootstrap, &owned_env_vars, &owned_context)
    })
    .await
    .map_err(|err| {
        crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
            "worker stop task panicked: {err}"
        ))
    })?
}

/// Stops an in-process cluster asynchronously.
#[cfg(feature = "async-api")]
pub(super) async fn stop_in_process_async(
    postgres: PostgreSQL,
    timeout: Duration,
    context: &str,
) -> BootstrapResult<()> {
    match time::timeout(timeout, postgres.stop()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            warn_stop_failure(context, &err);
            Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "failed to stop postgres: {err}"
            )))
        }
        Err(_) => {
            let timeout_secs = timeout.as_secs();
            warn_stop_timeout(timeout_secs, context);
            Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "stop timed out after {timeout_secs}s"
            )))
        }
    }
}

/// Synchronous worker stop for use with `spawn_blocking`.
#[cfg(feature = "async-api")]
pub(super) fn stop_via_worker_sync(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) -> BootstrapResult<()> {
    let runtime = build_runtime()?;
    let invoker = ClusterWorkerInvoker::new(&runtime, bootstrap, env_vars);
    invoker
        .invoke_as_root(worker_operation::WorkerOperation::Stop)
        .inspect_err(|err| warn_stop_failure(context, err))
}

/// Synchronous drop path: stops the cluster using the owned runtime.
#[expect(
    clippy::too_many_arguments,
    reason = "cluster shutdown requires all state components; grouping would obscure intent"
)]
pub(super) fn drop_sync_cluster(
    runtime: &tokio::runtime::Runtime,
    is_managed_via_worker: bool,
    postgres: &mut Option<PostgreSQL>,
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    if is_managed_via_worker {
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        if let Err(err) = invoker.invoke_as_root(worker_operation::WorkerOperation::Stop) {
            warn_stop_failure(context, &err);
        }
        return;
    }

    let Some(pg) = postgres.take() else {
        return;
    };

    let timeout = bootstrap.shutdown_timeout;
    let timeout_secs = timeout.as_secs();
    let outcome = runtime.block_on(async { time::timeout(timeout, pg.stop()).await });

    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn_stop_failure(context, &err),
        Err(_) => warn_stop_timeout(timeout_secs, context),
    }
}

/// Best-effort cleanup for async clusters dropped without `stop_async()`.
///
/// Attempts to spawn cleanup on the current runtime handle if available.
/// For worker-managed clusters, attempts to invoke the worker stop operation.
#[expect(
    clippy::too_many_arguments,
    reason = "cluster shutdown requires all state components; grouping would obscure intent"
)]
pub(super) fn drop_async_cluster(
    is_managed_via_worker: bool,
    postgres: &mut Option<PostgreSQL>,
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    warn_async_drop_without_stop(context);

    if is_managed_via_worker {
        drop_async_worker_managed(bootstrap, env_vars, context);
    } else if let Some(pg) = postgres.take() {
        drop_async_in_process(bootstrap.shutdown_timeout, context, pg);
    }
    // If neither worker-managed nor has postgres handle, already cleaned up via stop_async().
}

/// Best-effort worker stop for async clusters dropped without `stop_async()`.
fn drop_async_worker_managed(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        error_no_runtime_for_cleanup(context);
        return;
    };

    let owned_bootstrap = bootstrap.clone();
    let owned_env_vars = env_vars.to_vec();
    let owned_context = context.to_owned();

    drop(handle.spawn(spawn_worker_stop_task(
        owned_bootstrap,
        owned_env_vars,
        owned_context,
    )));
}

/// Best-effort in-process stop for async clusters dropped without `stop_async()`.
fn drop_async_in_process(timeout: Duration, context: &str, postgres: PostgreSQL) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        error_no_runtime_for_cleanup(context);
        return;
    };

    spawn_async_cleanup(&handle, postgres, timeout);
}

/// Spawns async cleanup of a `PostgreSQL` instance on the provided runtime handle.
///
/// The task is fire-and-forget; errors during shutdown are logged at debug level.
pub(super) fn spawn_async_cleanup(
    handle: &tokio::runtime::Handle,
    postgres: PostgreSQL,
    timeout: Duration,
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
    reason = "complexity is from spawn_blocking + error! macro expansion, not logic"
)]
pub(super) async fn spawn_worker_stop_task(
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
    if let Err(err) = invoker.invoke_as_root(worker_operation::WorkerOperation::Stop) {
        warn_stop_failure(context, &err);
    }
}

/// Logs a warning when an async cluster is dropped without calling `stop_async()`.
pub(super) fn warn_async_drop_without_stop(context: &str) {
    tracing::warn!(
        target: LOG_TARGET,
        context = %context,
        concat!(
            "async TestCluster dropped without calling stop_async(); ",
            "attempting best-effort cleanup"
        )
    );
}

/// Logs an error when no runtime is available for cleanup.
pub(super) fn error_no_runtime_for_cleanup(context: &str) {
    tracing::error!(
        target: LOG_TARGET,
        context = %context,
        "no async runtime available for cleanup; resources may leak"
    );
}

/// Logs a warning when stopping the cluster fails.
pub(super) fn warn_stop_failure(context: &str, err: &impl Display) {
    tracing::warn!(
        "SKIP-TEST-CLUSTER: failed to stop embedded postgres instance ({}): {}",
        context,
        err
    );
}

/// Logs a warning when stopping the cluster times out.
pub(super) fn warn_stop_timeout(timeout_secs: u64, context: &str) {
    tracing::warn!(
        "SKIP-TEST-CLUSTER: stop() timed out after {timeout_secs}s ({context}); proceeding with drop"
    );
}

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod tests {
    use super::*;
    use crate::test_support::capture_warn_logs;

    #[test]
    fn warn_stop_timeout_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| warn_stop_timeout(5, "ctx"));
        assert!(
            logs.iter()
                .any(|line| line.contains("stop() timed out after 5s (ctx)")),
            "expected timeout warning, got {logs:?}"
        );
    }

    #[test]
    fn warn_stop_failure_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| warn_stop_failure("ctx", &"boom"));
        assert!(
            logs.iter()
                .any(|line| line.contains("failed to stop embedded postgres instance")),
            "expected failure warning, got {logs:?}"
        );
    }
}
