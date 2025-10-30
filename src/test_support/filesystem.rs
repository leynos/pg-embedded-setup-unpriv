//! Capability-aware filesystem helpers for tests, mirroring the public `fs`
//! API while exposing ambient operations for scaffolding.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Metadata},
};
use color_eyre::eyre::{Context, Report, Result};

use crate::fs;

/// Opens the ambient directory containing `path` and returns its relative component.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::ambient_dir_and_path;
/// # fn main() -> Result<()> {
/// let (_dir, relative) = ambient_dir_and_path(Utf8Path::new("."))?;
/// assert_eq!(relative.as_str(), ".");
///
/// let (_root, root_rel) = ambient_dir_and_path(Utf8Path::new("/"))?;
/// assert!(root_rel.as_str().is_empty());
/// # Ok(())
/// # }
/// ```
pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    fs::ambient_dir_and_path(path)
}

/// Ensures the provided directory exists, creating intermediate components when missing.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::ensure_dir_exists;
/// # fn main() -> Result<()> {
/// ensure_dir_exists(Utf8Path::new("./target/tmp/cache"))?;
/// # Ok(())
/// # }
/// ```
pub fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    fs::ensure_dir_exists(path)
}

/// Applies POSIX permissions to the provided path when it already exists.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::set_permissions;
/// # fn main() -> Result<()> {
/// set_permissions(Utf8Path::new("./target/tmp/cache"), 0o755)?;
/// # Ok(())
/// # }
/// ```
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

        let system_tmp_dir = std::env::temp_dir();
        let system_tmp_path = Utf8PathBuf::try_from(system_tmp_dir)
            .map_err(|_| Report::msg("system temp dir is not valid UTF-8"))?;
        let ambient = Dir::open_ambient_dir(system_tmp_path.as_std_path(), ambient_authority())
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
                    let path = system_tmp_path.join(&name);
                    return Ok(Self {
                        dir: Some(dir),
                        path,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("create capability tempdir {name}"));
                }
            }
        }

        Err(Report::msg(
            "exhausted attempts creating capability tempdir",
        ))
    }

    /// Returns the UTF-8 path to the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    fn remove_dir(dir: Dir, path: &Utf8Path) {
        if let Err(err) = dir.remove_open_dir_all() {
            tracing::warn!("SKIP-CAP-TEMPDIR: failed to remove {}: {err}", path);
        }
    }
}

impl Drop for CapabilityTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            Self::remove_dir(dir, &self.path);
        }
    }
}
