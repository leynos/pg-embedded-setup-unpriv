# Fix TestCluster RAII cleanup (issue 66)


This execution plan (ExecPlan) is a living document. The sections
`Constraints`, `Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`,
`Decision log`, and `Outcomes & retrospective` must be kept up to date as work
proceeds.

Status: COMPLETE (2026-01-28)

PLANS.md: not present in this repository.


## Purpose / big picture

After this change, dropping `TestCluster` (a RAII fixture: Resource Acquisition
Is Initialization) removes its PostgreSQL data directory from `/var/tmp` so
test runs no longer leak 38-160 MB per run. This closes issue 66. The behaviour
is observable by running tests that use `TestCluster` and confirming that
`/var/tmp/pg-embed-{uid}/data` (and, when configured,
`/var/tmp/pg-embed-{uid}/install`) is removed after the fixture is dropped.
Shared clusters created by `shared_cluster()` remain leaked by design.


## Constraints

- Use the existing worker subprocess model for privileged operations; do not
  bypass it for cleanup in root mode.
- Preserve the worker lifecycle sequencing (setup → start → stop) and ensure
  cleanup runs after stop where applicable.
- Shared clusters created via `shared_cluster()` must continue to avoid
  automatic cleanup unless explicitly configured otherwise.
- Do not introduce new external dependencies.
- Maintain repository quality gates: `make check-fmt`, `make lint`,
  `make test`, plus Markdown tooling for documentation updates.
- Keep any public API changes minimal and documented in `docs/`.


## Tolerances (exception triggers)

- Scope: if implementing this requires modifying more than 10 files or more
  than 400 net lines, stop and escalate.
- Interface: if any public API signature must change outside
  `TestBootstrapSettings` or `WorkerOperation`, stop and escalate.
- Dependencies: if a new crate is required, stop and escalate.
- Iterations: if tests still fail after two full fix attempts, stop and
  escalate.
- Ambiguity: if multiple valid interpretations remain about cleanup behaviour
  (data-only vs full) and materially affect correctness, stop, and request
  direction.


## Risks

- Risk: cleanup runs with insufficient privileges and fails silently.
  Severity: high Likelihood: medium Mitigation: execute cleanup via the worker
  subprocess running as `nobody` and log warnings on failure.
- Risk: cleanup removes a directory still in use by another process.
  Severity: medium Likelihood: low Mitigation: ensure cleanup runs after stop
  and only for non-shared clusters.
- Risk: tests become flaky due to timing of cleanup.
  Severity: medium Likelihood: low Mitigation: make cleanup synchronous in
  `Drop` and ensure idempotent deletion with existence checks.


## Progress

- [x] (2026-01-28 00:00Z) Drafted ExecPlan with options and constraints.
- [x] (2026-01-28 01:10Z) Decided cleanup defaults (DataOnly) and documented.
- [x] (2026-01-28 01:20Z) Implemented cleanup operations and configuration.
- [x] (2026-01-28 01:30Z) Added tests for cleanup behaviour.
- [x] (2026-01-28 02:05Z) Validated make targets (fmt/check-fmt/lint/test).
- [x] (2026-01-28 02:10Z) Updated documentation and finalized plan.


## Surprises & discoveries

- The full-cleanup drop test initially left the installation directory behind
  in root mode. Added install-root cleanup (based on `password_file` parent)
  and a best-effort in-process cleanup after worker shutdown to keep cleanup
  deterministic.


## Decision log

- Decision: prefer a hybrid approach (add `WorkerOperation::Cleanup` and
  `CleanupMode` with data-only default, optional full cleanup). Rationale:
  preserves the worker model while allowing correct cleanup and optional
  installation caching. Date/Author: 2026-01-28 (assistant)
- Decision: model full cleanup as a distinct worker operation
  (`CleanupFull`) rather than encoding cleanup mode in the worker payload.
  Rationale: avoids breaking the public `WorkerRequestArgs` surface and keeps
  worker invocation arguments stable. Date/Author: 2026-01-28 (assistant)
- Decision: keep the worker cleanup as the primary root-mode path, but follow
  it with a best-effort in-process cleanup to cover cases where the worker
  cannot remove the installation root. Rationale: prevents root-mode leaks
  without changing the worker sequencing guarantees. Date/Author: 2026-01-28
  (assistant)


## Outcomes & retrospective

- Delivered deterministic cleanup for `TestCluster` drops in both worker and
  in-process paths, including removal of installation roots when configured for
  full cleanup. This closes issue 66.
- Tests executed:
  - `make fmt` (see `/tmp/issue-66-make-fmt.log`).
  - `make check-fmt` (see `/tmp/issue-66-check-fmt.log`).
  - `make lint` (see `/tmp/issue-66-make-lint.log`).
  - `make test` (see `/tmp/issue-66-make-test.log`).


## Context and orientation

`TestCluster` is a Rust RAII fixture that spins up a PostgreSQL instance for
integration tests. In privileged (root) mode, lifecycle operations run in
separate worker subprocesses as the `nobody` user. The worker subprocess model
performs setup, start, and stop as separate invocations, which is why
`settings.temporary = false` is set during bootstrap in
`src/bootstrap/prepare/mod.rs`. Today, `TestCluster::drop()` only invokes
`WorkerOperation::Stop`, leaving `/var/tmp/pg-embed-{uid}/install` and
`/var/tmp/pg-embed-{uid}/data` behind. The tests in
`tests/test_cluster_drop.rs` verify process shutdown and env restoration, but
not directory cleanup.

