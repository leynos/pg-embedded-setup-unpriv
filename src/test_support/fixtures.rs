//! Shared fixtures for tests that need bootstrap scaffolding.

use super::scoped_env::scoped_env;
use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
#[cfg(not(doc))]
use rstest::fixture;
use std::ffi::OsString;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

use super::worker_env;
use crate::{
    ClusterHandle, ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment,
    TestBootstrapSettings, TestCluster, detect_execution_privileges, env::ScopedEnv,
};
use postgresql_embedded::Settings;

/// Builds a single-threaded Tokio runtime for synchronous tests.
///
/// # Examples
/// ```rust
/// use pg_embedded_setup_unpriv::test_support::test_runtime;
///
/// # fn demo() -> color_eyre::eyre::Result<()> {
/// let runtime = test_runtime()?;
/// # drop(runtime);
/// # Ok(())
/// # }
/// # demo().unwrap();
/// ```
pub fn test_runtime() -> Result<Runtime> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| eyre!(err))
}

/// Creates a deterministic sandboxed environment description for tests.
///
/// # Examples
/// ```rust
/// use pg_embedded_setup_unpriv::test_support::dummy_environment;
///
/// let env = dummy_environment();
/// assert_eq!(env.timezone, "UTC");
/// ```
#[must_use]
pub fn dummy_environment() -> TestBootstrapEnvironment {
    TestBootstrapEnvironment {
        home: Utf8PathBuf::from("/tmp/pg-home"),
        xdg_cache_home: Utf8PathBuf::from("/tmp/pg-cache"),
        xdg_runtime_dir: Utf8PathBuf::from("/tmp/pg-run"),
        pgpass_file: Utf8PathBuf::from("/tmp/.pgpass"),
        tz_dir: Some(Utf8PathBuf::from("/usr/share/zoneinfo")),
        timezone: "UTC".into(),
    }
}

/// Synthesises bootstrap settings for unit tests targeting the invoker logic.
///
/// # Examples
/// ```rust
/// use pg_embedded_setup_unpriv::test_support::dummy_settings;
/// use pg_embedded_setup_unpriv::ExecutionPrivileges;
///
/// let settings = dummy_settings(ExecutionPrivileges::Unprivileged);
/// assert_eq!(settings.privileges, ExecutionPrivileges::Unprivileged);
/// ```
#[must_use]
pub fn dummy_settings(privileges: ExecutionPrivileges) -> TestBootstrapSettings {
    TestBootstrapSettings {
        privileges,
        execution_mode: match privileges {
            ExecutionPrivileges::Unprivileged => ExecutionMode::InProcess,
            ExecutionPrivileges::Root => ExecutionMode::Subprocess,
        },
        settings: Settings::default(),
        environment: dummy_environment(),
        worker_binary: None,
        setup_timeout: Duration::from_secs(180),
        start_timeout: Duration::from_secs(60),
        shutdown_timeout: Duration::from_secs(15),
        binary_cache_dir: None,
    }
}

#[must_use]
#[cfg_attr(not(doc), fixture)]
pub fn test_cluster() -> TestCluster {
    let worker_guard = ensure_worker_env();
    let cluster = match TestCluster::new() {
        Ok(cluster) => cluster,
        Err(err) => {
            panic!("SKIP-TEST-CLUSTER: test_cluster fixture failed to start PostgreSQL: {err:?}")
        }
    };
    cluster.with_worker_guard(worker_guard)
}

/// Ensures `PG_EMBEDDED_WORKER` is set when privileged test runs require it.
///
/// Returns `Some(ScopedEnv)` when the helper configures the environment, and
/// `None` when no changes are needed (for example, when already unprivileged
/// or when `PG_EMBEDDED_WORKER` is present).
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::ensure_worker_env;
///
/// let guard = ensure_worker_env();
/// drop(guard); // Restores the previous environment values.
/// ```
pub fn ensure_worker_env() -> Option<ScopedEnv> {
    let worker_path = resolve_worker_path(
        detect_execution_privileges(),
        std::env::var_os("PG_EMBEDDED_WORKER").is_some(),
        worker_env::worker_binary,
    )?;

    Some(scoped_env(vec![(
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(worker_path),
    )]))
}

/// Returns true if the worker environment needs to be configured.
///
/// Worker setup is required only when running as root without an existing
/// `PG_EMBEDDED_WORKER` environment variable. Unprivileged users run in-process
/// and do not need the worker binary.
fn is_worker_env_required(privileges: ExecutionPrivileges, worker_env_present: bool) -> bool {
    privileges == ExecutionPrivileges::Root && !worker_env_present
}

