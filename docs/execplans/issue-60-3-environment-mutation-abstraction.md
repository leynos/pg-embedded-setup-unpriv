# Issue 60.3: Environment Mutation Abstraction

This execution plan (ExecPlan) is a living document. The sections
`Constraints`, `Tolerances`, `Risks`, `Progress`, `Surprises & Discoveries`,
`Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work
proceeds.

Status: COMPLETE

## Purpose / big picture

Create a testable environment variable abstraction for the `pg_worker` binary
that enables direct unit testing of environment mutation logic without relying
on the real process environment. The intended user-visible outcome is that the
environment application logic can be verified deterministically through
in-memory tests, while production code continues to safely mutate the real
process environment.

After this change:

- The `apply_worker_environment` function accepts a `&mut dyn EnvStore`
  parameter
- Production code uses `ProcessEnvStore` which safely wraps `env::set_var` and
  `env::remove_var` with explicit SAFETY comments
- Tests can use `TestEnvStore` with an in-memory `HashMap` for deterministic
  assertions
- A new test demonstrates `TestEnvStore`'s `get` method for verifying set and
  remove operations

## Constraints

Hard invariants that must hold throughout implementation. These are not
suggestions; violation requires escalation, not workarounds.

- Do not modify `tests/support/pg_worker_helpers.rs`. The existing
  `EnvironmentOperations` trait and `apply_worker_environment_with` helper must
  remain unchanged.
- Do not remove or modify the existing `apply_worker_environment` function's
  signature in a way that breaks existing callers; the function must continue
  to work correctly after the refactor.
- Do not introduce new external dependencies. Use only existing crates from
  `Cargo.toml`.
- Follow repository quality gates: run `make check-fmt`, `make lint`, and
  `make test` (using `tee` logs) before committing.
- Maintain en-GB-oxendict spelling in documentation edits.
- Use repository-relative paths or generic placeholders when documenting
  commands; avoid local absolute paths.
- All unsafe blocks must include explicit SAFETY comments explaining why the
  operation is safe in this context.
- The `EnvStore` trait and its implementations must be placed in
  `tests/support/pg_worker.rs`, not in a separate module or file.

## Tolerances (exception triggers)

Thresholds that trigger escalation when breached. These define the boundaries
of autonomous action, not quality criteria.

- Scope: if the change requires editing more than 2 files or exceeds 150 net
  lines of code, stop and escalate.
- Interface: if any public API outside `tests/support/pg_worker.rs` must
  change, stop and escalate.
- Dependencies: if a new external dependency is required, stop and escalate.
- Iterations: if tests still fail after two retries, stop and escalate with
  failure logs.
- Ambiguity: if the requirements conflict with existing patterns or leave
  critical design decisions underspecified, stop and ask for direction.

## Risks

Known uncertainties that might affect the plan. Identify these upfront and
update as work proceeds. Each risk should note severity, likelihood, and
mitigation or contingency.

- Risk: The existing `EnvironmentOperations` trait in `pg_worker_helpers.rs`
  might confuse developers or cause duplication. Severity: low Likelihood:
  medium Mitigation: Leave the existing trait untouched; the new `EnvStore`
  trait is intentionally separate and uses mutable references, making the
  distinction clear in code. Document the rationale if needed.
- Risk: Adding mutable trait methods might cause borrow checker issues when
  passing the store through the async runtime. Severity: low Likelihood: low
  Mitigation: The store will be created and consumed before entering the async
  block (in `run_worker`), avoiding any cross-async-boundary borrowing issues.
- Risk: The existing test `apply_worker_environment_uses_plaintext_and_unsets`
  uses `MockEnvironmentOperations` and might conflict with the new approach.
  Severity: low Likelihood: low Mitigation: The existing test will continue to
  use the helper from `pg_worker_helpers.rs`; the new test for `TestEnvStore`
  will be separate and demonstrate the new abstraction's benefits.

## Progress

Use a list with checkboxes to summarize granular steps. Every stopping point
must be documented here, even if it requires splitting a partially completed
task into two ("done" vs. "remaining"). This section must always reflect the
actual current state of the work.

- [x] (2026-01-21 21:20Z) Draft ExecPlan and await user approval.
- [x] (2026-01-21 21:21Z) Add `EnvStore` trait definition with `set` and
  `remove` methods to `tests/support/pg_worker.rs`.
- [x] (2026-01-21 21:21Z) Implement `ProcessEnvStore` struct with SAFETY
  comments for unsafe operations.
- [x] (2026-01-21 21:21Z) Implement `TestEnvStore` struct with
  `HashMap<String, Option<String>>` storage and `get` method for assertions.
- [x] (2026-01-21 21:21Z) Refactor `apply_worker_environment` to accept
  `&mut dyn EnvStore` parameter.
- [x] (2026-01-21 21:22Z) Update `run_worker` to create `ProcessEnvStore`
  and pass to `apply_worker_environment`.
- [x] (2026-01-21 21:22Z) Add unit test for `TestEnvStore`
  demonstrating `get`, `set`, and `remove`.
- [x] (2026-01-21 21:28Z) Run `make check-fmt`, `make lint`, and `make
  test` to validate all changes.
- [x] (2026-01-21 21:29Z) Commit the changes with a descriptive message.

## Surprises & discoveries

Unexpected findings during implementation that were not anticipated as risks.
Document with evidence so future work benefits.

## Decision log

Record every significant decision made while working on the plan. Include
decisions to escalate, decisions on ambiguous requirements, and design choices.

- Decision: Add `#[must_use]` attributes to `TestEnvStore::new()` and
  `TestEnvStore::get()` methods, and implement `Default` trait for
  `TestEnvStore`. Rationale: Clippy requires `#[must_use]` for methods
  returning values that should not be ignored, and suggests `Default`
  implementation when a `new()` method exists. Date/Author: 2026-01-21 (Codex).

