# ExecPlan: Send-safe cluster handle (issue #84)

## Big picture

Make `TestCluster` usable in `Send`-bound contexts (e.g., `OnceLock`, `rstest`
timeouts) by separating concerns: environment management remains `!Send` (tied
to the creating thread), whilst cluster access becomes `Send` through a
dedicated handle type.

## Constraints

- **Backward compatibility**: Existing `TestCluster` API must continue to work
  unchanged for users who do not need `Send`.
- **Safety**: The `!Send` constraint on environment guards exists for good
  reason (thread-local storage). This safety must not be compromised.
- **Minimal API surface**: Avoid exposing unnecessary complexity to users who
  do not need shared cluster patterns.
- **No unsafe code in user space**: The existing `SharedClusterPtr` workaround
  uses `unsafe impl Send`; the new API should eliminate the need for this.

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                              TestCluster::new()                             │
└───────────────────────────────────┬─────────────────────────────────────────┘
                                    │
                    ┌───────────────┴───────────────┐
                    ▼                               ▼
         ┌──────────────────┐           ┌─────────────────────┐
         │   ClusterHandle  │           │    ClusterGuard     │
         │    (Send-safe)   │           │      (!Send)        │
         ├──────────────────┤           ├─────────────────────┤
         │ - bootstrap      │           │ - _env_guard        │
         │ - settings       │◄──────────┤ - worker_guard      │
         │ - environment    │  Arc<>    │ - _cluster_span     │
         │ - connection()   │           │ - handle (for Drop) │
         └──────────────────┘           └─────────────────────┘
                 │                                │
                 │ Can be moved                   │ Must stay on
                 │ across threads                 │ creating thread
                 ▼                                ▼
         ┌──────────────────┐           ┌─────────────────────┐
         │  OnceLock<...>   │           │   Drop: shutdown    │
         │  rstest fixtures │           │   + restore env     │
         └──────────────────┘           └─────────────────────┘
```

*Figure: Handle/guard split and thread-affinity boundaries.*

## Implementation tasks

### Phase 1: Core type definitions

- [x] **1.1** Create `ClusterHandle` struct in `src/cluster/handle.rs`
  - Contains: `bootstrap: TestBootstrapSettings`
  - Provides: `settings()`, `environment()`, `bootstrap()`, `connection()`
  - Must implement `Send + Sync`

- [x] **1.2** Create `ClusterGuard` struct in `src/cluster/guard.rs`
  - Contains: `_env_guard: ScopedEnv`, `worker_guard: Option<ScopedEnv>`,
    `_cluster_span: tracing::Span`
  - Contains: shutdown resources (runtime, postgres, env_vars, flags)
  - Must be `!Send` (verified via compile-time assertion)
  - `Drop` implementation handles cluster shutdown

- [x] **1.3** Add compile-time trait assertions
  - `ClusterHandle: Send + Sync`
  - `ClusterGuard: !Send` (documented, not assertable at compile-time)

### Phase 2: Constructor updates

- [x] **2.1** Add
      `TestCluster::new_split() -> BootstrapResult<(ClusterHandle, ClusterGuard)>`
  - Creates both handle and guard from bootstrap process
  - Handle is cloneable; guard is not

- [x] **2.2** Refactor `TestCluster::new()` to use `new_split()` internally
  - `TestCluster` becomes a convenience wrapper holding both
  - Maintains full backward compatibility

- [x] **2.3** Add `TestCluster::start_async_split()` for async variant
  - Same split pattern for async API

### Phase 3: Delegation and method distribution

- [x] **3.1** Move read-only methods to `ClusterHandle`
  - `settings()`, `environment()`, `bootstrap()`, `connection()`
  - All delegation methods (create_database, etc.)

- [x] **3.2** Implement `Deref<Target = ClusterHandle>` for `TestCluster`
  - Provides transparent access to handle methods
  - Existing code continues to work unchanged

### Phase 4: Shutdown logic updates

- [x] **4.1** Update `ClusterGuard::Drop` to handle shutdown
  - Move shutdown logic from `TestCluster::Drop`
  - Guard holds all resources needed for shutdown

- [x] **4.2** Ensure proper resource cleanup ordering
  - PostgreSQL stops before environment is restored
  - Runtime is available for shutdown operations

### Phase 5: Fixture updates

- [x] **5.1** Update `shared_cluster()` to use safe API
  - Kept `SharedClusterPtr` for backward compatibility in `shared_cluster()`
  - Added `shared_cluster_handle()` using `OnceLock<ClusterHandle>` pattern
  - Guard is forgotten to keep cluster running for process lifetime

- [x] **5.2** Update `shared_test_cluster()` fixture
  - Added `shared_test_cluster_handle()` returning `&'static ClusterHandle`
  - Kept `shared_test_cluster()` for backward compatibility

