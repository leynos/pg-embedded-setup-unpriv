//! Prepares filesystem state for the bootstrap flows.

use camino::{Utf8Path, Utf8PathBuf};
#[cfg(unix)]
use color_eyre::eyre::Context;
use postgresql_embedded::Settings;

use crate::{
    PgEnvCfg,
    error::{BootstrapError, BootstrapResult},
    fs::{ensure_dir_exists, set_permissions},
};

use super::env::{TestBootstrapEnvironment, XdgDirs, prepare_timezone_env};

#[cfg(unix)]
use crate::privileges::{
    default_paths_for, ensure_dir_for_user, ensure_tree_owned_by_user, make_data_dir_private,
};
#[cfg(unix)]
use nix::unistd::{Uid, User, chown, geteuid};

pub(super) fn prepare_bootstrap(
    privileges: super::mode::ExecutionPrivileges,
    settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    #[cfg(unix)]
    {
        match privileges {
            super::mode::ExecutionPrivileges::Root => bootstrap_with_root(settings, cfg),
            super::mode::ExecutionPrivileges::Unprivileged => bootstrap_unprivileged(settings, cfg),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = privileges;
        bootstrap_unprivileged(settings, cfg)
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

    let paths = resolve_settings_paths_for_uid(&mut settings, cfg, nobody_user.uid)?;

    ensure_parents_for_paths(&paths, |path| ensure_parent_for_user(path, &nobody_user))?;

    ensure_install_dir_for_user(&paths.install_dir, &nobody_user)?;
    make_data_dir_private(&paths.data_dir, &nobody_user)?;

    let timezone = prepare_timezone_env()?;
    let xdg = prepare_xdg_dirs(&paths.install_dir)?;
    ensure_xdg_dirs_owned_by_user(&xdg, &nobody_user)?;

    ensure_pgpass_for_user(&paths.password_file, &nobody_user)?;

    ensure_tree_owned_by_user(&paths.install_dir, &nobody_user)?;
    if paths.data_default {
        ensure_tree_owned_by_user(&paths.data_dir, &nobody_user)?;
    }

    let environment = TestBootstrapEnvironment::from_components(xdg, paths.password_file, timezone);
    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

fn bootstrap_unprivileged(
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    let paths = resolve_settings_paths_for_current_user(&mut settings, cfg)?;

    ensure_parents_for_paths(&paths, ensure_parent_exists)?;

    ensure_dir_with_mode(&paths.install_dir, 0o755)?;
    ensure_dir_with_mode(&paths.data_dir, 0o700)?;
    ensure_pgpass_permissions(&paths.password_file)?;

    let timezone = prepare_timezone_env()?;
    let xdg = prepare_xdg_dirs(&paths.install_dir)?;
    let environment = TestBootstrapEnvironment::from_components(xdg, paths.password_file, timezone);
    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

struct SettingsPaths {
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
    install_default: bool,
    data_default: bool,
}

#[cfg(unix)]
fn resolve_settings_paths_for_uid(
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

    settings_paths_from_settings(settings, install_default, data_default)
}

#[cfg(unix)]
fn resolve_settings_paths_for_current_user(
    settings: &mut Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<SettingsPaths> {
    let uid = geteuid();
    resolve_settings_paths_for_uid(settings, cfg, uid)
}

#[cfg(not(unix))]
fn resolve_settings_paths_for_current_user(
    settings: &mut Settings,
    _cfg: &PgEnvCfg,
) -> BootstrapResult<SettingsPaths> {
    settings_paths_from_settings(settings, false, false)
}

fn settings_paths_from_settings(
    settings: &mut Settings,
    install_default: bool,
    data_default: bool,
) -> BootstrapResult<SettingsPaths> {
    let install_dir = Utf8PathBuf::from_path_buf(settings.installation_dir.clone())
        .map_err(|_| color_eyre::eyre::eyre!("installation_dir must be valid UTF-8"))?;
    let data_dir = Utf8PathBuf::from_path_buf(settings.data_dir.clone())
        .map_err(|_| color_eyre::eyre::eyre!("data_dir must be valid UTF-8"))?;

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

fn ensure_parents_for_paths<F>(paths: &SettingsPaths, mut ensure_parent: F) -> BootstrapResult<()>
where
    F: FnMut(&Utf8PathBuf) -> BootstrapResult<()>,
{
    if paths.install_default {
        ensure_parent(&paths.install_dir)?;
    }
    if paths.data_default {
        ensure_parent(&paths.data_dir)?;
    }
    Ok(())
}

fn ensure_dir_with_mode(path: &Utf8Path, mode: u32) -> BootstrapResult<()> {
    ensure_dir_exists(path).map_err(BootstrapError::from)?;
    set_permissions(path, mode).map_err(BootstrapError::from)
}

fn ensure_pgpass_permissions(path: &Utf8PathBuf) -> BootstrapResult<()> {
    match set_permissions(path, 0o600) {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
                if io_err.kind() == std::io::ErrorKind::NotFound {
                    return Ok(());
                }
            }
            Err(BootstrapError::from(err))
        }
    }
}

fn prepare_xdg_dirs(install_dir: &Utf8PathBuf) -> BootstrapResult<XdgDirs> {
    let cache = install_dir.join("cache");
    let runtime = install_dir.join("run");
    // Cache files are harmless to share, so grant read access for debugging.
    ensure_dir_with_mode(&cache, 0o755)?;
    // Runtime dir holds sockets/pids; clamp to user-only for safety.
    ensure_dir_with_mode(&runtime, 0o700)?;
    Ok(XdgDirs {
        home: install_dir.clone(),
        cache,
        runtime,
    })
}

#[cfg(unix)]
fn ensure_xdg_dirs_owned_by_user(xdg: &XdgDirs, user: &User) -> BootstrapResult<()> {
    // The cache/run directories are created by the root worker, so explicitly
    // hand them to the unprivileged user to keep custom install dirs usable.
    ensure_dir_for_user(&xdg.cache, user, 0o755)?;
    ensure_dir_for_user(&xdg.runtime, user, 0o700)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_parent_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_for_user(parent, user, 0o755)?;
    }
    Ok(())
}

fn ensure_parent_exists(path: &Utf8PathBuf) -> BootstrapResult<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_exists(parent).map_err(BootstrapError::from)?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_install_dir_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    ensure_dir_for_user(path, user, 0o755)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_pgpass_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    match chown(path.as_std_path(), Some(user.uid), Some(user.gid)) {
        Ok(()) => {}
        Err(nix::errno::Errno::ENOENT) => return Ok(()),
        Err(err) => {
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "chown {} failed: {err}",
                path.as_str()
            )));
        }
    }

    ensure_pgpass_permissions(path)
}

