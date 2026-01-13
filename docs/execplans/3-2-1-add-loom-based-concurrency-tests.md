# Add Loom-based ScopedEnv concurrency tests

This ExecPlan is a living document. The sections `Constraints`, `Tolerances`,
`Risks`, `Progress`, `Surprises & Discoveries`, `Decision Log`, and
`Outcomes & Retrospective` must be kept up to date as work proceeds.

Status: COMPLETE

PLANS.md was not found in the repository, so no additional plan governance
applies.

## Purpose / Big Picture

Introduce Loom-powered concurrency tests that exercise the `ScopedEnv` mutex so
we can model-check cross-thread interactions, while keeping these tests opt-in
behind a feature flag. Success means developers can run a documented Loom test
command, observe deterministic pass/fail results, and still have the normal
`make test` workflow remain green.

## Constraints

- Preserve existing public API behaviour for `ScopedEnv`, `TestCluster`, and
  the CLI; no breaking changes.
- Keep `ScopedEnv` non-Send and non-Sync semantics intact.
- Any new feature flag must be opt-in and documented in
  `docs/developers-guide.md`.
- Loom usage must be fully gated behind a feature flag and must not affect
  non-test builds.
- Follow Rust module rules: any new module must start with a `//!` comment.
- Keep file sizes under 400 lines; split modules if needed.
- Use en-GB-oxendict spelling in comments and documentation.
- Use caret dependency versions only (no `*`, `>=`, or `~`).

## Tolerances (Exception Triggers)

- Scope: if the change touches more than 15 files or exceeds 600 net lines,
  stop and escalate.
- Interface: if a public API signature must change, stop and escalate.
- Dependencies: if anything beyond `loom` (or an already-present crate) is
  needed, stop and escalate.
- Iterations: if tests still fail after three fix attempts, stop and escalate.
- Time: if any single stage takes longer than four hours, stop and escalate.
- Ambiguity: if multiple valid feature-flag or Loom integration approaches
  appear viable with materially different outcomes, stop and present options.

## Risks

    - Risk: Loom cannot safely model `ScopedEnv` because it mutates the real
      process environment across repeated model runs.
      Severity: high
      Likelihood: medium
      Mitigation: design Loom tests to use empty env change sets and/or inject
      a test-only environment store so no real env state leaks across runs.

    - Risk: `loom::sync::Mutex` does not provide a `const fn new`, forcing a
      change in how `ENV_LOCK` is initialized.
      Severity: medium
      Likelihood: low
      Mitigation: be prepared to introduce a small sync abstraction module or
      `OnceLock` initializer that works for both std and Loom builds.

    - Risk: `make test` with `--all-features` unexpectedly runs Loom tests and
      becomes slow or flaky.
      Severity: medium
      Likelihood: medium
      Mitigation: mark Loom tests `#[ignore]` and require an explicit
      `-- --ignored` run command so standard runs remain fast.

## Progress

    - [x] (2026-01-12 00:00Z) Drafted ExecPlan for roadmap item 3.2.1.
    - [x] (2026-01-12 18:05Z) Reviewed `ScopedEnv` implementation and env tests
      for concurrency assumptions.
    - [x] (2026-01-12 18:05Z) Chose to gate Loom tests behind the
      `loom-tests` feature and mark them `#[ignore]` by default.
    - [x] (2026-01-12 20:10Z) Implemented a `ScopedEnv` state accessor hook so
      Loom tests can use a Loom-specific lock without touching `ENV_LOCK`.
    - [x] (2026-01-12 20:15Z) Added Loom-backed concurrency tests under the
      feature flag and marked them `#[ignore]`.
    - [x] (2026-01-12 20:20Z) Updated design notes, developer guidance, and the
      roadmap entry; recorded that behavioural tests are not applicable.
    - [x] (2026-01-12 22:15Z) Ran `make check-fmt`, `make lint`, `make test`,
      and Loom tests with `cargo test --features "loom-tests" --lib -- --ignored`.

