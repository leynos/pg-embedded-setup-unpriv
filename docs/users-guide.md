# pg_embedded_setup_unpriv user guide

The `pg_embedded_setup_unpriv` binary prepares a PostgreSQL installation and
data directory regardless of whether it starts with `root` privileges. When the
process runs as `root` it stages directories for `nobody` and delegates
PostgreSQL lifecycle commands to the worker helper, which executes as the
sandbox user. Unprivileged invocations keep the current identity and provision
directories with the caller’s UID. This guide explains how to configure the
tool and integrate it into automated test flows.

## Prerequisites

- Linux host, VM, or container. `root` access enables the privilege-dropping
  path, but unprivileged executions are also supported.
- Rust toolchain specified in `rust-toolchain.toml`.
- Outbound network access to crates.io and the PostgreSQL binary archive.
- System timezone database (package usually named `tzdata`).

## Platform expectations

- Linux supports both privilege branches. Root executions use `pg_worker` to
  drop to `nobody` for filesystem work. The library discovers the worker
  automatically if it is in `PATH`, or `PG_EMBEDDED_WORKER` can be set
  explicitly.
- macOS runs the unprivileged path; root executions are expected to fail fast
  because privilege dropping is not supported on that target.
- Windows always behaves as unprivileged, so the helper runs in-process and
  ignores root-only scenarios.

## Quick start

1. Choose directories for the staged PostgreSQL distribution and the cluster’s
   data files. They must be writable by whichever user will run the helper; the
   tool reapplies ownership and permissions on every invocation.

2. Export configuration:

   ```bash
   export PG_VERSION_REQ="=16.4.0"
   export PG_RUNTIME_DIR="/var/tmp/pg-embedded-setup-it/install"
   export PG_DATA_DIR="/var/tmp/pg-embedded-setup-it/data"
   export PG_SUPERUSER="postgres"
   export PG_PASSWORD="postgres_pass"
   ```

   Optionally set `PG_SHUTDOWN_TIMEOUT_SECS` to override the 15-second drop
   budget. The helper accepts values between `1` and `600` seconds and reports
   an error when the override falls outside that range or cannot be parsed.

3. Run the helper (`cargo run --release --bin pg_embedded_setup_unpriv`). The
   command downloads the specified PostgreSQL release, ensures the directories
   exist, applies PostgreSQL-compatible permissions (0755 for the installation
   cache, 0700 for the runtime and data directories), and initialises the
   cluster with the provided credentials. Invocations that begin as `root`
   prepare directories for `nobody` and execute lifecycle commands through the
   worker helper so the privileged operations run entirely under the sandbox
   user. Ownership fix-ups occur on every call so running the tool twice
   remains idempotent.

4. Pass the resulting paths and credentials to your tests. If you use
   `postgresql_embedded` directly after the setup step, it can reuse the staged
   binaries and data directory without needing `root`.

## Bootstrap for test suites

Invoke `pg_embedded_setup_unpriv::bootstrap_for_tests()` in integration suites
when both the prepared filesystem layout and the resulting settings are needed.
The helper performs the same orchestration as the CLI entry point but returns a
`TestBootstrapSettings` struct containing the final
`postgresql_embedded::Settings` and the environment variables required to
exercise the cluster.

```rust
use pg_embedded_setup_unpriv::{bootstrap_for_tests, TestBootstrapSettings};
use pg_embedded_setup_unpriv::error::BootstrapResult;

fn bootstrap() -> BootstrapResult<TestBootstrapSettings> {
    let prepared = bootstrap_for_tests()?;
    for (key, value) in prepared.environment.to_env() {
        match value {
            Some(value) => std::env::set_var(&key, value),
            None => std::env::remove_var(&key),
        }
    }
    Ok(prepared)
}
```

`bootstrap_for_tests()` ensures that `PGPASSFILE`, `HOME`, `XDG_CACHE_HOME`,
`XDG_RUNTIME_DIR`, and `TZ` are populated with deterministic defaults. When a
timezone database can be discovered (currently on Unix-like hosts) the helper
also sets `TZDIR`; otherwise it leaves any caller-provided value untouched so
platform-specific defaults remain available. If the system timezone database is
missing, the helper returns an error advising the caller to install `tzdata` or
set `TZDIR` explicitly, making the dependency visible during test startup
rather than when PostgreSQL launches.

## Resource Acquisition Is Initialization (RAII) test clusters

