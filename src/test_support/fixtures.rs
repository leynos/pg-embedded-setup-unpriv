#![cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]

//! Shared fixtures for tests that need bootstrap scaffolding.

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

use crate::{ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings};
use postgresql_embedded::Settings;

/// Builds a single-threaded Tokio runtime for synchronous tests.
///
/// # Examples
/// ```ignore
/// use pg_embedded_setup_unpriv::test_support::test_runtime;
///
/// # fn demo() -> color_eyre::eyre::Result<()> {
/// let runtime = test_runtime()?;
/// # drop(runtime);
/// # Ok(())
/// # }
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
/// ```ignore
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
/// ```ignore
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