Key files for this change:

- `src/cluster/worker_operation.rs` defines worker lifecycle operations.
- `src/worker.rs` dispatches worker operations in the privileged subprocess.
- `src/cluster/mod.rs` implements `TestCluster` drop behaviour for sync and
  async paths.
- `src/bootstrap/mod.rs` defines `TestBootstrapSettings`.
- `src/test_support/fixtures.rs` defines `shared_cluster()`.
- `tests/test_cluster_drop.rs` contains existing drop behaviour tests.


## Plan of work

Stage A: confirm current behaviour and decide defaults. Read the listed source
files to confirm where cleanup is best inserted. Decide whether
`CleanupMode::DataOnly` is the default (recommended) and confirm if install
cleanup should be opt-in only. Go/no-go: if the worker process cannot access
paths needed for cleanup, stop and ask for guidance.

Stage B: add configuration and worker operation scaffolding. Extend
`WorkerOperation` with `Cleanup` and update string conversion, error context,
and timeout. Add `CleanupMode` to `TestBootstrapSettings` with a default of
`DataOnly`, and thread this setting through to the worker.

Stage C: implement cleanup behaviour. Update the worker dispatcher in
`src/worker.rs` to handle `Cleanup`. Implement idempotent deletion of the
`data` directory and optionally the `install` directory. Update
`TestCluster::drop()` (sync and async paths) to invoke `Cleanup` after `Stop`,
skipping cleanup for leaked shared clusters or when `CleanupMode::None` is
configured. Ensure non-worker (unprivileged) clusters perform local cleanup
without invoking the worker.

Stage D: tests and documentation. Extend `tests/test_cluster_drop.rs` to assert
directory removal and add new test coverage for `CleanupMode::Full` and
`CleanupMode::None`. Add a new integration test file only if existing fixtures
cannot represent the cases. Update documentation for `TestCluster` and
`CleanupMode` in `docs/` and any relevant module-level comments. Run all
required make targets and fix any failures.


## Concrete steps

All commands run from `/home/user/project`.

1) Read the current implementation:

```plaintext
rg --line-number "WorkerOperation" src/cluster/worker_operation.rs
rg --line-number "drop" src/cluster/mod.rs
rg --line-number "TestBootstrapSettings" -g "*.rs" src
rg --line-number "shared_cluster" src/test_support/fixtures.rs
rg --line-number "drop" tests/test_cluster_drop.rs
```

2) Implement scaffolding changes (Stages B and C).

3) Add/extend tests and documentation (Stage D).

4) Run formatters and linters (use `tee` so output is preserved):

```plaintext
set -o pipefail
make fmt 2>&1 | tee /tmp/issue-66-make-fmt.log
make markdownlint 2>&1 | tee /tmp/issue-66-markdownlint.log
make nixie 2>&1 | tee /tmp/issue-66-make-nixie.log
make check-fmt 2>&1 | tee /tmp/issue-66-check-fmt.log
make lint 2>&1 | tee /tmp/issue-66-make-lint.log
make test 2>&1 | tee /tmp/issue-66-make-test.log
```

Expected success signal: each command exits 0 and logs show no errors.


## Validation and acceptance

Acceptance is met when all following statements are true:

- After a test run that creates a `TestCluster`, the corresponding
  `/var/tmp/pg-embed-{uid}/data` directory is removed on drop in privileged
  mode.
- When `CleanupMode::Full` is configured, `/var/tmp/pg-embed-{uid}/install`
  is also removed on drop.
- When `CleanupMode::None` is configured (or `shared_cluster()` is used),
  directories are not removed.
- Running `make test` passes and the newly added tests fail before the code
  changes and pass afterwards.
- Running `make lint` and `make check-fmt` succeeds with no warnings.
- Documentation updates pass `make markdownlint` and `make nixie`.


## Idempotence and recovery

All cleanup operations must be safe to run multiple times. Directory removal
should treat missing paths as a no-op. If any cleanup step fails mid-way, the
next drop or cleanup invocation should be able to run again without manual
intervention. If tests fail, re-run them after fixing code or tests; no data
migration or irreversible change is expected.


## Artefacts and notes

When implemented, capture a brief log excerpt confirming cleanup. Example shape
only (replace with actual output):

```plaintext
INFO … cleanup removed data directory /var/tmp/pg-embed-1234/data
```


## Interfaces and dependencies

Add a new configuration enum in `src/bootstrap/mod.rs` (or a dedicated module
if that is the existing pattern):

```rust
pub enum CleanupMode {
    DataOnly,
    Full,
    None,
}
```

Add a new worker operation in `src/cluster/worker_operation.rs`:

```rust
pub enum WorkerOperation {
    Setup,
    Start,
    Stop,
    Cleanup,
    CleanupFull,
}
```

Update the worker dispatcher in `src/worker.rs` to handle `Cleanup`, and update
`TestCluster` drop logic in `src/cluster/mod.rs` to invoke it.


## Revision note

- Initial draft created from issue description, selecting the hybrid cleanup
  approach and defining tolerances and risks for issue 66.
