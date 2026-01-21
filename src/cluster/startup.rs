//! Startup orchestration for `TestCluster`.
//!
//! Contains logic for bootstrapping and starting the embedded `PostgreSQL` instance,
//! including cache integration, lifecycle invocation, and privilege handling.

use crate::cache::BinaryCacheConfig;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};
use postgresql_embedded::PostgreSQL;
use tokio::runtime::Runtime;
use tracing::info;

use super::cache_integration;
use super::installation;
#[cfg(feature = "async-api")]
use super::worker_invoker::AsyncInvoker;
use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::worker_operation;

/// Outcome from starting the `PostgreSQL` instance.
pub(super) struct StartupOutcome {
    pub(super) bootstrap: TestBootstrapSettings,
    pub(super) postgres: Option<PostgreSQL>,
    pub(super) is_managed_via_worker: bool,
}

/// Creates a `BinaryCacheConfig` from bootstrap settings.
///
/// Uses the explicitly configured `binary_cache_dir` if set, otherwise
/// falls back to the default resolution from environment variables.
pub(super) fn cache_config_from_bootstrap(bootstrap: &TestBootstrapSettings) -> BinaryCacheConfig {
    bootstrap
        .binary_cache_dir
        .as_ref()
        .map_or_else(BinaryCacheConfig::new, |dir| {
            BinaryCacheConfig::with_dir(dir.clone())
        })
}

/// Starts the `PostgreSQL` instance with privilege-aware lifecycle handling.
#[expect(
    clippy::cognitive_complexity,
    reason = "privilege-aware lifecycle setup requires explicit branching for observability"
)]
pub(super) fn start_postgres(
    runtime: &Runtime,
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<StartupOutcome> {
    let privileges = bootstrap.privileges;
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        mode = ?bootstrap.execution_mode,
        "starting embedded postgres lifecycle"
    );

    // Try to use cached binaries before starting the lifecycle
    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    let (is_managed_via_worker, postgres) = if privileges == ExecutionPrivileges::Root {
        invoke_lifecycle_root(runtime, &mut bootstrap, env_vars)?;
        (true, None)
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        invoke_lifecycle(runtime, &mut bootstrap, env_vars, &mut embedded)?;
        (
            false,
            prepare_postgres_handle(false, &mut bootstrap, embedded),
        )
    };

    // Populate cache after successful setup if it was a cache miss
    if !cache_hit {
        cache_integration::try_populate_binary_cache(cache_config, &bootstrap.settings);
    }

    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        worker_managed = is_managed_via_worker,
        cache_hit,
        "embedded postgres started"
    );
    Ok(StartupOutcome {
        bootstrap,
        postgres,
        is_managed_via_worker,
    })
}

/// Prepares the `PostgreSQL` handle based on whether it's worker-managed.
pub(super) fn prepare_postgres_handle(
    is_managed_via_worker: bool,
    bootstrap: &mut TestBootstrapSettings,
    embedded: PostgreSQL,
) -> Option<PostgreSQL> {
    if is_managed_via_worker {
        None
    } else {
        bootstrap.settings = embedded.settings().clone();
        Some(embedded)
    }
}

/// Invokes the lifecycle for root-privileged execution via worker subprocess.
pub(super) fn invoke_lifecycle_root(
    runtime: &Runtime,
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
) -> BootstrapResult<()> {
    let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    setup_invoker.invoke_as_root(worker_operation::WorkerOperation::Setup)?;
    installation::refresh_worker_installation_dir(bootstrap);
    let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    start_invoker.invoke_as_root(worker_operation::WorkerOperation::Start)?;
    installation::refresh_worker_port(bootstrap)
}

/// Invokes the lifecycle for unprivileged in-process execution.
pub(super) fn invoke_lifecycle(
    runtime: &Runtime,
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    embedded: &mut PostgreSQL,
) -> BootstrapResult<()> {
    // Scope ensures the setup invoker releases its borrows before we refresh the settings.
    let setup_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
        embedded.setup().await
    })?;
    installation::refresh_worker_installation_dir(bootstrap);
    let start_invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
        embedded.start().await
    })?;
    installation::refresh_worker_port(bootstrap)
}

// ============================================================================
// Async API (feature-gated)
// ============================================================================

/// Async variant of `start_postgres` that runs on the caller's runtime.
#[cfg(feature = "async-api")]
pub(super) async fn start_postgres_async(
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<StartupOutcome> {
    let privileges = bootstrap.privileges;
    log_lifecycle_start(privileges, &bootstrap);

    // Try to use cached binaries before starting the lifecycle
    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    let (is_managed_via_worker, postgres) = if privileges == ExecutionPrivileges::Root {
        Box::pin(invoke_lifecycle_root_async(&mut bootstrap, env_vars)).await?;
        (true, None)
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        Box::pin(invoke_lifecycle_async(
            &mut bootstrap,
            env_vars,
            &mut embedded,
        ))
        .await?;
        (
            false,
            prepare_postgres_handle(false, &mut bootstrap, embedded),
        )
    };

    // Populate cache after successful setup if it was a cache miss
    if !cache_hit {
        cache_integration::try_populate_binary_cache(cache_config, &bootstrap.settings);
    }

    log_lifecycle_complete(privileges, is_managed_via_worker, cache_hit);
    Ok(StartupOutcome {
        bootstrap,
        postgres,
        is_managed_via_worker,
    })
}

/// Logs the start of the async lifecycle.
#[cfg(feature = "async-api")]
pub(super) fn log_lifecycle_start(
    privileges: ExecutionPrivileges,
    bootstrap: &TestBootstrapSettings,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        mode = ?bootstrap.execution_mode,
        async_mode = true,
        "starting embedded postgres lifecycle"
    );
}

/// Logs completion of the async lifecycle.
#[cfg(feature = "async-api")]
pub(super) fn log_lifecycle_complete(
    privileges: ExecutionPrivileges,
    is_managed_via_worker: bool,
    cache_hit: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        worker_managed = is_managed_via_worker,
        cache_hit = cache_hit,
        async_mode = true,
        "embedded postgres started"
    );
}

/// Async variant of `invoke_lifecycle`.
#[cfg(feature = "async-api")]
pub(super) async fn invoke_lifecycle_async(
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    embedded: &mut PostgreSQL,
) -> BootstrapResult<()> {
    let invoker = AsyncInvoker::new(bootstrap, env_vars);
    Box::pin(
        invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            embedded.setup().await
        }),
    )
    .await?;
    installation::refresh_worker_installation_dir(bootstrap);
    let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
    Box::pin(
        start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        }),
    )
    .await?;
    installation::refresh_worker_port_async(bootstrap).await
}

/// Async variant of `invoke_lifecycle_root`.
#[cfg(feature = "async-api")]
pub(super) async fn invoke_lifecycle_root_async(
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
) -> BootstrapResult<()> {
    let setup_invoker = AsyncInvoker::new(bootstrap, env_vars);
    Box::pin(
        setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            Ok::<(), postgresql_embedded::Error>(())
        }),
    )
    .await?;
    installation::refresh_worker_installation_dir(bootstrap);
    let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
    Box::pin(
        start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
            Ok::<(), postgresql_embedded::Error>(())
        }),
    )
    .await?;
    installation::refresh_worker_port_async(bootstrap).await
}
