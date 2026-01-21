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

/// Sets the exact version requirement in settings to skip GitHub API resolution.
fn set_exact_version(settings: &mut Settings, version: &str) {
    let exact_version = format!("={version}");
    match VersionReq::parse(&exact_version) {
        Ok(exact_req) => settings.version = exact_req,
        Err(err) => {
            debug!(
                target: LOG_TARGET,
                version = %version,
                error = %err,
                "failed to parse exact version requirement"
            );
        }
    }
}

/// Copies binaries from cache and updates installation directory.
///
/// Returns the target version directory on success, or `None` on failure.
#[expect(
    clippy::cognitive_complexity,
    reason = "error handling branches for UTF-8 validation and copy operation"
)]
fn copy_cached_binaries(
    source_dir: &Utf8PathBuf,
    version: &str,
    settings: &mut Settings,
) -> Option<Utf8PathBuf> {
    let Ok(target) = Utf8PathBuf::from_path_buf(settings.installation_dir.clone()) else {
        warn!(
            target: LOG_TARGET,
            "installation_dir is not valid UTF-8, skipping cache"
        );
        return None;
    };

    let target_version_dir = target.join(version);

    if let Err(err) = copy_from_cache(source_dir, &target_version_dir) {
        warn!(
            target: LOG_TARGET,
            version = %version,
            error = %err,
            "cache copy failed, falling back to download"
        );
        return None;
    }

    settings.installation_dir = target_version_dir.clone().into();
    settings.trust_installation_dir = true;

    Some(target_version_dir)
}

/// Applies cached binaries to the bootstrap settings.
///
/// Copies binaries from the cache source directory to the target installation directory,
/// updates bootstrap settings to use the cached version, and logs success.
fn apply_cached_binaries(
    source_dir: &Utf8PathBuf,
    version: &str,
    version_req: &VersionReq,
    bootstrap: &mut TestBootstrapSettings,
) -> bool {
    let Some(target_version_dir) =
        copy_cached_binaries(source_dir, version, &mut bootstrap.settings)
    else {
        return false;
    };

    set_exact_version(&mut bootstrap.settings, version);

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

/// Logs when no matching cached version is found.
fn log_no_matching_version(version_req: &VersionReq) {
    debug!(
        target: LOG_TARGET,
        version_req = %version_req,
        "no matching cached version found"
    );
}

/// Logs when cache lock acquisition fails.
fn log_lock_acquisition_failed(version: &str) {
    debug!(
        target: LOG_TARGET,
        version = %version,
        "failed to acquire cache lock, skipping cache"
    );
}

/// Logs when a cache entry becomes invalid after lock acquisition.
fn log_cache_entry_invalid(version: &str) {
    debug!(
        target: LOG_TARGET,
        version = %version,
        "cache entry no longer valid"
    );
}

/// Attempts to use cached binaries for the given version requirement.
///
/// Returns `true` if binaries were successfully copied from cache, `false` otherwise.
/// On cache hit, sets `trust_installation_dir = true` to skip re-validation in setup.
pub(super) fn try_use_binary_cache(
    config: &BinaryCacheConfig,
    version_req: &VersionReq,
    bootstrap: &mut TestBootstrapSettings,
) -> bool {
    let Some((version, _source_dir)) = find_matching_cached_version(&config.cache_dir, version_req)
    else {
        log_no_matching_version(version_req);
        return false;
    };

    let Ok(_lock) = CacheLock::acquire_shared(&config.cache_dir, &version) else {
        log_lock_acquisition_failed(&version);
        return false;
    };

    match check_cache(&config.cache_dir, &version) {
        CacheLookupResult::Hit { source_dir } => {
            apply_cached_binaries(&source_dir, &version, version_req, bootstrap)
        }
        CacheLookupResult::Miss => {
            log_cache_entry_invalid(&version);
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