`pg_embedded_setup_unpriv::TestCluster` wraps `bootstrap_for_tests()` with a
Resource Acquisition Is Initialization (RAII) lifecycle. Constructing the guard
starts PostgreSQL using the discovered settings, applies the environment
produced by the bootstrap helper, and exposes the configuration to callers.
Dropping the guard stops the instance and restores the prior process
environment, so subsequent tests start from a clean slate.

```rust,no_run
use pg_embedded_setup_unpriv::{TestCluster, error::BootstrapResult};

fn exercise_cluster() -> BootstrapResult<()> {
    let cluster = TestCluster::new()?;
    let url = cluster.settings().url("app_db");
    // Issue queries using any preferred client here.
    drop(cluster); // PostgreSQL shuts down automatically.
    Ok(())
}
```

The guard keeps `PGPASSFILE`, `TZ`, `TZDIR`, and the XDG directories populated
for the duration of its lifetime, making synchronous tests usable without extra
setup.

### Async API for `#[tokio::test]` contexts

Tests within an async runtime (e.g. `#[tokio::test]`) must not use the standard
`TestCluster::new()` constructor, which panics with "Cannot start a runtime
from within a runtime" because it creates its own internal Tokio runtime. Async
contexts require enabling the `async-api` feature and using the async
constructor and shutdown methods.

Enable the feature in your `Cargo.toml`:

```toml
[dev-dependencies]
pg-embed-setup-unpriv = { version = "0.2", features = ["async-api"] }
```

Then use `start_async()` and `stop_async()` in your async tests:

```rust,no_run
use pg_embedded_setup_unpriv::{TestCluster, error::BootstrapResult};

#[tokio::test]
async fn test_async_database_operations() -> BootstrapResult<()> {
    let cluster = TestCluster::start_async().await?;

    // Access connection metadata as usual.
    let url = cluster.connection().database_url("app_db");
    // Issue async queries using sqlx or other async clients here.

    // Explicitly shut down to ensure clean resource release.
    cluster.stop_async().await?;
    Ok(())
}
```

Async clusters behave like the synchronous guard: the same accessors apply, and
the environment overrides are restored on shutdown. `stop_async()` consumes the
guard, so capture any required connection details before calling it.

**Important:** `stop_async()` must be called explicitly before the cluster goes
out of scope. Unlike the synchronous API where `Drop` can reliably shut down
PostgreSQL using its internal runtime, async-created clusters cannot guarantee
cleanup in `Drop` because `Drop` cannot be async. When `stop_async()` is not
called, the library will attempt best-effort cleanup and log a warning; if no
async runtime handle is available (for example, after the runtime has shut
down), resources may leak and the process may need to be stopped manually.

The async API runs PostgreSQL lifecycle operations on the caller's runtime
rather than creating a separate one, avoiding the nested-runtime panic whilst
maintaining the same zero-configuration experience as the synchronous API. When
running as `root`, the async API still delegates to the worker helper, and
those operations are executed with `spawn_blocking` so they do not block the
async executor.

## Observability

Set `RUST_LOG=pg_embed::observability=info` to emit tracing spans that describe
privilege drops, directory ownership or permission updates, scoped environment
application, and the `postgresql_embedded` setup/start/stop lifecycle. The log
target keeps sensitive values redacted: environment changes are rendered as
`KEY=set` or `KEY=unset`, and PostgreSQL settings avoid echoing passwords.
Enable `RUST_LOG=pg_embed::observability=debug` to surface a sanitized snapshot
of the prepared settings, including the version requirement, host and port,
installation and data directories, and the `.pgpass` location. Passwords log as
`<redacted>` and configuration entries are reduced to their keys, so secrets
stay out of the debug stream, even when bootstrap fails early. Subscribers that
record span enter/exit events, for example via `FmtSpan::ENTER|CLOSE`, can
reconstruct the lifecycle flow without needing additional instrumentation in
downstream crates.

Environment change summaries are truncated once they exceed roughly 512
characters, while the change count is always recorded. Lifecycle failures now
emit at `error` level, so log streams can distinguish genuine errors from the
normal informational lifecycle noise.

### Using the `rstest` fixture

`pg_embedded_setup_unpriv::test_support::test_cluster` exposes an `rstest`
fixture that constructs the RAII guard on demand. Import the fixture so it is
in scope and declare a `test_cluster: TestCluster` parameter inside an
`#[rstest]` function; the macro injects the running cluster automatically. The
`test_cluster` and `shared_test_cluster` fixtures are synchronous and
constructed via `TestCluster::new()`, so they must not be used in async tests;
`TestCluster::start_async()` should be used for async tests.

