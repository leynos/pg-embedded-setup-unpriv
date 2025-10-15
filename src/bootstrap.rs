//! Bootstraps embedded PostgreSQL while adapting to the caller's privileges.
use crate::PgEnvCfg;
use crate::privileges::{
    default_paths_for, drop_process_privileges, ensure_dir_for_user, ensure_tree_owned_by_user,
    make_data_dir_private,
};
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, PermissionsExt},
};
use color_eyre::eyre::{Context, Result};
use nix::unistd::{Uid, User, chown, geteuid};
use ortho_config::OrthoConfig;
use postgresql_embedded::{PostgreSQL, Settings};
use std::env;
use std::io::ErrorKind;

/// Represents the privileges the process is running with when bootstrapping PostgreSQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPrivileges {
    /// The process owns `root` privileges and must drop to `nobody` for filesystem work.
    Root,
    /// The process is already unprivileged, so bootstrap tasks run with the current UID/GID.
    Unprivileged,
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

fn ensure_settings_paths(
    settings: &mut Settings,
    cfg: &PgEnvCfg,
    uid: Uid,
) -> Result<SettingsPaths> {
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
    unsafe { env::set_var(key, value.as_str()) }
}

pub fn run() -> Result<()> {
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
    {
        match (privileges, settings) {
            (ExecutionPrivileges::Root, settings) => {
                bootstrap_with_root(&rt, settings, &cfg)?;
            }
            (ExecutionPrivileges::Unprivileged, settings) => {
                bootstrap_unprivileged(&rt, settings, &cfg)?;
            }
        }
    }
    #[cfg(not(unix))]
    {
        bootstrap_unprivileged(&rt, settings, &cfg)?;
    }

    Ok(())
}

#[cfg(unix)]
fn bootstrap_with_root(
    rt: &tokio::runtime::Runtime,
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> Result<()> {
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

    if install_default && let Some(base_dir) = install_dir.parent() {
        ensure_dir_for_user(base_dir, nobody_user.uid, 0o755)?;
    }
    if data_default && let Some(base_dir) = data_dir.parent() {
        ensure_dir_for_user(base_dir, nobody_user.uid, 0o755)?;
    }

    ensure_dir_for_user(&install_dir, nobody_user.uid, 0o755)?;
    if install_default {
        ensure_tree_owned_by_user(&install_dir, &nobody_user)?;
    }

    make_data_dir_private(&data_dir, nobody_user.uid)?;
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
        set_mode(&password_file, 0o600)?;
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

    rt.block_on(async move {
        let mut pg = PostgreSQL::new(settings);
        pg.setup()
            .await
            .wrap_err("postgresql_embedded::setup() failed")?;
        Ok::<(), color_eyre::Report>(())
    })?;
    drop(guard);

    Ok(())
}

#[cfg(unix)]
fn bootstrap_unprivileged(
    rt: &tokio::runtime::Runtime,
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> Result<()> {
    let uid = geteuid();
    let SettingsPaths {
        install_dir,
        data_dir,
        password_file,
        install_default,
        data_default,
    } = ensure_settings_paths(&mut settings, cfg, uid)?;

    if install_default && let Some(base_dir) = install_dir.parent() {
        ensure_dir_exists(base_dir)?;
    }
    if data_default && let Some(base_dir) = data_dir.parent() {
        ensure_dir_exists(base_dir)?;
    }

    ensure_dir_exists(&install_dir)?;
    set_mode(&install_dir, 0o755)?;

    ensure_dir_exists(&data_dir)?;
    set_mode(&data_dir, 0o700)?;

    if password_file.as_std_path().exists() {
        set_mode(&password_file, 0o600)?;
    }
    set_env_path("PGPASSFILE", &password_file);

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    set_env_path("HOME", &install_dir);
    set_env_path("XDG_CACHE_HOME", &cache_dir);
    set_env_path("XDG_RUNTIME_DIR", &runtime_dir);
    ensure_dir_exists(&cache_dir)?;
    ensure_dir_exists(&runtime_dir)?;

    rt.block_on(async move {
        let mut pg = PostgreSQL::new(settings);
        pg.setup()
            .await
            .wrap_err("postgresql_embedded::setup() failed")?;
        Ok::<(), color_eyre::Report>(())
    })?;

    Ok(())
}

#[cfg(not(unix))]
fn bootstrap_unprivileged(
    rt: &tokio::runtime::Runtime,
    settings: Settings,
    _cfg: &PgEnvCfg,
) -> Result<()> {
    rt.block_on(async move {
        let mut pg = PostgreSQL::new(settings);
        pg.setup()
            .await
            .wrap_err("postgresql_embedded::setup() failed")?;
        Ok::<(), color_eyre::Report>(())
    })?;

    Ok(())
}

fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }
    dir.create_dir_all(relative.as_std_path())
        .or_else(|err| {
            if err.kind() == ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(err)
            }
        })
        .with_context(|| format!("create {}", path.as_str()))
}

fn set_mode(path: &Utf8Path, mode: u32) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }
    dir.set_permissions(
        relative.as_std_path(),
        cap_std::fs::Permissions::from_mode(mode),
    )
    .with_context(|| format!("chmod {}", path.as_str()))
}

fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    if path.has_root() {
        let stripped = path
            .strip_prefix("/")
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf());
        let dir = Dir::open_ambient_dir("/", ambient_authority())
            .context("open ambient root directory")?;
        Ok((dir, stripped))
    } else {
        let dir = Dir::open_ambient_dir(".", ambient_authority())
            .context("open ambient working directory")?;
        Ok((dir, path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Uid;
    use std::path::PathBuf;

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
            runtime_dir: Some(PathBuf::from("/custom/install")),
            data_dir: Some(PathBuf::from("/custom/data")),
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