## Surprises & Discoveries

    - Observation: `RUSTFLAGS="--cfg loom"` disables `tokio::net` behind
      `cfg(not(loom))`, causing `hyper-util` builds to fail.
      Evidence: /tmp/loom.log shows `tokio::net` missing when compiling
      `hyper-util` under `cfg(loom)`.
      Impact: switch to feature-only Loom gating and mark Loom tests
      `#[ignore]` to avoid global `cfg(loom)`.

    - Observation: Loom sync primitives panic when used outside the
      `loom::model` scheduler.
      Evidence: running `cargo test --features loom-tests` without `loom::model`
      triggered panics in `loom::sync` types.
      Impact: keep standard primitives in production and provide Loom-specific
      state only inside the Loom test module.

    - Observation: `cargo test --features loom-tests -- --ignored` still
      compiles integration tests.
      Evidence: build errors in `tests/` when `loom-tests` is the only feature
      enabled.
      Impact: document `cargo test --features "loom-tests" --lib -- --ignored`
      to target library tests only.

## Decision Log

    - Decision: Gate Loom tests behind the `loom-tests` feature and mark
      them `#[ignore]` so `make test` does not automatically run the
      model-checking suite.
      Rationale: avoids `cfg(loom)` disabling Tokio networking while keeping
      Loom checks opt-in for focused runs.
      Date/Author: 2026-01-12 / Codex

    - Decision: Keep `ENV_LOCK` on `std::sync::Mutex` and keep `ScopedEnv`
      concrete, exposing a private state accessor hook so Loom tests can use
      their own lock and thread-local state.
      Rationale: Loom primitives must only run under `loom::model` and should
      not leak into production or standard test builds.
      Date/Author: 2026-01-12 / Codex

## Outcomes & Retrospective

- Delivered Loom-backed `ScopedEnv` concurrency tests gated behind the
  `loom-tests` feature, with a dedicated Loom lock and thread-local state.
- Verified the standard gates (`make check-fmt`, `make lint`, `make test`) and
  the Loom suite (`cargo test --features "loom-tests" --lib -- --ignored`).
- Noted that the `database_lifecycle` integration tests are long-running under
  `make test`, so plan for extended runtimes during validation.

## Context and Orientation

The `ScopedEnv` guard lives in `src/env/mod.rs` and uses a global mutex
`ENV_LOCK` defined in `src/env/state.rs`. The guard is re-entrant on a single
thread via a thread-local `ThreadState`, but serializes across threads via the
mutex. Current tests live in `src/env/tests/` and use `serial_test` to avoid
cross-test environment corruption. There are no Loom tests yet and no feature
flag for them. Behavioural tests (rstest-bdd) live in `tests/` with feature
files under `tests/features/`.

The roadmap entry to complete is in `docs/roadmap.md` under 3.2.1. The design
history and decisions are tracked in
`docs/zero-config-raii-postgres-test-fixture-design.md`. Developer-facing
testing notes live in `docs/developers-guide.md`; user-facing behaviour is
captured in `docs/users-guide.md`.

## Plan of Work

Stage A: Review and plan the Loom integration. Identify where `ENV_LOCK` and
`ThreadState` are used, confirm how existing tests protect the process
environment, and decide on the Loom gating strategy (feature only vs feature
plus `#[ignore]`). Record the decision in the design document.

Stage B: Add the Loom feature flag and any required sync abstraction. Introduce
an optional `loom` dependency and a `loom-tests` feature. If needed, add a
small `src/env/sync.rs` module to alias `Mutex`, `MutexGuard`, and
`PoisonError` to std or Loom types. Ensure `ENV_LOCK` initialization works for
both backends. Add module-level docs for any new module.

Stage C: Implement Loom-based concurrency tests. Add a new test module under
`src/env/tests/` (for example `loom.rs`) gated by the feature flag. Use
`loom::model` with a bounded scheduler configuration to keep runs small.
Construct tests that use empty env changes so no global environment state is
mutated, while still exercising mutex acquisition and release across threads.
Cover at least one happy path (serialized access) and one unhappy/edge path
(e.g. nested scopes across threads or drop ordering). Keep tests deterministic
and scoped.

Stage D: Add behavioural tests with rstest-bdd where applicable. If the new
behaviour can be expressed as observable outcomes, add a `.feature` file and
step definitions under `tests/` that verify scoped environment serialization
and restoration. If no meaningful user-visible behaviour exists, explicitly
record that rationale in the design document instead of adding forced BDD
coverage.

