//! Binary cache integration for `TestCluster`.
//!
//! Provides methods to check and populate the shared `PostgreSQL` binary cache,
//! avoiding repeated downloads across test runs.

use crate::TestBootstrapSettings;
use crate::cache::{
    BinaryCacheConfig, CacheLock, CacheLookupResult, check_cache, copy_from_cache,
    find_matching_cached_version, populate_cache,
};
use crate::observability::LOG_TARGET;
use camino::Utf8PathBuf;
use postgresql_embedded::{Settings, VersionReq};
use tracing::{debug, info, warn};

use super::installation;

/// Attempts to use cached binaries for the given version requirement.
///
/// Returns `true` if binaries were successfully copied from cache, `false` otherwise.
/// On cache hit, sets `trust_installation_dir = true` to skip re-validation in setup.
#[expect(
    clippy::cognitive_complexity,
    reason = "cache lookup flow with lock acquisition and error handling is readable as-is"
)]
pub(super) fn try_use_binary_cache(
    config: &BinaryCacheConfig,
    version_req: &VersionReq,
    bootstrap: &mut TestBootstrapSettings,
) -> bool {
    // Find a cached version that matches the requirement
    let Some((version, _source_dir)) = find_matching_cached_version(&config.cache_dir, version_req)
    else {
        debug!(
            target: LOG_TARGET,
            version_req = %version_req,
            "no matching cached version found"
        );
        return false;
    };

    // Acquire shared lock for the specific version
    let Ok(_lock) = CacheLock::acquire_shared(&config.cache_dir, &version) else {
        debug!(
            target: LOG_TARGET,
            version = %version,
            "failed to acquire cache lock, skipping cache"
        );
        return false;
    };

    // Double-check the cache is still valid after acquiring the lock
    match check_cache(&config.cache_dir, &version) {
        CacheLookupResult::Hit { source_dir } => {
            let Ok(target) =
                Utf8PathBuf::from_path_buf(bootstrap.settings.installation_dir.clone())
            else {
                warn!(
                    target: LOG_TARGET,
                    "installation_dir is not valid UTF-8, skipping cache"
                );
                return false;
            };

            // The cache stores binaries in {cache_dir}/{version}/
            // We need to copy to {installation_dir}/{version}/ to match expected layout
            let target_version_dir = target.join(&version);

            let copy_result = copy_from_cache(&source_dir, &target_version_dir);
            if copy_result.is_err() {
                warn!(
                    target: LOG_TARGET,
                    version = %version,
                    "cache copy failed, falling back to download"
                );
                return false;
            }

            // Update installation_dir to point to the versioned directory where binaries were copied.
            // postgresql_embedded expects installation_dir to contain bin/postgres directly.
            bootstrap.settings.installation_dir = target_version_dir.clone().into();
            bootstrap.settings.trust_installation_dir = true;

            // Set exact version to skip GitHub API version resolution.
            // This avoids rate limiting when running many tests.
            let exact_version = format!("={version}");
            if let Ok(exact_req) = VersionReq::parse(&exact_version) {
                bootstrap.settings.version = exact_req;
            }

            info!(
                target: LOG_TARGET,
                version_req = %version_req,
                matched_version = %version,
                source = %source_dir,
                target = %target_version_dir,
                "using cached binaries"
            );
            true
        }
        CacheLookupResult::Miss => {
            // Cache entry was removed after initial lookup
            debug!(
                target: LOG_TARGET,
                version = %version,
                "cache entry no longer valid"
            );
            false
        }
    }
}

/// Attempts to populate the cache with binaries from the installation directory.
///
/// This is called after a successful setup to cache the downloaded binaries for future use.
#[expect(
    clippy::cognitive_complexity,
    reason = "cache population flow with lock acquisition and double-check is readable as-is"
)]
pub(super) fn try_populate_binary_cache(config: &BinaryCacheConfig, settings: &Settings) {
    // Find the actual installed version directory
    let Some(installed_dir) = installation::resolve_installed_dir(settings) else {
        debug!(
            target: LOG_TARGET,
            "no installed directory found, skipping cache population"
        );
        return;
    };

    // Extract version from the installed directory name
    let Some(version) = extract_version_from_path(&installed_dir) else {
        debug!(
            target: LOG_TARGET,
            path = %installed_dir.display(),
            "could not extract version from path, skipping cache population"
        );
        return;
    };

    // Check if already cached (avoid redundant work)
    if matches!(
        check_cache(&config.cache_dir, &version),
        CacheLookupResult::Hit { .. }
    ) {
        debug!(
            target: LOG_TARGET,
            version = %version,
            "version already cached, skipping population"
        );
        return;
    }

    // Acquire exclusive lock for writing
    let Ok(_lock) = CacheLock::acquire_exclusive(&config.cache_dir, &version) else {
        warn!(
            target: LOG_TARGET,
            version = %version,
            "failed to acquire exclusive cache lock, skipping population"
        );
        return;
    };

    // Double-check after acquiring lock (another process may have populated)
    if matches!(
        check_cache(&config.cache_dir, &version),
        CacheLookupResult::Hit { .. }
    ) {
        debug!(
            target: LOG_TARGET,
            version = %version,
            "version cached by another process"
        );
        return;
    }

    let Ok(source) = Utf8PathBuf::from_path_buf(installed_dir.clone()) else {
        warn!(
            target: LOG_TARGET,
            "installed directory is not valid UTF-8, skipping cache population"
        );
        return;
    };

    if let Err(err) = populate_cache(&source, &config.cache_dir, &version) {
        warn!(
            target: LOG_TARGET,
            error = %err,
            version = %version,
            "failed to populate cache"
        );
    } else {
        info!(
            target: LOG_TARGET,
            version = %version,
            cache_dir = %config.cache_dir,
            "populated binary cache"
        );
    }
}

/// Extracts the version string from an installation directory path.
///
/// Expects paths like `/path/to/install/17.4.0/` and extracts `17.4.0`.
/// Returns `None` if the directory name is not a valid semver version,
/// ensuring consistency with cache lookup which parses directory names as versions.
fn extract_version_from_path(path: &std::path::Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    // Validate that the name parses as a semver version to ensure
    // consistency with find_matching_cached_version which uses Version::parse.
    postgresql_embedded::Version::parse(name).ok()?;
    Some(name.to_owned())
}
