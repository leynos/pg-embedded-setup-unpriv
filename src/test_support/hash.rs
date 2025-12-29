//! Directory hashing utilities for template naming.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;

use crate::error::BootstrapResult;

/// Computes a SHA-256 hash of a directory's contents for use in template names.
///
/// This function walks the directory tree, hashing file names and contents
/// in a deterministic order. It can be used to create migration-versioned
/// template names that automatically invalidate when migrations change.
///
/// # Errors
///
/// Returns an error if the directory cannot be read or if file I/O fails.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::hash_directory;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let hash = hash_directory("migrations")?;
/// let template_name = format!("template_{}", &hash[..8]);
/// # Ok(())
/// # }
/// ```
pub fn hash_directory(dir_path: impl AsRef<Path>) -> BootstrapResult<String> {
    let base = dir_path.as_ref();
    let mut hasher = Sha256::new();

    hash_directory_recursive(base, base, &mut hasher)?;

    let result = hasher.finalize();
    Ok(format!("{result:x}"))
}

fn hash_directory_recursive(
    base: &Path,
    current: &Path,
    hasher: &mut Sha256,
) -> BootstrapResult<()> {
    let read_dir = fs::read_dir(current).map_err(|e| {
        color_eyre::eyre::eyre!("failed to read directory '{}': {e}", current.display())
    })?;

    let mut entries: Vec<_> = read_dir
        .map(|entry| {
            entry.map_err(|e| {
                color_eyre::eyre::eyre!(
                    "failed to read directory entry in '{}': {e}",
                    current.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Sort entries for deterministic ordering
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy();

        // Hash the relative path
        hasher.update(relative.as_bytes());

        if path.is_dir() {
            hash_directory_recursive(base, &path, hasher)?;
        } else if path.is_file() {
            hash_file_contents(&path, hasher)?;
        }
    }

    Ok(())
}

/// Hashes file contents using chunked streaming I/O.
#[expect(
    clippy::indexing_slicing,
    reason = "bytes_read is always <= buffer.len()"
)]
fn hash_file_contents(path: &Path, hasher: &mut Sha256) -> BootstrapResult<()> {
    let mut file = fs::File::open(path)
        .map_err(|e| color_eyre::eyre::eyre!("failed to open file '{}': {e}", path.display()))?;
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = file.read(&mut buffer).map_err(|e| {
            color_eyre::eyre::eyre!("failed to read file '{}': {e}", path.display())
        })?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn hash_directory_produces_consistent_results() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("file1.sql"), "CREATE TABLE a;").expect("write");
        fs::write(temp.path().join("file2.sql"), "CREATE TABLE b;").expect("write");

        let hash1 = hash_directory(temp.path()).expect("hash1");
        let hash2 = hash_directory(temp.path()).expect("hash2");

        assert_eq!(hash1, hash2, "same contents should produce same hash");
    }

    #[test]
    fn hash_directory_changes_with_content() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("file.sql"), "CREATE TABLE a;").expect("write");

        let hash1 = hash_directory(temp.path()).expect("hash1");

        fs::write(temp.path().join("file.sql"), "CREATE TABLE b;").expect("write");
        let hash2 = hash_directory(temp.path()).expect("hash2");

        assert_ne!(
            hash1, hash2,
            "different contents should produce different hash"
        );
    }

    #[test]
    fn hash_directory_is_64_hex_chars() {
        let temp = TempDir::new().expect("tempdir");
        fs::write(temp.path().join("test.sql"), "SELECT 1;").expect("write");

        let hash = hash_directory(temp.path()).expect("hash");

        assert_eq!(hash.len(), 64, "SHA-256 hex should be 64 characters");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex"
        );
    }

    #[test]
    fn hash_directory_handles_nested_directories() {
        let temp = TempDir::new().expect("tempdir");

        // Create nested directory structure
        let subdir = temp.path().join("migrations");
        fs::create_dir(&subdir).expect("create subdir");
        fs::write(subdir.join("001_init.sql"), "CREATE TABLE users;").expect("write");

        let nested = subdir.join("nested");
        fs::create_dir(&nested).expect("create nested");
        fs::write(nested.join("002_add_posts.sql"), "CREATE TABLE posts;").expect("write");

        // Root level file
        fs::write(temp.path().join("schema.sql"), "-- schema").expect("write");

        let hash1 = hash_directory(temp.path()).expect("hash1");
        let hash2 = hash_directory(temp.path()).expect("hash2");

        assert_eq!(
            hash1, hash2,
            "nested directory hash should be deterministic"
        );

        // Modify nested file and verify hash changes
        fs::write(nested.join("002_add_posts.sql"), "CREATE TABLE comments;").expect("write");
        let hash3 = hash_directory(temp.path()).expect("hash3");

        assert_ne!(hash1, hash3, "nested file change should affect hash");
    }

    #[test]
    fn hash_directory_handles_empty_directory() {
        let temp = TempDir::new().expect("tempdir");

        let hash = hash_directory(temp.path()).expect("hash");

        // Empty directory should produce a valid hash
        assert_eq!(hash.len(), 64, "empty directory hash should be 64 chars");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "empty directory hash should be hex"
        );

        // Add a file and verify hash changes
        fs::write(temp.path().join("file.sql"), "SELECT 1;").expect("write");
        let hash2 = hash_directory(temp.path()).expect("hash2");

        assert_ne!(hash, hash2, "adding file should change hash");
    }
}
