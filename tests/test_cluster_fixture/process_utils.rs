use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, eyre};
use tracing::warn;
use libc::pid_t;

use super::{cap_fs, sandbox::TestSandbox};

pub(super) fn prepare_read_only_dir(path: &Utf8PathBuf) -> Result<()> {
    std::fs::create_dir_all(path.as_std_path())
        .with_context(|| format!("create read-only directory at {path}"))?;
    cap_fs::set_permissions(path.as_ref(), 0o555)
        .with_context(|| format!("restrict permissions for {path}"))
}

pub(super) fn sandbox_root_path(sandbox: &TestSandbox) -> Result<PathBuf> {
    let Some(root) = sandbox.install_dir().parent() else {
        return Err(eyre!("sandbox install directory is missing a parent"));
    };
    Ok(root.to_path_buf().into_std_path_buf())
}

pub(super) fn read_postmaster_pid(data_dir: impl AsRef<Path>) -> Result<Option<pid_t>> {
    let data_dir_ref = data_dir.as_ref();
    let pid_file = data_dir_ref.join("postmaster.pid");
    if !pid_file.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&pid_file)
        .with_context(|| format!("read postmaster pid file at {}", pid_file.display()))?;
    let Some(first_line) = contents.lines().next() else {
        return Ok(None);
    };
    let pid: pid_t = first_line
        .trim()
        .parse()
        .with_context(|| format!("parse postmaster pid from '{first_line}'"))?;
    Ok(Some(pid))
}

pub(super) fn wait_for_process_exit(pid: Option<pid_t>) -> Result<()> {
    let Some(child_pid) = pid else {
        return Ok(());
    };
    let config = WaitConfig::from_env();
    if config.wait_until(|| !process_is_running(child_pid)) {
        return Ok(());
    }
    Err(eyre!(format!(
        "PostgreSQL process {child_pid} should exit after the fixture drops"
    )))
}

pub(super) fn wait_for_pid_file_removal(data_dir: impl AsRef<Path>) -> Result<()> {
    let data_dir_ref = data_dir.as_ref();
    let pid_file = data_dir_ref.join("postmaster.pid");
    let config = WaitConfig::from_env();
    if config.wait_until(|| !pid_file.exists()) {
        return Ok(());
    }
    Err(eyre!(format!(
        "postmaster.pid should be removed from {:?} once PostgreSQL stops",
        data_dir_ref
    )))
}

struct WaitConfig {
    attempts: u32,
    initial_delay: Duration,
    max_delay: Duration,
}

impl WaitConfig {
    fn from_env() -> Self {
        const ENV_KEY: &str = "PG_EMBEDDED_FIXTURE_WAIT_MULTIPLIER";
        let base = Self::default();
        let Ok(raw) = std::env::var(ENV_KEY) else {
            return base;
        };
        let Ok(multiplier) = raw.parse::<f64>() else {
            warn!("Ignoring invalid {ENV_KEY} value {raw}; expected a floating point multiplier");
            return base;
        };
        base.scaled(multiplier)
    }

    fn scaled(mut self, multiplier: f64) -> Self {
        let clamped = multiplier.clamp(0.1, 10.0);
        self.attempts = ((self.attempts as f64) * clamped).ceil() as u32;
        self.initial_delay = self
            .initial_delay
            .mul_f64(clamped)
            .min(self.max_delay);
        self
    }

    fn wait_until<F>(&self, mut predicate: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let mut delay = self.initial_delay;
        for _ in 0..self.attempts {
            if predicate() {
                return true;
            }
            thread::sleep(delay);
            delay = (delay.mul_f64(1.5)).min(self.max_delay);
        }
        false
    }
}

impl Default for WaitConfig {
    fn default() -> Self {
        Self {
            attempts: 120,
            initial_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(250),
        }
    }
}

fn process_is_running(pid: pid_t) -> bool {
    // SAFETY: `kill` with signal `0` probes whether the process exists without sending a signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::ESRCH
    )
}
