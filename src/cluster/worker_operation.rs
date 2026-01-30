//! Worker operation identifiers for lifecycle orchestration.

use std::time::Duration;

use crate::TestBootstrapSettings;

/// Identifies worker lifecycle operations executed via the helper binary.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum WorkerOperation {
    Setup,
    Start,
    Stop,
    Cleanup,
    CleanupFull,
}

impl WorkerOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Cleanup => "cleanup",
            Self::CleanupFull => "cleanup-full",
        }
    }

    #[must_use]
    pub const fn error_context(self) -> &'static str {
        match self {
            Self::Setup => "postgresql_embedded::setup() failed",
            Self::Start => "postgresql_embedded::start() failed",
            Self::Stop => "postgresql_embedded::stop() failed",
            Self::Cleanup => "pg_worker cleanup operation failed",
            Self::CleanupFull => "pg_worker cleanup-full operation failed",
        }
    }

    #[must_use]
    pub const fn timeout(self, bootstrap: &TestBootstrapSettings) -> Duration {
        match self {
            Self::Setup => bootstrap.setup_timeout,
            Self::Start => bootstrap.start_timeout,
            Self::Stop | Self::Cleanup | Self::CleanupFull => bootstrap.shutdown_timeout,
        }
    }
}
