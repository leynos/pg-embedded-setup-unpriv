//! Internal helpers re-exported for integration tests.
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Metadata},
};
use color_eyre::eyre::{Context, Report, Result};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{BootstrapError, Error, PrivilegeError};
use crate::fs;

pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    fs::ambient_dir_and_path(path)
}

pub fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    fs::ensure_dir_exists(path)
}

pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, mode)
}

/// Retrieves metadata for the provided path using capability APIs.
pub fn metadata(path: &Utf8Path) -> std::io::Result<Metadata> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| std::io::Error::other(err.to_string()))?;
    if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    }
}

pub fn bootstrap_error(err: Report) -> Error {
    Error::Bootstrap(BootstrapError::from(err))
}

pub fn privilege_error(err: Report) -> Error {
    Error::Privilege(PrivilegeError::from(err))
}

/// Capability-aware temporary directory that exposes both a [`Dir`] handle and the UTF-8 path.
#[derive(Debug)]
pub struct CapabilityTempDir {
    dir: Option<Dir>,
    path: Utf8PathBuf,
}

impl CapabilityTempDir {
    /// Creates a new temporary directory rooted under the system temporary location.
    pub fn new(prefix: &str) -> Result<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let system_tmp = std::env::temp_dir();
        let system_tmp = Utf8PathBuf::try_from(system_tmp)
            .map_err(|_| color_eyre::eyre::eyre!("system temp dir is not valid UTF-8"))?;
        let ambient = Dir::open_ambient_dir(system_tmp.as_std_path(), ambient_authority())
            .context("open ambient temp directory")?;

        let pid = std::process::id();
        let epoch_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();

        for attempt in 0..32 {
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{}-{}-{}-{}", prefix, pid, epoch_ns, counter + attempt);
            match ambient.create_dir(&name) {
                Ok(()) => {
                    let dir = ambient.open_dir(&name).context("open capability tempdir")?;
                    let path = system_tmp.join(&name);
                    return Ok(Self {
                        dir: Some(dir),
                        path,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(err).with_context(|| format!("create capability tempdir {name}"));
                }
            }
        }

        Err(color_eyre::eyre::eyre!(
            "exhausted attempts creating capability tempdir"
        ))
    }

    /// Returns the UTF-8 path to the temporary directory.
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

impl Drop for CapabilityTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            match dir.remove_open_dir_all() {
                Ok(()) => {}
                Err(err) => {
                    eprintln!("SKIP-CAP-TEMPDIR: failed to remove {}: {err}", self.path);
                }
            }
        }
    }
}
