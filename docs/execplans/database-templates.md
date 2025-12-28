# Execution plan: template database support for fast test isolation

This execution plan implements ADR 002, enabling PostgreSQL template databases
for fast test isolation. The feature reduces per-test overhead from seconds to
milliseconds by cloning pre-migrated template databases instead of
bootstrapping fresh clusters for each test.

**Related documents:**

- ADR: `docs/adr-002-template-database-support.md`
- Design: `docs/zero-config-raii-postgres-test-fixture-design.md`

## 1. Core database lifecycle API

Add database creation and deletion methods to enable programmatic database
management on a running cluster.

### 1.1. Database Data Definition Language (DDL) methods on TestClusterConnection

- [ ] **1.1.1.** Implement `create_database(name: &str) -> BootstrapResult<()>`
  - [ ] Add `postgres` crate dependency (`postgres = "0.19"`) to `Cargo.toml`.
  - [ ] Connect to `postgres` database using `admin_url()`.
  - [ ] Execute `CREATE DATABASE "{name}"` via `batch_execute`.
  - [ ] Emit tracing span for observability.
  - [ ] Handle error for duplicate database name.

- [ ] **1.1.2.** Implement `drop_database(name: &str) -> BootstrapResult<()>`
  - [ ] Execute `DROP DATABASE "{name}"` via `batch_execute`.
  - [ ] Handle error for non-existent database.
  - [ ] Handle error for database with active connections.

- [ ] **1.1.3.** Implement
      `database_exists(name: &str) -> BootstrapResult<bool>`
  - [ ] Query `pg_database` catalogue for database name.
  - [ ] Return boolean result.

- [ ] **1.1.4.** Add unit tests for DDL methods
  - [ ] Test `create_database` creates accessible database.
  - [ ] Test `drop_database` removes database.
  - [ ] Test `database_exists` returns correct boolean.
  - [ ] Test error handling for invalid names.
  - [ ] Test error handling for existing databases.

- [ ] **1.1.5.** Add behavioural tests (rstest-bdd)
  - [ ] Test database creation and query workflow.
  - [ ] Test error scenarios (duplicate names, non-existent drops).

**Files:**

- `Cargo.toml` — add `postgres` dependency
- `src/cluster/connection.rs` — add DDL methods
- `tests/database_lifecycle.rs` — unit tests
- `tests/database_lifecycle_bdd.rs` — behavioural tests

### 1.2. Delegation methods on TestCluster

- [ ] **1.2.1.** Add convenience wrappers on `TestCluster`
  - [ ] `create_database(name)` delegates to
        `self.connection().create_database(name)`.
  - [ ] `drop_database(name)` delegates to
        `self.connection().drop_database(name)`.
  - [ ] `database_exists(name)` delegates to
    `self.connection().database_exists(name)`.

- [ ] **1.2.2.** Add unit tests for delegation methods
  - [ ] Verify delegation produces same results as direct connection calls.

**Files:**

- `src/cluster/mod.rs` — add delegation methods

### 1.3. Documentation updates

- [ ] **1.3.1.** Update ADR 002 with `postgres` crate choice rationale
- [ ] **1.3.2.** Document APIs in `docs/users-guide.md`
  - [ ] Document `create_database`, `drop_database`, `database_exists`.
  - [ ] Provide usage example.

## 2. Shared cluster fixture

Add a process-global shared cluster fixture to eliminate per-test cluster
bootstrap overhead.

### 2.1. Shared cluster function

- [ ] **2.1.1.** Implement
      `shared_cluster() -> BootstrapResult<&'static TestCluster>`
  - [ ] Use `OnceLock<TestCluster>` for lazy initialization.
  - [ ] Use `get_or_try_init` for fallible initialization.
  - [ ] Handle worker environment setup via existing `ensure_worker_env()`.

- [ ] **2.1.2.** Implement rstest fixture wrapper
  - [ ] Add `#[fixture] shared_test_cluster() -> &'static TestCluster`.
  - [ ] Panic with `SKIP-TEST-CLUSTER:` prefix on bootstrap failure.

