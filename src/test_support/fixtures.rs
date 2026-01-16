//! Shared fixtures for tests that need bootstrap scaffolding.

use super::scoped_env::scoped_env;
use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
#[cfg(not(doc))]
use rstest::fixture;
use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

use crate::error::BootstrapResult;
use crate::{
    ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    TestCluster, detect_execution_privileges, env::ScopedEnv,
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

fn ensure_worker_env() -> Option<ScopedEnv> {
    // Unprivileged users can run tests in-process without the worker binary.
    if detect_execution_privileges() == ExecutionPrivileges::Unprivileged {
        return None;
    }

    if std::env::var_os("PG_EMBEDDED_WORKER").is_some() {
        return None;
    }

    let Some(worker) = worker_binary() else {
        panic!(
            "SKIP-TEST-CLUSTER: PG_EMBEDDED_WORKER is not set and pg_worker binary was not found"
        );
    };

    Some(scoped_env(vec![(
        OsString::from("PG_EMBEDDED_WORKER"),
        Some(worker),
    )]))
}

fn worker_binary() -> Option<OsString> {
    static WORKER_PATH: OnceLock<Option<OsString>> = OnceLock::new();
    WORKER_PATH
        .get_or_init(|| std::env::var_os("CARGO_BIN_EXE_pg_worker").or_else(locate_worker_binary))
        .clone()
}

fn locate_worker_binary() -> Option<OsString> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_dir = deps_dir.parent()?;
    let worker_path = target_dir.join("pg_worker");
    worker_path.exists().then(|| worker_path.into_os_string())
}

use std::sync::Mutex;

/// Global state for the shared cluster singleton.
///
/// Uses `OnceLock<Mutex<...>>` to support fallible initialisation whilst
/// maintaining thread-safe singleton semantics. The `Mutex` protects
/// initialisation; once complete, the pointer is stable.
///
/// We store a raw pointer because `TestCluster` is `!Sync` (it contains
/// `ScopedEnv` which uses `PhantomData<Rc<()>>`). The pointer is safe to
/// share across threads because:
/// 1. The cluster is only initialised once and never moved.
/// 2. All access goes through immutable references.
/// 3. The cluster's public API is thread-safe (database operations are
///    independent connections).
static SHARED_CLUSTER: OnceLock<Mutex<SharedClusterState>> = OnceLock::new();

enum SharedClusterState {
    Uninitialised,
    Initialised(SharedClusterPtr),
    Failed(String),
}

/// A wrapper around a raw pointer to `TestCluster` that implements `Send`
/// and `Sync`.
///
/// # Safety
///
/// This is safe because:
/// 1. The pointer is only created from a leaked `Box<TestCluster>`.
/// 2. The pointed-to data lives for the entire process lifetime.
/// 3. Access is read-only through the returned `&'static TestCluster`.
struct SharedClusterPtr(*const TestCluster);

// SAFETY: The pointer points to a leaked Box that lives forever.
// The TestCluster's public API (connection(), database_exists(), etc.)
// creates new connections for each operation and is thread-safe.
unsafe impl Send for SharedClusterPtr {}
unsafe impl Sync for SharedClusterPtr {}

/// Returns a reference to the shared test cluster.
///
/// The cluster is initialised lazily on first access using [`OnceLock`] for
/// thread-safe singleton semantics. Subsequent calls return the same cluster
/// instance, eliminating per-test bootstrap overhead.
///
/// # Errors
///
/// Returns a [`BootstrapError`](crate::error::BootstrapError) if the cluster
/// cannot be started. Once initialisation fails, subsequent calls return the
/// same error.
///
/// # Thread safety
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
        SharedClusterState::Failed(msg) => Err(crate::error::BootstrapError::from(
            color_eyre::eyre::eyre!("shared cluster initialisation failed: {msg}"),
        )),
        SharedClusterState::Uninitialised => {
            let worker_guard = ensure_worker_env();
            match TestCluster::new() {
                Ok(new_cluster) => {
                    let guarded_cluster = new_cluster.with_worker_guard(worker_guard);
                    // Leak the cluster to get a stable pointer.
                    // This is intentional: the shared cluster lives for the
                    // entire process lifetime and is never dropped.
                    let leaked: &'static TestCluster = Box::leak(Box::new(guarded_cluster));
                    let ptr = SharedClusterPtr(std::ptr::from_ref::<TestCluster>(leaked));
                    *guard = SharedClusterState::Initialised(ptr);
                    Ok(leaked)
                }
                Err(err) => {
                    let msg = format!("{err:?}");
                    *guard = SharedClusterState::Failed(msg.clone());
                    Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                        "shared cluster initialisation failed: {msg}"
                    )))
                }
            }
        }
    }
}

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
