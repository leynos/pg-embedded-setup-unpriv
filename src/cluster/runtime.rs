//! Helpers for constructing Tokio runtimes used by `TestCluster`.

use crate::error::{BootstrapError, BootstrapResult};
use color_eyre::eyre::Context;
use tokio::runtime::{Builder, Runtime};

use super::panic_utils::nested_runtime_thread_panic;

/// Constructs a current-thread Tokio runtime for `TestCluster` lifecycle work.
///
/// Enables all Tokio features (I/O, time, etc.) so embedded `PostgreSQL`
/// operations can run to completion on the dedicated runtime.
///
/// # Errors
///
/// Returns an error when the runtime cannot be built, for example due to
/// resource limits or incompatible platform configuration.
pub(crate) fn build_runtime() -> BootstrapResult<Runtime> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime for TestCluster")
        .map_err(BootstrapError::from)
}

/// Runs lifecycle work with a Tokio runtime, even when already inside one.
///
/// When called from within a Tokio runtime, this helper executes `operation`
/// on a scoped helper thread so runtime creation and teardown stay outside the
/// async context.
pub(crate) fn run_with_runtime<T, F>(context: &'static str, operation: F) -> BootstrapResult<T>
where
    F: FnOnce(&Runtime) -> BootstrapResult<T> + Send,
    T: Send,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = build_runtime()?;
                    operation(&runtime)
                })
                .join()
        })
        .map_err(|panic_payload| {
            nested_runtime_thread_panic(context, "lifecycle", panic_payload)
        })?;
    }

    let runtime = build_runtime()?;
    operation(&runtime)
}
