//! Bootstraps embedded PostgreSQL while adapting to the caller's privileges.
use crate::PgEnvCfg;
use crate::error::BootstrapResult;
use crate::fs::{ensure_dir_exists, set_permissions};
use crate::privileges::{
    default_paths_for, drop_process_privileges, ensure_dir_for_user, ensure_tree_owned_by_user,
    make_data_dir_private,
};
use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Context;
use nix::unistd::{Uid, User, chown, geteuid};
use postgresql_embedded::{PostgreSQL, Settings};
use std::env;
use std::path::PathBuf;

/// Represents the privileges the process is running with when bootstrapping PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPrivileges {
    /// The process owns `root` privileges and must drop to `nobody` for filesystem work.
    Root,
    /// The process is already unprivileged, so bootstrap tasks run with the current UID/GID.
    Unprivileged,
}

/// Captures the environment variables prepared for test executions.
#[derive(Debug, Clone)]
pub struct TestBootstrapEnvironment {
    pub home: Utf8PathBuf,
    pub xdg_cache_home: Utf8PathBuf,
    pub xdg_runtime_dir: Utf8PathBuf,
    pub pgpass_file: Utf8PathBuf,
    pub tz_dir: Utf8PathBuf,
    pub timezone: String,
}

impl TestBootstrapEnvironment {
    fn new(
        home: Utf8PathBuf,
        cache: Utf8PathBuf,
        runtime: Utf8PathBuf,
        pgpass_file: Utf8PathBuf,
        timezone: TimezoneEnv,
    ) -> Self {
        Self {
            home,
            xdg_cache_home: cache,
            xdg_runtime_dir: runtime,
            pgpass_file,
            tz_dir: timezone.dir,
            timezone: timezone.zone,
        }
    }

    /// Returns the prepared environment variables as key/value pairs.
    pub fn to_env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.home.as_str().into()),
            ("XDG_CACHE_HOME".into(), self.xdg_cache_home.as_str().into()),
            (
                "XDG_RUNTIME_DIR".into(),
                self.xdg_runtime_dir.as_str().into(),
            ),
            ("PGPASSFILE".into(), self.pgpass_file.as_str().into()),
            ("TZDIR".into(), self.tz_dir.as_str().into()),
            ("TZ".into(), self.timezone.clone()),
        ]
    }
}

/// Structured settings returned from [`bootstrap_for_tests`].
#[derive(Debug, Clone)]
pub struct TestBootstrapSettings {
    pub privileges: ExecutionPrivileges,
    pub settings: Settings,
    pub environment: TestBootstrapEnvironment,
}

/// Determines the current execution privileges for the bootstrap sequence.
///
/// Linux root users trigger the privileged path, whilst all other contexts – including
/// non-Unix platforms – follow the unprivileged flow. The detection itself is deliberately
/// lightweight: a simple effective-UID probe avoids shelling out, keeps start-up fast, and is
/// testable via `with_temp_euid`.
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
    dir: Utf8PathBuf,
    zone: String,
}

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

fn set_env_path(key: &str, value: &Utf8Path) {
    // `std::env::set_var` remains `unsafe` while the crate forbids ambient mutation without
    // explicit acknowledgement. Keep the `unsafe` block narrowly scoped to this helper so the
    // callers do not need to propagate it.
    unsafe {
        env::set_var(key, value.as_str());
    }
}

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
                "timezone database not found at {}. Set TZDIR or install tzdata.",
                path
            )
            .into());
        }
        path
    } else {
        let candidate = timezone_dir_candidates()
            .iter()
            .map(Utf8Path::new)
            .find(|path| path.exists())
            .ok_or_else(|| -> crate::error::BootstrapError {
                color_eyre::eyre::eyre!("timezone database not found. Set TZDIR or install tzdata.")
                    .into()
            })?;
        set_env_path("TZDIR", candidate);
        candidate.to_owned()
    };

    let timezone = match env::var("TZ") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            unsafe {
                env::set_var("TZ", DEFAULT_TIMEZONE);
            }
            DEFAULT_TIMEZONE.to_string()
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(color_eyre::eyre::eyre!("TZ must be valid UTF-8").into());
        }
    };

    Ok(TimezoneEnv {
        dir: tz_dir,
        zone: timezone,
    })
}

