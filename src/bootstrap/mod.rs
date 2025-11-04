//! Bootstraps embedded `PostgreSQL` while adapting to the caller's privileges.
//!
//! Provides [`bootstrap_for_tests`] so suites can retrieve structured settings and
//! prepared environment variables without reimplementing bootstrap orchestration.
mod env;
mod mode;
mod prepare;

use std::time::Duration;

use color_eyre::eyre::Context;
use postgresql_embedded::Settings;

use crate::{
    PgEnvCfg,
    error::{BootstrapResult, Result as CrateResult},
};

pub use mode::{ExecutionMode, ExecutionPrivileges, detect_execution_privileges};
pub use prepare::TestBootstrapEnvironment;

use self::{
    env::{shutdown_timeout_from_env, worker_binary_from_env},
    mode::determine_execution_mode,
    prepare::prepare_bootstrap,
};

const DEFAULT_SETUP_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(60);

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
}

/// Bootstraps an embedded `PostgreSQL` instance, branching between root and unprivileged flows.
///
/// The bootstrap honours the following environment variables when present:
/// - `PG_RUNTIME_DIR`: Overrides the `PostgreSQL` installation directory.
/// - `PG_DATA_DIR`: Overrides the data directory used for initialisation.
/// - `PG_SUPERUSER`: Sets the superuser account name.
/// - `PG_PASSWORD`: Supplies the superuser password.
///
/// When executed as `root` on Unix platforms the runtime drops privileges to the `nobody` user
/// and prepares the filesystem on that user's behalf. Unprivileged executions reuse the current
/// user identity. The function returns a [`crate::Error`] describing failures encountered during
/// bootstrap.
///
/// This convenience wrapper discards the detailed [`TestBootstrapSettings`]. Call
/// [`bootstrap_for_tests`] to obtain the structured response for assertions.
///
/// # Examples
/// ```rust
/// use pg_embedded_setup_unpriv::run;
///
/// fn main() -> Result<(), pg_embedded_setup_unpriv::Error> {
///     run()?;
///     Ok(())
/// }
/// ```
///
/// # Errors
/// Returns an error when bootstrap preparation fails or when subprocess orchestration
/// cannot be configured.
pub fn run() -> CrateResult<()> {
    orchestrate_bootstrap()?;
    Ok(())
}

/// Bootstraps `PostgreSQL` for integration tests and surfaces the prepared settings.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::bootstrap_for_tests;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let bootstrap = bootstrap_for_tests()?;
/// for (key, value) in bootstrap.environment.to_env() {
///     match value {
///         Some(value) => std::env::set_var(&key, &value),
///         None => std::env::remove_var(&key),
///     }
/// }
/// // Launch application logic that relies on `bootstrap.settings` here.
/// # Ok(())
/// # }
/// ```
///
/// # Errors
/// Returns an error when bootstrap preparation fails or when subprocess orchestration
/// cannot be configured.
pub fn bootstrap_for_tests() -> BootstrapResult<TestBootstrapSettings> {
    orchestrate_bootstrap()
}

fn orchestrate_bootstrap() -> BootstrapResult<TestBootstrapSettings> {
    if let Err(err) = color_eyre::install() {
        tracing::debug!("color_eyre already installed: {err}");
    }

    let privileges = detect_execution_privileges();
    let cfg = PgEnvCfg::load().context("failed to load configuration via OrthoConfig")?;
    let settings = cfg.to_settings()?;
    let worker_binary = worker_binary_from_env()?;
    let shutdown_timeout = shutdown_timeout_from_env()?;
    let prepared = prepare_bootstrap(privileges, settings, &cfg)?;
    let execution_mode = determine_execution_mode(privileges, worker_binary.as_ref())?;

    Ok(TestBootstrapSettings {
        privileges,
        execution_mode,
        settings: prepared.settings,
        environment: prepared.environment,
        worker_binary,
        setup_timeout: DEFAULT_SETUP_TIMEOUT,
        start_timeout: DEFAULT_START_TIMEOUT,
        shutdown_timeout,
    })
}
