//! Cluster startup lifecycle helpers.
//!
//! Provides shared logging functions for the sync and async cluster startup
//! paths, ensuring consistent observability across both execution modes.

use tracing::info;

use crate::observability::LOG_TARGET;
use crate::{ExecutionPrivileges, TestBootstrapSettings};

/// Logs the start of the embedded postgres lifecycle.
pub(super) fn log_lifecycle_start(
    privileges: ExecutionPrivileges,
    bootstrap: &TestBootstrapSettings,
    async_mode: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        mode = ?bootstrap.execution_mode,
        async_mode,
        "starting embedded postgres lifecycle"
    );
}

/// Logs the successful completion of the embedded postgres startup.
pub(super) fn log_lifecycle_complete(
    privileges: ExecutionPrivileges,
    is_managed_via_worker: bool,
    async_mode: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        worker_managed = is_managed_via_worker,
        async_mode,
        "embedded postgres started"
    );
}
