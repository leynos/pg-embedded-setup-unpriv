//! Serialization guard shared by behavioural test suites.
//!
//! Acquire this guard **before** calling environment helpers such as
//! [`crate::test_support::with_scoped_env`] to maintain the lock-ordering
//! contract used throughout the integration scenarios (process lock, scenario
//! mutex, then environment mutex). Following this order prevents deadlocks when
//! multiple suites mutate process-wide state.

use rstest::fixture;
use std::sync::{Mutex, MutexGuard};

#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(unix)]
use std::path::PathBuf;

static SCENARIO_MUTEX: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

#[cfg(unix)]
type ProcessLock = std::fs::File;

#[cfg(not(unix))]
type ProcessLock = ();

#[derive(Debug)]
#[must_use = "Hold this guard for the duration of the serialized scenario"]
pub struct ScenarioSerialGuard {
    _guard: MutexGuard<'static, ()>,
    _lock_file: ProcessLock,
}

/// Provides a serialization guard for behavioural test scenarios.
///
/// Acquires a global mutex to ensure that scenarios relying on shared state
/// (such as process environment variables or singleton resources) execute
/// serially, preventing cross-test interference. A cross-process file lock is
/// also acquired so independent test binaries coordinate access to the shared
/// `PostgreSQL` cache and installation directories.
///
/// # Behaviour
///
/// - Acquires the global `SCENARIO_MUTEX` and wraps the guard.
/// - If the mutex is poisoned (a previous test panicked whilst holding the lock),
///   the poison is cleared and execution continues.
/// - The guard is automatically released when dropped at the end of the test.
///
/// # Examples
///
/// ```rust,ignore
/// use rstest::rstest;
/// use tests::support::serial::serial_guard;
///
/// #[rstest]
/// fn my_scenario(_guard: serial_guard) {
///     // Test code that mutates shared state
/// }
/// ```
#[fixture]
pub fn serial_guard() -> ScenarioSerialGuard {
    let lock_file = acquire_process_lock();
    let guard = SCENARIO_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ScenarioSerialGuard {
        _guard: guard,
        _lock_file: lock_file,
    }
}

#[cfg(unix)]
fn acquire_process_lock() -> ProcessLock {
    let target_dir =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    std::fs::create_dir_all(&target_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create target dir for scenario lock at {}: {err}",
            target_dir.display()
        );
    });
    let lock_path = target_dir.join("pg-embed-setup-unpriv.serial.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|err| {
            panic!(
                "failed to open scenario lock file at {}: {err}",
                lock_path.display()
            );
        });
    // SAFETY: The file descriptor obtained from `lock_file.as_raw_fd()` is valid
    // because `lock_file` was opened via `OpenOptions::open` and remains owned by
    // this scope until after the `flock` call completes. No other code moves or
    // closes the descriptor while this block runs. The `libc::flock` syscall
    // operates on the OS-level file descriptor and does not access Rust memory,
    // so there are no data-race concerns from Rust's perspective.
    let result = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    assert!(
        result == 0,
        "failed to acquire scenario lock at {}: {}",
        lock_path.display(),
        std::io::Error::last_os_error()
    );
    lock_file
}

#[cfg(not(unix))]
fn acquire_process_lock() -> ProcessLock {
    ()
}

#[cfg(test)]
mod tests {
    //! Unit tests for scenario serialization guards.

    use rstest::rstest;

    use super::*;

    /// Drop guard to restore `CARGO_TARGET_DIR` even on panic.
    #[cfg(unix)]
    struct EnvGuard {
        original: Option<std::ffi::OsString>,
    }

    #[cfg(unix)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            use std::env;
            // SAFETY: Test runs serially via serial_guard fixture.
            if let Some(val) = &self.original {
                unsafe { env::set_var("CARGO_TARGET_DIR", val) };
            } else {
                unsafe { env::remove_var("CARGO_TARGET_DIR") };
            }
        }
    }

    #[test]
    fn serial_guard_is_not_reentrant() {
        let guard = serial_guard();
        assert!(SCENARIO_MUTEX.try_lock().is_err());
        drop(guard);
        let reacquired = SCENARIO_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(reacquired);
    }

    #[cfg(unix)]
    #[rstest]
    fn acquire_process_lock_places_lock_file_in_cargo_target_dir(
        serial_guard: ScenarioSerialGuard,
    ) {
        use std::{env, fs};

        let _guard = serial_guard;

        let tmp_dir = env::temp_dir().join("pg_scenario_lock_test");
        // Best-effort cleanup of any previous test run; errors are expected if
        // the directory does not exist.
        match fs::remove_dir_all(&tmp_dir) {
            Ok(()) | Err(_) => {}
        }
        fs::create_dir_all(&tmp_dir)
            .expect("failed to create temporary CARGO_TARGET_DIR for acquire_process_lock test");

        // SAFETY: This test runs serially via the serial_guard fixture, so no
        // other threads are concurrently reading or writing environment variables.
        let _env_guard = EnvGuard {
            original: env::var_os("CARGO_TARGET_DIR"),
        };

        // Temporarily set CARGO_TARGET_DIR to our test directory.
        // SAFETY: Same reasoning as above - test runs serially.
        unsafe { env::set_var("CARGO_TARGET_DIR", &tmp_dir) };
        let _lock = acquire_process_lock();

        let entries: Vec<_> = fs::read_dir(&tmp_dir)
            .expect("failed to read temporary CARGO_TARGET_DIR for acquire_process_lock test")
            .collect();
        assert!(
            !entries.is_empty(),
            "expected acquire_process_lock to create a lock file in {tmp_dir:?}, but directory was empty"
        );

        // Best-effort cleanup; errors are non-fatal in test teardown.
        match fs::remove_dir_all(&tmp_dir) {
            Ok(()) | Err(_) => {}
        }
    }
}
