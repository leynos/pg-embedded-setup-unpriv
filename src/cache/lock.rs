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
        let result = unsafe { libc::flock(file.as_raw_fd(), flock_arg) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { _file: file })
    }

    /// No-op lock acquisition on non-Unix platforms.
    #[cfg(not(unix))]
    fn acquire(_cache_dir: &Utf8Path, version: &str, _lock_type: LockType) -> io::Result<Self> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquire_exclusive_creates_lock_file() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = camino::Utf8Path::from_path(temp.path()).expect("utf8 path");
        let _lock = CacheLock::acquire_exclusive(cache_dir, "17.4.0").expect("acquire lock");

        let lock_path = temp.path().join(LOCKS_SUBDIR).join("17.4.0.lock");
        assert!(lock_path.exists(), "lock file should be created");
    }

    #[test]
    fn acquire_shared_creates_lock_file() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = camino::Utf8Path::from_path(temp.path()).expect("utf8 path");
        let _lock = CacheLock::acquire_shared(cache_dir, "16.3.0").expect("acquire lock");

        let lock_path = temp.path().join(LOCKS_SUBDIR).join("16.3.0.lock");
        assert!(lock_path.exists(), "lock file should be created");
    }

    #[test]
    fn multiple_shared_locks_can_coexist() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = camino::Utf8Path::from_path(temp.path()).expect("utf8 path");

        let lock1 = CacheLock::acquire_shared(cache_dir, "17.4.0").expect("acquire lock 1");
        let lock2 = CacheLock::acquire_shared(cache_dir, "17.4.0").expect("acquire lock 2");

        // Both locks should be held successfully
        drop(lock1);
        drop(lock2);
    }

    #[test]
    fn different_versions_have_separate_locks() {
        let temp = tempdir().expect("tempdir");
        let cache_dir = camino::Utf8Path::from_path(temp.path()).expect("utf8 path");

        let lock1 = CacheLock::acquire_exclusive(cache_dir, "17.4.0").expect("acquire lock 1");
        let lock2 = CacheLock::acquire_exclusive(cache_dir, "16.3.0").expect("acquire lock 2");

        // Different versions should not block each other
        drop(lock1);
        drop(lock2);
    }
}
