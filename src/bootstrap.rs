use crate::PgEnvCfg;
use crate::privileges::{
    default_paths_for, drop_process_privileges, ensure_dir_for_user, ensure_tree_owned_by_user,
    make_data_dir_private,
};
use color_eyre::eyre::{Context, Result};
use nix::unistd::{Uid, User, chown, geteuid};
use ortho_config::OrthoConfig;
use postgresql_embedded::{PostgreSQL, Settings};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

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
    install_dir: PathBuf,
    data_dir: PathBuf,
    password_file: PathBuf,
    install_default: bool,
    data_default: bool,
}

fn ensure_settings_paths(settings: &mut Settings, cfg: &PgEnvCfg, uid: Uid) -> SettingsPaths {
    let (default_install_dir, default_data_dir) = default_paths_for(uid);
    let mut install_default = false;
    let mut data_default = false;
    if cfg.runtime_dir.is_none() {
        settings.installation_dir = default_install_dir.clone();
        install_default = true;
    }
    // Rebase the password file into the installation directory regardless of how Settings populated
    // it. The default uses a root-owned temporary directory which becomes inaccessible once we drop
    // privileges, so force it into a predictable, user-owned path instead.
    let password_file = settings.installation_dir.join(".pgpass");
    settings.password_file = password_file.clone();
    if cfg.data_dir.is_none() {
        settings.data_dir = default_data_dir.clone();
        data_default = true;
    }
    SettingsPaths {
        install_dir: settings.installation_dir.clone(),
        data_dir: settings.data_dir.clone(),
        password_file,
        install_default,
        data_default,
    }
}

fn set_env_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    // SAFETY: `std::env::set_var` mutates global process state. We pass only
    // trusted UTF-8 keys and values derived from configuration so platform
    // invariants hold.
    unsafe { env::set_var(key, value) }
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
            (ExecutionPrivileges::Root, settings) => bootstrap_with_root(&rt, settings, &cfg),
            (ExecutionPrivileges::Unprivileged, settings) => {
                bootstrap_unprivileged(&rt, settings, &cfg)
            }
        }
    }
    #[cfg(not(unix))]
    {
        bootstrap_unprivileged(&rt, settings, &cfg)
    }
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

    let paths = ensure_settings_paths(&mut settings, cfg, nobody_user.uid);
    let install_dir = paths.install_dir.clone();
    let data_dir = paths.data_dir.clone();
    let password_file = paths.password_file.clone();

    if paths.install_default
        && let Some(base_dir) = install_dir.parent()
    {
        ensure_dir_for_user(base_dir, nobody_user.uid, 0o755)?;
    }
    if paths.data_default
        && let Some(base_dir) = data_dir.parent()
    {
        ensure_dir_for_user(base_dir, nobody_user.uid, 0o755)?;
    }

    ensure_dir_for_user(&install_dir, nobody_user.uid, 0o755)?;
    if paths.install_default {
        ensure_tree_owned_by_user(&install_dir, &nobody_user)?;
    }

    make_data_dir_private(&data_dir, nobody_user.uid)?;
    if paths.data_default {
        ensure_tree_owned_by_user(&data_dir, &nobody_user)?;
    }

    if password_file.exists() {
        chown(&password_file, Some(nobody_user.uid), Some(nobody_user.gid))
            .with_context(|| format!("chown {}", password_file.display()))?;
        fs::set_permissions(&password_file, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", password_file.display()))?;
    }
    set_env_var("PGPASSFILE", &password_file);

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");

    let guard = drop_process_privileges(&nobody_user)?;
    set_env_var("HOME", &install_dir);
    set_env_var("XDG_CACHE_HOME", &cache_dir);
    set_env_var("XDG_RUNTIME_DIR", &runtime_dir);
    fs::create_dir_all(&cache_dir).with_context(|| format!("create {}", cache_dir.display()))?;
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create {}", runtime_dir.display()))?;

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
    let paths = ensure_settings_paths(&mut settings, cfg, uid);
    let install_dir = paths.install_dir.clone();
    let data_dir = paths.data_dir.clone();
    let password_file = paths.password_file.clone();

    if paths.install_default
        && let Some(base_dir) = install_dir.parent()
    {
        fs::create_dir_all(base_dir).with_context(|| format!("create {}", base_dir.display()))?;
    }
    if paths.data_default
        && let Some(base_dir) = data_dir.parent()
    {
        fs::create_dir_all(base_dir).with_context(|| format!("create {}", base_dir.display()))?;
    }

    fs::create_dir_all(&install_dir)
        .with_context(|| format!("create {}", install_dir.display()))?;
    fs::set_permissions(&install_dir, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod {}", install_dir.display()))?;

    fs::create_dir_all(&data_dir).with_context(|| format!("create {}", data_dir.display()))?;
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod {}", data_dir.display()))?;

    if password_file.exists() {
        fs::set_permissions(&password_file, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", password_file.display()))?;
    }
    set_env_var("PGPASSFILE", &password_file);

    let cache_dir = install_dir.join("cache");
    let runtime_dir = install_dir.join("run");
    set_env_var("HOME", &install_dir);
    set_env_var("XDG_CACHE_HOME", &cache_dir);
    set_env_var("XDG_RUNTIME_DIR", &runtime_dir);
    fs::create_dir_all(&cache_dir).with_context(|| format!("create {}", cache_dir.display()))?;
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create {}", runtime_dir.display()))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Uid;

    #[test]
    fn ensure_settings_paths_applies_defaults() {
        let cfg = PgEnvCfg::default();
        let mut settings = cfg.to_settings().expect("default config should convert");
        let uid = Uid::from_raw(9999);

        let paths = ensure_settings_paths(&mut settings, &cfg, uid);
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

        let paths = ensure_settings_paths(&mut settings, &cfg, uid);

        assert_eq!(paths.install_dir, PathBuf::from("/custom/install"));
        assert_eq!(paths.data_dir, PathBuf::from("/custom/data"));
        assert_eq!(
            paths.password_file,
            PathBuf::from("/custom/install").join(".pgpass")
        );
        assert!(!paths.install_default);
        assert!(!paths.data_default);
    }
}
