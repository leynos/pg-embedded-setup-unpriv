//! Rustdoc-only wrappers for rstest fixtures.
//!
//! These functions mirror the runtime fixtures but remain separate so the
//! `#[fixture]` macro (which expands into additional attributes) is never
//! applied to items with doc comments. This keeps Whitaker's
//! `function_attrs_follow_docs` lint happy while preserving documentation.

use crate::TestCluster;
use crate::test_support::fixtures as runtime_fixtures;

/// `rstest` fixture that yields a running [`TestCluster`].
///
/// The fixture blocks until `PostgreSQL` is ready, making it ideal for
/// integration tests that only need to declare a `cluster: TestCluster`
/// parameter without invoking [`TestCluster::new`] manually.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
/// use pg_embedded_setup_unpriv::test_support::test_cluster;
/// use rstest::rstest;
///
/// #[rstest]
/// fn exercises_database(test_cluster: TestCluster) {
///     let metadata = test_cluster.connection().metadata();
///     assert!(metadata.port() > 0);
/// }
/// ```
#[must_use]
pub fn test_cluster() -> TestCluster {
    runtime_fixtures::test_cluster()
}

/// `rstest` fixture that yields a reference to the shared [`TestCluster`].
///
/// This fixture provides access to a process-global cluster that is
/// initialised once and reused across all tests in the same binary. Use this
/// when tests can share a cluster and create per-test databases for isolation.
///
/// # Panics
///
/// Panics with a `SKIP-TEST-CLUSTER:`-prefixed message if the shared cluster
/// cannot be started. This allows test harnesses to detect and skip tests when
/// `PostgreSQL` is unavailable.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
/// use pg_embedded_setup_unpriv::test_support::shared_test_cluster;
/// use rstest::rstest;
///
/// #[rstest]
/// fn uses_shared_cluster(shared_test_cluster: &'static TestCluster) {
///     let metadata = shared_test_cluster.connection().metadata();
///     assert!(metadata.port() > 0);
/// }
/// ```
#[must_use]
pub fn shared_test_cluster() -> &'static TestCluster {
    runtime_fixtures::shared_test_cluster()
}