## Outcomes & retrospective

Summarize outcomes, gaps, and lessons learned at major milestones or at
completion. Compare the result against the original purpose. Note what would be
done differently next time.

All objectives achieved:

- `EnvStore` trait successfully abstracts environment variable mutation
  with `set` and `remove` methods.
- `ProcessEnvStore` wraps real `env::set_var` and `env::remove_var` with
  explicit SAFETY comments.
- `TestEnvStore` provides in-memory storage with `HashMap<String,
  Option<String>>` and a `get` method for test assertions.
- `apply_worker_environment` refactored to accept `&mut dyn EnvStore`
  parameter.
- `run_worker` updated to create `ProcessEnvStore` and pass to
  `apply_worker_environment`.
- New test `test_env_store_test_impl_get_set_and_remove` demonstrates
  deterministic testing without touching real process environment.
- All 117 tests pass, including the new test and all existing tests.

No tolerance breaches encountered. All quality gates passed:

- `make check-fmt`: succeeded
- `make lint`: succeeded (after adding `#[must_use]` attributes and
  `Default` implementation)
- `make test`: all 117 tests passed

Changes were confined to a single file (tests/support/pg_worker.rs), with 88
lines added, and 10 lines removed, well within the 150-line tolerance.

## Rebase notes

Rebased onto origin/main (commit db74783: Issue 60.5 - Implement idempotent
lifecycle helpers). One conflict was automatically resolved by zdiff3 in the
imports section:

Conflict: Both branches modified imports in pg_worker.rs.

- origin/main added: `use postgresql_embedded::{PostgreSQL, Status};`
  and `use tracing::info;`
- Issue 60.3 added: `use std::collections::HashMap;`

Resolution: zdiff3 correctly preserved all imports, combining `PostgreSQL`,
`Status`, `HashMap`, and `tracing::info`.

The rebase successfully integrated Issue 60.3's `EnvStore` trait and
implementations with Issue 60.5's lifecycle helpers. Both features now coexist
in the same file:

- Issue 60.5: Lifecycle state management (extract_data_dir,
  is_setup_complete, ensure_postgres_setup, ensure_postgres_started)
- Issue 60.3: Environment mutation abstraction (EnvStore trait,
  ProcessEnvStore, TestEnvStore)

All quality gates passed after rebase:

