//! End-to-end lifecycle test for the atexit shutdown hook.
//!
//! Verifies that `PostgreSQL` processes do not survive test binary exit when
//! the shutdown hook is registered. Uses a subprocess pattern: the parent
//! spawns itself as a child process which creates a cluster, registers the
//! hook, writes the postmaster PID to a temp file, then calls
//! `std::process::exit(0)`. The parent waits for the child to exit and then
//! confirms the postmaster has also terminated.
#![cfg(unix)]

#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/skip.rs"]
mod skip;

use std::path::Path;
use std::time::Duration;
use std::{env, fs, thread};

use cluster_skip::cluster_skip_message;
use color_eyre::eyre::{Context, Result, eyre};
use libc::pid_t;
use pg_embedded_setup_unpriv::test_support::{process_is_running, read_postmaster_pid};

/// Environment variable used to signal that this binary is running as the
/// child subprocess.
const CHILD_ENV_KEY: &str = "SHUTDOWN_HOOK_LIFECYCLE_CHILD";

/// Maximum time to wait for the postmaster to exit after the child process
/// terminates.
const POSTMASTER_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Polling interval when waiting for the postmaster to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

// ============================================================================
// Parent (test harness)
// ============================================================================

/// Spawns a child process that creates a cluster with the shutdown hook,
/// then verifies the postmaster is stopped after the child exits.
#[test]
#[ignore = "requires real PostgreSQL — run with `cargo test -- --ignored`"]
fn postmaster_exits_after_child_process_with_shutdown_hook() -> Result<()> {
    let tmp_dir = tempfile::tempdir().context("create temp dir")?;
    let pid_file = tmp_dir.path().join("postmaster_pid");

    let child_status = spawn_child(&pid_file)?;

    if !child_status.success() {
        return Err(eyre!("child process exited with status {child_status}"));
    }

    // The child writes either a PID or "SKIP" to the temp file.
    // "SKIP" signals that the environment cannot support cluster creation
    // (e.g. missing PostgreSQL binaries).
    let content = fs::read_to_string(&pid_file).context("read PID file from child")?;
    if content.trim() == "SKIP" {
        tracing::warn!("SKIP: child could not create a cluster in this environment");
        return Ok(());
    }

    let pid: pid_t = content
        .trim()
        .parse()
        .context("parse postmaster PID from child")?;
    wait_for_postmaster_exit(pid)
}

/// Spawns the child subprocess that creates and forgets a cluster.
fn spawn_child(pid_file: &Path) -> Result<std::process::ExitStatus> {
    let exe = env::current_exe().context("resolve current exe")?;
    let pid_path = pid_file
        .to_str()
        .ok_or_else(|| eyre!("non-UTF-8 temp path"))?;
    std::process::Command::new(exe)
        .env(CHILD_ENV_KEY, pid_path)
        .arg("--ignored")
        .arg("shutdown_hook_lifecycle_child_entry")
        .status()
        .context("spawn child process")
}

fn wait_for_postmaster_exit(pid: pid_t) -> Result<()> {
    let deadline = std::time::Instant::now() + POSTMASTER_EXIT_TIMEOUT;
    loop {
        if !process_is_running(pid) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(eyre!(
                "postmaster (PID {pid}) did not exit within {POSTMASTER_EXIT_TIMEOUT:?}"
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Returns `true` if the error should cause a soft skip rather than a hard
/// failure.
fn should_skip(message: &str, debug: &str) -> bool {
    cluster_skip_message(message, Some(debug)).is_some()
        || debug.contains("another server might be running")
}

// ============================================================================
// Child (subprocess entry point)
// ============================================================================

/// Entry point for the child subprocess.
///
/// This function is invoked when the binary detects the `CHILD_ENV_KEY`
/// environment variable. It creates a cluster, registers the shutdown hook,
/// writes the postmaster PID, and exits.
///
/// When the environment cannot support cluster creation (e.g. missing
/// `PostgreSQL` binaries), the child writes "SKIP" to the PID file and
/// exits cleanly so the parent can detect the soft skip.
#[test]
#[ignore = "child subprocess entry point — not a standalone test"]
fn shutdown_hook_lifecycle_child_entry() -> Result<()> {
    let Ok(pid_file_path) = env::var(CHILD_ENV_KEY) else {
        // Not running as the child subprocess — skip silently.
        return Ok(());
    };

    let (handle, guard) = match pg_embedded_setup_unpriv::TestCluster::new_split() {
        Ok(pair) => pair,
        Err(err) => {
            let message = err.to_string();
            let debug = format!("{err:?}");
            if should_skip(&message, &debug) {
                // Signal soft skip to the parent process by writing "SKIP"
                // and exiting cleanly, so the parent does not treat this as
                // a hard failure.
                let _unused = fs::write(&pid_file_path, "SKIP");
                std::process::exit(0);
            }
            return Err(err).context("create cluster in child");
        }
    };

    handle
        .register_shutdown_on_exit()
        .context("register shutdown hook")?;

    // Write postmaster PID to the temp file for the parent to verify.
    let pid = read_postmaster_pid(&handle.settings().data_dir)
        .ok_or_else(|| eyre!("postmaster.pid not found after cluster start"))?;
    fs::write(&pid_file_path, pid.to_string()).context("write PID file")?;

    // Forget the guard so Drop doesn't shut down the cluster — the atexit
    // hook is responsible.
    std::mem::forget(guard);

    // exit(0) triggers atexit handlers.
    std::process::exit(0);
}
