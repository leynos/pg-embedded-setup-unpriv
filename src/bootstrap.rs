//! Bootstraps embedded PostgreSQL while adapting to the caller's privileges.
//!
//! Provides [`bootstrap_for_tests`] so suites can retrieve structured settings and
//! prepared environment variables without reimplementing bootstrap orchestration.
use crate::PgEnvCfg;
use crate::error::{BootstrapError, BootstrapResult};
use crate::fs::{ensure_dir_exists, set_permissions};
use crate::privileges::{
    default_paths_for, ensure_dir_for_user, ensure_tree_owned_by_user, make_data_dir_private,
};
use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Context;
#[cfg(unix)]
use nix::unistd::{Uid, User, chown, geteuid};
use postgresql_embedded::Settings;
use std::env;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

/// Represents the privileges the process is running with when bootstrapping PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPrivileges {
    /// The process owns `root` privileges and must drop to `nobody` for filesystem work.
    Root,
    /// The process is already unprivileged, so bootstrap tasks run with the current UID/GID.
    Unprivileged,
}

/// Selects how PostgreSQL lifecycle commands run when privileged execution is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Execute lifecycle commands directly within the current process.
    ///
    /// This mode is only appropriate when the process already runs without elevated privileges.
    InProcess,
    /// Delegate lifecycle commands to a helper subprocess executed with reduced privileges.
    Subprocess,
}

/// Groups related XDG Base Directory paths to reduce parameter clutter.
#[derive(Debug, Clone)]
struct XdgDirs {
    home: Utf8PathBuf,
    cache: Utf8PathBuf,
    runtime: Utf8PathBuf,
}

/// Captures the environment variables prepared for test executions.
#[derive(Debug, Clone)]
pub struct TestBootstrapEnvironment {
    /// Effective home directory for the PostgreSQL user during the tests.
    pub home: Utf8PathBuf,
    /// Directory used for cached PostgreSQL artefacts.
    pub xdg_cache_home: Utf8PathBuf,
    /// Directory used for PostgreSQL runtime state, such as sockets.
    pub xdg_runtime_dir: Utf8PathBuf,
    /// Location of the generated `.pgpass` file.
    pub pgpass_file: Utf8PathBuf,
    /// Resolved time zone database directory, if discovery succeeded.
    pub tz_dir: Option<Utf8PathBuf>,
    /// Time zone identifier exported via the `TZ` environment variable.
    pub timezone: String,
}

impl TestBootstrapEnvironment {
    fn new(xdg: XdgDirs, pgpass_file: Utf8PathBuf, timezone: TimezoneEnv) -> Self {
        Self {
            home: xdg.home,
            xdg_cache_home: xdg.cache,
            xdg_runtime_dir: xdg.runtime,
            pgpass_file,
            tz_dir: timezone.dir,
            timezone: timezone.zone,
        }
    }

    /// Returns the prepared environment variables as key/value pairs.
    pub fn to_env(&self) -> Vec<(String, Option<String>)> {
        let mut env = vec![
            ("HOME".into(), Some(self.home.as_str().into())),
            (
                "XDG_CACHE_HOME".into(),
                Some(self.xdg_cache_home.as_str().into()),
            ),
            (
                "XDG_RUNTIME_DIR".into(),
                Some(self.xdg_runtime_dir.as_str().into()),
            ),
            ("PGPASSFILE".into(), Some(self.pgpass_file.as_str().into())),
        ];

        env.push((
            "TZDIR".into(),
            self.tz_dir.as_ref().map(|dir| dir.as_str().into()),
        ));

        env.push(("TZ".into(), Some(self.timezone.clone())));

        env
    }
}

/// Structured settings returned from [`bootstrap_for_tests`].
#[derive(Debug, Clone)]
pub struct TestBootstrapSettings {
    /// Privilege level detected for the current process.
    pub privileges: ExecutionPrivileges,
    /// Strategy for executing PostgreSQL lifecycle commands.
    pub execution_mode: ExecutionMode,
    /// PostgreSQL configuration prepared for the embedded instance.
    pub settings: Settings,
    /// Environment variables required to exercise the embedded instance.
    pub environment: TestBootstrapEnvironment,
    /// Optional path to the helper binary used for subprocess execution.
    pub worker_binary: Option<Utf8PathBuf>,
    /// Grace period granted to PostgreSQL during drop before teardown proceeds regardless.
    pub shutdown_timeout: Duration,
}

