# Refactor worker payload serde via secrecy

This Execution Plan (ExecPlan) is a living document. The sections `Constraints`,
`Tolerances`, `Risks`, `Progress`, `Surprises & Discoveries`, `Decision Log`,
and `Outcomes & Retrospective` must be kept up to date as work proceeds.

Status: COMPLETE

PLANS.md was not found in the repository root on 2026-01-12.

## Purpose / Big Picture

This change removes manual serialization, deserialization, and redaction
boilerplate in `src/worker.rs` by relying on existing serde ecosystem support. A
successful change keeps the worker payload schema stable, keeps secrets redacted
in `Debug`, and preserves UTF-8 validation for paths while reducing custom code.
Success is observable by running the full test suite and by round-tripping a
`WorkerPayload` through JSON without lossy path or secret handling changes.

## Constraints

- Keep the public surface of `pg_embedded_setup_unpriv::worker` stable unless
  the user explicitly approves a breaking change.
- Preserve `WorkerPayload` JSON field names and structure to avoid breaking
  inter-process communication (IPC) compatibility.
- Do not allow non-UTF-8 paths to be silently accepted or lossy-encoded.
- Keep secret values redacted in `Debug` output.
- Do not introduce new dependencies or Cargo features beyond those already in
  `Cargo.toml` without explicit approval.
- Keep module-level `//!` documentation at the top of every module touched.

## Tolerances (Exception Triggers)

- Scope: if this requires changes to more than 4 files or more than 250 net
  lines, stop and escalate.
- Interface: if removing `PlainSecret` or otherwise changing public signatures
  is required, stop and escalate with options.
- Dependencies: if new crates or Cargo features are needed, stop and escalate.
- Iterations: if `make check-fmt`, `make lint`, or `make test` still fail after
  two fix attempts each, stop and escalate with logs.
- Ambiguity: if multiple valid serde strategies would change payload shape, stop
  and ask which compatibility target is intended.

## Risks

- Risk: serde for `secrecy::SecretString` might encode differently than the
  current `PlainSecret` serde implementation, changing payload JSON. Severity:
  medium. Likelihood: medium. Mitigation: compare JSON output in tests or add a
  regression test.

- Risk: replacing manual UTF-8 checks might move the failure point to
  serialisation or deserialisation time. Severity: medium. Likelihood: medium.
  Mitigation: keep explicit UTF-8 validation or document and test the new
  failure point.

- Risk: removing `PlainSecret` could break tests or external users. Severity:
  medium. Likelihood: low. Mitigation: prefer a type alias or re-export to
  preserve the name.

## Progress

- [x] (2026-01-12 11:10Z) Drafted ExecPlan for issue 20.
- [x] (2026-01-12 11:23Z) Inspected `worker` types and tests; confirmed
  `PlainSecret::expose()` is used by the worker helper.
- [x] (2026-01-12 11:23Z) Chose a `PlainSecret` newtype that derives
  `Deserialize` and `Debug` with `#[serde(transparent)]` plus a manual
  `Serialize` implementation.
- [x] (2026-01-12 11:26Z) Implemented PlainSecret derives and unit tests.
- [x] (2026-01-12 11:40Z) Ran make fmt, markdownlint, nixie, check-fmt, lint,
  and test; logs captured in /tmp.
- [x] (2026-01-12 11:50Z) Completed post-change review and prepared commit.

## Surprises & Discoveries

- Observation: `tests/support/pg_worker.rs` calls `PlainSecret::expose()` when
  applying environment overrides. Evidence: `apply_worker_environment` takes
  `Vec<(String, Option<PlainSecret>)>`. Impact: retain the `PlainSecret` type
  and `expose` method to avoid API churn.
- Observation: `secrecy::SecretString` is not `Serialize` even with the `serde`
  feature enabled because it requires `SerializableSecret` for `str`. Evidence:
  `make lint` failed with `E0277` when deriving `Serialize` for `PlainSecret`.
  Impact: keep a manual `Serialize` impl for `PlainSecret` while deriving
  `Debug` and `Deserialize`.

## Decision Log

- Decision: Defer the choice between `SecretString` direct use vs. aliasing
  `PlainSecret` until the implementation stage. Rationale: This requires
  confirming how `PlainSecret` is used in tests and public APIs to minimize
  breakage. Date/Author: 2026-01-12, Codex.
- Decision: Keep `PlainSecret` as a newtype over `SecretString` and derive
  `Deserialize` and `Debug` with `#[serde(transparent)]`. Rationale: Preserves
  the public name and method while reducing manual boilerplate and keeping JSON
  compatibility. Date/Author: 2026-01-12, Codex.
- Decision: Implement `Serialize` manually for `PlainSecret` while deriving
  `Deserialize` and `Debug`. Rationale: `SecretString` does not implement
  `Serialize`, so deriving it for `PlainSecret` is not possible without a custom
  Serde implementation. Date/Author: 2026-01-12, Codex.

## Outcomes & Retrospective

- `PlainSecret` now derives `Deserialize` and `Debug` with a manual `Serialize`,
  keeping secrets redacted and payloads stable.
- Added unit coverage for secret serialization and redaction.
- Make targets `fmt`, `markdownlint`, `nixie`, `check-fmt`, `lint`, and `test`
  completed successfully.
- Lessons learned: `SecretString` deliberately omits `Serialize`, so the manual
  Serde implementation remains required for IPC payloads.

## Context and Orientation

