//! End-to-end lifecycle test for the atexit shutdown hook.
//!
//! Verifies that `PostgreSQL` processes do not survive test binary exit when
//! the shutdown hook is registered. Uses a subprocess pattern: the parent
//! spawns itself as a child process which creates a cluster, registers the
//! hook, writes the postmaster PID to a temp file, then calls
//! `std::process::exit(0)`. The parent waits for the child to exit and then
//! confirms the postmaster has also terminated.
#![cfg(unix)]

use std::path::Path;
use std::time::Duration;
use std::{env, fs, thread};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use libc::pid_t;

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
    ensure!(
        child_status.success(),
        "child process exited with status {child_status}"
    );

    let pid = read_pid_from_file(&pid_file)?;
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

/// Reads a PID written by the child process to a temp file.
fn read_pid_from_file(pid_file: &Path) -> Result<pid_t> {
    let pid_str = fs::read_to_string(pid_file).context("read PID file from child")?;
    pid_str
        .trim()
        .parse()
        .context("parse postmaster PID from child")
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

fn process_is_running(pid: pid_t) -> bool {
    // SAFETY: `kill` with signal 0 probes whether the process exists without
    // delivering a signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::ESRCH
    )
}

fn read_postmaster_pid(data_dir: &Path) -> Option<pid_t> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = fs::read_to_string(&pid_file).ok()?;
    let first_line = contents.lines().next()?;
    first_line.trim().parse::<pid_t>().ok()
}

// ============================================================================
// Child (subprocess entry point)
// ============================================================================

/// Entry point for the child subprocess.
///
/// This function is invoked when the binary detects the `CHILD_ENV_KEY`
/// environment variable. It creates a cluster, registers the shutdown hook,
/// writes the postmaster PID, and exits.
#[test]
#[ignore = "child subprocess entry point — not a standalone test"]
fn shutdown_hook_lifecycle_child_entry() -> Result<()> {
    let Ok(pid_file_path) = env::var(CHILD_ENV_KEY) else {
        // Not running as the child subprocess — skip silently.
        return Ok(());
    };

    let (handle, guard) =
        pg_embedded_setup_unpriv::TestCluster::new_split().context("create cluster in child")?;

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