- [ ] **2.1.3.** Add unit tests
  - [ ] Test multiple calls return same instance (pointer equality).
  - [ ] Test thread-safe concurrent access.
  - [ ] Test bootstrap error handling.

- [ ] **2.1.4.** Add behavioural tests
  - [ ] Test fixture reuse across multiple tests in same binary.
  - [ ] Test environment variable inheritance.

**Files:**

- `src/test_support/fixtures.rs` — add `shared_cluster()` and fixture
- `src/test_support/mod.rs` — export new fixture
- `tests/shared_cluster.rs` — unit tests
- `tests/shared_cluster_bdd.rs` — behavioural tests

### 2.2. Documentation updates

- [ ] **2.2.1.** Document shared fixture pattern in `docs/users-guide.md`
  - [ ] Explain when to use shared vs per-test cluster.
  - [ ] Provide complete example with template pattern.

## 3. Template support with concurrency safety

Add template database cloning and concurrent-safe template creation.

### 3.1. Template cloning

- [ ] **3.1.1.** Implement
  `create_database_from_template(name: &str, template: &str) -> BootstrapResult<()>`
  - [ ] Execute `CREATE DATABASE "{name}" TEMPLATE "{template}"`.
  - [ ] Handle error for non-existent template.
  - [ ] Handle error for template with active connections.

- [ ] **3.1.2.** Add unit tests
  - [ ] Test template cloning creates independent database.
  - [ ] Test cloned database has same schema as template.
  - [ ] Test error handling for non-existent template.

**Files:**

- `src/cluster/connection.rs` — add `create_database_from_template`
- `tests/template_database.rs` — unit tests

### 3.2. Concurrent-safe template creation

- [ ] **3.2.1.** Add `dashmap` dependency (`dashmap = "6"`) to `Cargo.toml`

- [ ] **3.2.2.** Implement
      `ensure_template_exists<F>(name, setup_fn) -> BootstrapResult<()>`
  - [ ] Use `DashMap<String, Mutex<()>>` for per-template locking.
  - [ ] Acquire lock before checking existence.
  - [ ] Call `setup_fn` only if template does not exist.
  - [ ] Release lock after setup completes.

- [ ] **3.2.3.** Add unit tests
  - [ ] Test `ensure_template_exists` only calls setup once.
  - [ ] Test concurrent calls are serialized (no duplicate setup).
  - [ ] Test setup function errors are propagated.

- [ ] **3.2.4.** Add behavioural tests
  - [ ] Test full template workflow (create, migrate, clone).
  - [ ] Test concurrent test template creation.

**Files:**

- `Cargo.toml` — add `dashmap` dependency
- `src/cluster/connection.rs` — add `ensure_template_exists`
- `tests/template_concurrency.rs` — concurrency tests

### 3.3. Migration directory hashing

- [ ] **3.3.1.** Add `sha2` dependency (`sha2 = "0.10"`) to `Cargo.toml`

- [ ] **3.3.2.** Implement
      `hash_directory(path: &Path) -> BootstrapResult<String>`
  - [ ] Collect all files in directory recursively, sorted by path.
  - [ ] Compute SHA-256 hash of concatenated file contents.
  - [ ] Return first 12 hexadecimal characters.

- [ ] **3.3.3.** Add unit tests
  - [ ] Test `hash_directory` produces consistent hashes.
  - [ ] Test hash changes when file contents change.
  - [ ] Test hash changes when files are added or removed.
  - [ ] Test empty directory produces valid hash.
  - [ ] Test non-existent directory returns error.

**Files:**

- `Cargo.toml` — add `sha2` dependency
- `src/cluster/hash.rs` — new module for hashing utilities
- `src/cluster/mod.rs` — declare `hash` module
- `tests/hash_directory.rs` — unit tests

### 3.4. Documentation updates

- [ ] **3.4.1.** Update ADR 002 with migration hashing decision
  - [ ] Record directory hash approach.
  - [ ] Record 12 hex character truncation.

- [ ] **3.4.2.** Document template pattern in `docs/users-guide.md`
  - [ ] Explain template workflow with migration hashing.
  - [ ] Provide complete code example.

## 4. Documentation

Comprehensive documentation of the template database pattern.

### 4.1. User guide updates

