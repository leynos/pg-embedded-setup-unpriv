use camino::Utf8PathBuf;
#[cfg(unix)]
use color_eyre::eyre::Context;
use postgresql_embedded::Settings;

use crate::{
    PgEnvCfg,
    error::BootstrapResult,
    fs::{ensure_dir_exists, set_permissions},
};

use super::{env::TimezoneEnv, env::prepare_timezone_env, mode::ExecutionPrivileges};

#[cfg(unix)]
use crate::privileges::{
    default_paths_for, ensure_dir_for_user, ensure_tree_owned_by_user, make_data_dir_private,
};
#[cfg(unix)]
use nix::unistd::{Uid, User, chown, geteuid};

#[cfg(unix)]
struct SettingsPaths {
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
    install_default: bool,
    data_default: bool,
}

#[derive(Debug, Clone)]
struct XdgDirs {
    home: Utf8PathBuf,
    cache: Utf8PathBuf,
    runtime: Utf8PathBuf,
}

pub(super) fn prepare_bootstrap(
    privileges: ExecutionPrivileges,
    settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    #[cfg(unix)]
    {
        match privileges {
            ExecutionPrivileges::Root => bootstrap_with_root(settings, cfg),
            ExecutionPrivileges::Unprivileged => bootstrap_unprivileged(settings, cfg),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = privileges;
        bootstrap_unprivileged(settings, cfg)
    }
}

#[derive(Debug, Clone)]
pub struct TestBootstrapEnvironment {
    /// Effective home directory for the `PostgreSQL` user during the tests.
    pub home: Utf8PathBuf,
    /// Directory used for cached `PostgreSQL` artefacts.
    pub xdg_cache_home: Utf8PathBuf,
    /// Directory used for `PostgreSQL` runtime state, such as sockets.
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
    #[must_use]
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

pub(super) struct PreparedBootstrap {
    pub(super) settings: Settings,
    pub(super) environment: TestBootstrapEnvironment,
}

#[cfg(unix)]
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

#[cfg(unix)]
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
