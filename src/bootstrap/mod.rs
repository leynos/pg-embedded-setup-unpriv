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
    /// Optional override for the binary cache directory.
    ///
    /// When set, `TestCluster` uses this directory instead of the default
    /// resolved from environment variables.
    pub binary_cache_dir: Option<camino::Utf8PathBuf>,
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
    orchestrate_bootstrap(BootstrapKind::Default)?;
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
        binary_cache_dir: cfg.binary_cache_dir,
    })
}

fn install_color_eyre() {
    if let Err(err) = color_eyre::install() {
        tracing::debug!("color_eyre already installed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scoped_env;
    use camino::Utf8PathBuf;
    use rstest::{fixture, rstest};
    use std::ffi::OsString;
    use tempfile::tempdir;

    /// Converts string key-value pairs to `OsString` pairs for `scoped_env`.
    fn env_vars<const N: usize>(
        pairs: [(&str, Option<&str>); N],
    ) -> Vec<(OsString, Option<OsString>)> {
        pairs
            .into_iter()
            .map(|(k, v)| (OsString::from(k), v.map(OsString::from)))
            .collect()
    }

    #[test]
    fn orchestrate_bootstrap_respects_env_overrides() {
        if detect_execution_privileges() == ExecutionPrivileges::Root {
            tracing::warn!(
                "skipping orchestrate test because root privileges require PG_EMBEDDED_WORKER"
            );
            return;
        }

        let runtime = tempdir().expect("runtime dir");
        let data = tempdir().expect("data dir");
        let runtime_path =
            Utf8PathBuf::from_path_buf(runtime.path().to_path_buf()).expect("runtime dir utf8");
        let data_path =
            Utf8PathBuf::from_path_buf(data.path().to_path_buf()).expect("data dir utf8");

        let _guard = scoped_env(env_vars([
            ("PG_RUNTIME_DIR", Some(runtime_path.as_str())),
            ("PG_DATA_DIR", Some(data_path.as_str())),
            ("PG_SUPERUSER", Some("bootstrap_test")),
            ("PG_PASSWORD", Some("bootstrap_test_pw")),
            ("PG_EMBEDDED_WORKER", None),
        ]));
        let settings = orchestrate_bootstrap(BootstrapKind::Default).expect("bootstrap to succeed");

        assert_paths(&settings, &runtime_path, &data_path);
        assert_identity(&settings, "bootstrap_test", "bootstrap_test_pw");
        assert_environment(&settings, &runtime_path);
    }

    /// Holds temporary directories for `run()` tests.
    struct RunTestPaths {
        _runtime: tempfile::TempDir,
        _data: tempfile::TempDir,
        runtime_path: Utf8PathBuf,
        data_path: Utf8PathBuf,
    }

    /// Fixture providing run test paths, returning `None` if running as root.
    #[fixture]
    fn run_test_paths() -> Option<RunTestPaths> {
        if detect_execution_privileges() == ExecutionPrivileges::Root {
            tracing::warn!("skipping run test because root privileges require PG_EMBEDDED_WORKER");
            return None;
        }

        let runtime = tempdir().expect("runtime dir");
        let data = tempdir().expect("data dir");
        let runtime_path =
            Utf8PathBuf::from_path_buf(runtime.path().to_path_buf()).expect("runtime dir utf8");
        let data_path =
            Utf8PathBuf::from_path_buf(data.path().to_path_buf()).expect("data dir utf8");

        Some(RunTestPaths {
            _runtime: runtime,
            _data: data,
            runtime_path,
            data_path,
        })
    }

    #[rstest]
    fn run_succeeds_with_customised_paths(run_test_paths: Option<RunTestPaths>) {
        let Some(paths) = run_test_paths else {
            return;
        };

        let _guard = scoped_env(env_vars([
            ("PG_RUNTIME_DIR", Some(paths.runtime_path.as_str())),
            ("PG_DATA_DIR", Some(paths.data_path.as_str())),
            ("PG_SUPERUSER", Some("bootstrap_run")),
            ("PG_PASSWORD", Some("bootstrap_run_pw")),
            ("PG_EMBEDDED_WORKER", None),
        ]));

        run().expect("run should bootstrap successfully");

        assert!(
            paths.runtime_path.join("cache").exists(),
            "cache directory should be created"
        );
        assert!(
            paths.runtime_path.join("run").exists(),
            "runtime directory should be created"
        );
    }

    /// Holds temporary directories and their UTF-8 paths for bootstrap tests.
    struct BootstrapPaths {
        _runtime: tempfile::TempDir,
        _data: tempfile::TempDir,
        _cache: tempfile::TempDir,
        runtime_path: Utf8PathBuf,
        data_path: Utf8PathBuf,
        cache_path: Utf8PathBuf,
    }

    /// Fixture providing bootstrap test paths, returning `None` if running as root.
    #[fixture]
    fn bootstrap_paths() -> Option<BootstrapPaths> {
        if detect_execution_privileges() == ExecutionPrivileges::Root {
            tracing::warn!(
                "skipping orchestrate test because root privileges require PG_EMBEDDED_WORKER"
            );
            return None;
        }

        let runtime = tempdir().expect("runtime dir");
        let data = tempdir().expect("data dir");
        let cache = tempdir().expect("cache dir");
        let runtime_path =
            Utf8PathBuf::from_path_buf(runtime.path().to_path_buf()).expect("runtime dir utf8");
        let data_path =
            Utf8PathBuf::from_path_buf(data.path().to_path_buf()).expect("data dir utf8");
        let cache_path =
            Utf8PathBuf::from_path_buf(cache.path().to_path_buf()).expect("cache dir utf8");

        Some(BootstrapPaths {
            _runtime: runtime,
            _data: data,
            _cache: cache,
            runtime_path,
            data_path,
            cache_path,
        })
    }

    /// Runs `orchestrate_bootstrap` with cache-related environment variables set.
    ///
    /// Uses the mutex-protected `scoped_env` to avoid racing with other tests.
    fn orchestrate_with_cache_env(paths: &BootstrapPaths) -> TestBootstrapSettings {
        let _guard = scoped_env(env_vars([
            ("PG_RUNTIME_DIR", Some(paths.runtime_path.as_str())),
            ("PG_DATA_DIR", Some(paths.data_path.as_str())),
            ("PG_BINARY_CACHE_DIR", Some(paths.cache_path.as_str())),
            ("PG_SUPERUSER", Some("cache_test")),
            ("PG_PASSWORD", Some("cache_test_pw")),
            ("PG_EMBEDDED_WORKER", None),
        ]));
        orchestrate_bootstrap(BootstrapKind::Default).expect("bootstrap to succeed")
    }

    #[rstest]
    fn orchestrate_bootstrap_propagates_binary_cache_dir(bootstrap_paths: Option<BootstrapPaths>) {
        let Some(paths) = bootstrap_paths else {
            return;
        };

        let settings = orchestrate_with_cache_env(&paths);

        assert_eq!(
            settings.binary_cache_dir,
            Some(paths.cache_path.clone()),
            "binary_cache_dir should propagate from PG_BINARY_CACHE_DIR"
        );
    }

    fn assert_paths(
        settings: &TestBootstrapSettings,
        runtime_path: &Utf8PathBuf,
        data_path: &Utf8PathBuf,
    ) {
        let observed_install =
            Utf8PathBuf::from_path_buf(settings.settings.installation_dir.clone())
                .expect("installation dir utf8");
        let observed_data =
            Utf8PathBuf::from_path_buf(settings.settings.data_dir.clone()).expect("data dir utf8");

        assert_eq!(observed_install.as_path(), runtime_path.as_path());
        assert_eq!(observed_data.as_path(), data_path.as_path());
    }

    fn assert_identity(
        settings: &TestBootstrapSettings,
        expected_user: &str,
        expected_password: &str,
    ) {
        assert_eq!(settings.settings.username, expected_user);
        assert_eq!(settings.settings.password, expected_password);
        assert_eq!(settings.privileges, ExecutionPrivileges::Unprivileged);
        assert_eq!(settings.execution_mode, ExecutionMode::InProcess);
        assert!(settings.worker_binary.is_none());
    }

    fn assert_environment(settings: &TestBootstrapSettings, runtime_path: &Utf8PathBuf) {
        let env_pairs = settings.environment.to_env();
        let pgpass = runtime_path.join(".pgpass");
        assert!(env_pairs.contains(&("PGPASSFILE".into(), Some(pgpass.as_str().into()))));
        assert_eq!(settings.environment.home.as_path(), runtime_path.as_path());
    }
}