```rust,no_run
use pg_embedded_setup_unpriv::{test_support::test_cluster, TestCluster};
use rstest::rstest;

#[rstest]
fn runs_migrations(test_cluster: TestCluster) {
    let metadata = test_cluster.connection().metadata();
    assert!(metadata.port() > 0);
}
```

The fixture integrates with `rstest-bdd`, a Behaviour-Driven Development (BDD)
crate, so behaviour tests can remain declarative as well:

```rust,no_run
use pg_embedded_setup_unpriv::{test_support::test_cluster, TestCluster};
use rstest_bdd_macros::scenario;

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 0)]
fn coverage(test_cluster: TestCluster) {
    let _ = test_cluster.environment();
}
```

If PostgreSQL cannot start, the fixture panics with a
`SKIP-TEST-CLUSTER`-prefixed message that retains the original error. Unit
tests fail immediately, while behaviour tests can convert known transient
conditions into soft skips via the shared `skip_message` helper.

### Shared cluster fixture for fast test isolation

When test execution time is critical, use the `shared_test_cluster` fixture
instead of `test_cluster`. The shared fixture initializes a single `PostgreSQL`
cluster on first access and reuses it across all tests in the same binary,
eliminating per-test bootstrap overhead.

```rust,no_run
use pg_embedded_setup_unpriv::{test_support::shared_test_cluster, TestCluster};
use rstest::rstest;

#[rstest]
fn uses_shared_cluster(shared_test_cluster: &'static TestCluster) {
    // Create a per-test database for isolation
    shared_test_cluster.create_database("my_test_db").unwrap();

    // Run tests against the database
    let url = shared_test_cluster.connection().database_url("my_test_db");
    assert!(url.contains("my_test_db"));

    // Clean up (optional - the database is dropped when the cluster shuts down)
    shared_test_cluster.drop_database("my_test_db").unwrap();
}
```

For programmatic access without `rstest`, use `shared_cluster()` directly:

```rust,no_run
use pg_embedded_setup_unpriv::test_support::shared_cluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = shared_cluster()?;

// Multiple calls return the same instance
let cluster2 = shared_cluster()?;
assert!(std::ptr::eq(cluster, cluster2));
# Ok(())
# }
```

**When to use each fixture:**

| Fixture               | Use case                                          |
| --------------------- | ------------------------------------------------- |
| `test_cluster`        | Tests that modify cluster-level settings or state |
| `shared_test_cluster` | Tests that only need database-level isolation     |

The shared cluster is particularly effective when combined with template
databases (see "Database lifecycle management" below) to reduce per-test
overhead from seconds to milliseconds.

### Connection helpers and Diesel integration

`TestCluster::connection()` exposes `TestClusterConnection`, a lightweight view
over the running cluster's connection metadata. Use it to read the host, port,
superuser name, generated password, or the `.pgpass` path without cloning the
entire bootstrap struct. When you need to persist those values beyond the guard
you can call `metadata()` to obtain an owned `ConnectionMetadata`.

Enable the `diesel-support` feature to call `diesel_connection()` and obtain a
ready-to-use `diesel::PgConnection`. The default feature set keeps Diesel
optional for consumers.

```rust,no_run
use diesel::prelude::*;
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;
let connection = cluster.connection();
let url = connection.database_url("postgres");
assert!(url.starts_with("postgresql://"));

#[cfg(feature = "diesel-support")]
{
    let mut diesel_conn = connection.diesel_connection("postgres")?;
    #[derive(QueryableByName)]
    struct ValueRow {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        value: i32,
    }

    let rows: Vec<ValueRow> = diesel::sql_query("SELECT 1 AS value")
        .load(&mut diesel_conn)?;
    assert_eq!(rows[0].value, 1);
}
# Ok(())
# }
```

### Database lifecycle management

`TestClusterConnection` provides methods for programmatically creating and
dropping databases on the running cluster. These are useful for test isolation
patterns where each test creates its own database to avoid cross-test
interference.

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;
let conn = cluster.connection();

// Create a new database
conn.create_database("my_test_db")?;

// Check if a database exists
assert!(conn.database_exists("my_test_db")?);
assert!(conn.database_exists("postgres")?); // Built-in database

// Drop the database when done
conn.drop_database("my_test_db")?;
assert!(!conn.database_exists("my_test_db")?);
# Ok(())
# }
```

The `TestCluster` type also exposes convenience wrappers that delegate to the
connection methods:

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;

// These delegate to cluster.connection().create_database(...) etc.
cluster.create_database("my_test_db")?;
assert!(cluster.database_exists("my_test_db")?);
cluster.drop_database("my_test_db")?;
# Ok(())
# }
```

