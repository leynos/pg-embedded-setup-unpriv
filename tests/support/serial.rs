//! Serialisation guard shared by behavioural test suites.

use once_cell::sync::Lazy;
use rstest::fixture;
use std::sync::{Mutex, MutexGuard};

static SCENARIO_MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[derive(Debug)]
pub struct ScenarioSerialGuard {
    _guard: MutexGuard<'static, ()>,
}

#[fixture]
pub fn serial_guard() -> ScenarioSerialGuard {
    let guard = SCENARIO_MUTEX
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    ScenarioSerialGuard { _guard: guard }
}
