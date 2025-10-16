# pg_embedded_setup_unpriv user guide

The `pg_embedded_setup_unpriv` binary prepares a PostgreSQL installation and
data directory regardless of whether it starts with `root` privileges. When the
process runs as `root` it automatically drops to `nobody` before invoking
`postgresql_embedded`; when launched as an unprivileged user it keeps the
current identity and provisions directories with the caller’s UID. This guide
explains how to configure the tool and integrate it into automated test flows.

## Prerequisites

- Linux host, VM, or container. `root` access enables the privilege-dropping
  path, but unprivileged executions are also supported.
- Rust toolchain specified in `rust-toolchain.toml`.
- Outbound network access to crates.io and the PostgreSQL binary archive.
- System timezone database (package usually named `tzdata`).

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

3. Run the helper (`cargo run --release --bin pg_embedded_setup_unpriv`). The
   command downloads the specified PostgreSQL release, ensures the directories
   exist, applies PostgreSQL-compatible permissions (0755 for runtime, 0700 for
   data), and initialises the cluster with the provided credentials.
   Invocations that begin as `root` drop to `nobody` before bootstrapping and
   repeat the ownership fix-ups on every call so running the tool twice remains
   idempotent.
4. Pass the resulting paths and credentials to your tests. If you use
   `postgresql_embedded` directly after the setup step, it can reuse the staged
   binaries and data directory without needing `root`.

## Privilege detection and idempotence

- `pg_embedded_setup_unpriv` detects its effective user ID at runtime. Root
  processes follow the privileged branch and complete all filesystem work as
  `nobody`; non-root invocations leave permissions untouched and keep the
  caller’s UID on the runtime directories.
- Both flows create the runtime directory with mode `0755` and the data
  directory with mode `0700`. Existing directories are re-chowned or re-mode’d
  to enforce the expected invariants, allowing two consecutive runs to succeed
  without manual cleanup.
- Behavioural tests driven by `rstest-bdd` exercise both branches to guard
  against regressions in privilege detection or ownership management.

## Integrating with root-only test agents

When authoring end-to-end tests that exercise PostgreSQL while the harness is
still running as `root`, follow these steps:

- Invoke `pg_embedded_setup_unpriv` before dropping privileges. This prepares
  file ownership, caches the binaries, and records the superuser password in a
  location accessible to `nobody`.
- Enable the `privileged-tests` Cargo feature when depending on the library so
  the `with_temp_euid` helper is available to orchestrate privilege changes in
  end-to-end suites.
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
- **CLI arguments in tests**: `PgEnvCfg::load()` ignores `std::env::args`
  during library use so Cargo test filters (for example,
  `bootstrap_privileges::bootstrap_as_root`) do not trip the underlying Clap
  parser. Provide configuration through environment variables or config files
  when embedding the crate.

## Further reading

- `README.md` – overview, configuration reference, and troubleshooting tips.
- `tests/e2e_postgresql_embedded_diesel.rs` – example of combining the helper
  with Diesel-based integration tests while running under `root`.
