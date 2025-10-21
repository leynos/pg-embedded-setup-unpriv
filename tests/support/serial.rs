//! Serialisation guard shared by behavioural test suites.

use once_cell::sync::Lazy;
use rstest::fixture;
use std::sync::{Mutex, MutexGuard};

static SCENARIO_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[derive(Debug)]
#[must_use = "Hold this guard for the duration of the serialised scenario"]
pub struct ScenarioSerialGuard {
    _guard: MutexGuard<'static, ()>,
}

/// Provides a serialisation guard for behavioural test scenarios.
///
/// Acquires a global mutex to ensure that scenarios relying on shared state
/// (such as process environment variables or singleton resources) execute
/// serially, preventing cross-test interference.
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
/// ```rust
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
    let guard = SCENARIO_MUTEX
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    ScenarioSerialGuard { _guard: guard }
}
