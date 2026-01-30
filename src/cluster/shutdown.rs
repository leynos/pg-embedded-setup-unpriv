//! Shutdown and cleanup logic for `TestCluster`.
//!
//! Provides synchronous and asynchronous shutdown methods, as well as
//! drop-time cleanup for both worker-managed and in-process clusters.

use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{CleanupMode, TestBootstrapSettings};
use postgresql_embedded::{PostgreSQL, Settings};
use std::{fmt::Display, time::Duration};
use tokio::time;

use super::cleanup;
use super::runtime::build_runtime;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::worker_operation;

/// Context for cluster drop operations, grouping related shutdown state.
pub(super) struct DropContext<'a> {
    pub is_managed_via_worker: bool,
    pub postgres: &'a mut Option<PostgreSQL>,
    pub bootstrap: &'a TestBootstrapSettings,
    pub env_vars: &'a [(String, Option<String>)],
    pub context: &'a str,
}

/// Bundles in-process cleanup metadata.
#[cfg(feature = "async-api")]
pub(super) struct InProcessCleanup<'a> {
    pub cleanup_mode: CleanupMode,
    pub settings: &'a Settings,
    pub context: &'a str,
}

struct OwnedCleanup {
    cleanup_mode: CleanupMode,
    settings: Settings,
    context: String,
}

impl OwnedCleanup {
    fn new(cleanup_mode: CleanupMode, settings: Settings, context: &str) -> Self {
        Self {
            cleanup_mode,
            settings,
            context: context.to_owned(),
        }
    }
}

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
    cleanup: InProcessCleanup<'_>,
) -> BootstrapResult<()> {
    let result = match time::timeout(timeout, postgres.stop()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            warn_stop_failure(cleanup.context, &err);
            Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "failed to stop postgres: {err}"
            )))
        }
        Err(_) => {
            let timeout_secs = timeout.as_secs();
            warn_stop_timeout(timeout_secs, cleanup.context);
            Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "stop timed out after {timeout_secs}s"
            )))
        }
    };
    if result.is_ok() {
        cleanup::cleanup_in_process(cleanup.cleanup_mode, cleanup.settings, cleanup.context);
    }
    result
}

/// Synchronous worker stop for use with `spawn_blocking`.
#[cfg(feature = "async-api")]
pub(super) fn stop_via_worker_sync(
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) -> BootstrapResult<()> {
    let runtime = build_runtime()?;
    stop_worker_managed_with_runtime(&runtime, bootstrap, env_vars, context)
}

fn stop_worker_managed_with_runtime(
    runtime: &tokio::runtime::Runtime,
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) -> BootstrapResult<()> {
    let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    let result = invoker.invoke_as_root(worker_operation::WorkerOperation::Stop);
    if let Err(err) = &result {
        warn_stop_failure(context, err);
    }
    if result.is_ok() {
        cleanup::cleanup_worker_managed_with_runtime(runtime, bootstrap, env_vars, context);
        cleanup::cleanup_in_process(bootstrap.cleanup_mode, &bootstrap.settings, context);
    }
    result
}

/// Synchronous drop path: stops the cluster using the owned runtime.
pub(super) fn drop_sync_cluster(runtime: &tokio::runtime::Runtime, ctx: DropContext<'_>) {
    let DropContext {
        is_managed_via_worker,
        postgres,
        bootstrap,
        env_vars,
        context,
    } = ctx;

    if is_managed_via_worker {
        drop(stop_worker_managed_with_runtime(
            runtime, bootstrap, env_vars, context,
        ));
        return;
    }

    let timeout = bootstrap.shutdown_timeout;
    let timeout_secs = timeout.as_secs();
    let mut stop_ok = false;
    if let Some(pg) = postgres.take() {
        let outcome = runtime.block_on(async { time::timeout(timeout, pg.stop()).await });
        match outcome {
            Ok(Ok(())) => stop_ok = true,
            Ok(Err(err)) => warn_stop_failure(context, &err),
            Err(_) => warn_stop_timeout(timeout_secs, context),
        }
    }

    if stop_ok {
        cleanup::cleanup_in_process(bootstrap.cleanup_mode, &bootstrap.settings, context);
    }
}

/// Best-effort cleanup for async clusters dropped without `stop_async()`.
///
/// Attempts to spawn cleanup on the current runtime handle if available.
/// For worker-managed clusters, attempts to invoke the worker stop operation.
pub(super) fn drop_async_cluster(ctx: DropContext<'_>) {
    let DropContext {
        is_managed_via_worker,
        postgres,
        bootstrap,
        env_vars,
        context,
    } = ctx;

    warn_async_drop_without_stop(context);

    if is_managed_via_worker {
        drop_async_worker_managed(bootstrap, env_vars, context);
    } else if let Some(pg) = postgres.take() {
        let cleanup =
            OwnedCleanup::new(bootstrap.cleanup_mode, bootstrap.settings.clone(), context);
        drop_async_in_process(bootstrap.shutdown_timeout, cleanup, pg);
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
fn drop_async_in_process(timeout: Duration, cleanup: OwnedCleanup, postgres: PostgreSQL) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        error_no_runtime_for_cleanup(&cleanup.context);
        return;
    };

    spawn_async_cleanup(&handle, postgres, timeout, cleanup);
}

/// Spawns async cleanup of a `PostgreSQL` instance on the provided runtime handle.
///
/// The task is fire-and-forget; errors during shutdown are logged at debug level.
fn spawn_async_cleanup(
    handle: &tokio::runtime::Handle,
    postgres: PostgreSQL,
    timeout: Duration,
    cleanup: OwnedCleanup,
) {
    drop(handle.spawn(async move {
        match time::timeout(timeout, postgres.stop()).await {
            Ok(Ok(())) => {
                tracing::debug!(target: LOG_TARGET, "async cleanup completed successfully");
                cleanup::cleanup_in_process(
                    cleanup.cleanup_mode,
                    &cleanup.settings,
                    &cleanup.context,
                );
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
pub(super) async fn spawn_worker_stop_task(
    bootstrap: TestBootstrapSettings,
    env_vars: Vec<(String, Option<String>)>,
    context: String,
) {
    handle_worker_stop_join(
        tokio::task::spawn_blocking(move || worker_stop_sync(&bootstrap, &env_vars, &context))
            .await,
    );
}

/// Handles the result of a worker stop task join.
fn handle_worker_stop_join(result: Result<(), tokio::task::JoinError>) {
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

    drop(stop_worker_managed_with_runtime(
        &runtime, bootstrap, env_vars, context,
    ));
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
    use rstest::rstest;

    #[rstest]
    #[case::timeout(
        || warn_stop_timeout(5, "ctx"),
        "stop() timed out after 5s (ctx)"
    )]
    #[case::failure(
        || warn_stop_failure("ctx", &"boom"),
        "failed to stop embedded postgres instance"
    )]
    fn warning_functions_emit_expected_logs(
        #[case] action: fn(),
        #[case] expected_substring: &str,
    ) {
        let (logs, ()) = capture_warn_logs(action);
        assert!(
            logs.iter().any(|line| line.contains(expected_substring)),
            "expected warning containing '{expected_substring}', got {logs:?}"
        );
    }
}
