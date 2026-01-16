# Confirm Linux root and unprivileged CI coverage

This ExecPlan is a living document. The sections `Constraints`, `Tolerances`,
`Risks`, `Progress`, `Surprises & Discoveries`, `Decision Log`, and
`Outcomes & Retrospective` must be kept up to date as work proceeds.

Status: COMPLETE

No `PLANS.md` file exists in the repository.

## Purpose / Big Picture

Deliver explicit CI coverage for both Linux privilege paths (root and
unprivileged) and document the expected behaviour on macOS and Windows in the
roadmap appendix. Success is observable when CI runs two Linux jobs that each
exercise the corresponding privilege path, tests pass in both contexts, and the
roadmap appendix describes the macOS/Windows expectations. Users and
contributors should be able to read the documentation and understand which
platforms and privilege levels are supported without needing to inspect the
code.

## Constraints

- Preserve existing public APIs and CLI behaviour unless a change is required
  by the roadmap task. Any public API change requires escalation.
- Use `rstest-bdd` v0.3.2 for behavioural tests and keep unit tests alongside
  the relevant modules.
- Keep documentation wrapped at 80 columns and follow en-GB-oxendict spelling
  rules.
- Prefer Makefile targets (`make check-fmt`, `make lint`, `make test`,
  `make markdownlint`, `make nixie`, `make fmt`) over ad-hoc commands.
- Update `docs/zero-config-raii-postgres-test-fixture-design.md` with any
  design decisions taken during implementation.
- Update `docs/users-guide.md` and `docs/developers-guide.md` for any user or
  contributor-visible behaviour changes.
- Mark the roadmap task as done once the feature is complete.

## Tolerances (Exception Triggers)

- Scope: if changes require more than 8 files or more than 300 net lines of
  code, stop and escalate.
- Interface: if any public API signature must change, stop and escalate.
- Dependencies: if a new dependency (beyond confirming `rstest-bdd` v0.3.2) is
  required, stop and escalate.
- Tests: if tests still fail after 2 targeted fixes, stop and escalate.
- Time: if any single milestone exceeds 3 hours, stop and escalate.
- Ambiguity: if multiple valid CI strategies (container vs sudo vs matrix
  scripts) remain after initial research, stop and present options with
  trade-offs.

## Risks

- Risk: GitHub Actions runners may not permit the desired root/unprivileged
  split without containerization or `sudo`. Severity: medium Likelihood: medium
  Mitigation: prototype the CI commands locally or in a minimal workflow
  snippet, and document the chosen approach with reasoning.
- Risk: behavioural tests might rely on global environment mutation that
  becomes flaky when run in parallel CI jobs. Severity: medium Likelihood: low
  Mitigation: reuse existing scoped environment helpers and keep tests
  serialized where required.
- Risk: macOS/Windows expectations might be unclear in existing docs.
  Severity: low Likelihood: medium Mitigation: cross-reference the design
  document and update the roadmap appendix with explicit expected outcomes and
  rationale.

## Progress

- [x] (2026-01-13 00:00Z) Drafted initial ExecPlan.
- [x] (2026-01-13 00:05Z) Plan approved to proceed with implementation.
- [x] (2026-01-13 00:15Z) Inspect existing CI workflow, privilege tests,
  and documentation references.
- [x] (2026-01-13 01:05Z) Add tests covering root/unprivileged happy paths
  plus the missing-worker error path.
- [x] (2026-01-13 01:15Z) Extend CI workflow to run Linux root and
  unprivileged matrix jobs.
- [x] (2026-01-13 01:30Z) Update roadmap appendix, user guide, developer
  guide, and design notes; mark roadmap task done.
- [x] (2026-01-13 02:10Z) Add cross-process serialization guards and a
  nextest serial group for PostgreSQL behavioural suites.
- [x] (2026-01-13 02:20Z) Surface bootstrap errors when behavioural worlds
  fail to create a `TestCluster`.
- [x] (2026-01-13 02:45Z) Run all quality gates and capture outputs.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Use a CI matrix with unprivileged coverage via the existing
  shared coverage action and a root variant that runs `make test` under `sudo`
  to exercise privilege-aware paths. Rationale: Keeps coverage collection
  unchanged while adding explicit root coverage without altering the shared
  action. Date/Author: 2026-01-13 (Codex)
- Decision: Serialize PostgreSQL behavioural suites across test binaries
  with a shared lock file and a nextest serial test group. Rationale: Prevents
  concurrent bootstrap/download races that caused intermittent CI failures
  while keeping other tests parallel. Date/Author: 2026-01-13 (Codex)

## Outcomes & Retrospective

- Added explicit root/unprivileged CI coverage alongside new behavioural and
  unit checks for privilege handling.
- Serialized PostgreSQL behavioural suites across binaries to stabilize
  nextest runs and improved error messages when bootstraps fail.

## Context and Orientation

The current CI definition lives at `.github/workflows/ci.yml` and runs a single
Ubuntu job that formats, lints, and tests the workspace. The roadmap task in
`docs/roadmap.md` requires explicit confirmation that both Linux privilege
paths (root and unprivileged) run in CI and that macOS/Windows expectations are
recorded in a roadmap appendix. The privilege logic is described in
`docs/zero-config-raii-postgres-test-fixture-design.md` and is exercised by
existing `rstest-bdd` scenarios under `tests/` and `tests/features/`. User and
developer expectations live in `docs/users-guide.md` and
`docs/developers-guide.md`. Guidance for test structure and documentation lives
in `docs/rust-testing-with-rstest-fixtures.md`,
`docs/rust-doctest-dry-guide.md`,
`docs/reliable-testing-in-rust-via-dependency-injection.md`,
`docs/ortho-config-users-guide.md`, and `docs/rstest-bdd-users-guide.md`.

