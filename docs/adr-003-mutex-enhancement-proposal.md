# Architectural decision record (ADR) 003: mutex-backed environment lock for tests

## Status

Accepted (2026-01-12). The `ScopedEnv` guard serializes environment mutations
for test helpers using a process-global mutex with poison recovery and
thread-local re-entrancy. Concurrency tests cover cross-thread serialization
and restoration.

## Date

2026-01-12.

## Context and problem statement

Process-global environment variables are mutated by test helpers such as
`TestCluster` and `bootstrap_for_tests`. Rust test runners execute tests in
parallel by default, so concurrent helpers can overwrite or observe partially
applied environment values. This leads to intermittent failures and undermines
the Resource Acquisition Is Initialization (RAII) contract of the test helpers.

## Decision drivers

- Test helpers must be safe under parallel execution within a single process.
- Environment mutations must be restored deterministically after guards drop.
- The public API should remain stable.
- Additional dependencies should be avoided.

## Goals and non-goals

### Goals

- Provide a single, internal lock for environment mutations.
- Hold the lock for the full lifetime of the environment guard.
- Recover after mutex poisoning to keep test suites running.

### Non-goals

- Cross-process locking for multiple test processes.
- General protection for arbitrary application environment access.
- Serialization of unrelated global state such as the current working
  directory, unless the crate mutates it.

## Requirements

### Functional requirements

- Acquire the environment lock before any mutation.
- Restore environment values before releasing the lock.
- Allow nested scopes on a single thread without deadlocking.

### Technical requirements

- Recover from poisoned locks to avoid cascading test failures.
- Avoid new public API surface by default.

## Options considered

### Option A: internal lock held by test guards

Maintain an internal process-global mutex and hold it for the lifetime of
guards such as `ScopedEnv` and `TestCluster`.

### Option B: explicit public lock helper

Expose a public guard for callers who need to serialize environment changes
without `TestCluster`.

### Option C: document single-threaded test execution

Document a requirement for `cargo test -- --test-threads=1` and similar
single-threaded runners.

### Option D: depend on env-lock

Adopt the [env-lock](https://crates.io/crates/env-lock) crate to provide
locking and restoration.

| Topic             | Option A: internal lock | Option B: public helper | Option C: single-threaded docs | Option D: env-lock dependency |
| ----------------- | ----------------------- | ----------------------- | ------------------------------ | ----------------------------- |
| API surface       | No new public API       | Adds a new public guard | No change                      | No change                     |
| Safety by default | Yes                     | Only if used            | No                             | Yes                           |
| Dependencies      | None                    | None                    | None                           | Adds `env-lock`               |

_Table 1: Trade-offs between locking options._

## Decision outcome / proposed direction

Option A is selected. The internal `ScopedEnv` guard acquires a process-global
mutex and holds it for the lifetime of test helpers, keeping environment values
stable throughout a test scope.

## Implementation status

Implementation is complete.

- `ENV_LOCK` and `ThreadState` live in `src/env/state.rs`.
- `ScopedEnv` in `src/env/mod.rs` acquires the lock, handles re-entrancy, and
  clears poisoned locks.
- `TestCluster` stores an `_env_guard` to keep the lock for the cluster
  lifetime.
- The crate does not mutate the current working directory, so no directory
  lock is implemented.

## Testing

Unit tests in `src/env/tests/mod.rs` cover the mutex behaviour and recovery
paths. Acceptance checks are:

- Poisoned lock recovery:
  - The test must recover from a poisoned lock, allow subsequent lock
    acquisition, and complete new `ScopedEnv` operations without panics.
- Nested scopes:
  - Inner and outer guards must enforce ordering and resource visibility so the
    inner value is visible within the inner scope, the outer value is restored
    after the inner guard drops, and the value is removed after the outer guard
    drops.
- Corrupt state recovery:
  - `ThreadState` must restore depth to `0`, empty the stack, release the lock,
    and restore the environment value to the pre-test original after recovery.
- Cross-thread serialization:
  - The serialized value sent across the channel must match the expected value
    deterministically, and the receiver must successfully deserialize it into
    the expected environment string.
- Concurrency:
  - The second thread must block until the outer guard is dropped, then observe
    the expected consistent environment value and the released mutex.

## Known risks and limitations

- Parallel test helpers become serialized within a process, reducing
  concurrency for heavyweight integration tests.
- Cross-process coordination is out of scope; callers should isolate data and
  runtime directories.
- Other global state remains unprotected.
