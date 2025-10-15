//! Privilege management helpers for dropping root access safely.
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, PermissionsExt},
};
use color_eyre::eyre::{Context, Result, bail};
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

pub(crate) fn drop_process_privileges(user: &User) -> Result<PrivilegeDropGuard> {
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

pub(crate) fn ensure_dir_for_user<P: AsRef<Utf8Path>>(dir: P, uid: Uid, mode: u32) -> Result<()> {
    let dir = dir.as_ref();
    ensure_dir_exists(dir)?;
    chown(dir.as_std_path(), Some(uid), None).with_context(|| format!("chown {}", dir.as_str()))?;
    set_permissions(dir, mode)?;
    Ok(())
}

pub fn make_dir_accessible<P: AsRef<Utf8Path>>(dir: P, uid: Uid) -> Result<()> {
    ensure_dir_for_user(dir, uid, 0o755)
}

/// Ensures `dir` exists, is owned by `uid`, and has PostgreSQL-compatible 0700 permissions.
///
/// PostgreSQL refuses to use a data directory that is accessible to other
/// users. This helper creates the directory (if needed), chowns it to `uid`,
/// and clamps permissions to `0700` to satisfy that requirement.
pub fn make_data_dir_private<P: AsRef<Utf8Path>>(dir: P, uid: Uid) -> Result<()> {
    ensure_dir_for_user(dir, uid, 0o700)
}

pub(crate) fn ensure_tree_owned_by_user<P: AsRef<Utf8Path>>(root: P, user: &User) -> Result<()> {
    let mut stack = vec![root.as_ref().to_path_buf()];
    while let Some(path) = stack.pop() {
        let dir = match Dir::open_ambient_dir(path.as_std_path(), ambient_authority()) {
            Ok(dir) => dir,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("open directory {}", path.as_str()));
            }
        };

        for entry in dir
            .entries()
            .with_context(|| format!("read_dir {}", path.as_str()))?
        {
            let entry = entry.with_context(|| format!("iterate {}", path.as_str()))?;
            let joined = path.as_std_path().join(entry.file_name());
            let entry_path = Utf8PathBuf::from_path_buf(joined)
                .map_err(|_| color_eyre::eyre::eyre!("non-UTF-8 path under {}", path.as_str()))?;
            chown(entry_path.as_std_path(), Some(user.uid), Some(user.gid))
                .with_context(|| format!("chown {}", entry_path.as_str()))?;
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

pub fn default_paths_for(uid: Uid) -> (Utf8PathBuf, Utf8PathBuf) {
    let base = Utf8PathBuf::from(format!("/var/tmp/pg-embed-{}", uid.as_raw()));
    (base.join("install"), base.join("data"))
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

fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
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
