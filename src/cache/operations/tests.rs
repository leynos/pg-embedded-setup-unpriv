//! Tests for cache operations.

use super::lookup::COMPLETION_MARKER;
use super::*;
use camino::Utf8Path;
use postgresql_embedded::VersionReq;
use rstest::{fixture, rstest};
use std::fs;
use tempfile::{TempDir, tempdir};

/// Creates a `bin` subdirectory in the given path with mock `postgres` and
/// `pg_ctl` binary files for cache tests.
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

/// Fixture providing a temporary cache directory as a UTF-8 path.
#[fixture]
fn cache_fixture() -> (TempDir, camino::Utf8PathBuf) {
    let temp = tempdir().expect("tempdir");
    let cache_dir =
        camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 path");
    (temp, cache_dir)
}

#[rstest]
fn check_cache_returns_miss_for_empty_directory(cache_fixture: (TempDir, camino::Utf8PathBuf)) {
    let (_temp, cache_dir) = cache_fixture;

    let result = check_cache(&cache_dir, "17.4.0");
    assert!(matches!(result, CacheLookupResult::Miss));
}

#[rstest]
fn check_cache_returns_miss_without_marker(cache_fixture: (TempDir, camino::Utf8PathBuf)) {
    let (_temp, cache_dir) = cache_fixture;
    let version_dir = cache_dir.join("17.4.0");
    create_mock_binaries(&version_dir);

    let result = check_cache(&cache_dir, "17.4.0");
    assert!(matches!(result, CacheLookupResult::Miss));
}

#[rstest]
fn check_cache_returns_miss_without_bin_directory(cache_fixture: (TempDir, camino::Utf8PathBuf)) {
    let (_temp, cache_dir) = cache_fixture;
    let version_dir = cache_dir.join("17.4.0");
    fs::create_dir_all(&version_dir).expect("create version dir");
    fs::write(version_dir.join(COMPLETION_MARKER), "").expect("write marker");

    let result = check_cache(&cache_dir, "17.4.0");
    assert!(matches!(result, CacheLookupResult::Miss));
}

#[rstest]
fn check_cache_returns_hit_with_marker_and_bin(cache_fixture: (TempDir, camino::Utf8PathBuf)) {
    let (_temp, cache_dir) = cache_fixture;
    let version_dir = create_complete_cache_entry(&cache_dir, "17.4.0");

    let result = check_cache(&cache_dir, "17.4.0");
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

/// Asserts `try_use_cache` behaviour with configurable setup and expectations.
fn assert_try_use_cache(populate_cache: bool, expected_result: bool, check_files_copied: bool) {
    let cache_temp = tempdir().expect("cache tempdir");
    let target_temp = tempdir().expect("target tempdir");
    let cache_dir = Utf8Path::from_path(cache_temp.path()).expect("utf8 cache");
    let target = Utf8Path::from_path(target_temp.path()).expect("utf8 target");

    if populate_cache {
        create_complete_cache_entry(cache_dir, "17.4.0");
    }

    let result = try_use_cache(cache_dir, "17.4.0", target);
    assert_eq!(result, expected_result);

    if check_files_copied {
        assert!(target.join("bin/postgres").exists());
    }
}

#[test]
fn try_use_cache_returns_false_on_miss() {
    assert_try_use_cache(false, false, false);
}

#[test]
fn try_use_cache_returns_true_on_hit() {
    assert_try_use_cache(true, true, true);
}

#[rstest]
fn find_matching_cached_version_returns_none_for_empty_cache(
    cache_fixture: (TempDir, camino::Utf8PathBuf),
) {
    let (_temp, cache_dir) = cache_fixture;
    let version_req = VersionReq::parse("^17").expect("parse version req");

    let result = find_matching_cached_version(&cache_dir, &version_req);
    assert!(result.is_none());
}

#[rstest]
#[case::exact_match(&["17.4.0"], "=17.4.0", Some("17.4.0"))]
#[case::caret_requirement(&["17.4.0"], "^17", Some("17.4.0"))]
#[case::highest_matching(&["17.2.0", "17.4.0"], "^17", Some("17.4.0"))]
fn find_matching_cached_version_scenarios(
    #[case] versions: &[&str],
    #[case] req: &str,
    #[case] expected: Option<&str>,
) {
    assert_cached_match(versions, req, expected);
}

#[rstest]
fn find_matching_cached_version_ignores_non_matching(
    cache_fixture: (TempDir, camino::Utf8PathBuf),
) {
    let (_temp, cache_dir) = cache_fixture;
    create_complete_cache_entry(&cache_dir, "16.0.0");

    let version_req = VersionReq::parse("^17").expect("parse version req");
    let result = find_matching_cached_version(&cache_dir, &version_req);

    assert!(result.is_none());
}

#[rstest]
fn find_matching_cached_version_ignores_incomplete_entries(
    cache_fixture: (TempDir, camino::Utf8PathBuf),
) {
    let (_temp, cache_dir) = cache_fixture;

    // Create an incomplete cache entry (no marker)
    let version_dir = cache_dir.join("17.4.0");
    create_mock_binaries(&version_dir);

    let version_req = VersionReq::parse("^17").expect("parse version req");
    let result = find_matching_cached_version(&cache_dir, &version_req);

    assert!(result.is_none());
}
