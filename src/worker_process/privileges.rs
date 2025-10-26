//! Drops elevated privileges for worker subprocesses where supported.
//!
//! The helper enforces that payload files are owned by the target unprivileged
//! account before execing the worker binary with the downgraded identity.

use crate::error::BootstrapResult;
use color_eyre::eyre::{Context, eyre};
use std::path::Path;
use std::process::Command;

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use nix::unistd::{Gid, Uid, User, chown};
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use std::os::unix::process::CommandExt;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use std::sync::atomic::{AtomicBool, Ordering};

/// Applies privilege-dropping configuration to a worker command.
///
/// On supported Unix platforms, resolves the "nobody" account, reassigns the
/// worker payload to that user, and arranges to demote credentials immediately
/// before `exec`. Unsupported platforms treat the helper as a no-op so tests and
/// non-Unix builds continue to function.
///
/// # Errors
///
/// Returns an error if resolving the "nobody" account fails or if updating the
/// payload ownership is unsuccessful.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
/// use std::process::Command;
///
/// use pg_embedded_setup_unpriv::worker_process::privileges;
///
/// # fn demo() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let payload = Path::new("/tmp/worker_payload.json");
/// let mut command = Command::new("/usr/local/bin/worker");
/// privileges::apply(payload, &mut command)?;
/// # Ok(())
/// # }
/// ```
pub(crate) fn apply(payload_path: &Path, command: &mut Command) -> BootstrapResult<()> {
    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    ))]
    {
        if skip_privilege_drop_for_tests() {
            return Ok(());
        }

        let user = User::from_name("nobody")
            .context("failed to resolve user 'nobody'")?
            .ok_or_else(|| eyre!("user 'nobody' not found"))?;
        let uid = user.uid.as_raw();
        let gid = user.gid.as_raw();

        chown(
            payload_path,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .context("failed to chown worker payload to nobody")?;

        unsafe {
            // SAFETY: This closure executes immediately before `exec` whilst the process
            // still owns elevated credentials. The synchronous UID/GID demotion mirrors the
            // previous inlined implementation in `TestCluster::spawn_worker` and keeps the
            // privilege adjustments ordered: groups, gid, then uid.
            command.pre_exec(move || {
                if libc::setgroups(0, std::ptr::null()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setgid(gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setuid(uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(not(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    )))]
    {
        let _ = payload_path;
        let _ = command;
    }

    Ok(())
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
static SKIP_PRIVILEGE_DROP: AtomicBool = AtomicBool::new(false);

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
/// Guard that restores the privilege-drop toggle when dropped.
///
/// Obtain the guard through [`disable_privilege_drop_for_tests`] when
/// temporarily bypassing demotion during integration tests; dropping the guard
/// re-enables the standard privilege enforcement automatically.
#[derive(Debug)]
pub(crate) struct PrivilegeDropGuard {
    previous: bool,
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
impl Drop for PrivilegeDropGuard {
    fn drop(&mut self) {
        SKIP_PRIVILEGE_DROP.store(self.previous, Ordering::SeqCst);
    }
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
#[must_use]
pub(crate) fn disable_privilege_drop_for_tests() -> PrivilegeDropGuard {
    let previous = SKIP_PRIVILEGE_DROP.swap(true, Ordering::SeqCst);
    PrivilegeDropGuard { previous }
}

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
fn skip_privilege_drop_for_tests() -> bool {
    SKIP_PRIVILEGE_DROP.load(Ordering::SeqCst)
}

#[cfg(not(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
)))]
const fn skip_privilege_drop_for_tests() -> bool {
    false
}
