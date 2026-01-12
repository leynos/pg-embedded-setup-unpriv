//! Guards process environment mutations for deterministic orchestration.
//!
//! The guard is re-entrant within a thread. Nested scopes reuse the same global
//! mutex whilst keeping track of the outer state so environment restoration
//! still occurs in reverse order.
//!
//! # Example
//! ```ignore
//! use pg_embedded_setup_unpriv::ScopedEnv;
//!
//! # fn main() {
//! let guard = ScopedEnv::apply(&[(
//!     "PGUSER".into(),
//!     Some("postgres".into()),
//! )]);
//! // Perform work that relies on the scoped environment here.
//! drop(guard); // Restores the original environment once the scope ends.
//! # }
//! ```
//!
//! Nested guards on the same thread reuse the held mutex whilst tracking the
//! depth so callers can compose helpers without deadlocking. Different threads
//! are still serialised.

use crate::observability::LOG_TARGET;
use std::cell::RefCell;
use std::ffi::OsString;
use std::marker::PhantomData;
use std::rc::Rc;
use std::thread_local;
use tracing::{info, info_span};

#[cfg(all(test, feature = "loom-tests"))]
mod loom_tests;
mod state;
mod summary;
#[cfg(test)]
mod tests;

use state::{EnvLockOps, StdEnvLock, ThreadState, ThreadStateInner};
use summary::{MAX_ENV_CHANGES_SUMMARY_LEN, truncate_env_changes_summary};

pub(crate) trait ThreadStateAccess {
    type Lock: EnvLockOps;

    fn with_state<F, R>(f: F) -> R
    where
        F: FnOnce(&mut ThreadStateInner<Self::Lock>) -> R;
}

#[derive(Debug)]
struct StdThreadStateAccess;

thread_local! {
    static THREAD_STATE: RefCell<ThreadState> = const { RefCell::new(ThreadState::new()) };
}

impl ThreadStateAccess for StdThreadStateAccess {
    type Lock = StdEnvLock;

    fn with_state<F, R>(f: F) -> R
    where
        F: FnOnce(&mut ThreadStateInner<Self::Lock>) -> R,
    {
        THREAD_STATE.with(|cell| {
            let mut state = cell.borrow_mut();
            f(&mut state)
        })
    }
}

/// Restores the process environment when dropped, reverting to prior values.
#[derive(Debug)]
#[must_use = "Hold the guard until the end of the environment scope"]
pub struct ScopedEnv(ScopedEnvCore<StdThreadStateAccess>);

#[derive(Debug)]
pub(crate) struct ScopedEnvCore<A: ThreadStateAccess> {
    index: usize,
    span: tracing::Span,
    change_count: usize,
    // !Send + !Sync so drops always occur on the creating thread.
    _not_send_or_sync: PhantomData<Rc<()>>,
    _thread_state_access: PhantomData<A>,
}

impl<A: ThreadStateAccess> ScopedEnvCore<A> {
    /// Applies the supplied environment variables and returns a guard that
    /// restores the previous values when dropped.
    pub(crate) fn apply(vars: &[(String, Option<String>)]) -> Self {
        let owned: Vec<(OsString, Option<OsString>)> = vars
            .iter()
            .map(|(key, value)| {
                debug_assert!(
                    !key.is_empty() && !key.contains('='),
                    "invalid env var name"
                );
                let owned_value = value.as_ref().map(OsString::from);
                (OsString::from(key), owned_value)
            })
            .collect();
        Self::apply_owned(owned)
    }

    /// Applies environment variables provided as `OsString` pairs by any owned iterator.
    pub(crate) fn apply_os<I>(vars: I) -> Self
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        let owned: Vec<(OsString, Option<OsString>)> = vars.into_iter().collect();
        Self::apply_owned(owned)
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "span entry plus bounded summary handling add structured branching"
    )]
    fn apply_owned(vars: Vec<(OsString, Option<OsString>)>) -> Self {
        // Build a concise summary of applied changes whilst preserving count for full context.
        let summary: Vec<String> = vars
            .iter()
            .map(|(key, value)| {
                let status = if value.is_some() { "set" } else { "unset" };
                format!("{}={status}", key.to_string_lossy())
            })
            .collect();
        let change_count = summary.len();
        let changes = summary.join(", ");

        let truncated_changes =
            truncate_env_changes_summary(&changes, MAX_ENV_CHANGES_SUMMARY_LEN, change_count);

        let span = info_span!(
            target: LOG_TARGET,
            "scoped_env",
            change_count,
            changes = %truncated_changes
        );
        let index = {
            let _entered = span.enter();
            let index = A::with_state(|state| state.enter_scope(vars));
            info!(
                target: LOG_TARGET,
                change_count,
                changes = %truncated_changes,
                "applied scoped environment variables"
            );
            index
        };
        Self {
            index,
            span,
            change_count,
            _not_send_or_sync: PhantomData,
            _thread_state_access: PhantomData,
        }
    }
}

impl<A: ThreadStateAccess> Drop for ScopedEnvCore<A> {
    fn drop(&mut self) {
        let _entered = self.span.enter();
        info!(
            target: LOG_TARGET,
            change_count = self.change_count,
            "restoring scoped environment variables"
        );
        A::with_state(|state| {
            state.exit_scope(self.index);
        });
    }
}

impl ScopedEnv {
    /// Applies the supplied environment variables and returns a guard that
    /// restores the previous values when dropped.
    pub(crate) fn apply(vars: &[(String, Option<String>)]) -> Self {
        Self(ScopedEnvCore::<StdThreadStateAccess>::apply(vars))
    }

    /// Applies environment variables provided as `OsString` pairs by any owned iterator.
    pub(crate) fn apply_os<I>(vars: I) -> Self
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        Self(ScopedEnvCore::<StdThreadStateAccess>::apply_os(vars))
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        // Ensure the inner guard is marked as used; ScopedEnvCore handles drop logic.
        let _ = &self.0;
    }
}
