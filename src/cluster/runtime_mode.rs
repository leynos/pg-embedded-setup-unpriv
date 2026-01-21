//! Runtime mode encoding for `TestCluster`.

use tokio::runtime::Runtime;

/// Encodes the runtime mode for a `TestCluster`.
///
/// This enum eliminates the need for separate `runtime: Option<Runtime>` and
/// `is_async_mode: bool` fields, preventing invalid states where the two could
/// disagree.
#[derive(Debug)]
pub(super) enum ClusterRuntime {
    /// Synchronous mode: the cluster owns its own Tokio runtime.
    Sync(Runtime),
    /// Async mode: the cluster runs on the caller's runtime.
    #[cfg_attr(
        not(feature = "async-api"),
        expect(dead_code, reason = "used when async-api feature is enabled")
    )]
    Async,
}

impl ClusterRuntime {
    /// Returns `true` if this is async mode.
    pub(super) const fn is_async(&self) -> bool {
        matches!(self, Self::Async)
    }
}
