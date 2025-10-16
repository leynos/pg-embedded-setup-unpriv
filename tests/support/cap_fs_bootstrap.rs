//! Capability-based filesystem helpers for tests.

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Metadata},
};
use color_eyre::eyre::{Context, Result};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pg_embedded_setup_unpriv::test_support::{
    ambient_dir_and_path as shared_ambient_dir_and_path, set_permissions as shared_set_permissions,
};

/// Splits an absolute or relative path into a capability directory and the relative path.
///
/// Absolute paths are rebased under the ambient root directory. Relative paths reuse the
/// current working directory.
pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    shared_ambient_dir_and_path(path)
}

/// Applies the provided POSIX mode to the path when it exists.
pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    shared_set_permissions(path, mode)
}

/// Removes a directory tree when present, ignoring `NotFound` errors.
pub fn remove_tree(path: &Utf8Path) -> Result<()> {
    let (dir, relative) = ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Ok(());
    }

    match dir.remove_dir_all(relative.as_std_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path)),
    }
}

/// Retrieves metadata for the path using capability APIs.
pub fn metadata(path: &Utf8Path) -> std::io::Result<Metadata> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| std::io::Error::other(err.to_string()))?;
    if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    }
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
