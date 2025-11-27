//! Helpers for constructing Tokio runtimes used by `TestCluster`.

use crate::error::{BootstrapError, BootstrapResult};
use color_eyre::eyre::Context;
use tokio::runtime::{Builder, Runtime};

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