- [x] **5.3** Keep `test_cluster()` fixture unchanged
  - Returns `TestCluster` for per-test usage

### Phase 6: Export and documentation

- [x] **6.1** Export new types from `src/lib.rs`
  - `pub use cluster::{ClusterHandle, ClusterGuard};`

- [x] **6.2** Add module-level documentation for new types
  - Explain the handle/guard split pattern
  - Document when to use each type

- [x] **6.3** Update examples in module docs
  - Show shared cluster with `OnceLock<ClusterHandle>`
  - Show per-test usage with `TestCluster`

### Phase 7: Testing

- [x] **7.1** Add compile-time Send/Sync assertions
  - Verify `ClusterHandle: Send + Sync` (in handle.rs and tests)
  - Document `ClusterGuard: !Send` (cannot be asserted at compile-time)

- [x] **7.2** Add unit tests for handle/guard split
  - Test that handle can be moved across threads
  - Test that cluster operations work through handle

- [x] **7.3** Add integration test for `OnceLock` pattern
  - Verify the primary use case works in `tests/cluster_handle_send.rs`

- [x] **7.4** Verify existing tests pass unchanged
  - Backward compatibility validation - all 117 tests pass

### Phase 8: Cleanup

- [ ] **8.1** Remove deprecated unsafe workaround
  - Decision: Keep `shared_cluster()` with `SharedClusterPtr` for backward
    compatibility
  - New `shared_cluster_handle()` is the recommended safe API

- [x] **8.2** Run full quality gates
  - `make check-fmt && make lint && make test` - all pass

## Progress log

### 2026-01-23: Implementation complete

**Commits:**
1. `Add Send-safe ClusterHandle for shared cluster patterns` - Core
   handle/guard split
2. `Add shared_cluster_handle() for Send-safe shared cluster fixture` - New
   fixture API
3. `Add Send/Sync trait tests and From impl for ClusterHandle` - Test coverage

**Key implementation notes:**

1. **Handle/guard architecture**: Successfully separated `TestCluster` into:
   - `ClusterHandle`: Send + Sync, contains only `TestBootstrapSettings`
   - `ClusterGuard`: !Send, manages shutdown and environment restoration

2. **Backward compatibility**: Maintained via:
   - `Deref<Target = ClusterHandle>` on `TestCluster`
   - Keeping `shared_cluster()` with `SharedClusterPtr` workaround
   - All existing tests pass unchanged

3. **New APIs added:**
   - `TestCluster::new_split()` and `start_async_split()`
   - `shared_cluster_handle()` and `shared_test_cluster_handle()` fixtures
   - `From<TestBootstrapSettings>` for `ClusterHandle`

4. **!Send assertion limitation**: Cannot assert `!Send` at compile-time due to
   Rust coherence rules. Documented this constraint; the `!Send` property is
   enforced by `ScopedEnv` containing `PhantomData<Rc<()>>`.

5. **Shared cluster pattern**: The `shared_cluster_handle()` function leaks the
   `ClusterHandle` via `Box::leak()` to obtain a `&'static` reference suitable
   for `OnceLock` patterns. The `ClusterGuard` is forgotten with
   `std::mem::forget()` so the PostgreSQL process continues running for the
   process lifetime.

______________________________________________________________________

## Key decisions

### Why separate types instead of making TestCluster Send?

The `ScopedEnv` component uses thread-local storage (`THREAD_STATE`) to track
environment variable changes. This is fundamental to its design - it ensures
that environment restoration happens on the same thread that made the changes.
Making this `Send` would require either:

1. Removing thread-local storage (breaks the safety model)
2. Adding complex synchronization (adds overhead, potential deadlocks)
3. Changing to a global mutex (serializes all environment operations)

The handle/guard split preserves the safety of environment management whilst
enabling the primary use case (shared cluster access) without unsafe code.

### Why leak the cluster in shared_cluster()?

The shared cluster lives for the entire process lifetime. Leaking is
intentional - this approach avoids the complexity of tracking when all
references are done and the overhead of reference counting. The `ClusterGuard`
is forgotten after initialization, which means environment variables are
restored, but the PostgreSQL process continues running (it's an external
process, not tied to Rust lifetimes).

### Why not make ClusterHandle hold an Arc internally?

The handle itself is already lightweight (just `TestBootstrapSettings` which is
`Clone`). Users who need shared access can wrap it in `Arc` themselves. This
keeps the API simple and avoids forcing allocation overhead on users who don't
need it.

______________________________________________________________________

## References

- [Issue #84][^1]
- Branch: `issue-84-send-safe-cluster-handle`

[^1]: https://github.com/leynos/pg-embedded-setup-unpriv/issues/84