Stage E: Documentation and roadmap updates. Document how to run Loom tests in
`docs/developers-guide.md`, update `docs/users-guide.md` only if user-visible
behaviour changes, and record the Loom-testing decision in
`docs/zero-config-raii-postgres-test-fixture-design.md`. Mark roadmap item
3.2.1 as done in `docs/roadmap.md` once tests and docs land.

Each stage ends with validation; do not move to the next stage if validation
fails.

## Concrete Steps

1. Re-scan the codebase for `ScopedEnv`, `ENV_LOCK`, and existing tests:

    rg -n "ScopedEnv|ENV_LOCK" src tests

2. Inspect `src/env/mod.rs`, `src/env/state.rs`, and `src/env/tests/` to confirm
   where the mutex is used and how the environment is mutated.

3. Update `Cargo.toml` to add an optional `loom` dependency and a
   `loom-tests` feature flag. Decide on a version that supports Rust 1.85.

4. Add any required sync abstraction module (for example `src/env/sync.rs`) and
   switch `state.rs` to use the alias types so the mutex can be Loom-backed
   under the feature.

5. Add Loom tests under `src/env/tests/loom.rs` (or equivalent) gated by the
   feature and marked `#[ignore]`. Use `loom::model` with a bounded schedule,
   and ensure tests operate on empty env change sets so global env state is
   unchanged across runs.

6. Add or update behavioural tests using rstest-bdd (v0.3.2). If adding new
   `.feature` files, place them under `tests/features/` and wire up scenario
   macros in a new or existing `tests/*.rs` file.

7. Update documentation:
   - Record the Loom-testing decision in
     `docs/zero-config-raii-postgres-test-fixture-design.md`.
   - Add a Loom test run section in `docs/developers-guide.md`.
   - Update `docs/users-guide.md` only if user-visible behaviour changes.
   - Mark 3.2.1 as done in `docs/roadmap.md`.

8. Run validation commands (use `tee` for long output):

    make check-fmt | tee /tmp/check-fmt.log
    make lint | tee /tmp/lint.log
    make test | tee /tmp/test.log

   For Loom tests, run the documented command (for example):

    cargo test --features "loom-tests" --lib -- --ignored | tee /tmp/loom.log

   Adjust the command to include any required env vars if the gating approach
   needs them.

## Validation and Acceptance

Behavioural acceptance:

- Running the Loom test command described in `docs/developers-guide.md` passes
  and exercises at least one multi-threaded `ScopedEnv` scenario.
- The new Loom tests fail if the mutex is removed or if serialization is
  broken (prove this by observing a failing test before the fix when possible).
- Any new rstest-bdd scenarios pass and cover at least one happy and one
  unhappy/edge path.

Quality criteria:

- Tests: `make test` succeeds; Loom tests pass under the documented command.
- Lint/typecheck: `make lint` succeeds without warnings.
- Formatting: `make check-fmt` succeeds.
- Docs: `make markdownlint` and `make fmt` run after documentation changes;
  `make nixie` passes if Mermaid diagrams are added or modified.

## Idempotence and Recovery

All steps should be re-runnable. If a Loom test introduces flakiness, reduce
its scheduling bounds or scope first rather than silencing failures. If feature
gating causes unexpected build failures, revert to the last clean commit and
re-apply the change in smaller increments.

## Artifacts and Notes

Capture any relevant test logs in `/tmp/*.log` files created via `tee` and
summarize the pass/fail state in the final report.

## Interfaces and Dependencies

- `Cargo.toml`: add `loom` as an optional dependency with a caret version and
  add a `loom-tests` feature (for example `loom-tests = ["dep:loom"]`).
- `src/env/state.rs`: use alias types for `Mutex`, `MutexGuard`, and
  `PoisonError` so Loom can substitute the sync primitives when enabled.
- `src/env/tests/loom.rs`: add Loom-specific tests using `loom::model` and
  `loom::thread`.
- `docs/developers-guide.md`: document the Loom test command and any required
  environment variables.

## Revision note

Initial draft created to cover roadmap item 3.2.1.

2026-01-12: Marked plan in progress, recorded the review, and set the Loom
feature gating approach.