All methods connect to the `postgres` database as the superuser to execute the
Data Definition Language (DDL) statements. Errors are returned when:

- Creating a database that already exists
- Dropping a database that does not exist
- Dropping a database with active connections
- Connection to the cluster fails

### Template databases for fast test isolation

PostgreSQL's `CREATE DATABASE … TEMPLATE` mechanism clones an existing database
via a filesystem-level copy, completing in milliseconds regardless of schema
complexity. This is significantly faster than running migrations on each test
database.

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;

// Create a template database and apply migrations
cluster.create_database("my_template")?;
// ... run migrations on my_template ...

// Clone the template for each test (milliseconds vs seconds)
cluster.create_database_from_template("test_db_1", "my_template")?;
cluster.create_database_from_template("test_db_2", "my_template")?;
# Ok(())
# }
```

Template helpers live on `TestClusterConnection` and are also exposed on
`TestCluster` for convenience. Use unique database names (for example,
`format!("test_{}", uuid::Uuid::new_v4())`) to avoid collisions under parallel
execution.

The `ensure_template_exists` method provides concurrency-safe template creation
with per-template locking to prevent race conditions when multiple tests try to
initialize the same template simultaneously:

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;

// Only creates and migrates if the template doesn't exist
cluster.ensure_template_exists("migrated_template", |db_name| {
    // Run migrations on the newly created database
    // e.g., diesel::migration::run(&mut conn)?;
    Ok(())
})?;

// Clone for the test
cluster.create_database_from_template("test_db", "migrated_template")?;
# Ok(())
# }
```

For versioned template names that automatically invalidate when migrations
change, use the `hash_directory` helper to generate a content-based hash:

```rust,no_run
use pg_embedded_setup_unpriv::test_support::hash_directory;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let hash = hash_directory("migrations")?;
let template_name = format!("template_{}", &hash[..8]);
// Template name changes when any migration file changes
# Ok(())
# }
```

If you already track a migration version, include it in the template name
instead (for example, `format!("template_v{SCHEMA_VERSION}")`). This keeps
template invalidation explicit without hashing the migration directory.

### Performance comparison

The following table compares test isolation approaches:

| Approach                       | Bootstrap | Per-test overhead | Isolation |
| ------------------------------ | --------- | ----------------- | --------- |
| Per-test `TestCluster`         | Per test  | 20–30 seconds     | Full      |
| Shared cluster, fresh database | Once      | 1–5 seconds       | Database  |
| Shared cluster, template clone | Once      | 10–50 ms          | Database  |

**When to use each approach:**

- **Per-test cluster (`test_cluster` fixture):** Use when tests modify
  cluster-level settings, require specific PostgreSQL versions, or need
  complete isolation from other tests.
- **Shared cluster with fresh databases:** Use when tests need database-level
  isolation but can share the same cluster. Suitable when migration overhead is
  acceptable.
- **Shared cluster with template cloning (`shared_test_cluster` fixture):** Use
  for maximum performance when tests only need database-level isolation.
  Requires upfront template creation, but reduces per-test overhead by orders
  of magnitude.

### Database cleanup strategies

When using a shared cluster, databases created during tests persist until
explicitly dropped or the cluster shuts down. Consider these strategies:

**Explicit cleanup:** Drop databases after each test to reclaim disk space and
prevent name collisions:

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;
let db_name = format!("test_{}", uuid::Uuid::new_v4());
cluster.create_database_from_template(&db_name, "my_template")?;

// ... run test ...

cluster.drop_database(&db_name)?; // Explicit cleanup
# Ok(())
# }
```

**Cluster teardown cleanup:** Let the shared cluster drop all databases when
the test binary exits. This is simpler but uses more disk space during the test
run:

```rust,no_run
use pg_embedded_setup_unpriv::test_support::shared_cluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = shared_cluster()?;
let db_name = format!("test_{}", uuid::Uuid::new_v4());
cluster.create_database_from_template(&db_name, "my_template")?;

// ... run test ...
// Database dropped automatically when cluster shuts down
# Ok(())
# }
```

**Active connection handling:** Dropping a database with active connections
fails. Ensure all connections are closed before calling `drop_database`. If
using connection pools, drain the pool first.

### Automatic cleanup with TemporaryDatabase

The `TemporaryDatabase` guard provides RAII cleanup semantics. When the guard
goes out of scope, the database is automatically dropped:

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;

// Create a temporary database with automatic cleanup
let temp_db = cluster.temporary_database("my_test_db")?;

// Use the database
let url = temp_db.url();
// ... run queries ...

// Database is dropped automatically when temp_db goes out of scope
drop(temp_db);
# Ok(())
# }
```

