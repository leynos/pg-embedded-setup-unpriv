# pg_embedded_setup_unpriv

`pg_embedded_setup_unpriv` prepares a `postgresql_embedded` data directory
while you are running as `root`, dropping privileges to `nobody` only for the
filesystem mutations that must occur as the target user. The binary is useful
when you need to initialize PostgreSQL assets inside build pipelines or CI
images where direct access to the `nobody` account is required.

## Prerequisites

- Linux host with `sudo` or direct `root` access.
- Rust toolchain (matching `rust-toolchain.toml`) and `cargo` installed.
- Ability to download crates from crates.io.

## Configuration model

Configuration is discovered via environment variables, files, and CLI arguments
thanks to `ortho_config`. All fields are optional; when omitted the defaults
from `postgresql_embedded::Settings::default()` are used.

- `PG_VERSION_REQ` – SemVer requirement such as `^17` or `=16.4.0`.
- `PG_PORT` – TCP port for the server to listen on. Defaults to `5432`.
- `PG_SUPERUSER` – Administrator account name.
- `PG_PASSWORD` – Password for the superuser.
- `PG_DATA_DIR` – Directory where PostgreSQL data files should live.
- `PG_RUNTIME_DIR` – Directory for the downloaded distribution and binaries.
- `PG_LOCALE` – Locale passed to `initdb`.
- `PG_ENCODING` – Cluster encoding (for example `UTF8`).
- `PG_SHUTDOWN_TIMEOUT_SECS` – Optional number of seconds to wait for
  PostgreSQL to stop during teardown. Defaults to `15` seconds and accepts
  values between `1` and `600`.

You may also provide these values through a configuration file named
`pg.toml`, `pg.yaml`, or `pg.json5` (depending on enabled features) located in
any path recognised by `ortho_config`, or through CLI flags if you wrap the
binary inside your own launcher.

## Root usage and worker binary

When running as `root`, the library requires a privileged worker binary
(`pg_worker`) to execute PostgreSQL operations safely while dropping privileges
to `nobody`.

### Installation

```bash
cargo install --path .
```

This installs both `pg_embedded_setup_unpriv` and `pg_worker`. Verify
installation:

```bash
which pg_embedded_setup_unpriv pg_worker
```

### Worker discovery

The library automatically discovers `pg_worker` from PATH. For custom workers,
set the `PG_EMBEDDED_WORKER` environment variable:

```bash
export PG_EMBEDDED_WORKER=/path/to/custom/worker
```

### Common issues

- **Worker not found**: Ensure `pg_worker` is installed and in PATH.
- **Permission errors**: Verify the binary is executable (`chmod +x`).

Unprivileged users do not need to install the worker binary.

## Running the setup helper

1. Ensure the desired directories exist or can be created. They will be owned
   by `nobody` after the tool completes.
2. Export any configuration overrides, for example:

   ```bash
   export PG_VERSION_REQ="^17"
   export PG_DATA_DIR="/var/lib/postgres/data"
   export PG_RUNTIME_DIR="/var/lib/postgres/runtime"
   ```

3. Execute the binary as `root` so it can chown the directories before dropping
   privileges:

   ```bash
   sudo -E cargo run --release --bin pg_embedded_setup_unpriv
   ```

   The `-E` flag preserves any exported `PG_*` variables for the run.

4. On success the command exits with status `0`. The PostgreSQL payload is
   downloaded into `PG_RUNTIME_DIR`, initialized into `PG_DATA_DIR`, and both
   paths are owned by `nobody`. Any failure emits a structured error via
   `color-eyre` to standard error and the process exits with status `1`.

## Troubleshooting

- **Must be run as root** – The helper aborts when the effective UID is not
  `0`. Re-run the command using `sudo` or inside a `root` shell.
- **Directory permission errors** – Confirm the paths specified in
  `PG_DATA_DIR` and `PG_RUNTIME_DIR` are writable by `root` so ownership can be
  transferred to `nobody`.
- **PostgreSQL download issues** – Ensure outbound network access is available
  to fetch the PostgreSQL distribution used by `postgresql_embedded`.
- **Invalid `TimeZone` parameter** – The embedded cluster requires access to
  the system timezone database. Install your distribution's `tzdata` (or
  equivalent) package inside the container or VM running the tool.

## Integration testing with `rstest`

The crate ships an `rstest` fixture, `test_support::test_cluster`, so test
modules can request a ready `TestCluster` without invoking constructors
manually. Bring the fixture into scope and declare a parameter named
`test_cluster` to opt into automatic setup and teardown.

```rust,no_run
use pg_embedded_setup_unpriv::{test_support::test_cluster, TestCluster};
use rstest::rstest;

#[rstest]
fn migrates_schema(test_cluster: TestCluster) {
    let url = test_cluster.connection().database_url("postgres");
    assert!(url.starts_with("postgresql://"));
}
```

Because the fixture handles environment preparation, tests stay declarative and
can focus on behaviours instead of bootstrap plumbing. When a bootstrap failure
occurs the fixture panics with a `SKIP-TEST-CLUSTER` prefix, so higher-level
behaviour tests can convert known transient errors into soft skips.

## Fast test isolation with template databases

For test suites where per-test cluster bootstrap is too slow, use
`shared_test_cluster` to share a single cluster across all tests and clone
template databases for isolation:

```rust,no_run
use pg_embedded_setup_unpriv::{test_support::shared_test_cluster, TestCluster};
use rstest::rstest;

#[rstest]
fn fast_isolated_test(shared_test_cluster: &'static TestCluster) {
    // Clone a pre-migrated template (milliseconds, not seconds)
    shared_test_cluster
        .create_database_from_template("my_test_db", "migrated_template")
        .unwrap();

    let url = shared_test_cluster.connection().database_url("my_test_db");
    // ... run test queries ...

    shared_test_cluster.drop_database("my_test_db").unwrap();
}
```

Key features:

- **`shared_cluster()`** – process-global cluster initialized once per binary
- **`create_database_from_template()`** – clone databases via PostgreSQL's
  `TEMPLATE` clause (filesystem copy, completes in milliseconds)
- **`ensure_template_exists()`** – concurrency-safe template creation with
  per-template locking
- **`hash_directory()`** – generate content-based template names that
  auto-invalidate when migrations change

See [`docs/users-guide.md`](docs/users-guide.md) for complete examples and
performance guidance.

## Behaviour-driven diagnostics

Behavioural coverage relies on `rstest-bdd` (Behaviour-Driven Development,
BDD), which bundles Fluent localization files. The test suite includes
`tests/localized_diagnostics.rs`, a Dutch Gherkin scenario that switches
diagnostics to French via `rstest_bdd::select_localizations` and fails if the
embedded assets are missing. Run `make test` (or the focused
`cargo test localized_diagnostics`) in CI to ensure every target platform loads
the lazy localization payload correctly.
## Next steps

After the bootstrap completes you can start PostgreSQL with
`postgresql_embedded` (or another supervisor of your choice) using the same
directories and superuser credentials established by the helper.
