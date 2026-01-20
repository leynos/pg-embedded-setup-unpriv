//! Tests for cache operations.

use super::*;
use camino::Utf8Path;
use postgresql_embedded::VersionReq;
use std::fs;
use tempfile::tempdir;

/// Marker file name indicating a complete cache entry.
const COMPLETION_MARKER: &str = ".complete";

fn create_mock_binaries(dir: &Utf8Path) {
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    fs::write(bin_dir.join("postgres"), "mock postgres binary").expect("write mock binary");
    fs::write(bin_dir.join("pg_ctl"), "mock pg_ctl binary").expect("write mock binary");
}

/// Creates a complete cache entry with binaries and completion marker.
fn create_complete_cache_entry(cache_dir: &Utf8Path, version: &str) -> camino::Utf8PathBuf {
    let version_dir = cache_dir.join(version);
    create_mock_binaries(&version_dir);
    fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");
    version_dir
}

/// Asserts that a cache lookup with the given versions and requirement returns the expected result.
fn assert_cached_match(versions: &[&str], req: &str, expected: Option<&str>) {
    let temp = tempdir().expect("tempdir");
    let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");

    for version in versions {
        create_complete_cache_entry(cache_dir, version);
    }

    let version_req = VersionReq::parse(req).expect("parse version req");
    let result = find_matching_cached_version(cache_dir, &version_req);

    match expected {
        Some(ver) => {
            let (version, path) = result.expect("should find cached version");
            assert_eq!(version, ver);
            assert!(path.ends_with(ver));
        }
        None => {
            assert!(result.is_none());
        }
    }
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
    let version_dir = create_complete_cache_entry(cache_dir, "17.4.0");

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
    assert_cached_match(&["17.4.0"], "=17.4.0", Some("17.4.0"));
}

#[test]
fn find_matching_cached_version_matches_caret_requirement() {
    assert_cached_match(&["17.4.0"], "^17", Some("17.4.0"));
}

#[test]
fn find_matching_cached_version_returns_highest_matching() {
    assert_cached_match(&["17.2.0", "17.4.0"], "^17", Some("17.4.0"));
}

#[test]
fn find_matching_cached_version_ignores_non_matching() {
    let temp = tempdir().expect("tempdir");
    let cache_dir = Utf8Path::from_path(temp.path()).expect("utf8 path");
    create_complete_cache_entry(cache_dir, "16.0.0");

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

    let version_req = VersionReq::parse("^17").expect("parse version req");
    let result = find_matching_cached_version(cache_dir, &version_req);

    assert!(result.is_none());
}
