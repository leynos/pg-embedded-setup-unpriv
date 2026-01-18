# Gate integration tests behind cluster-unit-tests

This ExecPlan is a living document. The sections `Constraints`, `Tolerances`,
`Risks`, `Progress`, `Surprises & Discoveries`, `Decision Log`, and
`Outcomes & Retrospective` must be kept up to date as work proceeds.

Status: COMPLETE

## Purpose / Big Picture

Ensure `cargo check --all-targets` with no features enabled succeeds by
preventing integration tests from importing
`pg_embedded_setup_unpriv::test_support` unless the `cluster-unit-tests`
feature is enabled. The intended user-visible outcome is that default builds no
longer fail with unresolved imports and missing `tracing-subscriber` when
running checks without features, while the cluster-focused suites still compile
and run when the feature is explicitly enabled.

## Constraints

- Do not remove or broaden the existing feature gating in
  `src/test_support/mod.rs` or `src/lib.rs`.
- Do not introduce new dependencies or feature flags.
- Keep integration tests that rely on `test_support` gated behind
  `cluster-unit-tests` to align with ADR guidance on integration-style suites.
- Follow repository quality gates: run `make check-fmt`, `make lint`, and
  `make test` (using `tee` logs) before committing.
- Maintain en-GB-oxendict spelling in documentation edits.

## Tolerances (Exception Triggers)

- Scope: if the change requires editing more than 12 files or exceeds 250 net
  lines of code, stop and escalate.
- Interface: if any public API signature or feature definition must change,
  stop and escalate.
- Dependencies: if a new dependency or feature flag becomes necessary, stop
  and escalate.
- Iterations: if tests still fail after two retries, stop and escalate with
  failure logs.
- Ambiguity: if ADRs or documentation conflict on how tests should be gated,
  stop and ask for direction.

## Risks

- Risk: Gating test crates could unintentionally skip tests that are expected
  to run by default in CI. Severity: medium Likelihood: low Mitigation: confirm
  the Makefile and existing test gating patterns; only gate tests that
  currently import `test_support` and align with the ADR intent.
- Risk: Adding a crate-level `#![cfg(...)]` might hide compilation errors
  unrelated to the feature gating. Severity: low Likelihood: low Mitigation:
  keep `make test` (all features) as the primary validation and add a targeted
  `cargo check --all-targets` without features.

## Progress

- [x] (2026-01-18 00:00Z) Drafted ExecPlan for feature-gated tests.
- [x] (2026-01-18 00:10Z) Inspected listed test crates and current gating.
- [x] (2026-01-18 00:18Z) Updated integration test crate attributes to require
  `cluster-unit-tests` (and `diesel-support` where needed).
- [x] (2026-01-18 00:18Z) Adjusted `tests/settings.rs` so only cap-fs sections
  are gated.
- [x] (2026-01-18 00:42Z) Ran validation commands and captured logs via `tee`.
- [x] (2026-01-18 00:44Z) Committed the change with a descriptive message.
- [x] (2026-01-18 01:12Z) Centralised test gating in `Cargo.toml` and removed
  repeated feature gates from test crates.
- [x] (2026-01-18 02:05Z) Updated the `test_cluster_fixture` target to use
  shared support modules and made error expectations deterministic after the
  suite was enabled.

## Surprises & Discoveries

- Observation: Crate-level `#![cfg(...)]` placed before `//!` triggered
  `missing_docs` and unused-import warnings during `cargo check --all-targets`
  without features. Evidence: `cargo check --all-targets` failed with
  `missing documentation for the crate` and `unused imports` errors. Impact:
  Moved crate docs above the `cfg` attribute and gated imports in
  `tests/settings.rs` to keep the no-feature build clean.
- Observation: Enabling the `test_cluster_fixture` target surfaced missing
  support module paths, fixture imports, and read-only scenario assumptions.
  Evidence: `make lint` and `make test` failed until module paths were updated
  and the read-only scenario handled successful starts. Impact: Paths now use
  `../support`, fixture functions are imported, and read-only runs skip when
  permissions are effectively bypassed.

