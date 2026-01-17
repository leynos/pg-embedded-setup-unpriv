//! Unit tests for database lifecycle helpers.

use rstest::rstest;

use super::*;

#[rstest]
#[case::with_bootstrap_error(
    Some("failed to connect to test database"),
    "TestCluster was not created: failed to connect to test database"
)]
#[case::without_bootstrap_error(None, "TestCluster was not created")]
fn cluster_not_created_error_formats_message(
    #[case] bootstrap_error: Option<&str>,
    #[case] expected: &str,
) {
    let msg = format_cluster_not_created_error(bootstrap_error);
    assert_eq!(msg, expected);
}

#[rstest]
#[case::with_bootstrap_error(
    Some("bootstrap failed: missing worker".to_owned()),
    "TestCluster was not created: bootstrap failed: missing worker"
)]
#[case::without_bootstrap_error(None, "TestCluster was not created")]
fn cluster_error_includes_bootstrap_context(
    #[case] bootstrap_error: Option<String>,
    #[case] expected: &str,
) {
    let mut world = DatabaseWorld::new().expect("create world");
    world.bootstrap_error = bootstrap_error;
    world.cluster = None;

    let result = world.cluster();
    let err = result.expect_err("cluster() should return error when cluster is None");
    let msg = err.to_string();

    assert_eq!(msg, expected);
}
