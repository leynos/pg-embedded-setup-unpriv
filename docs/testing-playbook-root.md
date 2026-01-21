# Root-required testing playbook

This document provides copy-pastable recipes for working with PostgreSQL
clusters in root-required workflows. These recipes assume execution with
elevated privileges on Linux, where the helper prepares directories for the
`sandbox` user and delegates lifecycle commands to the `pg_worker` subprocess.

## Prerequisites

- Root or elevated privileges on Linux
- `pg_embedded_setup_unpriv` binary built and available
- PostgreSQL version configured via environment variables or defaults
- Network access to download PostgreSQL binaries if not cached

## Starting a cluster

### Full bootstrap with automatic startup

The bootstrap command prepares the installation and data directories, applies
correct ownership for the sandbox user, and initialises the PostgreSQL cluster
with the configured credentials. Root executions automatically delegate setup to
the worker subprocess.

```bash
# Set required environment variables
export PG_VERSION_REQ="=16.4.0"
export PG_RUNTIME_DIR="/var/tmp/pg-embedded-testing/install"
export PG_DATA_DIR="/var/tmp/pg-embedded-testing/data"
export PG_SUPERUSER="postgres"
export PG_PASSWORD="postgres_pass"

# Optional: customise sandbox user (defaults to 'nobody')
export PG_EMBEDDED_SANDBOX_USER="nobody"

# Run the bootstrap - this performs setup and initialisation
cargo run --release --bin pg_embedded_setup_unpriv
```

The command returns connection parameters in the output, including host, port,
and authentication details for the initialised cluster.

### Using the RAII helper from code

For test suites, the `TestCluster` RAII wrapper provides automatic lifecycle
management. The helper detects execution context and routes root operations
through the worker subprocess.

```rust
use pg_embedded_setup_unpriv::TestCluster;

// Start cluster - automatically handles privilege detection
let cluster = TestCluster::new().expect("failed to start cluster");

let conn_str = cluster.connection_string();

// Use cluster...

// Drop is called automatically to stop and clean up
```

### Incremental setup and start

Separate setup and start steps allow customisation between initialisation and
launching the server process.

```bash
# Bootstrap (install and initialise)
export PG_VERSION_REQ="=16.4.0"
export PG_RUNTIME_DIR="/var/tmp/pg-embedded-testing/install"
export PG_DATA_DIR="/var/tmp/pg-embedded-testing/data"
export PG_SUPERUSER="postgres"
export PG_PASSWORD="postgres_pass"

cargo run --release --bin pg_embedded_setup_unpriv -- setup

# Apply custom configuration to data directory if needed
sudo -u nobody cp custom_postgresql.conf $PG_DATA_DIR/

# Start the cluster
cargo run --release --bin pg_embedded_setup_unpriv -- start
```

## Seeding data

### Using `psql` directly

Connect to the running cluster and execute SQL scripts or commands.

```bash
# Set connection parameters from bootstrap output or configuration
export PGHOST="127.0.0.1"
export PGPORT="5432"
export PGUSER="postgres"
export PGPASSWORD="postgres_pass"

# Run a schema migration script
psql -f schema/migrations/001_initial.sql

# Execute ad-hoc SQL
psql -c "CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL);"
```

### Using Diesel ORM

For Rust applications using Diesel, establish a connection to the embedded
cluster and run migrations programmatically.

```rust
use diesel::prelude::*;
use diesel_migrations::{MigrationHarness, Migrations};

let database_url = cluster.connection_string();
let mut conn = PgConnection::establish(&database_url)?;

// Run embedded migrations
conn.run_pending_migrations(Migrations::find_migrations_directory()?)
    .expect("failed to run migrations");

// Insert test data
diesel::insert_into(users::table)
    .values(name.eq("Test User"))
    .execute(&mut conn)?;
```

### Using `sqlx`

For projects using `sqlx`, the embedded cluster works identically to any
PostgreSQL instance.

```rust
use sqlx::PgPool;

let pool = PgPool::connect(&cluster.connection_string()).await?;

// Create schema
sqlx::query(include_str!("schema.sql"))
    .execute(&pool)
    .await?;

// Seed data
sqlx::query("INSERT INTO products (name, price) VALUES ($1, $2)")
    .bind("Widget")
    .bind(9.99)
    .execute(&pool)
    .await?;
```

### Loading fixture data from file

For repeatable test fixtures, prepare SQL files and load them after cluster
initialisation.

