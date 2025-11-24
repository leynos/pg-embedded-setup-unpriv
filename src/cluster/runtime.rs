//! Helpers for constructing Tokio runtimes used by `TestCluster`.

use crate::error::{BootstrapError, BootstrapResult};
use color_eyre::eyre::Context;
use tokio::runtime::{Builder, Runtime};

pub(crate) fn build_runtime() -> BootstrapResult<Runtime> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Tokio runtime for TestCluster")
        .map_err(BootstrapError::from)
}
