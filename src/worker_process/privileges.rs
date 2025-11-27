//! Drops elevated privileges for worker subprocesses where supported.
//!
//! The helper enforces that payload files are owned by the target unprivileged
//! account before execing the worker binary with the downgraded identity.

use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use color_eyre::eyre::{Context, eyre};
use std::path::Path;
use std::process::Command;
use tracing::{info, info_span};

macro_rules! cfg_privilege_drop {
    ($($item:item)*) => {
        $(
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
            $item
        )*
    };
}

cfg_privilege_drop! {
    use nix::unistd::{Gid, Uid, User, chown};
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
}

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
    return apply_unix(payload_path, command);

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
        apply_noop(payload_path, command)
    }
}

cfg_privilege_drop! {
    // Tracks nesting so privilege drop stays disabled while any guard is held.
    static SKIP_PRIVILEGE_DROP: AtomicUsize = AtomicUsize::new(0);

    #[expect(
        clippy::cognitive_complexity,
        reason = "tracing spans and early-return logging inflate complexity while flow is linear"
    )]
    fn apply_unix(payload_path: &Path, command: &mut Command) -> BootstrapResult<()> {
        let span = info_span!(
            target: LOG_TARGET,
            "privilege_drop",
            payload = %payload_path.display()
        );
        let _entered = span.enter();

        if skip_privilege_drop(payload_path) {
            return Ok(());
        }

        let (uid, gid) = resolve_nobody_ids()?;
        chown_payload(payload_path, uid, gid)?;
        configure_pre_exec(command, uid, gid);

        info!(
            target: LOG_TARGET,
            payload = %payload_path.display(),
            uid,
            gid,
            "configured worker command to drop privileges"
        );
        Ok(())
    }

    fn skip_privilege_drop(payload_path: &Path) -> bool {
        if skip_privilege_drop_for_tests() {
            info!(
                target: LOG_TARGET,
                payload = %payload_path.display(),
                "skipping privilege drop for tests"
            );
            true
        } else {
            false
        }
    }

    fn resolve_nobody_ids() -> BootstrapResult<(u32, u32)> {
        let user = User::from_name("nobody")
            .context("failed to resolve user 'nobody'")?
            .ok_or_else(|| eyre!("user 'nobody' not found"))?;
        Ok((user.uid.as_raw(), user.gid.as_raw()))
    }

    fn chown_payload(payload_path: &Path, uid: u32, gid: u32) -> BootstrapResult<()> {
        chown(
            payload_path,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .context("failed to chown worker payload to nobody")?;
        Ok(())
    }

    fn configure_pre_exec(command: &mut Command, uid: u32, gid: u32) {
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
fn apply_noop(_payload_path: &Path, _command: &mut Command) -> BootstrapResult<()> {
    info!(
        target: LOG_TARGET,
        "privilege drop unsupported on this platform; worker command left unchanged"
    );
    Ok(())
}

cfg_privilege_drop! {
    /// Guard that restores the privilege-drop toggle when dropped.
    ///
    /// Obtain the guard through [`disable_privilege_drop_for_tests`] when
    /// temporarily bypassing demotion during integration tests; dropping the guard
    /// re-enables the standard privilege enforcement automatically.
    #[derive(Debug)]
    pub(crate) struct PrivilegeDropGuard;

    impl Drop for PrivilegeDropGuard {
        fn drop(&mut self) {
            decrement_skip_privilege_drop();
        }
    }

    #[must_use]
    pub(crate) fn disable_privilege_drop_for_tests() -> PrivilegeDropGuard {
        SKIP_PRIVILEGE_DROP.fetch_add(1, Ordering::SeqCst);
        PrivilegeDropGuard
    }

    fn skip_privilege_drop_for_tests() -> bool {
        SKIP_PRIVILEGE_DROP.load(Ordering::SeqCst) > 0
    }

    fn decrement_skip_privilege_drop() {
        let update_result = SKIP_PRIVILEGE_DROP.fetch_update(
            Ordering::SeqCst,
            Ordering::SeqCst,
            |value| {
                debug_assert!(
                    value > 0,
                    "PrivilegeDropGuard dropped with zero privilege-drop counter"
                );
                if value == 0 {
                    None
                } else {
                    Some(value - 1)
                }
            },
        );

        debug_assert!(
            update_result.is_ok(),
            "PrivilegeDropGuard drop failed to update counter"
        );
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
const fn skip_privilege_drop_for_tests() -> bool {
    false
}

#[cfg(all(
    test,
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
    feature = "cluster-unit-tests"
))]
mod tests {
    use super::*;
    use crate::test_support::capture_info_logs;
    use std::process::Command;
    use tempfile::NamedTempFile;

    #[test]
    fn skip_guard_logs_observability() {
        let payload = NamedTempFile::new().expect("payload file");
        let mut command = Command::new("true");
        let guard = disable_privilege_drop_for_tests();

        let (logs, result) = capture_info_logs(|| apply(payload.path(), &mut command));
        drop(guard);

        assert!(result.is_ok(), "privilege drop skip should succeed");
        assert!(
            logs.iter()
                .any(|line| line.contains("skipping privilege drop for tests")),
            "expected skip log entry, got {logs:?}"
        );
    }
}
