# pg_embedded_setup_unpriv user guide

The `pg_embedded_setup_unpriv` binary prepares a PostgreSQL installation and
data directory in environments where the controlling process must start as
`root`, then drop to an unprivileged user (`nobody`) before handing control to
tests or application code. This guide explains how to configure the tool and
integrate it into automated test flows.

## Prerequisites

- Linux host, VM, or container with `root` access.
- Rust toolchain specified in `rust-toolchain.toml`.
- Outbound network access to crates.io and the PostgreSQL binary archive.
- System timezone database (package usually named `tzdata`).

## Quick start

1. Choose directories for the staged PostgreSQL distribution and the cluster's
   data files. They must be writable by `root`; the tool will reassign
   ownership to `nobody`.
2. Export configuration:

   ```bash
   export PG_VERSION_REQ="=16.4.0"
   export PG_RUNTIME_DIR="/var/tmp/pg-embedded-setup-it/install"
   export PG_DATA_DIR="/var/tmp/pg-embedded-setup-it/data"
   export PG_SUPERUSER="postgres"
   export PG_PASSWORD="postgres_pass"
   ```

3. Run the helper as `root` (for example, `sudo -E cargo run --release --bin
   pg_embedded_setup_unpriv`). The command downloads the specified PostgreSQL
   release, ensures the directories exist, and initialises the cluster with the
   provided credentials.

4. Pass the resulting paths and credentials to your tests. If you use
   `postgresql_embedded` directly after the setup step, it can reuse the staged
   binaries and data directory without needing `root`.

## Integrating with root-only test agents

When authoring end-to-end tests that exercise PostgreSQL while the harness is
still running as `root`, follow these steps:

- Invoke `pg_embedded_setup_unpriv` before dropping privileges. This prepares
  file ownership, caches the binaries, and records the superuser password in a
  location accessible to `nobody`.
- Inside the test, temporarily adopt the `nobody` UID (for example,
  `pg_embedded_setup_unpriv::with_temp_euid`) prior to starting the database.
- Ensure the `PGPASSFILE` environment variable points to the file created in
  the runtime directory so subsequent Diesel or libpq connections can
  authenticate without interactive prompts.
- Provide `TZDIR=/usr/share/zoneinfo` (or the correct path for your
  distribution) to guarantee PostgreSQL can resolve the `TimeZone` setting.

## Known issues and mitigations

- **TimeZone errors**: The embedded cluster loads timezone data from the host
  `tzdata` package. Install it inside the execution environment if you see
  `invalid value for parameter "TimeZone": "UTC"`.
- **Download rate limits**: `postgresql_embedded` fetches binaries from the
  Theseus GitHub releases. Supply a `GITHUB_TOKEN` environment variable if you
  hit rate limits in CI.

## Further reading

- `README.md` – overview, configuration reference, and troubleshooting tips.
- `tests/e2e_postgresql_embedded_diesel.rs` – example of combining the helper
  with Diesel-based integration tests while running under `root`.