For this task, "root path" means running the bootstrap flow with effective user
ID (UID) 0 on Linux so the worker process is exercised, while "unprivileged
path" means running as a standard user. The CI change must make these paths
observable in separate matrix jobs so failures can be diagnosed independently.

## Plan of Work

Stage A: Review current CI workflow, test coverage, and privilege-detection
behaviour. Identify where root and unprivileged flows are currently tested and
what gaps exist for CI observability. If multiple CI approaches are viable
(e.g., containerized root job vs `sudo`), evaluate trade-offs and record the
choice in the Decision Log.

Stage B: Add or adjust unit and behavioural tests to explicitly confirm the
root and unprivileged paths. Use `rstest-bdd` v0.3.2 for behavioural coverage
and ensure scenarios include both happy and unhappy paths (for example, root
path success and a failure when privilege changes are unavailable). Keep tests
aligned with existing fixtures and scoped environment helpers to avoid global
state leakage.

Stage C: Update `.github/workflows/ci.yml` to introduce a Linux matrix that
runs the root and unprivileged jobs independently. Ensure both jobs execute the
same quality gates (format, lint, tests) and surface which privilege path is
being exercised. Capture the expected outputs in the ExecPlan once known.

Stage D: Update documentation. Add a roadmap appendix section describing the
expected macOS and Windows behaviour, and mark the roadmap task as done. Update
`docs/users-guide.md` and `docs/developers-guide.md` with any behavioural or
CI-related clarifications. Record design decisions in
`docs/zero-config-raii-postgres-test-fixture-design.md`.

Each stage ends by running the relevant validation steps before proceeding.

## Concrete Steps

1. Inspect current CI and test structure:

   - `rg --line-number "rstest_bdd|scenario|ExecutionPrivileges" tests src`
   - `sed -n '1,240p' .github/workflows/ci.yml`
   - `rg --line-number "root|unprivileged|privilege" docs/roadmap.md`

2. Establish baseline tests (optional but preferred) and capture output with
   `tee`:

   - `make test | tee /tmp/test-$(get-project)-$(git branch --show).out`

3. Implement or adjust tests (unit + behavioural) and feature files. Add any
   new `.feature` files under `tests/features/` and reference them with
   `#[scenario]` so `rstest-bdd` v0.3.2 drives them.

4. Update `.github/workflows/ci.yml` to add the Linux matrix and any required
   environment setup (e.g., root execution via container or `sudo`). Keep the
   existing quality gates intact for each matrix entry.

5. Update docs:

   - Add or extend a roadmap appendix section in `docs/roadmap.md` describing
     macOS and Windows expected outcomes.
   - Update `docs/users-guide.md` with any user-facing expectations for
     privilege support or platform limitations.
   - Update `docs/developers-guide.md` with CI or testing notes for privilege
     paths.
   - Record design decisions in
     `docs/zero-config-raii-postgres-test-fixture-design.md`.

6. Run quality gates after changes, capturing outputs:

   - `make check-fmt | tee /tmp/check-fmt-$(get-project)-$(git branch --show).out`
   - `make markdownlint | tee /tmp/markdownlint-$(get-project)-$(git branch --show).out`
   - `make nixie | tee /tmp/nixie-$(get-project)-$(git branch --show).out`
   - `make lint | tee /tmp/lint-$(get-project)-$(git branch --show).out`
   - `make test | tee /tmp/test-$(get-project)-$(git branch --show).out`
   - `make fmt` after documentation changes, if formatter reports updates.

7. Mark the roadmap entry for this task as done in `docs/roadmap.md`, then
   commit each logical change with a descriptive message and ensure gates pass
   before each commit.

## Validation and Acceptance

Success means:

- CI includes separate Linux matrix jobs for root and unprivileged execution.
- New or updated unit tests and `rstest-bdd` scenarios cover happy and unhappy
  paths for privilege handling.
- `docs/roadmap.md` contains an appendix describing expected macOS/Windows
  outcomes, and the Phase 3 task for CI confirmation is marked done.
- `docs/users-guide.md`, `docs/developers-guide.md`, and
  `docs/zero-config-raii-postgres-test-fixture-design.md` are updated to
  reflect the new expectations and decisions.
- `make check-fmt`, `make lint`, `make test`, `make markdownlint`, and
  `make nixie` succeed with clean logs.

Quality method:

- Run the Makefile targets listed in the Concrete Steps and confirm the new
  tests fail before the change and pass after.

## Idempotence and Recovery

All steps should be re-runnable. If a CI change fails, revert the workflow file
and reapply the edits with a smaller matrix or a simpler privilege strategy. If
tests become flaky, isolate the scenario or run with serial execution and
record the decision in the design document.

## Artifacts and Notes

Capture key command outputs in `/tmp/*-$(get-project)-$(git branch --show).out`
files so failures can be reviewed after truncated command output. Include any
notable CI error messages in the Decision Log for future reference.

## Interfaces and Dependencies

- `rstest-bdd` must remain at v0.3.2 in `Cargo.toml` and should be used for all
  new behavioural scenarios in this change.
- CI configuration changes are limited to `.github/workflows/ci.yml` unless a
  new workflow file is explicitly required.
- Tests should continue to use existing helpers (for example
  `test_support::with_scoped_env`) to avoid global environment races.

## Revision note (2026-01-13)

Updated the status to IN PROGRESS and recorded plan approval plus the initial
discovery step completion in Progress to reflect the start of implementation.

Recorded completed test additions, CI workflow updates, and documentation
changes in Progress, and logged the CI strategy decision so remaining work is
limited to running quality gates and capturing outputs.