/// Determines the current execution privileges for the bootstrap sequence.
///
/// Linux root users trigger the privileged path, whilst all other contexts – including
/// non-Unix platforms – follow the unprivileged flow. The detection itself is deliberately
/// lightweight: a simple effective-UID probe avoids shelling out and keeps start-up fast while
/// remaining easy to exercise inside integration tests that run the subprocess-based bootstrap.
pub fn detect_execution_privileges() -> ExecutionPrivileges {
    #[cfg(unix)]
    {
        if geteuid().is_root() {
            ExecutionPrivileges::Root
        } else {
            ExecutionPrivileges::Unprivileged
        }
    }

    #[cfg(not(unix))]
    {
        ExecutionPrivileges::Unprivileged
    }
}

#[cfg(unix)]
struct SettingsPaths {
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
    install_default: bool,
    data_default: bool,
}

struct PreparedBootstrap {
    settings: Settings,
    environment: TestBootstrapEnvironment,
}

#[derive(Debug, Clone)]
struct TimezoneEnv {
    dir: Option<Utf8PathBuf>,
    zone: String,
}

#[cfg(unix)]
fn ensure_settings_paths(
    settings: &mut Settings,
    cfg: &PgEnvCfg,
    uid: Uid,
) -> BootstrapResult<SettingsPaths> {
    let (default_install_dir, default_data_dir) = default_paths_for(uid);
    let mut install_default = false;
    let mut data_default = false;

    if cfg.runtime_dir.is_none() {
        settings.installation_dir = default_install_dir.clone().into_std_path_buf();
        install_default = true;
    }
    if cfg.data_dir.is_none() {
        settings.data_dir = default_data_dir.clone().into_std_path_buf();
        data_default = true;
    }

    let install_dir = Utf8PathBuf::from_path_buf(settings.installation_dir.clone())
        .map_err(|_| color_eyre::eyre::eyre!("installation_dir must be valid UTF-8"))?;
    let data_dir = Utf8PathBuf::from_path_buf(settings.data_dir.clone())
        .map_err(|_| color_eyre::eyre::eyre!("data_dir must be valid UTF-8"))?;

    // Rebase the password file into the installation directory regardless of how Settings
    // populated it. The default uses a root-owned temporary directory which becomes
    // inaccessible once we drop privileges, so force it into a predictable, user-owned path
    // instead.
    let password_file = install_dir.join(".pgpass");
    settings.password_file = password_file.clone().into_std_path_buf();

    Ok(SettingsPaths {
        install_dir,
        data_dir,
        password_file,
        install_default,
        data_default,
    })
}

/// Determines the appropriate timezone settings for the bootstrap sequence.
///
/// # Errors
/// On Unix, fails if `TZDIR` is unset and no time zone database is discovered in
/// the standard locations. Configure `TZDIR` explicitly when the database is
/// installed elsewhere.
fn prepare_timezone_env() -> BootstrapResult<TimezoneEnv> {
    const DEFAULT_TIMEZONE: &str = "UTC";

    let tz_dir = if let Some(dir) = env::var_os("TZDIR") {
        let path = Utf8PathBuf::from_path_buf(PathBuf::from(dir)).map_err(
            |_| -> crate::error::BootstrapError {
                color_eyre::eyre::eyre!("TZDIR must be valid UTF-8").into()
            },
        )?;
        if !path.exists() {
            return Err(color_eyre::eyre::eyre!(
                "time zone database not found at {}. Set TZDIR or install tzdata.",
                path
            )
            .into());
        }
        Some(path)
    } else {
        discover_timezone_dir()?
    };

    let timezone = match env::var("TZ") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(std::env::VarError::NotPresent) => DEFAULT_TIMEZONE.to_string(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(color_eyre::eyre::eyre!("TZ must be valid UTF-8").into());
        }
    };

    Ok(TimezoneEnv {
        dir: tz_dir,
        zone: timezone,
    })
}

