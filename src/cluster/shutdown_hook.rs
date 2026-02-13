//! Process-exit hook that stops the `PostgreSQL` postmaster on `atexit`.
//!
//! Shared test clusters use [`std::mem::forget`] on the [`ClusterGuard`](super::ClusterGuard)
//! to keep the cluster alive for the process lifetime. This prevents `Drop`
//! from running, leaving the postmaster orphaned after the test binary exits.
//!
//! This module provides [`register_shutdown_hook`], which stores cluster
//! metadata in a [`OnceLock`] and registers an `extern "C"` callback via
//! [`libc::atexit`]. When the process exits, the callback reads the
//! postmaster PID from disk, sends SIGTERM, polls for exit, and escalates
//! to SIGKILL if the timeout elapses.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use crate::CleanupMode;
use crate::error::BootstrapResult;
use postgresql_embedded::Settings;

/// State captured at registration time and read by the atexit callback.
struct ShutdownState {
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
}

/// One-time initialisation guard for the atexit callback.
static SHUTDOWN_STATE: OnceLock<ShutdownState> = OnceLock::new();

/// Polling interval when waiting for the postmaster to exit after SIGTERM.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Grace period after SIGKILL before proceeding to cleanup.
const POST_SIGKILL_GRACE: Duration = Duration::from_millis(100);

/// Registers an atexit hook that will stop the `PostgreSQL` postmaster on
/// process exit.
///
/// The hook is registered at most once per process. Subsequent calls are
/// idempotent no-ops and return `Ok(())`.
///
/// # Errors
///
/// Returns an error if `libc::atexit` reports failure (non-zero return).
pub(super) fn register_shutdown_hook(
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
) -> BootstrapResult<()> {
    if !try_store_state(settings, shutdown_timeout, cleanup_mode) {
        return Ok(());
    }
    register_atexit()?;
    log_registration_success();
    Ok(())
}

/// Attempts to store the shutdown state. Returns `true` if this is the
/// first registration, `false` if a hook was already registered.
fn try_store_state(
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
) -> bool {
    if SHUTDOWN_STATE
        .set(ShutdownState {
            settings,
            shutdown_timeout,
            cleanup_mode,
        })
        .is_err()
    {
        tracing::debug!(
            target: crate::observability::LOG_TARGET,
            "shutdown hook already registered; skipping duplicate registration"
        );
        return false;
    }
    true
}

/// Calls `libc::atexit` to register the shutdown callback.
fn register_atexit() -> BootstrapResult<()> {
    // SAFETY: `shutdown_callback` is an `extern "C"` function with no parameters
    // and no return value, matching the signature required by `atexit(3)`.
    // The function accesses only the `SHUTDOWN_STATE` static which is
    // initialised above and remains valid for the lifetime of the process.
    let rc = unsafe { libc::atexit(shutdown_callback) };
    if rc != 0 {
        return Err(color_eyre::eyre::eyre!("libc::atexit registration failed (rc={rc})").into());
    }
    Ok(())
}

/// Logs a successful hook registration.
fn log_registration_success() {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "registered atexit shutdown hook for PostgreSQL postmaster"
    );
}

/// Callback invoked by the C runtime during process exit.
///
/// Reads the postmaster PID from disk, sends SIGTERM, waits for exit, and
/// escalates to SIGKILL if the configured timeout expires.
extern "C" fn shutdown_callback() {
    let Some(state) = SHUTDOWN_STATE.get() else {
        return;
    };

    let Some(pid) = read_postmaster_pid(&state.settings.data_dir) else {
        // PID file missing — cluster already stopped or was never started.
        best_effort_cleanup(state);
        return;
    };

    if !process_is_running(pid) {
        best_effort_cleanup(state);
        return;
    }

    stop_postmaster(pid, state);
    best_effort_cleanup(state);
}

/// Sends SIGTERM to the postmaster and escalates to SIGKILL on timeout.
fn stop_postmaster(pid: libc::pid_t, state: &ShutdownState) {
    send_sigterm(pid);

    if wait_for_exit(pid, state.shutdown_timeout) {
        return;
    }

    // Timeout expired — escalate to SIGKILL.
    send_sigkill(pid);
    std::thread::sleep(POST_SIGKILL_GRACE);
}

// ---------------------------------------------------------------------------
// Signal helpers
// ---------------------------------------------------------------------------

/// Sends SIGTERM to the given PID.
fn send_sigterm(pid: libc::pid_t) {
    // SAFETY: Sending SIGTERM to a process we own. The PID was read from the
    // postmaster.pid file written by our PostgreSQL child process. If the PID
    // is stale (process already exited), `kill` returns -1 with ESRCH, which
    // we ignore.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// Sends SIGKILL to the given PID.
fn send_sigkill(pid: libc::pid_t) {
    // SAFETY: Same rationale as `send_sigterm`. SIGKILL cannot be caught or
    // ignored, so the process will terminate immediately if it still exists.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// Polls until the process exits or the timeout elapses.
///
/// Returns `true` if the process exited within the timeout.
fn wait_for_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if !process_is_running(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// PID file and process helpers
// ---------------------------------------------------------------------------

/// Reads the postmaster PID from `data_dir/postmaster.pid`.
///
/// Returns `None` if the file is missing, empty, or cannot be parsed.
fn read_postmaster_pid(data_dir: &Path) -> Option<libc::pid_t> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = std::fs::read_to_string(&pid_file).ok()?;
    let first_line = contents.lines().next()?;
    first_line.trim().parse::<libc::pid_t>().ok()
}

/// Returns `true` if a process with the given PID is currently running.
fn process_is_running(pid: libc::pid_t) -> bool {
    // SAFETY: `kill` with signal 0 probes whether the process exists without
    // delivering a signal. This is a standard POSIX technique for checking
    // process liveness.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::ESRCH
    )
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Best-effort directory cleanup after the postmaster has stopped.
fn best_effort_cleanup(state: &ShutdownState) {
    super::cleanup::cleanup_in_process(state.cleanup_mode, &state.settings, "atexit-shutdown-hook");
}

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn read_postmaster_pid_returns_pid_from_valid_file() {
        let dir = tempdir().expect("tempdir");
        let pid_file = dir.path().join("postmaster.pid");
        fs::write(&pid_file, "12345\nother\nlines\n").expect("write");

        let result = read_postmaster_pid(dir.path());

        assert_eq!(result, Some(12345));
    }

    #[test]
    fn read_postmaster_pid_returns_none_for_missing_file() {
        let dir = tempdir().expect("tempdir");

        let result = read_postmaster_pid(dir.path());

        assert_eq!(result, None);
    }

    #[test]
    fn read_postmaster_pid_returns_none_for_empty_file() {
        let dir = tempdir().expect("tempdir");
        let pid_file = dir.path().join("postmaster.pid");
        fs::write(&pid_file, "").expect("write");

        let result = read_postmaster_pid(dir.path());

        assert_eq!(result, None);
    }

    #[test]
    fn process_is_running_returns_true_for_current_process() {
        #[expect(
            clippy::cast_possible_wrap,
            reason = "process IDs are always within i32 range on Unix"
        )]
        let pid = std::process::id() as libc::pid_t;

        assert!(process_is_running(pid));
    }

    #[test]
    fn process_is_running_returns_false_for_nonexistent_pid() {
        // PID i32::MAX is extremely unlikely to be in use.
        assert!(!process_is_running(i32::MAX));
    }
}