- `make check-fmt`: succeeded
- `make test`: all 117 tests passed (including Issue 60.5's new tests)
- `make lint`: succeeded (no typecheck target in this project)

## Context and orientation

Describe the current state relevant to this task as if the reader knows
nothing. Name the key files and modules by full path. Define any non-obvious
term you will use. Do not refer to prior plans.

The `pg_worker` binary is a privileged worker process invoked by the main
`pg_embedded_setup_unpriv` library to perform PostgreSQL bootstrap operations.
The worker receives a `WorkerPayload` via JavaScript Object Notation (JSON)
containing PostgreSQL settings and environment variable overrides. The
`apply_worker_environment` function applies these environment overrides to the
current process before executing the requested operation (setup, start, or
stop).

Currently, `apply_worker_environment` in `tests/support/pg_worker.rs` (lines
208-221) directly calls `unsafe { env::set_var }` and
`unsafe { env::remove_var }` to mutate the real process environment. While the
SAFETY comments explain that this is safe because the worker is
single-threaded, this direct mutation makes the function difficult to test.
Existing tests use an `EnvironmentOperations` trait from
`tests/support/pg_worker_helpers.rs`, but that trait uses immutable references
(`&self`) and is separate from the production code.

This plan introduces a new `EnvStore` trait that:

- Defines `set(&mut self, key: &str, value: &str)` and `remove(&mut self, key:
  &str)` methods
- Is placed directly in `tests/support/pg_worker.rs` alongside the production
  code
- Provides two implementations: `ProcessEnvStore` for production and
  `TestEnvStore` for testing
- Enables direct unit testing without relying on the real process environment

Key files:

- `tests/support/pg_worker.rs`: Contains the worker binary entry point and
  environment application logic (335 lines)
- `tests/support/pg_worker_helpers.rs`: Contains existing
  `EnvironmentOperations` trait and helper functions (85 lines) - **do not
  modify**
- `src/worker.rs`: Contains `WorkerPayload`, `SettingsSnapshot`, and
  `PlainSecret` types used by the worker

The `PlainSecret` type wraps a `SecretString` from the `secrecy` crate and
provides an `expose()` method to retrieve the plaintext value when needed.

## Plan of work

Describe, in prose, the sequence of edits and additions. For each edit, name
the file and location (function, module) and what to insert or change. Keep it
concrete and minimal.

Structure as stages with explicit go/no-go points where appropriate:

- Stage A: add trait and implementations (no changes to existing logic)
- Stage B: refactor `apply_worker_environment` to use the trait
- Stage C: update production call site and add tests
- Stage D: validation and commit

Each stage ends with validation. Do not proceed to the next stage if the
current stage's validation fails.

### Stage A: add trait and implementations

In `tests/support/pg_worker.rs`, after the `WorkerError` enum definition and
before the `Operation` enum (around line 76), add the `EnvStore` trait and
implementations:

1. Define the `EnvStore` trait with two methods:
   `fn set(&mut self, key: &str, value: &str)` and
   `fn remove(&mut self, key: &str)`. Add a doc comment explaining the trait's
   purpose for abstracting environment variable mutation for testing.

2. Implement `ProcessEnvStore` as a zero-sized struct (no fields) that wraps
   the real process environment. Implement `set` to call
   `unsafe { env::set_var(key, value) }` with a SAFETY comment explaining that
   the worker is single-threaded. Implement `remove` to call
   `unsafe { env::remove_var(key) }` with the same SAFETY comment.

3. Implement `TestEnvStore` struct with a single field:
   `env: HashMap<String, Option<String>>`. Implement `set` to insert
   `Some(value)` into the map, `remove` to insert `None`, and add a public
   `get(&self, key: &str) -> Option<&str>` method that returns `None` if the
   key is not in the map or if the stored value is `None`, otherwise returns a
   reference to the string. Add doc comments explaining the in-memory storage
   and `get` method's use in test assertions.

Validation: The code should compile with
`cargo check --bin pg_worker --features dev-worker`. No behavioural changes yet.

### Stage B: refactor `apply_worker_environment`

In `tests/support/pg_worker.rs`, modify the `apply_worker_environment` function
(currently lines 208-221):

1. Change the function signature from `fn apply_worker_environment(environment:
   &[(String, Option<PlainSecret>)])` to `fn apply_worker_environment(store:
   &mut dyn EnvStore, environment: &[(String, Option<PlainSecret)])`.

2. Update the function body to use the store parameter instead of calling
   `env::set_var` and `env::remove_var` directly. Replace the unsafe calls with
   `store.set(key, env_value.expose())` for `Some(env_value)` cases and
   `store.remove(key)` for `None` cases. Remove the SAFETY comments since the
   unsafe logic is now encapsulated in `ProcessEnvStore`.

3. Ensure the function remains in the same location (before the helper functions
   like `stop_missing_pid_is_ok`).

Validation: The code should still compile. No tests are expected to fail yet
since the call site has not been updated.

### Stage C: update production call site and add tests

In `tests/support/pg_worker.rs`, make two changes:

1. Update the `run_worker` function (currently line 110) to create a
   `ProcessEnvStore` instance before the async block and pass it to
   `apply_worker_environment`. Add `let mut env_store = ProcessEnvStore;`
   before the `apply_worker_environment` call, and change the call to
   `apply_worker_environment(&mut env_store, &payload.environment);`.

2. In the `#[cfg(test)]` mod `tests` block (starting at line 233), add a new
   test function `fn test_env_store_test_impl_get_set_and_remove()` that:
   - Creates a `TestEnvStore` instance
   - Calls `set("KEY", "value")`
   - Calls `remove("OTHER_KEY")`
   - Uses `get` to assert that `KEY` returns `Some("value")`, `OTHER_KEY`
     returns `None`, and an unset key returns `None`
   - Demonstrates that `TestEnvStore` can be used to verify environment
     mutations deterministically

3. Add `use std::collections::HashMap;` to the test module's imports if needed
   (check existing imports in the test module first).

Validation:

- The new test should pass, demonstrating `TestEnvStore`'s in-memory storage.
- The existing test `apply_worker_environment_uses_plaintext_and_unsets`
  should continue to pass as it uses the separate `EnvironmentOperations` trait
  from helpers.
- All tests in the file should pass with `cargo test --bin pg_worker
  --features dev-worker --lib`.

### Stage D: validation and commit

Run the full validation suite:

1. Run `make check-fmt` to verify formatting (no changes expected).
2. Run `make lint` to verify no clippy warnings.
3. Run `make test` to ensure all tests pass, including the new test.
4. Review the diff to ensure only the intended changes are present.

Commit the changes with a message following the repository's conventions, using
imperative mood and wrapping the body at 72 characters.

## Concrete steps

State the exact commands to run and where to run them (working directory). When
a command generates output, show a short expected transcript so the reader can
compare. This section must be updated as work proceeds.

### Verify Stage A: add trait and implementations

Working directory: repository root (i.e., `./` or `<repo-root>`).

Command to verify Stage A:

```bash
cargo check --bin pg_worker --features dev-worker
```

Expected output: compilation succeeds with no errors.

### Verify Stage B: refactor `apply_worker_environment`

Working directory: same as above.

Command to verify Stage B:

```bash
cargo check --bin pg_worker --features dev-worker
```

Expected output: compilation succeeds with no errors. Note: tests may not pass
yet because the call site has not been updated.

### Verify Stage C: update production call site and add tests

Working directory: same as above.

Commands to verify Stage C:

```bash
# Run the new test specifically
cargo test --bin pg_worker --features dev-worker test_env_store_test_impl_get_set_and_remove -- --nocapture

# Run all pg_worker tests
cargo test --bin pg_worker --features dev-worker --lib
```

Expected output: all tests pass, including the new
`test_env_store_test_impl_get_set_and_remove`.

### Verify Stage D: validation and commit

Working directory: same as above.

Commands to validate and commit:

```bash
# Format check
make check-fmt

# Lint
make lint

# Full test suite
make test 2>&1 | tee test-run.log

# Review the diff
git diff tests/support/pg_worker.rs

# Commit (only after all checks pass)
git add tests/support/pg_worker.rs
git commit -m "Introduce EnvStore trait for testable environment mutation"
```

Expected outputs:

- `make check-fmt`: no output (exit code 0)
- `make lint`: no warnings
- `make test`: all tests pass
- `git diff`: shows only the intended changes to `pg_worker.rs`

## Validation and acceptance

Describe how to start or exercise the system and what to observe. Phrase
acceptance as behaviour, with specific inputs and outputs. If tests are
involved, say "run <project's test command> and expect \`N\` passed; the new
test \`name\` fails before the change and passes after".

Quality criteria (what "done" means):

- Tests: all existing tests continue to pass; the new test
  `test_env_store_test_impl_get_set_and_remove` passes, demonstrating
  `TestEnvStore`'s in-memory storage and `get` method for assertions.
- Lint/typecheck: `make check-fmt`, `make lint`, and `make test` all pass.
- Behaviour: running `pg_worker setup` with a worker config containing
  environment overrides correctly applies those overrides to the real process
  environment; the test version uses `TestEnvStore` for deterministic
  verification.

Quality method (how we check):

- `cargo test --bin pg_worker --features dev-worker --lib`: runs all pg_worker
  tests, including the new test.
- `make test`: runs the full workspace test suite to ensure no regressions.
- Manual inspection of the diff to confirm only the intended file was modified.

## Idempotence and recovery

If steps can be repeated safely, say so. If a step is risky, provide a safe
retry or rollback path. Keep the environment clean after completion.

All steps in this plan are idempotent:

- Adding code can be repeated by checking for existing definitions first.
- Running validation commands is always safe.
- Git commits can be amended or reset as needed.

If a mistake is made, use `git checkout tests/support/pg_worker.rs` to restore
the file to its original state and start over. No destructive operations are
performed outside the git working tree.

## Artifacts and notes

Include the most important transcripts, diffs, or snippets as indented
examples. Keep them concise and focused on what proves success.

### New trait definition (Stage A)

The `EnvStore` trait and implementations should appear as:

```rust
/// Abstracts environment variable mutation for testability.
pub trait EnvStore {
    fn set(&mut self, key: &str, value: &str);
    fn remove(&mut self, key: &str);
}

/// Wraps the real process environment for production use.
pub struct ProcessEnvStore;

impl EnvStore for ProcessEnvStore {
    fn set(&mut self, key: &str, value: &str) {
        unsafe {
            // SAFETY: the worker is single-threaded; environment updates
            // cannot race.
            env::set_var(key, value);
        }
    }

    fn remove(&mut self, key: &str) {
        unsafe {
            // SAFETY: the worker is single-threaded; environment updates
            // cannot race.
            env::remove_var(key);
        }
    }
}

/// In-memory environment store for deterministic testing.
pub struct TestEnvStore {
    env: HashMap<String, Option<String>>,
}

impl TestEnvStore {
    pub fn new() -> Self {
        Self { env: HashMap::new() }
    }

    /// Returns the value for a key, or `None` if unset or removed.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.env.get(key).and_then(|v| v.as_deref())
    }
}