fn timezone_dir_candidates() -> &'static [&'static str] {
    #[cfg(unix)]
    {
        static CANDIDATES: [&str; 2] = ["/usr/share/zoneinfo", "/usr/lib/zoneinfo"];
        &CANDIDATES
    }
    #[cfg(not(unix))]
    {
        &[]
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
/// # Examples
/// ```no_run
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

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime")?;

    #[cfg(unix)]
    let prepared = {
        match (privileges, settings) {
            (ExecutionPrivileges::Root, settings) => bootstrap_with_root(&rt, settings, &cfg)?,
            (ExecutionPrivileges::Unprivileged, settings) => {
                bootstrap_unprivileged(&rt, settings, &cfg)?
            }
        }
    };

    #[cfg(not(unix))]
    let prepared = bootstrap_unprivileged(&rt, settings, &cfg)?;

    Ok(TestBootstrapSettings {
        privileges,
        settings: prepared.settings,
        environment: prepared.environment,
    })
}

#[cfg(unix)]
#[expect(
    clippy::collapsible_if,
    reason = "Keep the privilege-branch parameters explicit for staged directory prep"
)]
fn bootstrap_with_root(
    rt: &tokio::runtime::Runtime,
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
    set_env_path("PGPASSFILE", &password_file);

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");

    let guard = drop_process_privileges(&nobody_user)?;
    set_env_path("HOME", &install_dir);
    set_env_path("XDG_CACHE_HOME", &cache_dir);
    set_env_path("XDG_RUNTIME_DIR", &runtime_dir);
    ensure_dir_exists(&cache_dir)?;
    ensure_dir_exists(&runtime_dir)?;
    let timezone = prepare_timezone_env()?;
    let setup_settings = settings.clone();

    rt.block_on(async move {
        let mut pg = PostgreSQL::new(setup_settings);
        pg.setup()
            .await
            .wrap_err("postgresql_embedded::setup() failed")?;
        Ok::<(), color_eyre::Report>(())
    })?;
    drop(guard);

    let environment =
        TestBootstrapEnvironment::new(install_dir, cache_dir, runtime_dir, password_file, timezone);

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
    rt: &tokio::runtime::Runtime,
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
    set_env_path("PGPASSFILE", &password_file);

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    set_env_path("HOME", &install_dir);
    set_env_path("XDG_CACHE_HOME", &cache_dir);
    set_env_path("XDG_RUNTIME_DIR", &runtime_dir);
    ensure_dir_exists(&cache_dir)?;
    ensure_dir_exists(&runtime_dir)?;
    let timezone = prepare_timezone_env()?;
    let setup_settings = settings.clone();

    rt.block_on(async move {
        let mut pg = PostgreSQL::new(setup_settings);
        pg.setup()
            .await
            .wrap_err("postgresql_embedded::setup() failed")?;
        Ok::<(), color_eyre::Report>(())
    })?;

    let environment =
        TestBootstrapEnvironment::new(install_dir, cache_dir, runtime_dir, password_file, timezone);

    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

#[cfg(not(unix))]
fn bootstrap_unprivileged(
    rt: &tokio::runtime::Runtime,
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
    set_env_path("PGPASSFILE", &password_file);

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    set_env_path("HOME", &install_dir);
    set_env_path("XDG_CACHE_HOME", &cache_dir);
    set_env_path("XDG_RUNTIME_DIR", &runtime_dir);
    ensure_dir_exists(&cache_dir)?;
    ensure_dir_exists(&runtime_dir)?;
    let timezone = prepare_timezone_env()?;
    let setup_settings = settings.clone();

    rt.block_on(async move {
        let mut pg = PostgreSQL::new(setup_settings);
        pg.setup()
            .await
            .wrap_err("postgresql_embedded::setup() failed")?;
        Ok::<(), color_eyre::Report>(())
    })?;

    let environment =
        TestBootstrapEnvironment::new(install_dir, cache_dir, runtime_dir, password_file, timezone);

    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

#[cfg(test)]
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