## Decision Log

- Decision: Use crate-level `#![cfg(all(unix, feature = "cluster-unit-tests"))]`
  attributes on the affected integration tests, plus a combined gate for
  `tests/test_cluster_connection.rs` to include `diesel-support`. Rationale:
  This is the smallest change that matches the existing pattern in
  `tests/test_cluster.rs` and avoids expanding public API surface or
  dependencies. Date/Author: 2026-01-18 (Codex)
- Decision: Keep `//!` crate docs ahead of the `cfg` attributes and gate
  imports used only by cap-fs tests in `tests/settings.rs`. Rationale: Prevents
  `missing_docs` and unused-import warnings when `cluster-unit-tests` is
  disabled. Date/Author: 2026-01-18 (Codex)
- Decision: Move `cluster-unit-tests` gating for integration tests into
  `Cargo.toml` `required-features` entries while keeping `#![cfg(unix)]` in the
  test crates. Rationale: Centralises configuration and reduces per-file
  attribute repetition without changing platform guards. Date/Author:
  2026-01-18 (Codex)
- Decision: Treat the read-only fixture scenario as a skip when the fixture
  starts successfully. Rationale: Some environments permit permission changes
  that defeat the read-only setup; skipping keeps the suite deterministic.
  Date/Author: 2026-01-18 (Codex)

## Outcomes & Retrospective

Completed: integration tests that depend on `test_support` are gated behind the
`cluster-unit-tests` feature, and the `diesel-support` integration suite now
requires the cluster feature too. `cargo check --all-targets` without features
passes, and full `make check-fmt`, `make lint`, and `make test` runs succeed.
Feature gating for these tests is now centralised in `Cargo.toml`
`required-features` entries, including `test_cluster_fixture`. No follow-up
actions required.

## Context and Orientation

The `pg_embedded_setup_unpriv::test_support` module is re-exported only when
`cfg(any(test, feature = "cluster-unit-tests", feature = "dev-worker"))` is
active (see `src/lib.rs` and `src/test_support/mod.rs`). Integration tests are
compiled as separate crates, so they do not get `cfg(test)` and must enable
`cluster-unit-tests` to access those re-exports. Several test crates under
`tests/` import `test_support` without feature gating, which causes
`cargo check --all-targets` without features to fail. The repository already
uses a crate-level gate in `tests/test_cluster.rs` and a Cargo
`required- features` entry in `Cargo.toml` for that test.

Relevant files:

- `src/lib.rs` and `src/test_support/mod.rs` for existing gating.
- `Cargo.toml` for features and `[[test]]` entries.
- Integration tests to gate:
  - `tests/bootstrap_for_tests.rs`
  - `tests/bootstrap_privileges.rs`
  - `tests/bootstrap_worker_binary.rs`
  - `tests/database_lifecycle.rs`
  - `tests/observability.rs`
  - `tests/shutdown_timeout.rs`
  - `tests/test_cluster_behaviour.rs`
  - `tests/test_cluster_drop.rs`
  - `tests/test_cluster_fixture/mod.rs`
  - `tests/test_cluster_connection.rs` (also needs `diesel-support`)
- `tests/settings.rs` for cap-fs settings tests that import `test_support`.

## Plan of Work

Stage A: Confirm scope and current gating. Read the listed test files to verify
they import `test_support` or cap-fs helpers and confirm they are not already
gated. Check `Cargo.toml` for existing `required-features` patterns and whether
any tests already carry feature-specific attributes.

Stage B: Centralise feature gates. Add `[[test]]` entries in `Cargo.toml` with
`required-features = ["cluster-unit-tests"]` for each affected integration
test, and use `["cluster-unit-tests", "diesel-support"]` for
`tests/test_cluster_connection.rs`. Keep `#![cfg(unix)]` in those test crates
to avoid non-unix compilation on unsupported platforms.