impl EnvStore for TestEnvStore {
    fn set(&mut self, key: &str, value: &str) {
        self.env.insert(key.to_owned(), Some(value.to_owned()));
    }

    fn remove(&mut self, key: &str) {
        self.env.insert(key.to_owned(), None);
    }
}
```

### Refactored `apply_worker_environment` (Stage B)

The function signature and body should change from:

```rust
fn apply_worker_environment(environment: &[(String, Option<PlainSecret>)]) {
    for (key, value) in environment {
        match value {
            Some(env_value) => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::set_var(key, env_value.expose());
            },
            None => unsafe {
                // SAFETY: the worker is single-threaded; environment updates cannot race.
                env::remove_var(key);
            },
        }
    }
}
```

To:

```rust
fn apply_worker_environment(
    store: &mut dyn EnvStore,
    environment: &[(String, Option<PlainSecret>)],
) {
    for (key, value) in environment {
        match value {
            Some(env_value) => store.set(key, env_value.expose()),
            None => store.remove(key),
        }
    }
}
```

### Updated call site (Stage C)

In `run_worker`, around line 110, change from:

```rust
apply_worker_environment(&payload.environment);
```

To:

```rust
let mut env_store = ProcessEnvStore;
apply_worker_environment(&mut env_store, &payload.environment);
```

### New test (Stage C)

Add to the `#[cfg(test)]` mod `tests` block:

```rust
#[test]
fn test_env_store_test_impl_get_set_and_remove() {
    let mut store = TestEnvStore::new();

    store.set("KEY", "value");
    store.remove("OTHER_KEY");

    assert_eq!(store.get("KEY"), Some("value"));
    assert_eq!(store.get("OTHER_KEY"), None);
    assert_eq!(store.get("UNSET"), None);
}
```

## Interfaces and dependencies

Be prescriptive. Name the libraries, modules, and services to use and why.
Specify the types, traits/interfaces, and function signatures that must exist
at the end of the milestone. Prefer stable names and paths such as
`crate::module::function` or `package.submodule.Interface`.

At the end of this implementation, the following must exist in
`tests/support/pg_worker.rs`:

1. `pub trait EnvStore` with methods:
   - `fn set(&mut self, key: &str, value: &str)`
   - `fn remove(&mut self, key: &str)`

2. `pub struct ProcessEnvStore` (zero-sized) implementing `EnvStore`

3. `pub struct TestEnvStore` with field `env: HashMap<String, Option<String>>`,
   implementing:
   - `pub fn new() -> Self`
   - `pub fn get(&self, key: &str) -> Option<&str>`
   - `EnvStore` trait

4. Function signature: `fn apply_worker_environment(store: &mut dyn EnvStore,
   environment: &[(String, Option<PlainSecret>)])`

No new external dependencies are required. The `HashMap` type is from the
standard library (`std::collections::HashMap`).
