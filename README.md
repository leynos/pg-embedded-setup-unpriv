# pg_embedded_setup_unpriv

*Zero-configuration PostgreSQL test fixtures for Rust—whether you're root or
not.*

## Why pg_embedded_setup_unpriv?

Writing integration tests against PostgreSQL should be straightforward, but
environments vary: CI containers often run as root, developer laptops don't,
and everyone wants the same tests to pass everywhere. This crate makes that
happen:

- **Zero configuration**: Spin up a real PostgreSQL instance with a single
  line of code. No environment variables, config files, or manual setup
  required.
- **Works anywhere**: The same test code runs on root CI (Codex, containers)
  and unprivileged machines (GitHub Actions, your laptop)—the library detects
  privileges at runtime and adapts.
- **RAII lifecycle**: `TestCluster` starts PostgreSQL on construction and
  stops it on drop. No orphan processes, no cleanup code.
- **Fast test isolation**: Clone pre-migrated template databases in
  milliseconds instead of bootstrapping fresh clusters for each test.

## Quick start

### Installation

Add to your `Cargo.toml`:

```toml
[dev-dependencies]
pg-embed-setup-unpriv = "0.4"
rstest = "0.26"
```

### Basic usage

```rust
use pg_embedded_setup_unpriv::{test_support::test_cluster, TestCluster};
use rstest::rstest;

#[rstest]
fn test_my_database_logic(test_cluster: TestCluster) {
    let url = test_cluster.connection().database_url("postgres");
    // Connect with your preferred client and run queries
    assert!(url.starts_with("postgresql://"));
}
```

That's it. The `test_cluster` fixture handles downloading PostgreSQL, creating
directories, starting the server, and cleaning up when the test ends.

## Features

- **Automatic privilege detection**: Root executions delegate filesystem work
  to a `pg_worker` subprocess running as `nobody`; unprivileged executions run
  entirely in-process.
- **rstest integration**: Ready-made fixtures (`test_cluster`,
  `shared_test_cluster`) for declarative test setup.
- **Async support**: Use `TestCluster::start_async()` in `#[tokio::test]`
  contexts (requires the `async-api` feature).
- **Template databases**: Clone databases via PostgreSQL's `TEMPLATE`
  mechanism for sub-second test isolation.
- **Diesel support**: Optional `diesel-support` feature provides
  `diesel_connection()` for direct database access.
- **Observability**: Tracing spans for lifecycle events, with sensitive values
  automatically redacted.

## Learn more

- [Users' Guide](docs/users-guide.md) — full documentation, async API,
  template databases, and performance tuning
- [Developers' Guide](docs/developers-guide.md) — contributing and development
- [Roadmap](docs/roadmap.md) — planned features and progress

## Licence

ISC — see the licence in [LICENSE](LICENSE) for details.

## Contributing

Contributions welcome! Please see [AGENTS.md](AGENTS.md) for guidelines.