/// Locates the worker binary path if required by the current execution context.
///
/// Returns `Some(path)` when running as root without `PG_EMBEDDED_WORKER` set,
/// `None` when no worker setup is needed (unprivileged or already configured).
/// Panics if root execution requires a worker but none can be found.
fn resolve_worker_path(
    privileges: ExecutionPrivileges,
    worker_env_present: bool,
    worker_finder: impl FnOnce() -> Option<OsString>,
) -> Option<OsString> {
    if !is_worker_env_required(privileges, worker_env_present) {
        return None;
    }

    let Some(worker) = worker_finder() else {
        panic!(
            "SKIP-TEST-CLUSTER: PG_EMBEDDED_WORKER is not set and pg_worker binary was not found"
        );
    };

    Some(worker)
}

// Re-export shared singleton functions from submodule.
pub use super::shared_singleton::{shared_cluster, shared_cluster_handle};

// ============================================================================
// Fixture functions
// ============================================================================

/// rstest fixture returning a shared `TestCluster` reference.
///
/// Panics if the cluster cannot be started, enabling tests to fail fast
/// with a clear error message.
#[must_use]
#[cfg_attr(not(doc), fixture)]
pub fn shared_test_cluster() -> &'static TestCluster {
    match shared_cluster() {
        Ok(cluster) => cluster,
        Err(err) => panic!(
            "SKIP-TEST-CLUSTER: shared_test_cluster fixture failed to start PostgreSQL: {err:?}"
        ),
    }
}

/// rstest fixture returning a shared `ClusterHandle` reference.
///
/// This fixture is `Send + Sync`, making it suitable for use with rstest
/// timeouts and other thread-safe contexts.
///
/// Panics if the cluster cannot be started, enabling tests to fail fast
/// with a clear error message.
///
/// # Examples
///
/// ```ignore
/// use rstest::rstest;
/// use pg_embedded_setup_unpriv::test_support::shared_test_cluster_handle;
/// use pg_embedded_setup_unpriv::ClusterHandle;
///
/// #[rstest]
/// #[timeout(std::time::Duration::from_secs(30))]
/// fn test_with_shared_cluster(shared_test_cluster_handle: &'static ClusterHandle) {
///     assert!(shared_test_cluster_handle.database_exists("postgres").unwrap());
/// }
/// ```
#[must_use]
#[cfg_attr(not(doc), fixture)]
pub fn shared_test_cluster_handle() -> &'static ClusterHandle {
    match shared_cluster_handle() {
        Ok(handle) => handle,
        Err(err) => panic!(
            "SKIP-TEST-CLUSTER: shared_test_cluster_handle fixture failed to start PostgreSQL: {err:?}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// Unprivileged users should not require the worker binary, regardless of
    /// whether it exists or whether `PG_EMBEDDED_WORKER` is set.
    #[rstest]
    #[case::worker_would_be_found(true)]
    #[case::worker_would_not_be_found(false)]
    fn unprivileged_user_does_not_require_worker(#[case] worker_exists: bool) {
        let worker_finder = move || worker_exists.then(|| OsString::from("/fake/worker"));
        let result = resolve_worker_path(
            ExecutionPrivileges::Unprivileged,
            false, // PG_EMBEDDED_WORKER not set
            worker_finder,
        );
        assert!(
            result.is_none(),
            "unprivileged execution should not resolve worker path"
        );
    }

    /// When `PG_EMBEDDED_WORKER` is already set, even privileged users should not
    /// override it (returns `None` without calling the worker finder).
    #[test]
    fn privileged_user_with_existing_worker_env_does_not_override() {
        let worker_finder = || panic!("worker_finder should not be called when env var is set");
        let result = resolve_worker_path(
            ExecutionPrivileges::Root,
            true, // PG_EMBEDDED_WORKER already set
            worker_finder,
        );
        assert!(
            result.is_none(),
            "should not resolve worker path when PG_EMBEDDED_WORKER is set"
        );
    }

    /// Privileged users without `PG_EMBEDDED_WORKER` set should receive the
    /// worker binary path for environment configuration.
    #[test]
    fn privileged_user_without_worker_env_resolves_worker_path() {
        let worker_path = OsString::from("/path/to/pg_worker");
        let expected_path = worker_path.clone();
        let worker_finder = move || Some(worker_path);
        let result = resolve_worker_path(
            ExecutionPrivileges::Root,
            false, // PG_EMBEDDED_WORKER not set
            worker_finder,
        );
        assert_eq!(
            result,
            Some(expected_path),
            "should return worker path for privileged execution"
        );
    }

    /// Privileged users without `PG_EMBEDDED_WORKER` and without a locatable
    /// worker binary should trigger the skip panic.
    #[test]
    #[should_panic(expected = "SKIP-TEST-CLUSTER")]
    fn privileged_user_without_worker_binary_panics() {
        let worker_finder = || None;
        let _result = resolve_worker_path(
            ExecutionPrivileges::Root,
            false, // PG_EMBEDDED_WORKER not set
            worker_finder,
        );
    }
}
