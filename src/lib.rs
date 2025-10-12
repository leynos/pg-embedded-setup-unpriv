//! Facilitates preparing an embedded PostgreSQL instance while dropping root
//! privileges.
//!
//! The library owns the lifecycle for configuring paths, permissions, and
//! process identity so the bundled PostgreSQL binaries can initialise safely
//! under an unprivileged account.
#![allow(non_snake_case)]

use color_eyre::eyre::{Context, Result, bail};
use nix::unistd::{
    Gid, Uid, User, chown, geteuid, getgroups, getresgid, getresuid, setgroups, setresgid,
    setresuid,
};
use ortho_config::OrthoConfig;
use postgresql_embedded::{PostgreSQL, Settings, VersionReq};
use serde::{Deserialize, Serialize};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Captures the process identity before dropping privileges so we can safely restore it.
struct PrivilegeDropGuard {
    ruid: Uid,
    euid: Uid,
    suid: Uid,
    rgid: Gid,
    egid: Gid,
    sgid: Gid,
    supplementary: Vec<Gid>,
}

impl Drop for PrivilegeDropGuard {
    fn drop(&mut self) {
        self.restore_best_effort();
    }
}

impl PrivilegeDropGuard {
    fn restore_best_effort(&self) {
        // Best-effort restoration; errors during drop should not panic.
        let _ = setgroups(&self.supplementary);
        let _ = setresgid(self.rgid, self.egid, self.sgid);
        let _ = setresuid(self.ruid, self.euid, self.suid);
    }
}

fn drop_process_privileges(user: &User) -> Result<PrivilegeDropGuard> {
    if !geteuid().is_root() {
        bail!("must start as root to drop privileges temporarily");
    }

    let uid_set = getresuid().context("getresuid failed")?;
    let gid_set = getresgid().context("getresgid failed")?;
    let ruid = uid_set.real;
    let euid = uid_set.effective;
    let suid = uid_set.saved;
    let rgid = gid_set.real;
    let egid = gid_set.effective;
    let sgid = gid_set.saved;
    let supplementary = getgroups().context("getgroups failed")?;

    let guard = PrivilegeDropGuard {
        ruid,
        euid,
        suid,
        rgid,
        egid,
        sgid,
        supplementary,
    };

    // Reduce supplementary groups first so subsequent permission checks do not
    // inherit ambient capabilities from the original uid.
    setgroups(&[user.gid]).context("setgroups failed")?;
    if let Err(err) = setresgid(user.gid, user.gid, guard.sgid) {
        guard.restore_best_effort();
        return Err(err).context("setresgid failed");
    }
    if let Err(err) = setresuid(user.uid, user.uid, guard.suid) {
        guard.restore_best_effort();
        return Err(err).context("setresuid failed");
    }

    Ok(guard)
}

