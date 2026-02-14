//! Process-exit hook that stops the `PostgreSQL` postmaster on `atexit`.
//!
//! Shared test clusters use [`std::mem::forget`] on the [`ClusterGuard`](super::ClusterGuard)
//! to keep the cluster alive for the process lifetime. This prevents `Drop`
//! from running, leaving the postmaster orphaned after the test binary exits.
//!
//! This module provides [`register_shutdown_hook`], which stores cluster
//! metadata in a [`Mutex`] and registers an `extern "C"` callback via
//! [`libc::atexit`]. When the process exits, the callback reads the
//! postmaster PID from disk, sends SIGTERM (signal 15, terminate), polls for
//! exit, and escalates to SIGKILL if the timeout elapses.

use std::path::Path;
use std::sync::Mutex;
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

/// Initialisation guard for the atexit callback.
///
/// Uses `Mutex<Option<...>>` rather than `OnceLock` so that state can be
/// rolled back if `libc::atexit` registration fails, avoiding a poisoned
/// state where subsequent calls silently no-op.
static SHUTDOWN_STATE: Mutex<Option<ShutdownState>> = Mutex::new(None);

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
    let mut guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if guard.is_some() {
        log_already_registered();
        return Ok(());
    }

    register_atexit()?;

    // Store state only AFTER atexit succeeds, so a failed registration
    // does not poison the slot for future attempts.
    *guard = Some(ShutdownState {
        settings,
        shutdown_timeout,
        cleanup_mode,
    });

    log_registration_success();
    Ok(())
}

/// Logs that a duplicate registration was skipped.
fn log_already_registered() {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "shutdown hook already registered; skipping duplicate registration"
    );
}

/// Logs a successful hook registration.
fn log_registration_success() {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "registered atexit shutdown hook for PostgreSQL postmaster"
    );
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

/// Callback invoked by the C runtime during process exit.
///
/// Reads the postmaster PID from disk, sends SIGTERM, waits for exit, and
/// escalates to SIGKILL if the configured timeout expires.
extern "C" fn shutdown_callback() {
    let guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let Some(state) = guard.as_ref() else {
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
/// Returns `None` if the file is missing, empty, cannot be parsed, or
/// contains a non-positive value.
#[must_use]
pub fn read_postmaster_pid(data_dir: &Path) -> Option<libc::pid_t> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = std::fs::read_to_string(&pid_file).ok()?;
    let first_line = contents.lines().next()?;
    let pid = first_line.trim().parse::<libc::pid_t>().ok()?;
    if pid > 0 { Some(pid) } else { None }
}

/// Returns `true` if a process with the given PID is currently running.
///
/// Non-positive PIDs are rejected immediately (returns `false`) to avoid
/// calling `libc::kill` with 0 (current process group) or negative values
/// (process groups).
#[must_use]
pub fn process_is_running(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 probes whether the process exists without
    // delivering a signal. This is a standard POSIX technique for checking
    // process liveness. The guard above ensures `pid` is positive.
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

    use color_eyre::eyre::{Result, ensure};
    use rstest::{fixture, rstest};
    use tempfile::TempDir;

    /// Creates a fresh temporary directory for PID file tests.
    #[fixture]
    fn pid_dir() -> Result<TempDir> {
        Ok(tempfile::tempdir()?)
    }

    #[rstest]
    #[case::valid_file(Some("12345\nother\nlines\n"), Some(12345))]
    #[case::missing_file(None, None)]
    #[case::empty_file(Some(""), None)]
    #[case::zero_pid(Some("0\n"), None)]
    #[case::negative_pid(Some("-1\n"), None)]
    fn read_postmaster_pid_parses_first_line(
        pid_dir: Result<TempDir>,
        #[case] file_content: Option<&str>,
        #[case] expected: Option<libc::pid_t>,
    ) -> Result<()> {
        let dir = pid_dir?;
        if let Some(content) = file_content {
            std::fs::write(dir.path().join("postmaster.pid"), content)?;
        }

        let result = read_postmaster_pid(dir.path());

        ensure!(result == expected, "expected {expected:?}, got {result:?}");
        Ok(())
    }

    #[test]
    fn process_is_running_returns_true_for_current_process() -> Result<()> {
        let pid = libc::pid_t::try_from(std::process::id())?;

        ensure!(process_is_running(pid), "current process should be running");
        Ok(())
    }

    #[test]
    fn process_is_running_returns_false_for_nonexistent_pid() -> Result<()> {
        // PID i32::MAX is extremely unlikely to be in use.
        ensure!(
            !process_is_running(i32::MAX),
            "nonexistent PID should not be running"
        );
        Ok(())
    }

    #[rstest]
    #[case::zero(0)]
    #[case::negative(-1)]
    fn process_is_running_rejects_non_positive_pid(#[case] pid: libc::pid_t) -> Result<()> {
        ensure!(
            !process_is_running(pid),
            "non-positive PID {pid} should not be considered running"
        );
        Ok(())
    }
}