For template-based workflows, use `temporary_database_from_template`:

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;

// Ensure the template exists
cluster.ensure_template_exists("migrated_template", |_| Ok(()))?;

// Create a temporary database from the template
let temp_db = cluster.temporary_database_from_template("test_db", "migrated_template")?;

// Database is automatically dropped when temp_db goes out of scope
# Ok(())
# }
```

**Drop behaviour:**

- `drop_database()` — Explicitly drop the database, failing if connections
  exist. Consumes the guard.
- `force_drop()` — Terminate active connections before dropping. Useful when
  connection pools haven't been drained.
- Implicit drop (guard goes out of scope) — Best-effort drop with a warning
  logged on failure.

## Privilege detection and idempotence

- `pg_embedded_setup_unpriv` detects its effective user ID at runtime. Root
  processes follow the privileged branch and complete all filesystem work as
  `nobody`; non-root invocations leave permissions untouched and keep the
  caller’s UID on the runtime directories.
- Both flows create the runtime directory with mode `0700` and the data
  directory with mode `0700`. Existing directories are re-chowned or re-mode’d
  to enforce the expected invariants, allowing two consecutive runs to succeed
  without manual cleanup.
- The XDG cache home stays `0755` so team-mates can inspect extracted binaries
  and logs when debugging CI issues. The runtime directory is clamped to `0700`
  because it holds the PostgreSQL socket, `postmaster.pid`, and `.pgpass`, so
  leaking read or execute access would expose credentials or let other users
  interfere with the helper’s cluster lifecycle.

## Integrating with root-only test agents

When authoring end-to-end tests that exercise PostgreSQL while the harness is
still running as `root`, follow these steps:

### Install the worker binary

Install both binaries via Cargo:

```bash
cargo install pg-embed-setup-unpriv
```

This installs `pg_embedded_setup_unpriv` (the main setup helper) and
`pg_worker` (the privilege-dropping worker). Ensure `~/.cargo/bin` (or your
Cargo install directory) is in `PATH`. The library will automatically discover
`pg_worker` when running as root.

Alternatively, set `PG_EMBEDDED_WORKER` to the absolute path of the worker
binary if it is installed elsewhere:

```bash
export PG_EMBEDDED_WORKER=/opt/custom/bin/pg_worker
```

### Configure the test environment

- Invoke `pg_embedded_setup_unpriv` before handing control to less-privileged
  workers. This prepares file ownership, caches the binaries, and records the
  superuser password in a location accessible to `nobody`.
- Keep the test process running as `root`; the helper binary demotes itself
  before calling into `postgresql_embedded` so the main process never changes
  UID mid-test.
- Ensure the `PGPASSFILE` environment variable points to the file created in the
  runtime directory so subsequent Diesel or libpq connections can authenticate
  without interactive prompts. The
  `bootstrap_for_tests().environment.pgpass_file` helper returns the path if
  the bootstrap ran inside the test process.
- Provide `TZDIR=/usr/share/zoneinfo` (or the correct path for your
  distribution) if you are running the CLI. The library helper sets `TZ`
  automatically and, on Unix-like hosts, also seeds `TZDIR` when it discovers a
  valid timezone database.

## Known issues and mitigations

- **TimeZone errors**: The embedded cluster loads timezone data from the host
  `tzdata` package. Install it inside the execution environment if you see
  `invalid value for parameter "TimeZone": "UTC"`.
- **Download rate limits**: `postgresql_embedded` fetches binaries from the
  Theseus GitHub releases. Supply a `GITHUB_TOKEN` environment variable if you
  hit rate limits in CI.
- **CLI arguments in tests**: `PgEnvCfg::load()` ignores `std::env::args` during
  library use so Cargo test filters (for example,
  `bootstrap_privileges::bootstrap_as_root`) do not trip the underlying Clap
  parser. Provide configuration through environment variables or config files
  when embedding the crate.
- **Legacy `with_temp_euid` helper**: The helper now returns an error because
  the library no longer mutates the process UID mid-test. Configure
  `PG_EMBEDDED_WORKER` instead so the subprocess performs the privilege drop.

## Further reading

- `README.md` – overview, configuration reference, and troubleshooting tips.
- `docs/developers-guide.md` – contributor notes and internal testing context.
