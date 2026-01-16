# Add Async API for TestCluster

This ExecPlan is a living document. The sections `Constraints`, `Tolerances`,
`Risks`, `Progress`, `Surprises & Discoveries`, `Decision Log`, and
`Outcomes & Retrospective` must be kept up to date as work proceeds.

Status: COMPLETE

## Purpose / Big Picture

Enable `TestCluster` to be used directly within async contexts such as
`#[tokio::test]` without panicking. Currently, calling `TestCluster::new()`
from within an async runtime panics with "Cannot start a runtime from within a
runtime" because the crate creates its own internal Tokio runtime and calls
`Runtime::block_on()`.

After this change, users can write:

    #[tokio::test]
    async fn test_async_database_operations() {
        let cluster = TestCluster::start_async().await.expect("PG start failed");
        // … async database work …
        cluster.stop_async().await.expect("PG stop failed");
    }

The synchronous API remains unchanged for backward compatibility.

## Constraints

- **C1: Backward compatibility** - The existing synchronous
  `TestCluster::new()` API must continue to work unchanged. Existing tests and
  user code must not break.

- **C2: Drop safety** - Rust's `Drop` trait cannot be async. The sync fallback
  must remain for resource cleanup when users forget to call `stop_async()`.

- **C3: Single-threaded runtime assumption** - The existing runtime uses
  `Builder::new_current_thread()`. Worker operations (`invoke_as_root`) spawn
  subprocesses synchronously - this is inherently blocking and should remain so.

- **C4: No modifications to postgresql_embedded** - All async operations come
  from this external crate. The implementation must work within its API.

- **C5: Code style** - Per AGENTS.md: files must stay under 400 lines, use
  en-GB-oxendict spelling, modules must have `//!` doc comments.

- **C6: Missing docs lint** - All public items require documentation
  (`#![deny(missing_docs)]`).

## Tolerances (Exception Triggers)

- **Scope**: If implementation requires changes to more than 8 files, stop and
  escalate.
- **Interface**: New public API items are expected; changing existing public
  method signatures requires escalation.
- **Dependencies**: If a new external dependency is required, stop and
  escalate.
- **Iterations**: If tests still fail after 3 attempts at fixing a particular
  issue, stop and escalate.
- **Ambiguity**: If the async shutdown behaviour in edge cases (e.g., runtime
  already dropped) is unclear, escalate for user input.

## Risks

- Risk: Users may create async clusters but forget to call `stop_async()`,
  leading to resource leaks or silent failures. Severity: medium Likelihood:
  medium Mitigation: Log a warning in `Drop` if async mode was used without
  explicit shutdown. Document the requirement clearly.

- Risk: Mixing sync and async APIs on the same cluster could cause undefined
  behaviour. Severity: medium Likelihood: low Mitigation: Use `is_async_mode`
  flag to detect and warn/error on misuse.

- Risk: `Drop` calling `block_on()` when cluster was created in async context
  could panic if the async runtime has been dropped. Severity: high Likelihood:
  low Mitigation: Check for active runtime handle before using `block_on()` in
  Drop. Use `tokio::runtime::Handle::try_current()` to detect.

## Progress

- [x] (2026-01-15) Stage A: Preparation - read all affected files, confirm
      approach
- [x] (2026-01-15) Stage B: Modify runtime ownership in TestCluster to
      `Option<Runtime>`
- [x] (2026-01-15) Stage C: Add async worker invoker methods
- [x] (2026-01-15) Stage D: Add async constructors and lifecycle methods
- [x] (2026-01-15) Stage E: Update Drop implementation with async-awareness
- [x] (2026-01-15) Stage F: Add feature flag `async-api`
- [x] (2026-01-15) Stage G: Write async tests
- [x] (2026-01-15) Stage H: Update documentation
- [x] (2026-01-15) Final validation and cleanup

## Surprises & Discoveries

- Observation: Large futures (18KB+) caused clippy `large_futures` warnings
  Evidence: Clippy output showing future sizes exceeding default thresholds
  Impact: Required wrapping with `Box::pin()` to avoid stack overflow concerns

- Observation: Cognitive complexity in refactored methods exceeded clippy limit
  Evidence: Multiple clippy warnings about cognitive complexity (18/9, 14/9)
  Impact: Required extracting helper functions like `log_lifecycle_start()`,
  `warn_async_drop_without_stop()`, `stop_worker_managed_async()`

