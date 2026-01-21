//! Cross-process file locking for cache coordination.
//!
//! Provides exclusive and shared locks to coordinate binary downloads across
//! parallel test runners. On Unix systems, uses `flock(2)` for advisory locking.
//! On non-Unix platforms, locking is a no-op.

use camino::Utf8Path;
use std::fs::{File, OpenOptions};
use std::io;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// Subdirectory within the cache for lock files.
const LOCKS_SUBDIR: &str = ".locks";

/// Guard that holds a file lock until dropped.
///
/// The lock is automatically released when the guard goes out of scope.
#[derive(Debug)]
pub struct CacheLock {
    _file: File,
}

impl CacheLock {
    /// Acquires an exclusive lock for a specific version.
    ///
    /// Use exclusive locks when downloading or populating the cache to prevent
    /// concurrent writes.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock file cannot be created or the lock cannot
    /// be acquired.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use camino::Utf8Path;
    /// use pg_embedded_setup_unpriv::cache::CacheLock;
    ///
    /// let cache_dir = Utf8Path::new("/tmp/pg-cache");
    /// let _lock = CacheLock::acquire_exclusive(cache_dir, "17.4.0")?;
    /// // Exclusive access to version 17.4.0 cache entry
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn acquire_exclusive(cache_dir: &Utf8Path, version: &str) -> io::Result<Self> {
        Self::acquire(cache_dir, version, LockType::Exclusive)
    }

    /// Acquires a shared lock for a specific version.
    ///
    /// Use shared locks when reading from the cache to allow concurrent reads
    /// whilst blocking writes.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock file cannot be created or the lock cannot
    /// be acquired.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use camino::Utf8Path;
    /// use pg_embedded_setup_unpriv::cache::CacheLock;
    ///
    /// let cache_dir = Utf8Path::new("/tmp/pg-cache");
    /// let _lock = CacheLock::acquire_shared(cache_dir, "17.4.0")?;
    /// // Shared access to version 17.4.0 cache entry
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn acquire_shared(cache_dir: &Utf8Path, version: &str) -> io::Result<Self> {
        Self::acquire(cache_dir, version, LockType::Shared)
    }

    /// Acquires a lock with the specified type.
    #[cfg(unix)]
    fn acquire(cache_dir: &Utf8Path, version: &str, lock_type: LockType) -> io::Result<Self> {
        validate_version(version)?;
        let locks_dir = cache_dir.join(LOCKS_SUBDIR);
        std::fs::create_dir_all(&locks_dir)?;

        let lock_path = locks_dir.join(format!("{version}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        let flock_arg = match lock_type {
            LockType::Exclusive => libc::LOCK_EX,
            LockType::Shared => libc::LOCK_SH,
        };

        // SAFETY: The file descriptor obtained from `file.as_raw_fd()` is valid
        // because `file` was opened via `OpenOptions::open` and remains owned by
        // this scope until after the `flock` call completes. No other code moves
        // or closes the descriptor while this block runs.
        //
        // Retry loop handles EINTR, which can occur when the process receives a
        // signal while blocked on flock.
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), flock_arg) };
            if result == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
            // EINTR: signal interrupted syscall, retry.
        }

        Ok(Self { _file: file })
    }

    /// No-op lock acquisition on non-Unix platforms.
    #[cfg(not(unix))]
    fn acquire(_cache_dir: &Utf8Path, version: &str, _lock_type: LockType) -> io::Result<Self> {
        validate_version(version)?;
        // Cross-process locking not supported; return a dummy lock.
        // Concurrent tests may race on non-Unix platforms.
        // Create a temporary file without external dependencies.
        let temp_path = std::env::temp_dir().join(format!("pg-cache-lock-{}.tmp", version));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;
        // Attempt cleanup; ignore errors as temp files are ephemeral.
        drop(std::fs::remove_file(&temp_path));
        Ok(Self { _file: file })
    }
}

/// Type of lock to acquire.
#[derive(Debug, Clone, Copy)]
enum LockType {
    /// Exclusive lock for writes.
    Exclusive,
    /// Shared lock for reads.
    Shared,
}

/// Validates that a version string is a single path component.
///
/// Rejects versions containing path separators or parent directory references
/// that could escape the cache directory.
fn validate_version(version: &str) -> io::Result<()> {
    use std::path::Component;

    let mut components = std::path::Path::new(version).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "version must be a single path component",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::{fixture, rstest};
    use tempfile::TempDir;

    /// Fixture providing a temporary cache directory as a UTF-8 path.
    #[fixture]
    fn cache_fixture() -> (TempDir, camino::Utf8PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_dir =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 path");
        (temp, cache_dir)
    }

    #[rstest]
    #[case::exclusive("17.4.0", true)]
    #[case::shared("16.3.0", false)]
    fn acquire_lock_creates_lock_file(
        cache_fixture: (TempDir, camino::Utf8PathBuf),
        #[case] version: &str,
        #[case] exclusive: bool,
    ) {
        let (temp, cache_dir) = cache_fixture;
        let _lock = if exclusive {
            CacheLock::acquire_exclusive(&cache_dir, version).expect("acquire lock")
        } else {
            CacheLock::acquire_shared(&cache_dir, version).expect("acquire lock")
        };

        let lock_path = temp
            .path()
            .join(LOCKS_SUBDIR)
            .join(format!("{version}.lock"));
        assert!(lock_path.exists(), "lock file should be created");
    }

    #[rstest]
    fn multiple_shared_locks_can_coexist(cache_fixture: (TempDir, camino::Utf8PathBuf)) {
        let (_temp, cache_dir) = cache_fixture;

        let lock1 = CacheLock::acquire_shared(&cache_dir, "17.4.0").expect("acquire lock 1");
        let lock2 = CacheLock::acquire_shared(&cache_dir, "17.4.0").expect("acquire lock 2");

        // Both locks should be held successfully
        drop(lock1);
        drop(lock2);
    }

    #[rstest]
    fn different_versions_have_separate_locks(cache_fixture: (TempDir, camino::Utf8PathBuf)) {
        let (_temp, cache_dir) = cache_fixture;

        let lock1 = CacheLock::acquire_exclusive(&cache_dir, "17.4.0").expect("acquire lock 1");
        let lock2 = CacheLock::acquire_exclusive(&cache_dir, "16.3.0").expect("acquire lock 2");

        // Different versions should not block each other
        drop(lock1);
        drop(lock2);
    }

    #[rstest]
    #[case::parent_dir_exclusive("..")]
    #[case::parent_dir_shared("..")]
    #[case::path_separator_exclusive("foo/bar")]
    #[case::path_separator_shared("foo/bar")]
    #[case::parent_in_path_exclusive("../17.4.0")]
    #[case::absolute_path_exclusive("/etc/passwd")]
    fn acquire_rejects_invalid_version_strings(
        cache_fixture: (TempDir, camino::Utf8PathBuf),
        #[case] invalid_version: &str,
    ) {
        let (_temp, cache_dir) = cache_fixture;

        let exclusive_err = CacheLock::acquire_exclusive(&cache_dir, invalid_version)
            .expect_err("acquire_exclusive should reject invalid version");
        assert_eq!(
            exclusive_err.kind(),
            io::ErrorKind::InvalidInput,
            "error kind should be InvalidInput for: {invalid_version}"
        );

        let shared_err = CacheLock::acquire_shared(&cache_dir, invalid_version)
            .expect_err("acquire_shared should reject invalid version");
        assert_eq!(
            shared_err.kind(),
            io::ErrorKind::InvalidInput,
            "error kind should be InvalidInput for: {invalid_version}"
        );
    }
}
