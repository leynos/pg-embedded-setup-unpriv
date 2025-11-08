//! Cluster-specific skip helpers for integration tests.

use crate::skip::skip_message;

/// Canonical prefix for soft skip messages emitted by the `TestCluster` helpers.
const SKIP_TEST_CLUSTER_PREFIX: &str = "SKIP-TEST-CLUSTER";

/// Formats a skip message for `TestCluster` failures using the shared prefix.
pub(crate) fn cluster_skip_message(message: &str, debug: Option<&str>) -> Option<String> {
    skip_message(SKIP_TEST_CLUSTER_PREFIX, message, debug)
}