- Observation: Async tests conflicted with other cluster tests using same data
  directory Evidence: initdb errors about non-empty directory when running full
  test suite Impact: Required `file_serial` attribute to serialize tests across
  binaries; needed `file_locks` feature for serial_test crate

- Observation: `AsyncInvoker` needed separate struct rather than async methods
  on `WorkerInvoker` Evidence: `WorkerInvoker` holds `&'a Runtime` which isn't
  available in async mode Impact: Created parallel `AsyncInvoker` struct
  without runtime reference

## Decision Log

- Decision: Use `Option<Runtime>` rather than runtime-per-mode enum
  Rationale: Simplest representation; `None` means async mode, `Some(_)` means
  sync mode. Avoids complexity of enum matching throughout codebase.
  Date/Author: 2026-01-15 / Plan author

- Decision: Name async constructor `start_async()` not `new_async()`
  Rationale: Matches the design document proposal. `start_async()` is more
  action-oriented and clearly indicates what happens (the cluster starts).
  Date/Author: 2026-01-15 / Plan author

- Decision: Feature-gate async API behind `async-api` feature
  Rationale: Allows sync-only users to avoid pulling in async codepaths.
  Default-enabled for convenience since tokio is already a dependency.
  Date/Author: 2026-01-15 / Plan author

## Outcomes & Retrospective

### What Was Achieved

Successfully implemented async API for `TestCluster`:

- `start_async()` - async constructor that runs on caller's runtime
- `stop_async()` - explicit async shutdown with proper cleanup
- Feature-gated behind `async-api` feature flag
- Full backward compatibility with existing sync API
- 4 new async tests validating the implementation
- Comprehensive documentation with examples

### Metrics

- Files modified: 4 (within 8-file tolerance)
- New test file: 1 (`tests/test_cluster_async.rs`)
- All 116 tests pass including 4 new async tests
- No new external dependencies (only internal feature flag for serial_test)

### Lessons Learned

1. **Test isolation matters**: Async tests running in parallel with other
   cluster tests caused data directory conflicts. File-based serialization
   (`file_serial`) solved this, but could consider using sandboxed directories
   for async tests in future.

2. **Future sizes need attention**: Large async state machines can cause stack
   issues. `Box::pin()` is the standard solution but adds allocation overhead.

3. **Separate invoker for async**: Rather than trying to make `WorkerInvoker`
   work for both sync and async, creating a parallel `AsyncInvoker` struct was
   cleaner and avoided lifetime issues with the runtime reference.

4. **Drop complexity**: Async-safe Drop implementation requires careful handling
   - checking for active runtime, spawning cleanup tasks, and warning users
   about resource leaks.

## Context and Orientation

### Key Files

- `src/cluster/mod.rs` (354 lines) - Contains `TestCluster` struct definition,
  `new()` constructor, lifecycle methods, and `Drop` implementation.
  - Lines 48-60: `TestCluster` struct with `runtime: Runtime` field
  - Lines 77-99: `new()` constructor
  - Lines 105-137: `start_postgres()` internal method
  - Lines 152-162: `invoke_lifecycle()` calls async ops via invoker
  - Lines 289-323: `Drop` implementation using `block_on()`

- `src/cluster/worker_invoker/mod.rs` (259 lines) - Dispatches PostgreSQL
  lifecycle operations. Core abstraction for running async operations.
  - Lines 18-22: `WorkerInvoker<'a>` struct holding `&'a Runtime`
  - Lines 78-88: `invoke()` public method
  - Lines 142-150: `invoke_unprivileged()` calls `runtime.block_on()`

- `src/cluster/runtime.rs` (22 lines) - Builds the single-threaded Tokio
  runtime via `build_runtime()`.

- `Cargo.toml` - Line 43: `tokio = { version = "1", features = ["rt", "macros"]
  }`. Line 70-77: Feature flags section.

### Current Architecture

    +---------------+        +------------------+        +-------------------+
    | TestCluster   |        | WorkerInvoker    |        | postgresql_       |
    | (owns Runtime)|--&rt-->| (borrows &Runtime)|--await->| embedded          |
    +---------------+        +------------------+        | (async methods)   |
                                                         +-------------------+
                                                                  ^
    +-------+     +---------------+     +----------+              |
    | Drop  |---->| invoke_unpriv |---->| block_on |----future----+
    +-------+     +---------------+     +----------+

### Terms

- **Block_on**: `Runtime::block_on()` synchronously waits for an async future
  to complete. Cannot be called from within an existing async context.
