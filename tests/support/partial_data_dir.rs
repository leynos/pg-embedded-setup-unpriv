//! Shared helpers for creating partial `PostgreSQL` data directories in tests.
//!
//! A "partial" data directory simulates an interrupted `initdb` operation:
//! - Has `PG_VERSION` (created early by `initdb`)
//! - Has `global/` directory
//! - Has `base/1/` directory with some dummy files
//! - MISSING `global/pg_filenode.map` (created late by `initdb`, our marker)
//!
//! This module provides a consistent way to create such directories across
//! different test files.

use std::fs;
use std::path::Path;

/// Creates a partial data directory structure that simulates an interrupted `initdb`.
///
/// The directory will have:
/// - `PG_VERSION` file with version "16"
/// - `global/` directory (empty, no `pg_filenode.map`)
/// - `base/1/pg_class` file with dummy content
///
/// This structure is detected as invalid by the recovery logic because it
/// lacks `global/pg_filenode.map`.
///
/// # Errors
///
/// Returns an error if any filesystem operation fails.
pub fn create_partial_data_dir(data_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(data_dir.join("global"))?;
    fs::write(data_dir.join("PG_VERSION"), "16\n")?;
    fs::create_dir_all(data_dir.join("base/1"))?;
    fs::write(data_dir.join("base/1/pg_class"), "dummy")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn creates_expected_structure() {
        let temp = tempdir().expect("failed to create temp dir");
        let data_dir = temp.path().join("data");

        create_partial_data_dir(&data_dir).expect("failed to create partial data dir");

        assert!(data_dir.join("PG_VERSION").exists());
        assert!(data_dir.join("global").is_dir());
        assert!(data_dir.join("base/1/pg_class").exists());
        // The marker file should NOT exist - that's what makes it "partial"
        assert!(!data_dir.join("global/pg_filenode.map").exists());
    }
}
