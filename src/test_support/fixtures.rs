#![cfg(any(
    doc,
    test,
    feature = "cluster-unit-tests",
    feature = "dev-worker",
    feature = "rstest-fixtures"
))]

//! Shared fixtures for tests that need bootstrap scaffolding.

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
use std::{env, fs, path::PathBuf, sync::LazyLock, time::Duration};
use tokio::runtime::{Builder, Runtime};

#[cfg(feature = "rstest-fixtures")]
use rstest::fixture;

use crate::{
    BootstrapResult, ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment,
    TestBootstrapSettings, TestCluster, env::ScopedEnv,
};

use postgresql_embedded::Settings;
#[cfg(all(feature = "rstest-fixtures", unix))]
use std::os::unix::fs::PermissionsExt;

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
    }
}

#[cfg(feature = "rstest-fixtures")]
/// Attempts to boot a [`TestCluster`] using the fixture logic.
pub fn try_test_cluster() -> BootstrapResult<TestCluster> {
    let worker_guard = worker_env_guard();
    let result = TestCluster::new();
    drop(worker_guard);
    result
}

#[cfg(feature = "rstest-fixtures")]
#[fixture]
/// Provides a ready-to-use [`TestCluster`] for `rstest` suites.
///
/// The fixture exposes a zero-boilerplate path for integration tests: declare a
/// `test_cluster: TestCluster` argument on any `#[rstest]` function and the
/// helper boots `PostgreSQL` automatically. Use [`try_test_cluster`] when you
/// need to inspect the [`BootstrapResult`] manually.
///
/// # Examples
///
/// ```rust,no_run
/// use pg_embedded_setup_unpriv::TestCluster;
/// use pg_embedded_setup_unpriv::test_support::test_cluster;
/// use rstest::rstest;
///
/// #[rstest]
/// fn exercise_cluster(test_cluster: TestCluster) -> pg_embedded_setup_unpriv::BootstrapResult<()> {
///     let metadata = test_cluster.connection().metadata();
///     assert_eq!(metadata.superuser(), "postgres");
///     Ok(())
/// }
/// ```
pub fn test_cluster() -> TestCluster {
    match try_test_cluster() {
        Ok(cluster) => cluster,
        Err(err) => panic!("failed to bootstrap TestCluster fixture: {err:?}"),
    }
}

#[cfg(feature = "rstest-fixtures")]
fn worker_env_guard() -> Option<ScopedEnv> {
    if env::var_os("PG_EMBEDDED_WORKER").is_some() {
        return None;
    }
    let Some(worker_path) = staged_worker_path() else {
        return None;
    };
    let worker = worker_path.to_string_lossy().into_owned();
    let vars = vec![("PG_EMBEDDED_WORKER".to_owned(), Some(worker))];
    Some(ScopedEnv::apply(&vars))
}

#[cfg(feature = "rstest-fixtures")]
static STAGED_WORKER: LazyLock<Option<PathBuf>> = LazyLock::new(stage_worker_binary);

#[cfg(feature = "rstest-fixtures")]
static STAGED_WORKER: LazyLock<Option<PathBuf>> = LazyLock::new(stage_worker_binary);

#[cfg(feature = "rstest-fixtures")]
fn staged_worker_path() -> Option<PathBuf> {
    (*STAGED_WORKER).clone()
}

#[cfg(feature = "rstest-fixtures")]
fn stage_worker_binary() -> Option<PathBuf> {
    let source = auto_worker_source_path()?;
    let destination = staging_destination();
    ensure_staging_parent(&destination)?;
    remove_stale_destination(&destination)?;
    link_or_copy_worker(&source, &destination)?;
    ensure_worker_permissions(&destination)?;
    Some(destination)
}

#[cfg(feature = "rstest-fixtures")]
fn auto_worker_source_path() -> Option<PathBuf> {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_pg_worker") {
        return Some(PathBuf::from(env_path));
    }
    let exe = env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_dir = deps_dir.parent()?;
    let worker_path = target_dir.join("pg_worker");
    worker_path.exists().then_some(worker_path)
}

#[cfg(feature = "rstest-fixtures")]
fn staging_destination() -> PathBuf {
    env::temp_dir()
        .join("pg_embedded_setup_unpriv")
        .join("pg_worker")
}

#[cfg(feature = "rstest-fixtures")]
fn ensure_staging_parent(destination: &PathBuf) -> Option<()> {
    let Some(parent) = destination.parent() else {
        tracing::warn!("staging destination {} has no parent", destination.display());
        return None;
    };
    if let Err(err) = fs::create_dir_all(parent) {
        tracing::warn!(
            "failed to create worker staging dir {}: {err}",
            parent.display()
        );
        return None;
    }
    Some(())
}

#[cfg(feature = "rstest-fixtures")]
fn remove_stale_destination(destination: &PathBuf) -> Option<()> {
    if destination.exists() {
        if let Err(err) = fs::remove_file(destination) {
            tracing::warn!(
                "failed to remove stale staged worker at {}: {err}",
                destination.display()
            );
            return None;
        }
    }
    Some(())
}

#[cfg(feature = "rstest-fixtures")]
fn link_or_copy_worker(source: &PathBuf, destination: &PathBuf) -> Option<()> {
    if let Err(err) = fs::hard_link(source, destination) {
        tracing::debug!(
            "failed to link worker binary from {} to {} ({err}); falling back to copy",
            source.display(),
            destination.display()
        );
        if let Err(copy_err) = fs::copy(source, destination) {
            tracing::warn!(
                "failed to copy worker binary from {} to {}: {copy_err}",
                source.display(),
                destination.display()
            );
            return None;
        }
    }
    Some(())
}

#[cfg(feature = "rstest-fixtures")]
fn ensure_worker_permissions(destination: &PathBuf) -> Option<()> {
    #[cfg(unix)]
    {
        if let Err(err) = fs::set_permissions(destination, fs::Permissions::from_mode(0o755)) {
            tracing::warn!(
                "failed to set executable permissions on {}: {err}",
                destination.display()
            );
            return None;
        }
    }
    Some(())
}
