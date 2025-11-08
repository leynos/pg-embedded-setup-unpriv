//! Shared fixtures for tests that need bootstrap scaffolding.

use super::scoped_env::scoped_env;
use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
use rstest::fixture;
use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

use crate::{
    ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    TestCluster, env::ScopedEnv,
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

/// `rstest` fixture that yields a running [`TestCluster`].
///
/// The fixture blocks until `PostgreSQL` is ready, making it ideal for
/// integration tests that only need to declare a `cluster: TestCluster`
/// parameter without invoking [`TestCluster::new`] manually.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
/// use pg_embedded_setup_unpriv::test_support::test_cluster;
/// use rstest::rstest;
///
/// #[rstest]
/// fn exercises_database(test_cluster: TestCluster) {
///     let metadata = test_cluster.connection().metadata();
///     assert!(metadata.port() > 0);
/// }
/// ```
#[fixture]
#[must_use]
pub fn test_cluster() -> TestCluster {
    let worker_guard = ensure_worker_env();
    let cluster = TestCluster::new().unwrap_or_else(|err| {
        panic!("SKIP-TEST-CLUSTER: test_cluster fixture failed to start PostgreSQL: {err:?}")
    });
    cluster.with_worker_guard(worker_guard)
}

fn ensure_worker_env() -> Option<ScopedEnv> {
    if std::env::var_os("PG_EMBEDDED_WORKER").is_some() {
        return None;
    }

    let worker = worker_binary().unwrap_or_else(|| {
        panic!(
            "SKIP-TEST-CLUSTER: PG_EMBEDDED_WORKER is not set and pg_worker binary was not found"
        )
    });

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