fn default_paths_for(uid: Uid) -> (PathBuf, PathBuf) {
    let base = PathBuf::from(format!("/var/tmp/pg-embed-{}", uid.as_raw()));
    (base.join("install"), base.join("data"))
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

#[allow(non_snake_case)]
#[derive(Debug, Clone, Serialize, Deserialize, OrthoConfig, Default)]
#[ortho_config(prefix = "PG")]
pub struct PgEnvCfg {
    /// e.g. "=16.4.0" or "^17"
    pub version_req: Option<String>,
    pub port: Option<u16>,
    pub superuser: Option<String>,
    pub password: Option<String>,
    pub data_dir: Option<std::path::PathBuf>,
    pub runtime_dir: Option<std::path::PathBuf>,
    pub locale: Option<String>,
    pub encoding: Option<String>,
}

impl PgEnvCfg {
    /// Converts the configuration into a complete `postgresql_embedded::Settings` object.
    ///
    /// Applies version, connection, path, and locale settings from the current configuration.
    /// Returns an error if the version requirement is invalid.
    ///
    /// # Returns
    /// A fully configured `Settings` instance on success, or an error if configuration fails.
    pub fn to_settings(&self) -> Result<Settings> {
        let mut s = Settings::default();

        self.apply_version(&mut s)?;
        self.apply_connection(&mut s);
        self.apply_paths(&mut s);
        self.apply_locale(&mut s);

        Ok(s)
    }

    fn apply_version(&self, settings: &mut Settings) -> Result<()> {
        if let Some(ref vr) = self.version_req {
            settings.version =
                VersionReq::parse(vr).context("PG_VERSION_REQ invalid semver spec")?;
        }
        Ok(())
    }

    fn apply_connection(&self, settings: &mut Settings) {
        if let Some(p) = self.port {
            settings.port = p;
        }
        if let Some(ref u) = self.superuser {
            settings.username = u.clone();
        }
        if let Some(ref pw) = self.password {
            settings.password = pw.clone();
        }
    }

    fn apply_paths(&self, settings: &mut Settings) {
        if let Some(ref dir) = self.data_dir {
            settings.data_dir = dir.clone();
        }
        if let Some(ref dir) = self.runtime_dir {
            settings.installation_dir = dir.clone();
        }
    }

    /// Applies locale and encoding settings to the PostgreSQL configuration if specified
    /// in the environment.
    ///
    /// Inserts the `locale` and `encoding` values into the settings configuration map when
    /// present in the environment configuration.
    fn apply_locale(&self, settings: &mut Settings) {
        if let Some(ref loc) = self.locale {
            settings.configuration.insert("locale".into(), loc.clone());
        }
        if let Some(ref enc) = self.encoding {
            settings
                .configuration
                .insert("encoding".into(), enc.clone());
        }
    }
}

/// Temporary privilege drop helper (process‑wide!)
pub fn with_temp_euid<F, R>(target: Uid, body: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    let user = User::from_uid(target)
        .context("User::from_uid failed")?
        .ok_or_else(|| color_eyre::eyre::eyre!("no passwd entry for uid {}", target))?;
    let guard = drop_process_privileges(&user)?;
    let result = body();
    drop(guard);
    result
}

/// Prepare `dir` so `uid` can access it.
///
/// Creates the directory if it does not exist, sets its owner to `uid`, and
/// applies permissions (0755) so the unprivileged user can read and execute its
/// contents.
#[cfg(unix)]
fn ensure_dir_for_user<P: AsRef<Path>>(dir: P, uid: Uid, mode: u32) -> Result<()> {
    let dir = dir.as_ref();
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    chown(dir, Some(uid), None).with_context(|| format!("chown {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", dir.display()))?;
    Ok(())
}

#[cfg(unix)]
pub fn make_dir_accessible<P: AsRef<Path>>(dir: P, uid: Uid) -> Result<()> {
    ensure_dir_for_user(dir, uid, 0o755)
}

#[cfg(unix)]
/// Ensures `dir` exists, is owned by `uid`, and has PostgreSQL-compatible 0700 permissions.
///
/// PostgreSQL refuses to use a data directory that is accessible to other
/// users. This helper creates the directory (if needed), chowns it to `uid`,
/// and clamps permissions to `0700` to satisfy that requirement.
pub fn make_data_dir_private<P: AsRef<Path>>(dir: P, uid: Uid) -> Result<()> {
    ensure_dir_for_user(dir, uid, 0o700)
}

#[cfg(unix)]
fn ensure_tree_owned_by_user<P: AsRef<Path>>(root: P, user: &User) -> Result<()> {
    let mut stack = vec![root.as_ref().to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("read_dir {}", path.display())),
        };

        for entry in entries {
            let entry = entry.with_context(|| format!("iterate {}", path.display()))?;
            let entry_path = entry.path();
            chown(&entry_path, Some(user.uid), Some(user.gid))
                .with_context(|| format!("chown {}", entry_path.display()))?;
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                stack.push(entry_path);
            }
        }
    }
    Ok(())
}

pub fn nobody_uid() -> Uid {
    use nix::unistd::User;
    User::from_name("nobody")
        .ok()
        .flatten()
        .map(|u| u.uid)
        .unwrap_or_else(|| Uid::from_raw(65534))
}

pub fn run() -> Result<()> {
    color_eyre::install()?;

    // Running as non-root can't change ownership of installation/data
    // directories. Fail fast instead of attempting setup and confusing users.
    if !geteuid().is_root() {
        bail!("must be run as root");
    }
    let cfg = PgEnvCfg::load().context("failed to load configuration via OrthoConfig")?;
    let mut settings = cfg.to_settings()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime")?;

    #[cfg(unix)]
    {
        let nobody_user = User::from_name("nobody")
            .context("failed to resolve user 'nobody'")?
            .ok_or_else(|| color_eyre::eyre::eyre!("user 'nobody' not found"))?;

        let paths = ensure_settings_paths(&mut settings, &cfg, nobody_user.uid);
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
        fs::create_dir_all(&cache_dir)
            .with_context(|| format!("create {}", cache_dir.display()))?;
        fs::create_dir_all(&runtime_dir)
            .with_context(|| format!("create {}", runtime_dir.display()))?;

        rt.block_on(async {
            let mut pg = PostgreSQL::new(settings);
            pg.setup()
                .await
                .wrap_err("postgresql_embedded::setup() failed")?;
            Ok::<(), color_eyre::Report>(())
        })?;
        drop(guard);
    }
    #[cfg(not(unix))]
    {
        rt.block_on(async {
            let mut pg = PostgreSQL::new(settings);
            pg.setup()
                .await
                .wrap_err("postgresql_embedded::setup() failed")?;
            Ok::<(), color_eyre::Report>(())
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
