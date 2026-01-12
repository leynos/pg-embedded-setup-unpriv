//! Corruption recovery test helpers.
//!
//! Provides fixtures and utilities for exercising `ScopedEnv` recovery when
//! thread-local state is corrupted (invalid scope exits and out-of-order guard
//! drops). These helpers are used by the parameterised `rstest` cases in
//! `mod.rs`.

use super::ScopedEnv;
use super::THREAD_STATE;
use std::ffi::OsString;
use std::panic;

/// Runs a corruption scenario with a unique env key and delegated assertions.
pub(super) fn run_scoped_env_corruption_test<F>(test_name: &str, setup_and_corrupt: F)
where
    F: FnOnce(&OsString),
{
    let key = OsString::from(format!("SCOPED_ENV_{test_name}"));
    setup_and_corrupt(&key);
}

pub(super) enum GuardSet {
    Single(ScopedEnv),
    Nested { outer: ScopedEnv, inner: ScopedEnv },
}

#[derive(Clone, Copy)]
pub(super) struct CorruptionCase {
    pub(super) test_name: &'static str,
    pub(super) setup_guards: fn(&OsString) -> GuardSet,
    pub(super) corrupt_state: fn() -> bool,
    pub(super) drop_guards: fn(GuardSet),
    pub(super) drop_message: &'static str,
}

impl GuardSet {
    fn drop_in_order(self) {
        match self {
            Self::Single(guard) => drop(guard),
            Self::Nested { outer, inner } => {
                drop(inner);
                drop(outer);
            }
        }
    }

    fn drop_out_of_order(self) {
        match self {
            Self::Single(guard) => drop(guard),
            Self::Nested { outer, inner } => {
                drop(outer);
                drop(inner);
            }
        }
    }
}

pub(super) fn setup_single_guard(key: &OsString) -> GuardSet {
    GuardSet::Single(ScopedEnv::apply_os(vec![(
        key.clone(),
        Some(OsString::from("value")),
    )]))
}

pub(super) fn setup_nested_guards(key: &OsString) -> GuardSet {
    let outer = ScopedEnv::apply_os(vec![(key.clone(), Some(OsString::from("outer")))]);
    let inner = ScopedEnv::apply_os(vec![(key.clone(), Some(OsString::from("inner")))]);
    GuardSet::Nested { outer, inner }
}

/// Returns true to signal callers should assert state restoration.
pub(super) fn apply_invalid_scope_exit() -> bool {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        THREAD_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            state.exit_scope(usize::MAX);
        });
    }));
    assert!(result.is_ok(), "invalid scope exit should not panic");
    true
}

/// Returns false so callers skip restoration assertions.
pub(super) fn no_corruption() -> bool {
    false
}

pub(super) fn drop_guards_in_order(guards: GuardSet) {
    guards.drop_in_order();
}

pub(super) fn drop_guards_out_of_order(guards: GuardSet) {
    guards.drop_out_of_order();
}