- **Async context**: Code running inside a Tokio runtime (e.g., inside
  `#[tokio::test]` or after `.await`).
- **Worker-managed**: When running as root, lifecycle operations are delegated
  to a privileged subprocess (`pg_worker`) rather than executed in-process.

## Plan of Work

### Stage A: Preparation

Read all files that will be modified. Verify understanding of the current flow
from `TestCluster::new()` through `WorkerInvoker::invoke()` to
`runtime.block_on()`. No code changes.

### Stage B: Modify Runtime Ownership

In `src/cluster/mod.rs`, change the `runtime` field from `Runtime` to
`Option<Runtime>`. Add an `is_async_mode: bool` field to track mode.

Edit the struct definition:

    pub struct TestCluster {
        runtime: Option<Runtime>,  // Was: Runtime
        is_async_mode: bool,       // New field
        postgres: Option<PostgreSQL>,
        // … rest unchanged
    }

Update the `new()` constructor to set `runtime: Some(runtime)` and
`is_async_mode: false`.

### Stage C: Async Worker Invoker Methods

In `src/cluster/worker_invoker/mod.rs`, add an async variant of
`invoke_unprivileged()`. The new method directly `.await`s the future instead
of using `block_on()`.

Add method:

    #[cfg(feature = "async-api")]
    pub async fn invoke_async<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        // Same span/logging logic as invoke()
        // But dispatch to async path for unprivileged operations
    }

The async dispatch will call a new `invoke_unprivileged_async()` that directly
awaits:

    async fn invoke_unprivileged_async<Fut>(
        future: Fut,
        ctx: &'static str,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        future.await.context(ctx).map_err(BootstrapError::from)
    }

Note: `invoke_as_root()` remains synchronous - subprocess spawning is
inherently blocking.

### Stage D: Async Constructors and Lifecycle Methods

In `src/cluster/mod.rs`, add the async constructor and shutdown method.

Add `start_async()`:

    #[cfg(feature = "async-api")]
    pub async fn start_async() -> BootstrapResult<Self> {
        // Similar to new() but:
        // 1. Does NOT call build_runtime()
        // 2. Calls start_postgres_async() instead of start_postgres()
        // 3. Sets runtime: None, is_async_mode: true
    }

Add internal `start_postgres_async()`:

    #[cfg(feature = "async-api")]
    async fn start_postgres_async(
        bootstrap: TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
    ) -> BootstrapResult<StartupOutcome> {
        // Similar to start_postgres() but directly .await lifecycle ops
    }

Add `stop_async()`:

    #[cfg(feature = "async-api")]
    pub async fn stop_async(mut self) -> BootstrapResult<()> {
        // Take ownership to prevent Drop from running
        // Call postgres.stop().await with timeout
        // For worker-managed: still use sync invoke_as_root
    }

The `stop_async()` method consumes `self` to prevent the `Drop` implementation
from attempting shutdown again.

### Stage E: Update Drop Implementation

Modify the `Drop` implementation to handle async-created clusters gracefully.

In `src/cluster/mod.rs` Drop impl:

    fn drop(&mut self) {
        if self.is_async_mode {
            // Cluster was created async - user should have called stop_async()
            // Try to detect if we're in an async context and warn appropriately
            if self.postgres.is_some() {
                tracing::warn!(
                    target: LOG_TARGET,
                    "async TestCluster dropped without calling stop_async(); \
                     resources may not be cleaned up properly"
                );
                // Attempt cleanup via Handle::try_current() if available
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    // We're in an async context - spawn blocking cleanup
                    let postgres = self.postgres.take();
                    let timeout = self.bootstrap.shutdown_timeout;
                    handle.spawn(async move {
                        if let Some(pg) = postgres {
                            let _ = tokio::time::timeout(timeout, pg.stop()).await;
                        }
                    });
                }
            }
            return;
        }

        // Existing sync drop logic follows…
    }

### Stage F: Add Feature Flag

In `Cargo.toml`, add the `async-api` feature:

    [features]
    async-api = []  # Enable async TestCluster API
    # … existing features

Default-enable it by adding to a `default` feature or documenting that users
should enable it. Consider whether to make it default or opt-in.

Update tokio dependency to ensure `time` feature is available:

    tokio = { version = "1", features = ["rt", "macros", "time"] }

### Stage G: Write Async Tests

