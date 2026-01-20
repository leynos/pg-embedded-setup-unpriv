//! Cache lookup, population, and validation operations.
//!
//! Provides functions for checking cache status, copying binaries from cache,
//! and populating the cache after downloads.

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Context;
use postgresql_embedded::{Version, VersionReq};
use std::fs;
use std::io;
use std::path::Path;
use tracing::{debug, warn};

use crate::error::BootstrapResult;

/// Marker file name indicating a complete cache entry.
const COMPLETION_MARKER: &str = ".complete";

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
#[expect(
    clippy::cognitive_complexity,
    reason = "simple cache check with existence validation is readable as-is"
)]
pub fn check_cache(cache_dir: &Utf8Path, version: &str) -> CacheLookupResult {
    let version_dir = cache_dir.join(version);
    let marker = version_dir.join(COMPLETION_MARKER);
    let bin_dir = version_dir.join("bin");

    if marker.exists() && bin_dir.is_dir() {
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
        debug!(
            target: LOG_TARGET,
            version = %version,
            marker_exists = marker.exists(),
            bin_exists = bin_dir.is_dir(),
            "cache miss"
        );
        CacheLookupResult::Miss
    }
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
    let dir_entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(err) => {
            debug!(
                target: LOG_TARGET,
                cache_dir = %cache_dir,
                error = %err,
                "failed to read cache directory"
            );
            return None;
        }
    };

    // Collect valid cached versions that match the requirement
    let mut matching_versions: Vec<(Version, Utf8PathBuf)> = dir_entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let dir_name = path.file_name()?.to_str()?;

            // Skip hidden directories and non-version entries
            if dir_name.starts_with('.') {
                return None;
            }

            // Try to parse as a semver version
            let version = Version::parse(dir_name).ok()?;

            // Check if it matches the requirement
            if !version_req.matches(&version) {
                return None;
            }

            // Verify this is a valid cache entry (has marker and bin dir)
            let utf8_path = Utf8PathBuf::from_path_buf(path).ok()?;
            let marker = utf8_path.join(COMPLETION_MARKER);
            let bin_dir = utf8_path.join("bin");

            if marker.exists() && bin_dir.is_dir() {
                Some((version, utf8_path))
            } else {
                None
            }
        })
        .collect();

    // Sort by version descending and take the highest
    matching_versions.sort_by(|a, b| b.0.cmp(&a.0));

    matching_versions.into_iter().next().map(|(version, path)| {
        let version_str = version.to_string();
        debug!(
            target: LOG_TARGET,
            version_req = %version_req,
            matched_version = %version_str,
            path = %path,
            "found matching cached version"
        );
        (version_str, path)
    })
}

/// Copies cached binaries to the target installation directory.
///
/// Performs a recursive copy of the source directory contents to the target,
/// preserving directory structure and file permissions.
///
/// # Arguments
///
/// * `source` - Source directory containing cached binaries
/// * `target` - Target installation directory
///
/// # Errors
///
/// Returns an error if:
/// - The source directory does not exist or cannot be read
/// - The target directory cannot be created
/// - Any file copy operation fails
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::cache::copy_from_cache;
///
/// let source = Utf8Path::new("/home/user/.cache/pg-embedded/binaries/17.4.0");
/// let target = Utf8Path::new("/tmp/sandbox/install");
/// copy_from_cache(source, target)?;
/// # Ok::<(), color_eyre::Report>(())
/// ```
#[expect(
    clippy::cognitive_complexity,
    reason = "linear copy flow with error handling is readable as-is"
)]
pub fn copy_from_cache(source: &Utf8Path, target: &Utf8Path) -> BootstrapResult<()> {
    debug!(
        target: LOG_TARGET,
        source = %source,
        target = %target,
        "copying binaries from cache"
    );

    // Create the target directory if it does not exist
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create target directory for cache copy: {target}"))?;

    copy_dir_recursive(source.as_std_path(), target.as_std_path())
        .with_context(|| format!("failed to copy cached binaries from {source} to {target}"))?;

    debug!(
        target: LOG_TARGET,
        source = %source,
        target = %target,
        "cache copy completed"
    );

    Ok(())
}

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
#[expect(
    clippy::cognitive_complexity,
    reason = "linear cache population flow with error handling is readable as-is"
)]
pub fn populate_cache(
    source: &Utf8Path,
    cache_dir: &Utf8Path,
    version: &str,
) -> BootstrapResult<()> {
    let version_dir = cache_dir.join(version);

    debug!(
        target: LOG_TARGET,
        source = %source,
        cache_dir = %cache_dir,
        version = %version,
        "populating cache"
    );

    // Ensure the version directory exists
    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create cache directory: {version_dir}"))?;

    // Copy binaries to cache
    copy_dir_recursive(source.as_std_path(), version_dir.as_std_path())
        .with_context(|| format!("failed to copy binaries to cache: {version_dir}"))?;

    // Write completion marker
    write_completion_marker(&version_dir)?;

    debug!(
        target: LOG_TARGET,
        version = %version,
        path = %version_dir,
        "cache population completed"
    );

    Ok(())
}