Stage C: Update `tests/settings.rs`. Gate the cap-fs module and the
`dir_accessible_tests` module behind
`cfg(all(unix, feature = "cluster-unit-tests"))`. Keep the rest of the settings
tests running without features.

Stage D: Validate. Run a targeted no-feature check to confirm the original
error is gone, then run the standard quality gates via Makefile targets.
Capture command output with `tee` as required.

Stage E: Commit. Create a single atomic commit that captures the gating
changes. Include a clear commit message with a short subject and a brief body
explaining why the gating aligns with the test-support feature design.

## Concrete Steps

Run the following from the repository root (the directory containing
`Cargo.toml`). Use `tee` log files with the recommended naming pattern. Replace
`$ACTION` with the command name and keep the rest unchanged.

1. Review each listed test file and confirm current attributes.

   - Example:
     rg -n "test_support" tests

2. Add crate-level `cfg` attributes and update `tests/settings.rs` as described
   in Stage B and C. Use `rg` to confirm the new attributes are present.

3. Validate no-feature build:

   - Command:
     RUSTFLAGS="-D warnings" cargo check --all-targets 2>&1 | \
       tee /tmp/check-$(get-project)-$(git branch --show).out

   - Expected: `Finished` with exit code 0 and no unresolved import errors.

4. Run format and lint gates:

   - Commands:
     make check-fmt 2>&1 | \
       tee /tmp/check-fmt-$(get-project)-$(git branch --show).out
     make lint 2>&1 | \
       tee /tmp/lint-$(get-project)-$(git branch --show).out

   - Expected: both commands complete with exit code 0.

5. Run tests:

   - Command:
     make test 2>&1 | \
       tee /tmp/test-$(get-project)-$(git branch --show).out

   - Expected: `nextest` reports all tests passed, including those that now
     require `cluster-unit-tests` under the all-features run.

6. Commit:

   - `git status` should show only the intended test files.
   - Commit message (example):
     - Subject: `Gate cluster integration tests by feature`
     - Body: explain that integration tests rely on `test_support` and should
       only compile when `cluster-unit-tests` is enabled.

## Validation and Acceptance

Quality criteria:

- `cargo check --all-targets` with no features succeeds without unresolved
  import errors.
- `make check-fmt`, `make lint`, and `make test` all pass.
- Integration tests that rely on `test_support` only compile when
  `cluster-unit-tests` is enabled.

Acceptance behaviour:

- Running `cargo check --all-targets` on a clean checkout with no features
  completes successfully.
- Running `make test` continues to exercise cluster integration tests under
  `--all-features`.

## Idempotence and Recovery

All commands in this plan are re-runnable. If a command fails, fix the
underlying issue and re-run the same command. Use `git status` and `git diff`
to verify only the intended changes are present before committing.

## Artifacts and Notes

Expected files to change (no new files):

- `tests/bootstrap_for_tests.rs`
- `tests/bootstrap_privileges.rs`
- `tests/bootstrap_worker_binary.rs`
- `tests/database_lifecycle.rs`
- `tests/observability.rs`
- `tests/shutdown_timeout.rs`
- `tests/test_cluster_behaviour.rs`
- `tests/test_cluster_drop.rs`
- `tests/test_cluster_fixture/mod.rs`
- `tests/test_cluster_connection.rs`
- `tests/settings.rs`

## Interfaces and Dependencies

No new dependencies. No public API changes. The change is strictly compile-time
feature gating of integration test crates and submodules.

## Revision note

2026-01-18: Marked validations and commit steps complete, recorded the doc
ordering discovery, and set status to COMPLETE with outcomes captured.
2026-01-18: Replaced the local path with a repository-root description and
centralised test gating in `Cargo.toml`. 2026-01-18: Updated fixture suite
paths and deterministic skip handling after enabling the test target.
2026-01-18: Corrected the `test_support` path typo in the Purpose section.
