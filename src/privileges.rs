#![cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    )
))]
//! Privilege management helpers for dropping root access safely.
use crate::error::{PrivilegeError, PrivilegeResult};
use crate::fs::{ensure_dir_exists, set_permissions};
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, DirEntry},
};
use color_eyre::eyre::{Context, eyre};
use nix::unistd::{
    Gid, Uid, User, chown, geteuid, getgroups, getresgid, getresuid, setgroups, setresgid,
    setresuid,
};
use std::io::ErrorKind;

/// Captures the process identity before dropping privileges so we can safely restore it.
pub(crate) struct PrivilegeDropGuard {
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

pub(crate) fn drop_process_privileges(user: &User) -> PrivilegeResult<PrivilegeDropGuard> {
    if !geteuid().is_root() {
        return Err(PrivilegeError::from(eyre!(
            "must start as root to drop privileges temporarily"
        )));
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
        let report = eyre!(err).wrap_err("setresgid failed");
        return Err(PrivilegeError::from(report));
    }
    if let Err(err) = setresuid(user.uid, user.uid, guard.suid) {
        guard.restore_best_effort();
        let report = eyre!(err).wrap_err("setresuid failed");
        return Err(PrivilegeError::from(report));
    }

    Ok(guard)
}

pub(crate) fn ensure_dir_for_user<P: AsRef<Utf8Path>>(
    dir: P,
    user: &User,
    mode: u32,
) -> PrivilegeResult<()> {
    let dir = dir.as_ref();
    ensure_dir_exists(dir)?;
    chown(dir.as_std_path(), Some(user.uid), Some(user.gid))
        .with_context(|| format!("chown {}", dir.as_str()))?;
    set_permissions(dir, mode)?;
    Ok(())
}

/// Ensures `dir` exists, is owned by `user`, and grants world-readable access.
///
/// The example returns `PrivilegeResult` so callers propagate the helper's
/// domain-specific error type rather than the opaque crate alias.
/// # Examples
/// ```no_run
/// use nix::unistd::User;
/// use pg_embedded_setup_unpriv::make_dir_accessible;
///
/// # fn demo(user: &User) -> pg_embedded_setup_unpriv::error::PrivilegeResult<()> {
/// let dir = camino::Utf8Path::new("/var/tmp/my-install");
/// make_dir_accessible(dir, user)?;
/// # Ok(())
/// # }
/// ```
pub fn make_dir_accessible<P: AsRef<Utf8Path>>(dir: P, user: &User) -> PrivilegeResult<()> {
    ensure_dir_for_user(dir, user, 0o755)
}

/// Ensures `dir` exists, is owned by `user`, and has PostgreSQL-compatible 0700 permissions.
///
/// PostgreSQL refuses to use a data directory that is accessible to other
/// users. This helper creates the directory (if needed), chowns it to `user`,
/// and clamps permissions to `0700` to satisfy that requirement.
///
/// The example returns `PrivilegeResult` to demonstrate how callers surface the
/// helper's domain errors when composing setup flows.
///
/// # Examples
/// ```no_run
/// use nix::unistd::User;
/// use pg_embedded_setup_unpriv::make_data_dir_private;
///
/// # fn demo(user: &User) -> pg_embedded_setup_unpriv::error::PrivilegeResult<()> {
/// let dir = camino::Utf8Path::new("/var/tmp/my-data");
/// make_data_dir_private(dir, user)?;
/// # Ok(())
/// # }
/// ```
pub fn make_data_dir_private<P: AsRef<Utf8Path>>(dir: P, user: &User) -> PrivilegeResult<()> {
    ensure_dir_for_user(dir, user, 0o700)
}

pub(crate) fn ensure_tree_owned_by_user<P: AsRef<Utf8Path>>(
    root: P,
    user: &User,
) -> PrivilegeResult<()> {
    let mut stack = vec![root.as_ref().to_path_buf()];
    while let Some(path_buf) = stack.pop() {
        let path = path_buf.as_path();
        if let Some(dir_result) = open_directory_if_exists(path) {
            let dir = dir_result?;
            process_directory_entries(path, &dir, user, &mut stack)?;
        }
    }
    Ok(())
}

fn open_directory_if_exists(path: &Utf8Path) -> Option<PrivilegeResult<Dir>> {
    match Dir::open_ambient_dir(path.as_std_path(), ambient_authority()) {
        Ok(dir) => Some(Ok(dir)),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => {
            let report = eyre!(err).wrap_err(format!("open directory {}", path.as_str()));
            Some(Err(PrivilegeError::from(report)))
        }
    }
}

fn process_directory_entries(
    path: &Utf8Path,
    dir: &Dir,
    user: &User,
    stack: &mut Vec<Utf8PathBuf>,
) -> PrivilegeResult<()> {
    for entry in dir
        .entries()
        .with_context(|| format!("read_dir {}", path.as_str()))?
    {
        let entry = entry.with_context(|| format!("iterate {}", path.as_str()))?;
        let entry_path = resolve_entry_path(path, &entry)?;
        chown_entry(&entry_path, user)?;
        if is_directory(&entry) {
            stack.push(entry_path);
        }
    }
    Ok(())
}

fn resolve_entry_path(path: &Utf8Path, entry: &DirEntry) -> PrivilegeResult<Utf8PathBuf> {
    let joined = path.as_std_path().join(entry.file_name());
    let entry_path = Utf8PathBuf::from_path_buf(joined)
        .map_err(|_| eyre!("non-UTF-8 path under {}", path.as_str()))?;
    Ok(entry_path)
}

fn chown_entry(path: &Utf8Path, user: &User) -> PrivilegeResult<()> {
    chown(path.as_std_path(), Some(user.uid), Some(user.gid))
        .with_context(|| format!("chown {}", path.as_str()))?;
    Ok(())
}

fn is_directory(entry: &DirEntry) -> bool {
    entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
}

/// Retrieves the UID of the `nobody` account, defaulting to 65534 when absent.
///
/// # Examples
/// ```
/// let uid = pg_embedded_setup_unpriv::nobody_uid();
/// assert!(uid.as_raw() > 0);
/// ```
pub fn nobody_uid() -> Uid {
    use nix::unistd::User;
    User::from_name("nobody")
        .ok()
        .flatten()
        .map(|u| u.uid)
        .unwrap_or_else(|| Uid::from_raw(65534))
}

/// Computes default installation and data directories for a given uid.
///
/// # Examples
/// ```
/// use nix::unistd::Uid;
///
/// let uid = Uid::from_raw(1000);
/// let (install, data) = pg_embedded_setup_unpriv::default_paths_for(uid);
/// assert!(install.as_str().contains("pg-embed-"));
/// assert!(data.as_str().contains("pg-embed-"));
/// ```
pub fn default_paths_for(uid: Uid) -> (Utf8PathBuf, Utf8PathBuf) {
    let base = Utf8PathBuf::from(format!("/var/tmp/pg-embed-{}", uid.as_raw()));
    (base.join("install"), base.join("data"))
}

/// Temporarily switches the process effective user ID for test scenarios.
///
/// # Safety
/// This function mutates the entire process identity. Callers must ensure no
/// other threads perform privileged operations while the guard is active.
/// Prefer invoking this helper in single-threaded test binaries only.
///
/// The example returns `PrivilegeResult` to reinforce that callers should
/// propagate privilege failures through the domain error surface.
///
/// # Examples
/// ```
/// # use nix::unistd::Uid;
/// use pg_embedded_setup_unpriv::with_temp_euid;
///
/// # fn demo(uid: Uid) -> pg_embedded_setup_unpriv::error::PrivilegeResult<()> {
/// with_temp_euid(uid, || Ok(()))?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "privileged-tests")]
pub fn with_temp_euid<F, R>(target: Uid, body: F) -> crate::Result<R>
where
    F: FnOnce() -> crate::Result<R>,
{
    let user = User::from_uid(target)
        .context("User::from_uid failed")
        .map_err(PrivilegeError::from)?
        .ok_or_else(|| color_eyre::eyre::eyre!("no passwd entry for uid {}", target))
        .map_err(PrivilegeError::from)?;
    let guard = drop_process_privileges(&user)?;
    let result = body()?;
    drop(guard);
    Ok(result)
}