- [ ] **4.1.1.** Add "Template databases for fast test isolation" section
  - [ ] Explain the problem (per-test bootstrap overhead).
  - [ ] Explain the solution (shared cluster + template cloning).
  - [ ] Provide complete example with all APIs.

- [ ] **4.1.2.** Add migration hashing example
  - [ ] Show `hash_directory` usage for template naming.
  - [ ] Explain when templates are invalidated.

- [ ] **4.1.3.** Add performance comparison
  - [ ] Compare per-test cluster vs shared cluster with templates.
  - [ ] Provide guidance on when to use each approach.

- [ ] **4.1.4.** Add cleanup strategy guidance
  - [ ] Explain explicit `drop_database` vs cluster teardown.
  - [ ] Explain `TemporaryDatabase` guard (Phase 5).

**Files:**

- `docs/users-guide.md` — add template database section

### 4.2. README updates

- [ ] **4.2.1.** Add template support to features section
  - [ ] Brief mention of `shared_cluster()` fixture.
  - [ ] Brief mention of template cloning.
  - [ ] Link to user guide for details.

**Files:**

- `README.md` — update features section

## 5. Cleanup automation

Add Resource Acquisition Is Initialization (RAII) guard for automatic database
cleanup.

### 5.1. TemporaryDatabase struct

- [ ] **5.1.1.** Implement `TemporaryDatabase` struct
  - [ ] Store database name and admin URL (not borrowed connection).
  - [ ] Implement `name(&self) -> &str` accessor.
  - [ ] Implement `url(&self) -> String` for database connection URL.

- [ ] **5.1.2.** Implement `drop(self) -> BootstrapResult<()>`
  - [ ] Drop database, failing if connections exist.
  - [ ] Mirror PostgreSQL native behaviour.

- [ ] **5.1.3.** Implement `force_drop(self) -> BootstrapResult<()>`
  - [ ] Terminate active connections via `pg_terminate_backend`.
  - [ ] Drop database after connections terminated.

- [ ] **5.1.4.** Implement `Drop` trait
  - [ ] Best-effort drop with `try_drop()`.
  - [ ] Log warning on failure via `tracing::warn!`.

- [ ] **5.1.5.** Add unit tests
  - [ ] Test `TemporaryDatabase` drops database on scope exit.
  - [ ] Test `drop()` fails with active connections.
  - [ ] Test `force_drop()` succeeds with active connections.
  - [ ] Test warning logged on `Drop` failure.

**Files:**

- `src/cluster/temporary_database.rs` — new module
- `src/cluster/mod.rs` — declare and export module
- `tests/temporary_database.rs` — unit tests

### 5.2. Factory methods

- [ ] **5.2.1.** Add factory methods to `TestClusterConnection`
  - [ ] `temporary_database(name) -> BootstrapResult<TemporaryDatabase>`.
  - [ ] `temporary_database_from_template(name, template) -> BootstrapResult<TemporaryDatabase>`.

- [ ] **5.2.2.** Add delegation methods to `TestCluster`
  - [ ] Delegate to connection methods.

- [ ] **5.2.3.** Add behavioural tests
  - [ ] Test RAII cleanup in test context.
  - [ ] Test `force_drop` terminates connections.

**Files:**

- `src/cluster/connection.rs` — add factory methods
- `src/cluster/mod.rs` — add delegation methods
- `tests/temporary_database_bdd.rs` — behavioural tests

### 5.3. Documentation updates

- [ ] **5.3.1.** Document `TemporaryDatabase` in `docs/users-guide.md`
  - [ ] Explain RAII cleanup semantics.
  - [ ] Document `drop()` vs `force_drop()`.
  - [ ] Provide usage example.

## Success criteria

- [ ] All unit tests pass (`make test`).
- [ ] All behavioural tests pass.
- [ ] Clippy passes with no warnings (`make lint`).
- [ ] Formatting validated (`make check-fmt`).
- [ ] Markdown validated (`make markdownlint`).
- [ ] ADR 002 updated with implementation decisions.
- [ ] User guide documents all new APIs.
- [ ] Consumer codebases (`../wildside/backend`, `../mxd`) can adopt template
  pattern with minimal changes.
