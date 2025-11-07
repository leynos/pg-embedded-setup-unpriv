# pg_embedded_setup_unpriv

`pg_embedded_setup_unpriv` prepares a `postgresql_embedded` data directory
while you are running as `root`, dropping privileges to `nobody` only for the
filesystem mutations that must occur as the target user. The binary is useful
when you need to initialise PostgreSQL assets inside build pipelines or CI
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

You may also provide these values through a configuration file named `pg.toml`,
`pg.yaml`, or `pg.json5` (depending on enabled features) located in any path
recognised by `ortho_config`, or through CLI flags if you wrap the binary
inside your own launcher.

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
   downloaded into `PG_RUNTIME_DIR`, initialised into `PG_DATA_DIR`, and both
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

## Next steps

After the bootstrap completes you can start PostgreSQL with
`postgresql_embedded` (or another supervisor of your choice) using the same
directories and superuser credentials established by the helper.

## Testing with `rstest`

The crate exposes an `rstest` fixture named
`pg_embedded_setup_unpriv::test_support::test_cluster`. The helper boots a
`TestCluster`, applies the environment discovered by `bootstrap_for_tests()`,
and tears everything down automatically when the test ends. The fixture ships
as part of the default `rstest-fixtures` feature so downstream crates only need
to add their own `rstest = "0.18"` dev-dependency.

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;
use pg_embedded_setup_unpriv::test_support::test_cluster;
use rstest::rstest;

#[rstest]
fn exercises_queries(test_cluster: TestCluster) -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    let url = test_cluster.connection().database_url("postgres");
    assert!(url.starts_with("postgresql://"));
    Ok(())
}
```

The fixture makes integration tests declarative: list `test_cluster` as a
parameter, run queries against the returned database, and let RAII handle
cleanup even on hosts that require root-to-`nobody` privilege demotion.