`src/worker.rs` defines `SettingsSnapshot`, `WorkerPayload`, and the
`PlainSecret` wrapper used to serialize secrets and redact them in `Debug`.
`SettingsSnapshot` currently converts `postgresql_embedded::Settings` into a
UTF-8 safe snapshot using manual `TryFrom` and `From` implementations. The
payload is exercised in `tests/support/pg_worker.rs`, which currently imports
`PlainSecret` directly. Cargo dependencies already include `camino` with
`serde1`, `secrecy` with `serde`, and `serde_with` for `Duration` and
`VersionReq` handling.

## Plan of Work

Stage A: understand and propose (no code changes). Read `src/worker.rs` and
`tests/support/pg_worker.rs` to confirm how `PlainSecret` and the snapshot
conversions are used. Confirm whether any external modules rely on `PlainSecret`
as a concrete type or only via `From` conversions.

Stage B: decide and scaffold. Choose the minimal-change approach:

- Prefer re-exporting or type aliasing `PlainSecret` to `SecretString` so the
  name remains stable, while serde and redaction come from `secrecy`.
- If aliasing is not viable, keep `PlainSecret` as a newtype and derive
  serde/Debug with `#[derive(Serialize, Deserialize, Debug)]` and
  `#[serde(transparent)]`. Document the decision in the Decision Log and update
  constraints if the public API must change.

Stage C: implementation. Remove manual serde impls and manual `Debug` for
`PlainSecret`, rely on `secrecy` redaction, and simplify `SettingsSnapshot`
conversion. If UTF-8 validation is still required at construction time, keep a
single conversion helper to perform it, otherwise document and test the new
failure point. Replace `into_settings` or `From<SettingsSnapshot>` only if
needed to keep the API surface stable. Ensure `WorkerPayload::new` remains the
entrypoint for constructing payloads.

Stage D: validation and cleanup. Update or add tests that assert the JSON
payload shape and redaction behaviour. Run formatting, lint, and tests, then
commit. Perform the post-commit refactor review to confirm no new smells were
introduced.

Each stage ends with validation, and the next stage does not begin until the
current stage passes its checks.

## Concrete Steps

All commands run from the repository root. Use `tee` to capture logs for any
long-running command.

1) Inspect code and tests:

   ```sh
   rg -n "SettingsSnapshot|WorkerPayload|PlainSecret" src/worker.rs \
     tests/support/pg_worker.rs
   sed -n '1,220p' src/worker.rs
   sed -n '1,240p' tests/support/pg_worker.rs
   ```

2) Implement changes (details depend on Stage B decision). Keep edits focused in
   `src/worker.rs` and update `tests/support/pg_worker.rs` as needed.

3) Run format, lint, and tests:

   ```sh
   make check-fmt | tee /tmp/issue-20-check-fmt.log
   make lint | tee /tmp/issue-20-lint.log
   make test | tee /tmp/issue-20-test.log
   ```

   If `make check-fmt` fails, run `make fmt` and then re-run `make check-fmt`.

4) Commit with a descriptive message once all gates pass, then perform the
   post-commit refactor review per `AGENTS.md`.

## Validation and Acceptance

Behavioural acceptance:

- Construct a `WorkerPayload`, serialize to JSON, deserialize, and confirm that
  `SettingsSnapshot` restores identical host/port and that secrets remain
  redacted in `Debug` output.
- Ensure invalid UTF-8 paths still produce a `BootstrapError` without lossy
  conversion.

Quality criteria (done means all pass):

- Tests: `make test` passes, with any new tests added for payload/secret
  behaviour failing before and passing after.
- Lint: `make lint` passes with no warnings.
- Formatting: `make check-fmt` passes.

## Idempotence and Recovery

All steps are safe to re-run. If a change introduces a regression, use `git
checkout -- <file>` to revert only the specific file, then retry the step. Avoid
destructive git operations.

## Artefacts and Notes

Expected log artefacts after validation:

```text
/tmp/issue-20-check-fmt.log
/tmp/issue-20-lint.log
/tmp/issue-20-test.log
```

## Interfaces and Dependencies

Target interfaces to preserve or document:

- `pg_embedded_setup_unpriv::worker::SettingsSnapshot`
- `pg_embedded_setup_unpriv::worker::WorkerPayload`
- `pg_embedded_setup_unpriv::worker::PlainSecret` (preferably preserved as a
  name, even if aliased to `SecretString`)

Key dependencies and traits:

- `secrecy::SecretString` with `serde` feature for redaction and serialization.
- `camino::Utf8PathBuf` with `serde1` feature for UTF-8 paths.
- `serde_with::DurationSeconds` and `serde_with::DisplayFromStr` already used
  for duration and version request handling.

## Revision note (required when editing an ExecPlan)

- Initial plan created on 2026-01-12 based on issue 20 scope and current repo
  state.
- Updated status, progress, and outcomes after completing the refactor and
  validations on 2026-01-12.
- 2026-01-12: Indented Concrete Steps code blocks to keep list numbering intact,
  and added commas for clarity.
- 2026-01-12: Standardized spellings, expanded acronyms, and corrected list
  sequencing after PR review feedback.
- 2026-01-12: Rewrapped risk bullets and switched command examples to fenced
  code blocks for markdownlint compliance; plan intent unchanged.
- 2026-01-12: Renumbered Concrete Steps sequentially to satisfy markdownlint
  list requirements.
- 2026-01-12: Rewrapped paragraphs and list items to 80 columns for
  markdownlint compliance.
- 2026-01-12: Added punctuation to risk fields, updated en-GB spellings, and
  indented the list continuation in Concrete Steps for markdownlint.
