//! Prepares filesystem state for the bootstrap flows.

use camino::{Utf8Path, Utf8PathBuf};
#[cfg(unix)]
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::Settings;

use crate::{
    PgEnvCfg,
    error::{BootstrapError, BootstrapResult},
    fs::{ensure_dir_exists, set_permissions},
    observability::LOG_TARGET,
};

use super::env::{TestBootstrapEnvironment, XdgDirs, prepare_timezone_env};

#[cfg(unix)]
use crate::privileges::{
    default_paths_for, ensure_dir_for_user, ensure_tree_owned_by_user, make_data_dir_private,
};
#[cfg(unix)]
use nix::unistd::{Uid, User, fchown, geteuid};
#[cfg(unix)]
use std::net::TcpListener;
use tracing::debug;

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
    // Worker subprocesses drop after each operation; keep the data dir so start can
    // proceed after setup.
    settings.temporary = false;
    ensure_root_port(&mut settings)?;

    let nobody_user = User::from_name("nobody")
        .context("failed to resolve user 'nobody'")?
        .ok_or_else(|| color_eyre::eyre::eyre!("user 'nobody' not found"))?;

    let paths = resolve_settings_paths_for_uid(&mut settings, cfg, nobody_user.uid)?;
    log_sanitized_settings(&settings);

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

#[cfg(unix)]
fn ensure_root_port(settings: &mut Settings) -> BootstrapResult<()> {
    if settings.port > 0 {
        return Ok(());
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|err| BootstrapError::from(eyre!("failed to allocate port: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| BootstrapError::from(eyre!("failed to read allocated port: {err}")))?
        .port();
    settings.port = port;
    Ok(())
}

fn bootstrap_unprivileged(
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    let paths = resolve_settings_paths_for_current_user(&mut settings, cfg)?;
    log_sanitized_settings(&settings);

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

/// Create the XDG cache and runtime directories with the expected
/// permissions.
///
/// The cache surface only stores extracted binaries and log files, so it
/// remains group/world-readable (0o755) to make debugging easier when the
/// helper runs inside CI sandboxes. The runtime directory, however, holds the
/// `PostgreSQL` socket, `postmaster.pid`, and `.pgpass`, so it is locked down
/// to user-only access (0o700) to avoid leaking credentials or allowing other
/// processes to tamper with the instance.
///
/// # Examples
///
/// ```ignore
/// use camino::Utf8PathBuf;
/// use crate::bootstrap::prepare::prepare_xdg_dirs;
///
/// let install_dir = Utf8PathBuf::from("/tmp/test-install");
/// let dirs = prepare_xdg_dirs(&install_dir).expect("xdg dirs");
/// assert_eq!(dirs.cache, install_dir.join("cache"));
/// assert_eq!(dirs.runtime, install_dir.join("run"));
/// ```
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

fn log_sanitized_settings(settings: &Settings) {
    let configuration_keys = sorted_configuration_keys(settings);
    let timeout_secs = settings.timeout.map(|duration| duration.as_secs());

    debug!(
        target: LOG_TARGET,
        version = %settings.version,
        host = %settings.host,
        port = settings.port,
        installation_dir = %settings.installation_dir.display(),
        data_dir = %settings.data_dir.display(),
        password_file = %settings.password_file.display(),
        username = %settings.username,
        password = "<redacted>",
        temporary = settings.temporary,
        timeout_secs,
        trust_installation_dir = settings.trust_installation_dir,
        configuration_keys = ?configuration_keys,
        "prepared postgres settings"
    );
}

fn sorted_configuration_keys(settings: &Settings) -> Vec<&str> {
    let mut keys: Vec<&str> = settings.configuration.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

#[cfg(unix)]
fn ensure_install_dir_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    ensure_dir_for_user(path, user, 0o755)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_pgpass_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    use nix::sys::stat::{Mode, fchmod};
    use std::os::fd::AsRawFd;

    // The descriptor-relative lookup anchors path resolution and prevents
    // ancestor directory swap attacks. O_NOFOLLOW additionally ensures the
    // final path component is not a symlink.
    let (dir, relative) = crate::fs::ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PGPASSFILE cannot point at the root directory"
        )));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .create(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = match dir.open_with(relative.as_std_path(), &options) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "open {} failed: {err}",
                path.as_str()
            )));
        }
    };
    let metadata = file.metadata().map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "stat {} failed: {err}",
            path.as_str()
        ))
    })?;
    if !metadata.is_file() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PGPASSFILE must reference a regular file: {}",
            path.as_str()
        )));
    }

    let fd = file.as_raw_fd();
    fchown(fd, Some(user.uid), Some(user.gid)).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchown {} failed: {err}",
            path.as_str()
        ))
    })?;
    fchmod(fd, Mode::from_bits_truncate(0o600)).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchmod {} failed: {err}",
            path.as_str()
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
