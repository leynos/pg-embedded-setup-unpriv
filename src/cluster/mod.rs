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

mod cache_integration;
mod connection;
mod delegation;
mod guard;
mod handle;
mod installation;
mod lifecycle;
mod runtime;
mod runtime_mode;
mod shutdown;
mod startup;
mod temporary_database;
mod worker_invoker;
mod worker_operation;

pub use self::connection::{ConnectionMetadata, TestClusterConnection};
pub use self::guard::ClusterGuard;
pub use self::handle::ClusterHandle;
pub use self::lifecycle::DatabaseName;
pub use self::temporary_database::TemporaryDatabase;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use self::worker_invoker::WorkerInvoker;
#[doc(hidden)]
pub use self::worker_operation::WorkerOperation;

use self::runtime::build_runtime;
use self::runtime_mode::ClusterRuntime;
#[cfg(feature = "async-api")]
use self::startup::start_postgres_async;
use self::startup::{cache_config_from_bootstrap, start_postgres};
use crate::bootstrap_for_tests;
use crate::env::ScopedEnv;
use crate::error::BootstrapResult;
use crate::observability::LOG_TARGET;
use std::ops::Deref;
use tracing::info_span;

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
///
/// `TestCluster` combines a [`ClusterHandle`] (for cluster access) with a
/// [`ClusterGuard`] (for lifecycle management). For most use cases, this
/// combined type is the simplest option.
///
/// # Send-Safe Patterns
///
/// `TestCluster` is `!Send` because it contains environment guards that must
/// be dropped on the creating thread. For patterns requiring `Send` (such as
/// `OnceLock` or rstest timeouts), use [`new_split()`](Self::new_split) to
/// obtain a `Send`-safe [`ClusterHandle`]:
///
/// ```no_run
/// use std::sync::OnceLock;
/// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
///
/// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
///
/// fn shared_cluster() -> &'static ClusterHandle {
///     SHARED.get_or_init(|| {
///         let (handle, _guard) = TestCluster::new_split()
///             .expect("cluster bootstrap failed");
///         handle
///     })
/// }
/// ```
#[derive(Debug)]
pub struct TestCluster {
    /// Send-safe handle providing cluster access.
    pub(crate) handle: ClusterHandle,
    /// Lifecycle guard managing shutdown and environment restoration.
    pub(crate) guard: ClusterGuard,
}

impl TestCluster {
    /// Boots a `PostgreSQL` instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    ///
    /// # Errors
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting the
    /// embedded cluster fails.
    pub fn new() -> BootstrapResult<Self> {
        let (handle, guard) = Self::new_split()?;
        Ok(Self { handle, guard })
    }

    /// Boots a `PostgreSQL` instance and returns a separate handle and guard.
    ///
    /// This constructor is useful for patterns requiring `Send`, such as shared
    /// cluster fixtures with [`OnceLock`](std::sync::OnceLock) or rstest fixtures
    /// with timeouts.
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// - [`ClusterHandle`]: `Send + Sync` handle for accessing the cluster
    /// - [`ClusterGuard`]: `!Send` guard managing shutdown and environment
    ///
    /// # Errors
    ///
    /// Returns an error if the bootstrap configuration cannot be prepared or if
    /// starting the embedded cluster fails.
    ///
    /// # Examples
    ///
    /// ## Shared Cluster with `OnceLock`
    ///
    /// ```no_run
    /// use std::sync::OnceLock;
    /// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
    ///
    /// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
    ///
    /// fn shared_cluster() -> &'static ClusterHandle {
    ///     SHARED.get_or_init(|| {
    ///         let (handle, _guard) = TestCluster::new_split()
    ///             .expect("cluster bootstrap failed");
    ///         // Guard drops, but cluster keeps running for the process lifetime
    ///         handle
    ///     })
    /// }
    /// ```
    pub fn new_split() -> BootstrapResult<(ClusterHandle, ClusterGuard)> {
        let span = info_span!(target: LOG_TARGET, "test_cluster");
        // Resolve cache directory BEFORE applying test environment.
        // Otherwise, the test sandbox's XDG_CACHE_HOME would be used.
        let (runtime, env_vars, env_guard, outcome) = {
            let _entered = span.enter();
            let initial_bootstrap = bootstrap_for_tests()?;
            let cache_config = cache_config_from_bootstrap(&initial_bootstrap);
            let runtime = build_runtime()?;
            let env_vars = initial_bootstrap.environment.to_env();
            let env_guard = ScopedEnv::apply(&env_vars);
            let outcome = start_postgres(&runtime, initial_bootstrap, &env_vars, &cache_config)?;
            (runtime, env_vars, env_guard, outcome)
        };

        let handle = ClusterHandle::new(outcome.bootstrap.clone());
        let guard = ClusterGuard {
            runtime: ClusterRuntime::Sync(runtime),
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        };

        Ok((handle, guard))
    }