fn discover_timezone_dir() -> BootstrapResult<Option<Utf8PathBuf>> {
    #[cfg(unix)]
    {
        static CANDIDATES: [&str; 4] = [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/etc/zoneinfo",
            "/share/zoneinfo",
        ];

        let candidate = CANDIDATES
            .iter()
            .map(Utf8Path::new)
            .find(|path| path.exists())
            .ok_or_else(|| -> crate::error::BootstrapError {
                color_eyre::eyre::eyre!(
                    "time zone database not found. Set TZDIR or install tzdata."
                )
                .into()
            })?;

        Ok(Some(candidate.to_owned()))
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Bootstraps an embedded PostgreSQL instance, branching between root and unprivileged flows.
///
/// The bootstrap honours the following environment variables when present:
/// - `PG_RUNTIME_DIR`: Overrides the PostgreSQL installation directory.
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
pub fn run() -> crate::Result<()> {
    orchestrate_bootstrap()?;
    Ok(())
}

/// Bootstraps PostgreSQL for integration tests and surfaces the prepared settings.
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
pub fn bootstrap_for_tests() -> BootstrapResult<TestBootstrapSettings> {
    orchestrate_bootstrap()
}

fn orchestrate_bootstrap() -> BootstrapResult<TestBootstrapSettings> {
    // `color_eyre::install()` is idempotent for logging but returns an error if invoked twice.
    // Behavioural tests exercise consecutive bootstraps, so ignore the duplicate registration.
    let _ = color_eyre::install();

    let privileges = detect_execution_privileges();
    let cfg = PgEnvCfg::load().context("failed to load configuration via OrthoConfig")?;
    let settings = cfg.to_settings()?;

    let worker_binary = env::var_os("PG_EMBEDDED_WORKER")
        .map(|raw| {
            Utf8PathBuf::from_path_buf(PathBuf::from(raw)).map_err(|_| {
                BootstrapError::from(color_eyre::eyre::eyre!(
                    "PG_EMBEDDED_WORKER must contain a valid UTF-8 path"
                ))
            })
        })
        .transpose()?;

    if let Some(worker) = worker_binary
        .as_ref()
        .filter(|path| !path.as_std_path().exists())
    {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must reference an existing file: {worker}"
        )));
    }

    #[cfg(unix)]
    let prepared = {
        match (privileges, settings) {
            (ExecutionPrivileges::Root, settings) => bootstrap_with_root(settings, &cfg)?,
            (ExecutionPrivileges::Unprivileged, settings) => {
                bootstrap_unprivileged(settings, &cfg)?
            }
        }
    };

    #[cfg(not(unix))]
    let prepared = bootstrap_unprivileged(settings, &cfg)?;

    #[cfg(unix)]
    let execution_mode = match privileges {
        ExecutionPrivileges::Root => {
            if worker_binary.is_none() {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "PG_EMBEDDED_WORKER must be set when running with root privileges"
                )));
            }
            ExecutionMode::Subprocess
        }
        ExecutionPrivileges::Unprivileged => ExecutionMode::InProcess,
    };

    #[cfg(not(unix))]
    let execution_mode = ExecutionMode::InProcess;

    Ok(TestBootstrapSettings {
        privileges,
        execution_mode,
        settings: prepared.settings,
        environment: prepared.environment,
        worker_binary,
        shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
    })
}

