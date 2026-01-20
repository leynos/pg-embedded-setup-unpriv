//! RAII wrapper that boots an embedded `PostgreSQL` instance for tests.
//!
//! The cluster starts during [`TestCluster::new`] and shuts down automatically when the
//! value drops out of scope.
//!
//! # Synchronous API
//!
//! Use [`TestCluster::new`] from synchronous contexts or when you want the cluster to
//! own its own Tokio runtime:
//!
//! ```no_run
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//! let cluster = TestCluster::new()?;
//! let url = cluster.settings().url("my_database");
//! // Perform test database work here.
//! drop(cluster); // `PostgreSQL` stops automatically.
//! # Ok(())
//! # }
//! ```
//!
//! # Async API
//!
//! When running within an existing async runtime (e.g., `#[tokio::test]`), use
//! [`TestCluster::start_async`] to avoid the "Cannot start a runtime from within a
//! runtime" panic that occurs when nesting Tokio runtimes:
//!
//! ```ignore
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! #[tokio::test]
//! async fn test_with_embedded_postgres() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//!     let cluster = TestCluster::start_async().await?;
//!     let url = cluster.settings().url("my_database");
//!     // ... async database operations ...
//!     cluster.stop_async().await?;
//!     Ok(())
//! }
//! ```
//!
//! The async API requires the `async-api` feature flag:
//!
//! ```toml
//! [dependencies]
//! pg-embedded-setup-unpriv = { version = "...", features = ["async-api"] }
//! ```

#[cfg(feature = "async-api")]
mod async_api;
mod connection;
mod delegation;
mod drop_handling;
mod installation;
mod lifecycle;
mod port_refresh;
mod runtime;
mod startup;
mod sync_api;
mod temporary_database;
mod worker_invoker;
mod worker_operation;

pub use self::connection::{ConnectionMetadata, TestClusterConnection};
pub use self::lifecycle::DatabaseName;
pub use self::temporary_database::TemporaryDatabase;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use self::worker_invoker::WorkerInvoker;
#[doc(hidden)]
pub use self::worker_operation::WorkerOperation;

use crate::TestBootstrapSettings;
use crate::env::ScopedEnv;
use postgresql_embedded::PostgreSQL;
use tokio::runtime::Runtime;

/// Encodes the runtime mode for a `TestCluster`.
///
/// This enum eliminates the need for separate `runtime: Option<Runtime>` and
/// `is_async_mode: bool` fields, preventing invalid states where the two could
/// disagree.
#[derive(Debug)]
enum ClusterRuntime {
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
    const fn is_async(&self) -> bool {
        matches!(self, Self::Async)
    }
}

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    /// Runtime mode: either owns a runtime (sync) or runs on caller's runtime (async).
    runtime: ClusterRuntime,
    postgres: Option<PostgreSQL>,
    bootstrap: TestBootstrapSettings,
    is_managed_via_worker: bool,
    env_vars: Vec<(String, Option<String>)>,
    worker_guard: Option<ScopedEnv>,
    _env_guard: ScopedEnv,
    // Keeps the cluster span alive for the lifetime of the guard.
    _cluster_span: tracing::Span,
}

struct StartupOutcome {
    bootstrap: TestBootstrapSettings,
    postgres: Option<PostgreSQL>,
    is_managed_via_worker: bool,
}

#[cfg(test)]
mod tests;