    /// Boots a `PostgreSQL` instance asynchronously for use in `#[tokio::test]` contexts.
    ///
    /// Unlike [`TestCluster::new`], this constructor does not create its own Tokio runtime.
    /// Instead, it runs on the caller's async runtime, making it safe to call from within
    /// `#[tokio::test]` and other async contexts.
    ///
    /// **Important:** Clusters created with `start_async()` should be shut down explicitly
    /// using [`stop_async()`](Self::stop_async). The `Drop` implementation will attempt
    /// best-effort cleanup but may not succeed if the runtime is no longer available.
    ///
    /// # Errors
    ///
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting
    /// the embedded cluster fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// #[tokio::test]
    /// async fn test_with_embedded_postgres() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    ///     let cluster = TestCluster::start_async().await?;
    ///     let url = cluster.settings().url("my_database");
    ///     // ... async database operations ...
    ///     cluster.stop_async().await?;
    ///     Ok(())
    /// }
    /// ```
    #[cfg(feature = "async-api")]
    pub async fn start_async() -> BootstrapResult<Self> {
        let (handle, guard) = Self::start_async_split().await?;
        Ok(Self { handle, guard })
    }

    /// Boots a `PostgreSQL` instance asynchronously and returns a separate handle and guard.
    ///
    /// This is the async equivalent of [`new_split()`](Self::new_split).
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// - [`ClusterHandle`]: `Send + Sync` handle for accessing the cluster
    /// - [`ClusterGuard`]: `!Send` guard managing shutdown and environment
    ///
    /// # Errors
    ///
    /// Returns an error if the bootstrap configuration cannot be prepared or if
    /// starting the embedded cluster fails.
    #[cfg(feature = "async-api")]
    pub async fn start_async_split() -> BootstrapResult<(ClusterHandle, ClusterGuard)> {
        use tracing::Instrument;

        let span = info_span!(target: LOG_TARGET, "test_cluster", async_mode = true);

        // Sync bootstrap preparation (no await needed).
        // Resolve cache directory BEFORE applying test environment.
        // Otherwise, the test sandbox's XDG_CACHE_HOME would be used.
        let initial_bootstrap = bootstrap_for_tests()?;
        let cache_config = cache_config_from_bootstrap(&initial_bootstrap);
        let env_vars = initial_bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);

        // Async postgres startup, instrumented with the span.
        // Box::pin to avoid large future on the stack.
        let outcome = Box::pin(start_postgres_async(
            initial_bootstrap,
            &env_vars,
            &cache_config,
        ))
        .instrument(span.clone())
        .await?;

        let handle = ClusterHandle::new(outcome.bootstrap.clone());
        let guard = ClusterGuard {
            runtime: ClusterRuntime::Async,
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        };

        Ok((handle, guard))
    }

    /// Extends the cluster lifetime to cover additional scoped environment guards.
    ///
    /// Primarily used by fixtures that need to ensure `PG_EMBEDDED_WORKER` remains set for the
    /// duration of the cluster lifetime.
    #[doc(hidden)]
    #[must_use]
    pub fn with_worker_guard(self, worker_guard: Option<ScopedEnv>) -> Self {
        Self {
            handle: self.handle,
            guard: self.guard.with_worker_guard(worker_guard),
        }
    }

    /// Explicitly shuts down an async cluster.
    ///
    /// This method should be called for clusters created with [`start_async()`](Self::start_async)
    /// to ensure proper cleanup. It consumes `self` to prevent the `Drop` implementation from
    /// attempting duplicate shutdown.
    ///
    /// For worker-managed clusters (root privileges), the worker subprocess is invoked
    /// synchronously via `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown operation fails. The cluster resources are released
    /// regardless of whether shutdown succeeds.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// #[tokio::test]
    /// async fn test_explicit_shutdown() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    ///     let cluster = TestCluster::start_async().await?;
    ///     // ... use cluster ...
    ///     cluster.stop_async().await?;
    ///     Ok(())
    /// }
    /// ```
    #[cfg(feature = "async-api")]
    pub async fn stop_async(mut self) -> BootstrapResult<()> {
        let context = shutdown::stop_context(self.handle.settings());
        shutdown::log_async_stop(&context, self.guard.is_managed_via_worker);

        if self.guard.is_managed_via_worker {
            shutdown::stop_worker_managed_async(
                &self.guard.bootstrap,
                &self.guard.env_vars,
                &context,
            )
            .await
        } else if let Some(postgres) = self.guard.postgres.take() {
            shutdown::stop_in_process_async(
                postgres,
                self.guard.bootstrap.shutdown_timeout,
                &context,
            )
            .await
        } else {
            Ok(())
        }
    }
}

/// Provides transparent access to [`ClusterHandle`] methods.
///
/// This allows `TestCluster` to be used interchangeably with `ClusterHandle`
/// for all read-only operations like `settings()`, `connection()`, etc.
impl Deref for TestCluster {
    type Target = ClusterHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

// Note: TestCluster does NOT implement Drop because the ClusterGuard handles shutdown.
// When TestCluster drops, its _guard field drops, which triggers ClusterGuard::Drop.

#[cfg(test)]
mod mod_tests;

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod drop_logging_tests {
    use crate::test_support::capture_warn_logs;

    use super::shutdown;

    #[test]
    fn warn_stop_timeout_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| shutdown::warn_stop_timeout(5, "ctx"));
        assert!(
            logs.iter()
                .any(|line| line.contains("stop() timed out after 5s (ctx)")),
            "expected timeout warning, got {logs:?}"
        );
    }

    #[test]
    fn warn_stop_failure_emits_warning() {
        let (logs, ()) = capture_warn_logs(|| shutdown::warn_stop_failure("ctx", &"boom"));
        assert!(
            logs.iter()
                .any(|line| line.contains("failed to stop embedded postgres instance")),
            "expected failure warning, got {logs:?}"
        );
    }
}

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../../tests/test_cluster.rs"]
mod test_cluster_tests;
