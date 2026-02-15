//! Startup orchestration for `TestCluster` and the CLI setup-only path.
//!
//! Contains logic for bootstrapping and starting the embedded `PostgreSQL` instance,
//! including cache integration, lifecycle invocation, and privilege handling.
//! The [`setup_postgres_only`] entry point drives download + `initdb` without
//! starting the server, used by the CLI binary.

use crate::cache::BinaryCacheConfig;
use crate::env::ScopedEnv;
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
pub(super) fn start_postgres(
    runtime: &Runtime,
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<StartupOutcome> {
    let privileges = bootstrap.privileges;
    log_lifecycle_start(privileges, &bootstrap, false);

    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    let (is_managed_via_worker, postgres) =
        handle_privilege_lifecycle(privileges, runtime, &mut bootstrap, env_vars)?;

    populate_cache_on_miss(cache_hit, cache_config, &bootstrap);
    log_lifecycle_complete(privileges, is_managed_via_worker, cache_hit, false);

    Ok(StartupOutcome {
        bootstrap,
        postgres,
        is_managed_via_worker,
    })
}

/// Logs the start of the lifecycle.
fn log_lifecycle_start(
    privileges: ExecutionPrivileges,
    bootstrap: &TestBootstrapSettings,
    is_async: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        mode = ?bootstrap.execution_mode,
        async_mode = is_async,
        "starting embedded postgres lifecycle"
    );
}

/// Logs completion of the lifecycle.
fn log_lifecycle_complete(
    privileges: ExecutionPrivileges,
    is_managed_via_worker: bool,
    cache_hit: bool,
    is_async: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        worker_managed = is_managed_via_worker,
        cache_hit,
        async_mode = is_async,
        "embedded postgres started"
    );
}

/// Populates the cache after successful setup if it was a cache miss.
fn populate_cache_on_miss(
    cache_hit: bool,
    cache_config: &BinaryCacheConfig,
    bootstrap: &TestBootstrapSettings,
) {
    if !cache_hit {
        cache_integration::try_populate_binary_cache(cache_config, &bootstrap.settings);
    }
}

/// Handles the privilege-aware lifecycle invocation.
///
/// Returns a tuple of `(is_managed_via_worker, postgres_handle)` where:
/// - Root execution: worker-managed (true, None)
/// - Unprivileged execution: in-process (false, Some(embedded))
fn handle_privilege_lifecycle(
    privileges: ExecutionPrivileges,
    runtime: &Runtime,
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
) -> BootstrapResult<(bool, Option<PostgreSQL>)> {
    if privileges == ExecutionPrivileges::Root {
        invoke_lifecycle_root(runtime, bootstrap, env_vars)?;
        Ok((true, None))
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        invoke_lifecycle(runtime, bootstrap, env_vars, &mut embedded)?;
        Ok((false, prepare_postgres_handle(false, bootstrap, embedded)))
    }
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

/// Performs `PostgreSQL` setup (download + `initdb`) without starting the server.
///
/// This entry point is intended for the CLI binary, which prepares the
/// installation and data directory so that a subsequent `TestCluster::new()`
/// can reuse the cached binaries without a redundant download.
///
/// The cache directory is resolved from the host environment *before*
/// `ScopedEnv` is applied, matching the resolution order in
/// `TestCluster::new_split()` so the CLI and test runs share the same cache.
pub(crate) fn setup_postgres_only(
    bootstrap: TestBootstrapSettings,
) -> BootstrapResult<TestBootstrapSettings> {
    // Resolve cache directory from the host environment before applying the
    // scoped sandbox, matching the resolution order in TestCluster::new_split().
    let cache_config = cache_config_from_bootstrap(&bootstrap);
    let runtime = super::runtime::build_runtime()?;
    let env_vars = bootstrap.environment.to_env();
    let _env_guard = ScopedEnv::apply(&env_vars);

    setup_lifecycle(&runtime, bootstrap, &env_vars, &cache_config)
}

/// Drives the setup-only lifecycle (download + `initdb`), populating the
/// binary cache on a miss.
fn setup_lifecycle(
    runtime: &Runtime,
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<TestBootstrapSettings> {
    let privileges = bootstrap.privileges;
    log_lifecycle_start(privileges, &bootstrap, false);

    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    setup_with_privileges(privileges, runtime, &mut bootstrap, env_vars)?;
    installation::refresh_worker_installation_dir(&mut bootstrap);
    populate_cache_on_miss(cache_hit, cache_config, &bootstrap);
    log_setup_complete(privileges, cache_hit);

    Ok(bootstrap)
}

/// Runs the privilege-aware `Setup` operation only (no `Start`).
fn setup_with_privileges(
    privileges: ExecutionPrivileges,
    runtime: &Runtime,
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
) -> BootstrapResult<()> {
    if privileges == ExecutionPrivileges::Root {
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoker.invoke_as_root(worker_operation::WorkerOperation::Setup)
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            embedded.setup().await
        })
    }
}

/// Logs completion of the setup-only lifecycle.
fn log_setup_complete(privileges: ExecutionPrivileges, cache_hit: bool) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        cache_hit,
        "embedded postgres setup complete (server not started)"
    );
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
    log_lifecycle_start(privileges, &bootstrap, true);

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

    populate_cache_on_miss(cache_hit, cache_config, &bootstrap);
    log_lifecycle_complete(privileges, is_managed_via_worker, cache_hit, true);
    Ok(StartupOutcome {
        bootstrap,
        postgres,
        is_managed_via_worker,
    })
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
    // No-op future: the worker subprocess performs the actual setup; this drives the invocation.
    Box::pin(
        setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            Ok::<(), postgresql_embedded::Error>(())
        }),
    )
    .await?;
    installation::refresh_worker_installation_dir(bootstrap);
    let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
    // No-op future: the worker subprocess performs the actual start; this drives the invocation.
    Box::pin(
        start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
            Ok::<(), postgresql_embedded::Error>(())
        }),
    )
    .await?;
    installation::refresh_worker_port_async(bootstrap).await
}