#[cfg(unix)]
#[expect(
    clippy::collapsible_if,
    reason = "Keep the privilege-branch parameters explicit for staged directory prep"
)]
fn bootstrap_with_root(
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    let nobody_user = User::from_name("nobody")
        .context("failed to resolve user 'nobody'")?
        .ok_or_else(|| color_eyre::eyre::eyre!("user 'nobody' not found"))?;

    let SettingsPaths {
        install_dir,
        data_dir,
        password_file,
        install_default,
        data_default,
    } = ensure_settings_paths(&mut settings, cfg, nobody_user.uid)?;

    if install_default {
        if let Some(base_dir) = install_dir.parent() {
            ensure_dir_for_user(base_dir, &nobody_user, 0o755)?;
        }
    }
    if data_default {
        if let Some(base_dir) = data_dir.parent() {
            ensure_dir_for_user(base_dir, &nobody_user, 0o755)?;
        }
    }

    ensure_dir_for_user(&install_dir, &nobody_user, 0o755)?;
    if install_default {
        ensure_tree_owned_by_user(&install_dir, &nobody_user)?;
    }

    make_data_dir_private(&data_dir, &nobody_user)?;
    if data_default {
        ensure_tree_owned_by_user(&data_dir, &nobody_user)?;
    }

    if password_file.as_std_path().exists() {
        chown(
            password_file.as_std_path(),
            Some(nobody_user.uid),
            Some(nobody_user.gid),
        )
        .with_context(|| format!("chown {}", password_file.as_str()))?;
        set_permissions(&password_file, 0o600)?;
    }

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");

    ensure_dir_exists(&cache_dir)?;
    set_permissions(&cache_dir, 0o755)?;
    ensure_dir_exists(&runtime_dir)?;
    set_permissions(&runtime_dir, 0o700)?;
    let timezone = prepare_timezone_env()?;
    let xdg = XdgDirs {
        home: install_dir,
        cache: cache_dir,
        runtime: runtime_dir,
    };
    let environment = TestBootstrapEnvironment::new(xdg, password_file, timezone);
    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

#[cfg(unix)]
#[expect(
    clippy::collapsible_if,
    reason = "Keep the privilege-branch parameters explicit for staged directory prep"
)]
fn bootstrap_unprivileged(
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    let uid = geteuid();
    let SettingsPaths {
        install_dir,
        data_dir,
        password_file,
        install_default,
        data_default,
    } = ensure_settings_paths(&mut settings, cfg, uid)?;

    if install_default {
        if let Some(base_dir) = install_dir.parent() {
            ensure_dir_exists(base_dir)?;
        }
    }
    if data_default {
        if let Some(base_dir) = data_dir.parent() {
            ensure_dir_exists(base_dir)?;
        }
    }

    ensure_dir_exists(&install_dir)?;
    set_permissions(&install_dir, 0o755)?;

    ensure_dir_exists(&data_dir)?;
    set_permissions(&data_dir, 0o700)?;

    if password_file.as_std_path().exists() {
        set_permissions(&password_file, 0o600)?;
    }

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    ensure_dir_exists(&cache_dir)?;
    set_permissions(&cache_dir, 0o755)?;
    ensure_dir_exists(&runtime_dir)?;
    set_permissions(&runtime_dir, 0o700)?;
    let timezone = prepare_timezone_env()?;
    let xdg = XdgDirs {
        home: install_dir,
        cache: cache_dir,
        runtime: runtime_dir,
    };
    let environment = TestBootstrapEnvironment::new(xdg, password_file, timezone);
    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

#[cfg(not(unix))]
fn bootstrap_unprivileged(
    mut settings: Settings,
    _cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    let install_dir = Utf8PathBuf::from_path_buf(settings.installation_dir.clone())
        .map_err(|_| color_eyre::eyre::eyre!("installation_dir must be valid UTF-8"))?;
    let data_dir = Utf8PathBuf::from_path_buf(settings.data_dir.clone())
        .map_err(|_| color_eyre::eyre::eyre!("data_dir must be valid UTF-8"))?;
    let password_file = install_dir.join(".pgpass");
    settings.password_file = password_file.clone().into_std_path_buf();

    ensure_dir_exists(&install_dir)?;
    set_permissions(&install_dir, 0o755)?;
    ensure_dir_exists(&data_dir)?;
    set_permissions(&data_dir, 0o700)?;

    if password_file.as_std_path().exists() {
        set_permissions(&password_file, 0o600)?;
    }

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    ensure_dir_exists(&cache_dir)?;
    set_permissions(&cache_dir, 0o755)?;
    ensure_dir_exists(&runtime_dir)?;
    set_permissions(&runtime_dir, 0o700)?;
    let timezone = prepare_timezone_env()?;
    let xdg = XdgDirs {
        home: install_dir,
        cache: cache_dir,
        runtime: runtime_dir,
    };
    let environment = TestBootstrapEnvironment::new(xdg, password_file, timezone);
    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nix::unistd::Uid;

    #[test]
    fn ensure_settings_paths_applies_defaults() {
        let cfg = PgEnvCfg::default();
        let mut settings = cfg.to_settings().expect("default config should convert");
        let uid = Uid::from_raw(9999);

        let paths = ensure_settings_paths(&mut settings, &cfg, uid).expect("settings paths");
        let (expected_install, expected_data) = default_paths_for(uid);

        assert_eq!(paths.install_dir, expected_install);
        assert_eq!(paths.data_dir, expected_data);
        assert_eq!(paths.password_file, expected_install.join(".pgpass"));
        assert!(paths.install_default);
        assert!(paths.data_default);
    }

    #[test]
    fn ensure_settings_paths_respects_user_provided_dirs() {
        let cfg = PgEnvCfg {
            runtime_dir: Some(Utf8PathBuf::from("/custom/install")),
            data_dir: Some(Utf8PathBuf::from("/custom/data")),
            ..PgEnvCfg::default()
        };
        let mut settings = cfg.to_settings().expect("custom config should convert");
        let uid = Uid::from_raw(4242);

        let paths = ensure_settings_paths(&mut settings, &cfg, uid).expect("settings paths");

        assert_eq!(paths.install_dir, Utf8PathBuf::from("/custom/install"));
        assert_eq!(paths.data_dir, Utf8PathBuf::from("/custom/data"));
        assert_eq!(
            paths.password_file,
            Utf8PathBuf::from("/custom/install").join(".pgpass")
        );
        assert!(!paths.install_default);
        assert!(!paths.data_default);
    }
}