```bash
# Create fixture directory structure
mkdir -p fixtures/data

# Prepare fixture file: fixtures/data/001_test_data.sql
# -- fixtures/data/001_test_data.sql --
# BEGIN;
# INSERT INTO users (name, email) VALUES
#     ('Alice', 'alice@example.com'),
#     ('Bob', 'bob@example.com');
# COMMIT;

# Load fixtures in order
for file in fixtures/data/*.sql; do
    psql -f "$file"
done
```

## Cleaning up

### Using the stop command

Gracefully stop the PostgreSQL server process without removing data files.

```bash
cargo run --release --bin pg_embedded_setup_unpriv -- stop
```

The command shuts down the cluster cleanly, waiting for connections to close
and checkpointing data. The data directory remains intact for subsequent
restarts.

### Manual cleanup of data directories

Remove all prepared directories when a fresh environment is required.

```bash
export PG_RUNTIME_DIR="/var/tmp/pg-embedded-testing/install"
export PG_DATA_DIR="/var/tmp/pg-embedded-testing/data"

# Remove directories (requires root for ownership adjustments)
rm -rf "$PG_RUNTIME_DIR" "$PG_DATA_DIR"
```

Warning: this operation is destructive and cannot be undone. Ensure no active
processes are using the directories before removal.

### Using RAII automatic cleanup

The `TestCluster` wrapper automatically stops the cluster when dropped, but
does not remove data directories by default. For complete cleanup:

```rust
{
    let cluster = TestCluster::new().expect("failed to start cluster");

    // Use cluster...

} // Drop is called here - cluster stops automatically

// Manually clean up directories if needed
std::fs::remove_dir_all("/var/tmp/pg-embedded-testing/install")?;
std::fs::remove_dir_all("/var/tmp/pg-embedded-testing/data")?;
```

## Idempotent operations

### Re-running bootstrap

The bootstrap command is idempotent and may be executed multiple times safely.

```bash
# First bootstrap - creates directories and initialises
cargo run --release --bin pg_embedded_setup_unpriv

# Subsequent runs - verifies setup and skips redundant work
cargo run --release --bin pg_embedded_setup_unpriv
```

Root executions re-apply ownership and permissions on every invocation to
guarantee the sandbox user can access the prepared directories.

### Idempotent start and stop

The lifecycle helpers prevent redundant operations while ensuring correct
state transitions.

```bash
# Start multiple times - idempotent, no errors
cargo run --release --bin pg_embedded_setup_unpriv -- start
cargo run --release --bin pg_embedded_setup_unpriv -- start

# Stop multiple times - idempotent, no errors
cargo run --release --bin pg_embedded_setup_unpriv -- stop
cargo run --release --bin pg_embedded_setup_unpriv -- stop
```

The `ensure_postgres_started()` helper guarantees setup runs before start,
and both operations log when redundant invocations are skipped.

## Troubleshooting

### Permission denied errors

If permission errors occur, verify the sandbox user has correct ownership of the
prepared directories.

```bash
export PG_EMBEDDED_SANDBOX_USER="nobody"
export PG_RUNTIME_DIR="/var/tmp/pg-embedded-testing/install"
export PG_DATA_DIR="/var/tmp/pg-embedded-testing/data"

# Check ownership
ls -ld "$PG_RUNTIME_DIR" "$PG_DATA_DIR"

# Re-apply ownership if needed
chown -R "$PG_EMBEDDED_SANDBOX_USER:$PG_EMBEDDED_SANDBOX_USER" \
    "$PG_RUNTIME_DIR" "$PG_DATA_DIR"

# Re-run bootstrap to verify
cargo run --release --bin pg_embedded_setup_unpriv
```

### Port already in use

If the configured port is already in use, either stop the conflicting process
or configure a different port.

```bash
# Find process using the port (assuming default 5432)
sudo lsof -i :5432

# Option 1: stop the conflicting process
sudo kill <PID>

# Option 2: use a different port
export PG_PORT="5433"
cargo run --release --bin pg_embedded_setup_unpriv
```

### Worker subprocess not found

If the helper cannot locate the `pg_worker` binary, ensure the build completed
successfully.

```bash
# Verify the worker binary exists
cargo build --release --bins
ls -lh target/release/pg_worker

# Rebuild if missing
cargo build --release --bin pg_worker
```

### Data directory corruption

If the data directory becomes corrupted, remove it and re-run bootstrap.

```bash
export PG_DATA_DIR="/var/tmp/pg-embedded-testing/data"

# Backup corrupted data if needed for investigation
mv "$PG_DATA_DIR" "$PG_DATA_DIR.corrupted"

# Re-bootstrap creates a fresh data directory
cargo run --release --bin pg_embedded_setup_unpriv
```
