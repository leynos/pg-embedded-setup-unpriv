//! Cache lookup and hit/miss detection.
//!
//! Provides functions for checking cache status and finding matching versions.

use camino::{Utf8Path, Utf8PathBuf};
use postgresql_embedded::{Version, VersionReq};
use std::fs;
use tracing::{debug, warn};

use super::copy::copy_from_cache;

/// Marker file name indicating a complete cache entry.
pub(crate) const COMPLETION_MARKER: &str = ".complete";

/// Observability target for cache operations.
const LOG_TARGET: &str = "pg_embed::cache";

/// Result of a cache lookup operation.
#[derive(Debug)]
pub enum CacheLookupResult {
    /// Cache hit: binaries exist and are valid.
    Hit {
        /// Path to the cached version directory.
        source_dir: Utf8PathBuf,
    },
    /// Cache miss: binaries need to be downloaded.
    Miss,
}

/// Returns true if the cache entry at the given path is complete.
///
/// A cache entry is complete if both the `.complete` marker and `bin/`
/// directory exist.
fn is_cache_entry_complete(version_dir: &Utf8Path) -> bool {
    let marker = version_dir.join(COMPLETION_MARKER);
    let bin_dir = version_dir.join("bin");
    marker.exists() && bin_dir.is_dir()
}

/// Checks if the cache contains valid binaries for the given version.
///
/// A cache entry is considered valid if:
/// 1. The version directory exists
/// 2. The `.complete` marker file is present
/// 3. The `bin` subdirectory exists (indicating extracted binaries)
///
/// # Arguments
///
/// * `cache_dir` - Root directory of the binary cache
/// * `version` - Exact version string to look up (e.g., "17.4.0")
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::cache::{check_cache, CacheLookupResult};
///
/// let cache_dir = Utf8Path::new("/home/user/.cache/pg-embedded/binaries");
/// match check_cache(cache_dir, "17.4.0") {
///     CacheLookupResult::Hit { source_dir } => {
///         println!("Found cached binaries at {source_dir}");
///     }
///     CacheLookupResult::Miss => {
///         println!("Cache miss, need to download");
///     }
/// }
/// ```
#[must_use]
pub fn check_cache(cache_dir: &Utf8Path, version: &str) -> CacheLookupResult {
    let version_dir = cache_dir.join(version);

    if is_cache_entry_complete(&version_dir) {
        debug!(
            target: LOG_TARGET,
            version = %version,
            path = %version_dir,
            "cache hit"
        );
        CacheLookupResult::Hit {
            source_dir: version_dir,
        }
    } else {
        log_cache_miss(&version_dir, version);
        CacheLookupResult::Miss
    }
}

/// Logs details about a cache miss for debugging.
fn log_cache_miss(version_dir: &Utf8Path, version: &str) {
    let marker = version_dir.join(COMPLETION_MARKER);
    let bin_dir = version_dir.join("bin");
    debug!(
        target: LOG_TARGET,
        version = %version,
        marker_exists = marker.exists(),
        bin_exists = bin_dir.is_dir(),
        "cache miss"
    );
}

/// Finds a cached version that satisfies the given version requirement.
///
/// Scans the cache directory for version subdirectories and returns the highest
/// version that matches the requirement. This allows a requirement like `^17` to
/// use a cached `17.4.0` entry.
///
/// # Arguments
///
/// * `cache_dir` - Root directory of the binary cache
/// * `version_req` - Version requirement to match against (e.g., `^17`, `=17.4.0`)
///
/// # Returns
///
/// Returns `Some((version_string, source_dir))` if a matching cache entry is found,
/// `None` otherwise.
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use postgresql_embedded::VersionReq;
/// use pg_embedded_setup_unpriv::cache::find_matching_cached_version;
///
/// let cache_dir = Utf8Path::new("/home/user/.cache/pg-embedded/binaries");
/// let version_req = VersionReq::parse("^17").expect("valid version req");
/// if let Some((version, source_dir)) = find_matching_cached_version(cache_dir, &version_req) {
///     println!("Found cached {version} at {source_dir}");
/// }
/// ```
#[must_use]
pub fn find_matching_cached_version(
    cache_dir: &Utf8Path,
    version_req: &VersionReq,
) -> Option<(String, Utf8PathBuf)> {
    let dir_entries = read_cache_directory(cache_dir)?;

    // Use max_by for O(n) instead of collect + sort for O(n log n)
    let (version, path) = dir_entries
        .filter_map(Result::ok)
        .filter_map(|entry| try_parse_cache_entry(&entry, version_req))
        .max_by(|a, b| a.0.cmp(&b.0))?;

    let version_str = version.to_string();
    debug!(
        target: LOG_TARGET,
        version_req = %version_req,
        matched_version = %version_str,
        path = %path,
        "found matching cached version"
    );
    Some((version_str, path))
}

/// Reads the cache directory, logging errors as debug messages.
fn read_cache_directory(cache_dir: &Utf8Path) -> Option<fs::ReadDir> {
    match fs::read_dir(cache_dir) {
        Ok(entries) => Some(entries),
        Err(err) => {
            debug!(
                target: LOG_TARGET,
                cache_dir = %cache_dir,
                error = %err,
                "failed to read cache directory"
            );
            None
        }
    }
}

/// Attempts to parse a directory entry as a valid cache entry matching the version requirement.
fn try_parse_cache_entry(
    entry: &fs::DirEntry,
    version_req: &VersionReq,
) -> Option<(Version, Utf8PathBuf)> {
    let path = entry.path();
    let dir_name = path.file_name()?.to_str()?;

    // Skip hidden directories
    if dir_name.starts_with('.') {
        return None;
    }

    let version = Version::parse(dir_name).ok()?;
    if !version_req.matches(&version) {
        return None;
    }

    let utf8_path = Utf8PathBuf::from_path_buf(path).ok()?;
    is_cache_entry_complete(&utf8_path).then_some((version, utf8_path))
}

/// Attempts to use the cache, falling back gracefully on errors.
///
/// This is a convenience wrapper that logs warnings instead of failing when
/// cache operations encounter errors.
///
/// # Arguments
///
/// * `cache_dir` - Root directory of the binary cache
/// * `version` - Version string to look up
/// * `target` - Target installation directory for copy
///
/// # Returns
///
/// Returns `true` if binaries were successfully copied from cache, `false` if
/// the cache was missed or an error occurred.
#[must_use]
pub fn try_use_cache(cache_dir: &Utf8Path, version: &str, target: &Utf8Path) -> bool {
    let CacheLookupResult::Hit { source_dir } = check_cache(cache_dir, version) else {
        return false;
    };

    match copy_from_cache(&source_dir, target) {
        Ok(()) => true,
        Err(err) => {
            warn!(
                target: LOG_TARGET,
                error = %err,
                version = %version,
                "cache copy failed, falling back to download"
            );
            false
        }
    }
}
