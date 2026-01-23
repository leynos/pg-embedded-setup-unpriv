# ExecPlan: Send-Safe Cluster Handle (Issue #84)

## Big Picture

Make `TestCluster` usable in `Send`-bounded contexts (e.g., `OnceLock`,
`rstest` timeouts) by separating concerns: environment management remains
`!Send` (tied to the creating thread), whilst cluster access becomes `Send`
through a dedicated handle type.

## Constraints

- **Backward compatibility**: Existing `TestCluster` API must continue to work
  unchanged for users who do not need `Send`.
- **Safety**: The `!Send` constraint on environment guards exists for good
  reason (thread-local storage). We must not compromise this safety.
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

## Implementation Tasks

### Phase 1: Core Type Definitions

- [ ] **1.1** Create `ClusterHandle` struct in `src/cluster/handle.rs`
  - Contains: `bootstrap: TestBootstrapSettings`
  - Provides: `settings()`, `environment()`, `bootstrap()`, `connection()`
  - Must implement `Send + Sync`

- [ ] **1.2** Create `ClusterGuard` struct in `src/cluster/guard.rs`
  - Contains: `_env_guard: ScopedEnv`, `worker_guard: Option<ScopedEnv>`,
    `_cluster_span: tracing::Span`
  - Contains: shutdown resources (runtime, postgres, env_vars, flags)
  - Must be `!Send` (verified via compile-time assertion)
  - `Drop` implementation handles cluster shutdown

- [ ] **1.3** Add compile-time trait assertions
  - `ClusterHandle: Send + Sync`
  - `ClusterGuard: !Send`

### Phase 2: Constructor Updates

- [ ] **2.1** Add
      `TestCluster::new_split() -> BootstrapResult<(ClusterHandle, ClusterGuard)>`
  - Creates both handle and guard from bootstrap process
  - Handle is cloneable; guard is not

- [ ] **2.2** Refactor `TestCluster::new()` to use `new_split()` internally
  - `TestCluster` becomes a convenience wrapper holding both
  - Maintains full backward compatibility

- [ ] **2.3** Add `TestCluster::start_async_split()` for async variant
  - Same split pattern for async API

### Phase 3: Delegation and Method Distribution

- [ ] **3.1** Move read-only methods to `ClusterHandle`
  - `settings()`, `environment()`, `bootstrap()`, `connection()`
  - All delegation methods (create_database, etc.)

- [ ] **3.2** Implement `Deref<Target = ClusterHandle>` for `TestCluster`
  - Provides transparent access to handle methods
  - Existing code continues to work unchanged

### Phase 4: Shutdown Logic Updates

- [ ] **4.1** Update `ClusterGuard::Drop` to handle shutdown
  - Move shutdown logic from `TestCluster::Drop`
  - Guard holds all resources needed for shutdown

- [ ] **4.2** Ensure proper resource cleanup ordering
  - PostgreSQL stops before environment is restored
  - Runtime is available for shutdown operations

### Phase 5: Fixture Updates

- [ ] **5.1** Update `shared_cluster()` to use safe API
  - Remove `SharedClusterPtr` and `unsafe impl Send/Sync`
  - Use `OnceLock<ClusterHandle>` directly
  - Guard can be dropped after creation (cluster keeps running)

- [ ] **5.2** Update `shared_test_cluster()` fixture
  - Returns `&'static ClusterHandle` instead of `&'static TestCluster`

- [ ] **5.3** Keep `test_cluster()` fixture unchanged
  - Returns `TestCluster` for per-test usage

### Phase 6: Export and Documentation

- [ ] **6.1** Export new types from `src/lib.rs`
  - `pub use cluster::{ClusterHandle, ClusterGuard};`

- [ ] **6.2** Add module-level documentation for new types
  - Explain the handle/guard split pattern
  - Document when to use each type

- [ ] **6.3** Update examples in module docs
  - Show shared cluster with `OnceLock<ClusterHandle>`
  - Show per-test usage with `TestCluster`

### Phase 7: Testing

- [ ] **7.1** Add compile-time Send/Sync assertions
  - Verify `ClusterHandle: Send + Sync`
  - Verify `ClusterGuard: !Send`

- [ ] **7.2** Add unit tests for handle/guard split
  - Test that handle can be moved across threads
  - Test that cluster operations work through handle

- [ ] **7.3** Add integration test for `OnceLock` pattern
  - Verify the primary use case works

- [ ] **7.4** Verify existing tests pass unchanged
  - Backward compatibility validation

### Phase 8: Cleanup

- [ ] **8.1** Remove deprecated unsafe workaround
  - Remove `SharedClusterPtr`
  - Remove `unsafe impl Send/Sync`

- [ ] **8.2** Run full quality gates
  - `make check-fmt && make lint && make test`

## Progress Log

*Record progress and lessons learned here as implementation proceeds.*

______________________________________________________________________

## Key Decisions

### Why separate types instead of making `TestCluster` Send?

The `ScopedEnv` component uses thread-local storage (`THREAD_STATE`) to track
environment variable changes. This is fundamental to its design - it ensures
that environment restoration happens on the same thread that made the changes.
Making this `Send` would require either:

1. Removing thread-local storage (breaks the safety model)
2. Adding complex synchronisation (adds overhead, potential deadlocks)
3. Changing to a global mutex (serialises all environment operations)

The handle/guard split preserves the safety of environment management whilst
enabling the primary use case (shared cluster access) without unsafe code.

### Why leak the cluster in `shared_cluster()`?

The shared cluster lives for the entire process lifetime. Leaking is
intentional - we avoid the complexity of tracking when all references are done
and the overhead of reference counting. The `ClusterGuard` is dropped after
initialisation, which means environment variables are restored, but the
PostgreSQL process continues running (it's an external process, not tied to
Rust lifetimes).

### Why not make `ClusterHandle` hold an `Arc` internally?

The handle itself is already lightweight (just `TestBootstrapSettings` which is
`Clone`). Users who need shared access can wrap it in `Arc` themselves. This
keeps the API simple and avoids forcing allocation overhead on users who don't
need it.

______________________________________________________________________

## References

- Issue: https://github.com/leynos/pg-embedded-setup-unpriv/issues/84
- Branch: `issue-84-send-safe-cluster-handle`
