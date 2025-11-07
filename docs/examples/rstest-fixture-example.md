# Canonical `rstest` Fixture Example

The `pg_embedded_setup_unpriv::test_support::test_cluster` fixture keeps a
`TestCluster` running for the duration of an `#[rstest]` case. Bring the
fixture into scope, declare a `test_cluster: TestCluster` argument, and the
macro wires everything up automatically.

```rust,no_run
use pg_embedded_setup_unpriv::{test_support::test_cluster, TestCluster};
use rstest::rstest;

#[rstest]
fn migrates_schema(test_cluster: TestCluster) {
    let url = test_cluster.connection().database_url("postgres");
    assert!(url.starts_with("postgresql://"));
}
```

When PostgreSQL fails to start, the fixture panics with the `SKIP-TEST-CLUSTER`
prefix so higher-level behaviour tests can translate known external failures
into soft skips.