/// Writes the completion marker to indicate a valid cache entry.
fn write_completion_marker(cache_path: &Utf8Path) -> BootstrapResult<()> {
    let marker = cache_path.join(COMPLETION_MARKER);
    fs::write(&marker, "")
        .with_context(|| format!("failed to write cache completion marker: {marker}"))?;
    Ok(())
}

/// Recursively copies a directory and its contents.
///
/// Preserves directory structure and copies file metadata where possible.
fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for dir_entry in fs::read_dir(src)? {
        let entry = dir_entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_symlink() {
            copy_symlink(&src_path, &dst_path)?;
        } else {
            copy_file_with_permissions(&src_path, &dst_path)?;
        }
    }

    // Copy directory permissions (best effort)
    copy_permissions(src, dst);

    Ok(())
}

/// Copies a file preserving its permissions.
fn copy_file_with_permissions(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst)?;

    // Preserve original permissions (best effort)
    copy_permissions(src, dst);

    Ok(())
}

/// Best-effort permission copy from source to destination.
#[expect(
    clippy::let_underscore_must_use,
    reason = "permission copy is best-effort; failure is acceptable"
)]
fn copy_permissions(src: &Path, dst: &Path) {
    if let Ok(metadata) = fs::metadata(src) {
        let _ = fs::set_permissions(dst, metadata.permissions());
    }
}