#[cfg(test)]
mod behaviour_tests {
    use super::*;
    use temp_env::with_vars;
    use tempfile::tempdir;

    #[test]
    fn bootstrap_unprivileged_sets_up_directories() {
        let runtime = tempdir().expect("runtime dir");
        let data = tempdir().expect("data dir");
        let runtime_dir =
            Utf8PathBuf::from_path_buf(runtime.path().to_path_buf()).expect("runtime dir utf8");
        let data_dir =
            Utf8PathBuf::from_path_buf(data.path().to_path_buf()).expect("data dir utf8");

        let cfg = PgEnvCfg {
            runtime_dir: Some(runtime_dir.clone()),
            data_dir: Some(data_dir.clone()),
            ..PgEnvCfg::default()
        };
        let settings = cfg.to_settings().expect("settings");

        let prepared = with_vars(
            [("TZDIR", Some(runtime_dir.as_str())), ("TZ", Some("UTC"))],
            move || bootstrap_unprivileged(settings, &cfg),
        )
        .expect("bootstrap");

        assert_eq!(prepared.environment.home, runtime_dir);
        assert!(prepared.environment.xdg_cache_home.exists());
        assert!(prepared.environment.xdg_runtime_dir.exists());
        assert_eq!(
            prepared.environment.pgpass_file,
            runtime_dir.join(".pgpass")
        );
        let observed_install =
            Utf8PathBuf::from_path_buf(prepared.settings.installation_dir.clone())
                .expect("installation dir utf8");
        let observed_data =
            Utf8PathBuf::from_path_buf(prepared.settings.data_dir.clone()).expect("data dir utf8");
        assert_eq!(observed_install, runtime_dir);
        assert_eq!(observed_data, data_dir);
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use nix::unistd::Uid;

    #[test]
    fn ensure_settings_paths_applies_defaults() {
        let cfg = PgEnvCfg::default();
        let mut settings = cfg.to_settings().expect("default config should convert");
        let uid = Uid::from_raw(9999);

        let paths =
            resolve_settings_paths_for_uid(&mut settings, &cfg, uid).expect("settings paths");
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

        let paths =
            resolve_settings_paths_for_uid(&mut settings, &cfg, uid).expect("settings paths");

        assert_eq!(paths.install_dir, Utf8PathBuf::from("/custom/install"));
        assert_eq!(paths.data_dir, Utf8PathBuf::from("/custom/data"));
        assert_eq!(
            paths.password_file,
            Utf8PathBuf::from("/custom/install").join(".pgpass"),
        );
        assert!(!paths.install_default);
        assert!(!paths.data_default);
    }
}
