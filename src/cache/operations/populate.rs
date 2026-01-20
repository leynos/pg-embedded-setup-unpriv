//! Cache population after successful downloads.
//!
//! Provides functions to copy freshly downloaded binaries into the cache.

use camino::Utf8Path;
use color_eyre::eyre::Context;
use std::fs;
use tracing::{debug, warn};

use super::copy::copy_dir_recursive;
use crate::error::BootstrapResult;

/// Marker file name indicating a complete cache entry.
const COMPLETION_MARKER: &str = ".complete";

/// Observability target for cache operations.
const LOG_TARGET: &str = "pg_embed::cache";

/// Populates the cache with binaries from the given source directory.
///
/// After a successful download, call this function to copy binaries to the
/// cache and write the completion marker.
///
/// # Arguments
///
/// * `source` - Directory containing freshly downloaded/extracted binaries
/// * `cache_dir` - Root directory of the binary cache
/// * `version` - Version string for the cache entry
///
/// # Errors
///
/// Returns an error if:
/// - The cache directory cannot be created
/// - Copying binaries fails
/// - Writing the completion marker fails
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::cache::populate_cache;
///
/// let source = Utf8Path::new("/tmp/sandbox/install/17.4.0");
/// let cache_dir = Utf8Path::new("/home/user/.cache/pg-embedded/binaries");
/// populate_cache(source, cache_dir, "17.4.0")?;
/// # Ok::<(), color_eyre::Report>(())
/// ```
pub fn populate_cache(
    source: &Utf8Path,
    cache_dir: &Utf8Path,
    version: &str,
) -> BootstrapResult<()> {
    let version_dir = cache_dir.join(version);

    log_populate_start(source, cache_dir, version);

    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create cache directory: {version_dir}"))?;

    copy_dir_recursive(source.as_std_path(), version_dir.as_std_path())
        .with_context(|| format!("failed to copy binaries to cache: {version_dir}"))?;

    write_completion_marker(&version_dir)?;

    log_populate_complete(version, &version_dir);
    Ok(())
}

/// Logs the start of a cache population operation.
fn log_populate_start(source: &Utf8Path, cache_dir: &Utf8Path, version: &str) {
    debug!(
        target: LOG_TARGET,
        source = %source,
        cache_dir = %cache_dir,
        version = %version,
        "populating cache"
    );
}

/// Logs the completion of a cache population operation.
fn log_populate_complete(version: &str, version_dir: &Utf8Path) {
    debug!(
        target: LOG_TARGET,
        version = %version,
        path = %version_dir,
        "cache population completed"
    );
}

/// Writes the completion marker to indicate a valid cache entry.
fn write_completion_marker(cache_path: &Utf8Path) -> BootstrapResult<()> {
    let marker = cache_path.join(COMPLETION_MARKER);
    fs::write(&marker, "")
        .with_context(|| format!("failed to write cache completion marker: {marker}"))?;
    Ok(())
}

/// Attempts to populate the cache after a download, logging warnings on failure.
///
/// This is a convenience wrapper that does not propagate errors, allowing the
/// main flow to continue even if caching fails.
///
/// # Arguments
///
/// * `source` - Directory containing freshly downloaded binaries
/// * `cache_dir` - Root directory of the binary cache
/// * `version` - Version string for the cache entry
pub fn try_populate_cache(source: &Utf8Path, cache_dir: &Utf8Path, version: &str) {
    if let Err(err) = populate_cache(source, cache_dir, version) {
        warn!(
            target: LOG_TARGET,
            error = %err,
            version = %version,
            "failed to populate cache, future runs may re-download"
        );
    }
}