/// Copies a symbolic link.
#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    let target = fs::read_link(src)?;
    std::os::unix::fs::symlink(&target, dst)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    // On non-Unix, fall back to copying the target file
    if src.is_file() {
        fs::copy(src, dst)?;
    }
    Ok(())
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
#[expect(
    clippy::cognitive_complexity,
    reason = "linear flow with match and error handling is readable as-is"
)]
pub fn try_use_cache(cache_dir: &Utf8Path, version: &str, target: &Utf8Path) -> bool {
    match check_cache(cache_dir, version) {
        CacheLookupResult::Hit { source_dir } => {
            if let Err(err) = copy_from_cache(&source_dir, target) {
                warn!(
                    target: LOG_TARGET,
                    error = %err,
                    version = %version,
                    "cache copy failed, falling back to download"
                );
                false
            } else {
                true
            }
        }
        CacheLookupResult::Miss => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_mock_binaries(dir: &Utf8Path) {
        let bin_dir = dir.join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        fs::write(bin_dir.join("postgres"), "mock postgres binary").expect("write mock binary");
        fs::write(bin_dir.join("pg_ctl"), "mock pg_ctl binary").expect("write mock binary");
    }

    #[test]
    fn check_cache_returns_miss_for_empty_directory() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

        let result = check_cache(cache_dir, "17.4.0");
        assert!(matches!(result, CacheLookupResult::Miss));
    }

    #[test]
    fn check_cache_returns_miss_without_marker() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");
        let version_dir = cache_dir.join("17.4.0");
        create_mock_binaries(&version_dir);

        let result = check_cache(cache_dir, "17.4.0");
        assert!(matches!(result, CacheLookupResult::Miss));
    }

    #[test]
    fn check_cache_returns_miss_without_bin_directory() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");
        let version_dir = cache_dir.join("17.4.0");
        fs::create_dir_all(&version_dir).expect("create version dir");
        fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

        let result = check_cache(cache_dir, "17.4.0");
        assert!(matches!(result, CacheLookupResult::Miss));
    }

    #[test]
    fn check_cache_returns_hit_with_marker_and_bin() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");
        let version_dir = cache_dir.join("17.4.0");
        create_mock_binaries(&version_dir);
        fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

        let result = check_cache(cache_dir, "17.4.0");
        match result {
            CacheLookupResult::Hit { source_dir } => {
                assert_eq!(source_dir, version_dir);
            }
            CacheLookupResult::Miss => panic!("expected cache hit"),
        }
    }

    #[test]
    fn copy_from_cache_copies_files() {
        let source_temp = tempdir().expect("source tempdir");
        let target_temp = tempdir().expect("target tempdir");
        let source = Utf8Path::from_path(source_temp.path()).expect("utf8 source");
        let target = Utf8Path::from_path(target_temp.path()).expect("utf8 target");

        create_mock_binaries(source);

        copy_from_cache(source, target).expect("copy from cache");

        assert!(target.join("bin/postgres").exists());
        assert!(target.join("bin/pg_ctl").exists());
    }

    #[test]
    fn populate_cache_creates_version_directory() {
        let source_temp = tempdir().expect("source tempdir");
        let cache_temp = tempdir().expect("cache tempdir");
        let source = Utf8Path::from_path(source_temp.path()).expect("utf8 source");
        let cache_dir = Utf8Path::from_path(cache_temp.path()).expect("utf8 cache");

        create_mock_binaries(source);

        populate_cache(source, cache_dir, "17.4.0").expect("populate cache");

        let version_dir = cache_dir.join("17.4.0");
        assert!(version_dir.join(COMPLETION_MARKER).exists());
        assert!(version_dir.join("bin/postgres").exists());
    }

    #[test]
    fn try_use_cache_returns_false_on_miss() {
        let cache_temp = tempdir().expect("cache tempdir");
        let target_temp = tempdir().expect("target tempdir");
        let cache_dir = Utf8Path::from_path(cache_temp.path()).expect("utf8 cache");
        let target = Utf8Path::from_path(target_temp.path()).expect("utf8 target");

        let result = try_use_cache(cache_dir, "17.4.0", target);
        assert!(!result);
    }

    #[test]
    fn try_use_cache_returns_true_on_hit() {
        let cache_temp = tempdir().expect("cache tempdir");
        let target_temp = tempdir().expect("target tempdir");
        let cache_dir = Utf8Path::from_path(cache_temp.path()).expect("utf8 cache");
        let target = Utf8Path::from_path(target_temp.path()).expect("utf8 target");

        // Set up cache
        let version_dir = cache_dir.join("17.4.0");
        create_mock_binaries(&version_dir);
        fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

        let result = try_use_cache(cache_dir, "17.4.0", target);
        assert!(result);
        assert!(target.join("bin/postgres").exists());
    }

    #[test]
    fn find_matching_cached_version_returns_none_for_empty_cache() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");
        let version_req = VersionReq::parse("^17").expect("parse version req");

        let result = find_matching_cached_version(cache_dir, &version_req);
        assert!(result.is_none());
    }

    #[test]
    fn find_matching_cached_version_finds_exact_match() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

        // Create a valid cache entry for 17.4.0
        let version_dir = cache_dir.join("17.4.0");
        create_mock_binaries(&version_dir);
        fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

        let version_req = VersionReq::parse("=17.4.0").expect("parse version req");
        let result = find_matching_cached_version(cache_dir, &version_req);

        let (version, path) = result.expect("should find cached version");
        assert_eq!(version, "17.4.0");
        assert!(path.ends_with("17.4.0"));
    }

    #[test]
    fn find_matching_cached_version_matches_caret_requirement() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

        // Create a valid cache entry for 17.4.0
        let version_dir = cache_dir.join("17.4.0");
        create_mock_binaries(&version_dir);
        fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

        // ^17 should match 17.4.0
        let version_req = VersionReq::parse("^17").expect("parse version req");
        let result = find_matching_cached_version(cache_dir, &version_req);

        let (version, _) = result.expect("^17 should match 17.4.0");
        assert_eq!(version, "17.4.0");
    }

    #[test]
    fn find_matching_cached_version_returns_highest_matching() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

        // Create cache entries for 17.2.0 and 17.4.0
        for version in ["17.2.0", "17.4.0"] {
            let version_dir = cache_dir.join(version);
            create_mock_binaries(&version_dir);
            fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");
        }

        // ^17 should match highest (17.4.0)
        let version_req = VersionReq::parse("^17").expect("parse version req");
        let result = find_matching_cached_version(cache_dir, &version_req);

        let (version, _) = result.expect("should find highest matching version");
        assert_eq!(version, "17.4.0");
    }

    #[test]
    fn find_matching_cached_version_ignores_non_matching() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

        // Create a valid cache entry for 16.0.0 only
        let version_dir = cache_dir.join("16.0.0");
        create_mock_binaries(&version_dir);
        fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

        // ^17 should not match 16.0.0
        let version_req = VersionReq::parse("^17").expect("parse version req");
        let result = find_matching_cached_version(cache_dir, &version_req);

        assert!(result.is_none());
    }

    #[test]
    fn find_matching_cached_version_ignores_incomplete_entries() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

        // Create an incomplete cache entry (no marker)
        let version_dir = cache_dir.join("17.4.0");
        create_mock_binaries(&version_dir);
        // Deliberately not writing completion marker

        let version_req = VersionReq::parse("^17").expect("parse version req");
        let result = find_matching_cached_version(cache_dir, &version_req);

        assert!(result.is_none());
    }
}
