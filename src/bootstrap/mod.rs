//! Bootstraps embedded `PostgreSQL` while adapting to the caller's privileges.
//!
//! Provides [`bootstrap_for_tests`] so suites can retrieve structured settings and
//! prepared environment variables without reimplementing bootstrap orchestration.
mod env;
mod env_types;
mod mode;
mod prepare;

use std::time::Duration;

use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::Settings;
use serde::{Deserialize, Serialize};

use crate::{
    PgEnvCfg,
    error::{BootstrapResult, Result as CrateResult},
};

pub use env::{TestBootstrapEnvironment, find_timezone_dir};
pub use mode::{ExecutionMode, ExecutionPrivileges, detect_execution_privileges};

use self::{
    env::{shutdown_timeout_from_env, worker_binary_from_env},
    mode::determine_execution_mode,
    prepare::prepare_bootstrap,
};

const DEFAULT_SETUP_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum BootstrapKind {
    Default,
    Test,
}

/// Controls cleanup behaviour when a cluster is dropped.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
pub enum CleanupMode {
    /// Remove only the data directory.
    #[default]
    DataOnly,
    /// Remove both the data and installation directories.
    Full,
    /// Skip cleanup entirely (useful for debugging).
    None,
}

/// Structured settings returned from [`bootstrap_for_tests`].
#[derive(Debug, Clone)]
pub struct TestBootstrapSettings {
    /// Privilege level detected for the current process.
    pub privileges: ExecutionPrivileges,
    /// Strategy for executing `PostgreSQL` lifecycle commands.
    pub execution_mode: ExecutionMode,
    /// `PostgreSQL` configuration prepared for the embedded instance.
    pub settings: Settings,
    /// Environment variables required to exercise the embedded instance.
    pub environment: TestBootstrapEnvironment,
    /// Optional path to the helper binary used for subprocess execution.
    pub worker_binary: Option<camino::Utf8PathBuf>,
    /// Maximum time to allow the worker to complete the setup phase.
    pub setup_timeout: Duration,
    /// Maximum time to allow the worker to complete the start phase.
    pub start_timeout: Duration,
    /// Grace period granted to `PostgreSQL` during drop before teardown proceeds regardless.
    pub shutdown_timeout: Duration,
    /// Controls cleanup behaviour when the cluster drops.
    pub cleanup_mode: CleanupMode,
    /// Optional override for the binary cache directory.
    ///
    /// When set, `TestCluster` uses this directory instead of the default
    /// resolved from environment variables.
    pub binary_cache_dir: Option<camino::Utf8PathBuf>,
}

/// Bootstraps an embedded `PostgreSQL` instance, downloads the distribution,
/// and initialises the data directory via `initdb`.
///
/// The server is **not** started — the resulting installation is ready for
/// subsequent use by [`TestCluster`](crate::TestCluster) or other tools.
///
/// The function honours the following environment variables when present:
/// - `PG_RUNTIME_DIR`: Overrides the `PostgreSQL` installation directory.
/// - `PG_DATA_DIR`: Overrides the data directory used for initialisation.
/// - `PG_SUPERUSER`: Sets the superuser account name.
/// - `PG_PASSWORD`: Supplies the superuser password.
///
/// When executed as `root` on Unix platforms the runtime drops privileges to the `nobody` user
/// and prepares the filesystem on that user's behalf. Unprivileged executions reuse the current
/// user identity. The function returns a [`crate::Error`] describing failures encountered during
/// bootstrap or setup.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::run;
///
/// fn main() -> Result<(), pg_embedded_setup_unpriv::Error> {
///     run()?;
///     Ok(())
/// }
/// ```
///
/// # Errors
/// Returns an error when bootstrap preparation fails, when the `PostgreSQL`
/// distribution cannot be downloaded, or when `initdb` fails.
pub fn run() -> CrateResult<()> {
    let bootstrap = orchestrate_bootstrap(BootstrapKind::Default)?;
    crate::cluster::setup_postgres_only(bootstrap)?;
    Ok(())
}

/// Bootstraps `PostgreSQL` for integration tests and surfaces the prepared settings.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::bootstrap_for_tests;
/// use temp_env::with_vars;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let bootstrap = bootstrap_for_tests()?;
/// with_vars(bootstrap.environment.to_env(), || -> pg_embedded_setup_unpriv::BootstrapResult<()> {
///     // Launch application logic that relies on `bootstrap.settings` here.
///     Ok(())
/// })?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
/// Returns an error when bootstrap preparation fails or when subprocess orchestration
/// cannot be configured.
pub fn bootstrap_for_tests() -> BootstrapResult<TestBootstrapSettings> {
    orchestrate_bootstrap(BootstrapKind::Test)
}

fn orchestrate_bootstrap(kind: BootstrapKind) -> BootstrapResult<TestBootstrapSettings> {
    install_color_eyre();
    if matches!(kind, BootstrapKind::Test) {
        validate_backend_selection()?;
    }

    let privileges = detect_execution_privileges();
    let cfg = PgEnvCfg::load().context("failed to load configuration via OrthoConfig")?;
    let settings = match kind {
        BootstrapKind::Default => cfg.to_settings()?,
        BootstrapKind::Test => cfg.to_settings_for_tests()?,
    };
    let worker_binary = worker_binary_from_env(privileges)?;
    let execution_mode = determine_execution_mode(privileges, worker_binary.as_ref())?;
    let shutdown_timeout = shutdown_timeout_from_env()?;
    let prepared = prepare_bootstrap(privileges, settings, &cfg)?;

    Ok(TestBootstrapSettings {
        privileges,
        execution_mode,
        settings: prepared.settings,
        environment: prepared.environment,
        worker_binary,
        setup_timeout: DEFAULT_SETUP_TIMEOUT,
        start_timeout: DEFAULT_START_TIMEOUT,
        shutdown_timeout,
        cleanup_mode: CleanupMode::default(),
        binary_cache_dir: cfg.binary_cache_dir,
    })
}

fn install_color_eyre() {
    if let Err(err) = color_eyre::install() {
        tracing::debug!("color_eyre already installed: {err}");
    }
}

fn validate_backend_selection() -> BootstrapResult<()> {
    let raw = std::env::var_os("PG_TEST_BACKEND").unwrap_or_default();
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "postgresql_embedded" {
        return Ok(());
    }
    Err(eyre!(
        "SKIP-TEST-CLUSTER: unsupported PG_TEST_BACKEND '{trimmed}'; supported backends: postgresql_embedded"
    )
    .into())
}

#[cfg(test)]
mod mod_tests;