Create `tests/test_cluster_async.rs`:

    #![cfg(feature = "async-api")]

    use pg_embedded_setup_unpriv::TestCluster;

    #[tokio::test]
    async fn start_async_creates_cluster_without_panic() {
        let cluster = TestCluster::start_async().await;
        assert!(cluster.is_ok());
        let cluster = cluster.unwrap();
        cluster.stop_async().await.expect("stop failed");
    }

    #[tokio::test]
    async fn stop_async_cleans_up_resources() {
        let cluster = TestCluster::start_async().await.expect("start failed");
        let result = cluster.stop_async().await;
        assert!(result.is_ok());
    }

Add feature gate to Cargo.toml for test:

    [[test]]
    name = "test_cluster_async"
    path = "tests/test_cluster_async.rs"
    required-features = ["async-api"]

### Stage H: Documentation

Update module-level docs in `src/cluster/mod.rs` with examples for both sync
and async usage patterns.

Add doc comments to new public methods:

- `start_async()` - Document when to use, show `#[tokio::test]` example
- `stop_async()` - Document importance of calling explicitly in async context

## Concrete Steps

All commands run from repository root:
`/data/leynos/Projects/pg-embedded-setup-unpriv.worktrees/issue-50-async-support`

### After each stage

    make check-fmt && make lint && make test

Expected: All pass with no warnings.

### After Stage G (with async tests)

    cargo test --features async-api test_cluster_async

Expected: async tests pass without "Cannot start a runtime from within a
runtime" panic.

### Final validation

    make check-fmt && make lint && make test
    cargo test --features async-api

Expected transcript fragment:

    running 2 tests
    test start_async_creates_cluster_without_panic … ok
    test stop_async_cleans_up_resources … ok

## Validation and Acceptance

Quality criteria:

- Tests: All existing tests pass unchanged. New async tests in
  `test_cluster_async.rs` pass.
- Lint/typecheck: `make lint` passes with no warnings. `make check-fmt` passes.
- Backward compatibility: Existing code using `TestCluster::new()` continues to
  work.
- Async functionality: `#[tokio::test]` functions can create and use
  `TestCluster` via `start_async()` without panic.

Quality method:

    make check-fmt && make lint && make test
    cargo test --features async-api

Observable behaviour:

1. Running `cargo test --features async-api test_cluster_async` produces
   passing tests.
2. Running existing sync tests (`make test`) produces the same results as
   before this change.

## Idempotence and Recovery

All stages can be re-run safely. Each stage builds on the previous but does not
destroy intermediate state. If a stage fails partway:

1. Discard uncommitted changes: `git checkout -- .`
2. Re-read the affected files to understand current state
3. Resume from the beginning of the failed stage

## Artifacts and Notes

### WorkerInvoker modification pattern

The key change in `WorkerInvoker` is adding an async path that bypasses
`block_on()`. The sync path remains for backward compatibility:

    // Sync path (existing)
    fn invoke_unprivileged<Fut>(&self, future: Fut, ctx: &'static str) -> BootstrapResult<()>
    where Fut: Future<…> + Send
    {
        self.runtime.block_on(future).context(ctx).map_err(…)
    }

    // Async path (new)
    async fn invoke_unprivileged_async<Fut>(future: Fut, ctx: &'static str) -> BootstrapResult<()>
    where Fut: Future<…> + Send
    {
        future.await.context(ctx).map_err(…)
    }

### Drop behaviour summary

| Mode | runtime field | Drop behaviour |
|------|---------------|----------------|
| Sync (`new()`) | `Some(Runtime)` | Uses `runtime.block_on()` to call `postgres.stop()` |
| Async (`start_async()`) | `None` | Warns if `postgres` not None; attempts `Handle::try_current()` spawn for cleanup |

## Interfaces and Dependencies

### New Public API

In `src/cluster/mod.rs`:

    impl TestCluster {
        #[cfg(feature = "async-api")]
        pub async fn start_async() -> BootstrapResult<Self>;

        #[cfg(feature = "async-api")]
        pub async fn stop_async(self) -> BootstrapResult<()>;
    }

### Internal Changes

In `src/cluster/worker_invoker/mod.rs`:

    impl<'a> WorkerInvoker<'a> {
        #[cfg(feature = "async-api")]
        pub async fn invoke_async<Fut>(…) -> BootstrapResult<()>;
    }

### Dependencies

No new external dependencies. Existing tokio dependency gains `time` feature
(already implicitly used via `tokio::time`).
