# Configure postgres-embedded worker counts at cluster setup

This execution plan (ExecPlan) is a living document. The sections
`Constraints`, `Tolerances`, `Risks`, `Progress`, `Surprises & Discoveries`,
`Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work
proceeds.

Status: COMPLETE

PLANS.md does not exist in this repository.

## Purpose / big picture

Reduce the number of background and parallel worker processes started by
`postgresql_embedded` when the project provisions an embedded Postgres cluster
for tests or ephemeral environments. The user-visible behaviour is that
starting the embedded cluster spawns fewer helper processes while preserving
functional correctness. Success is observable by running the project’s cluster
setup and confirming the expected Postgres server settings (e.g.
`max_worker_processes`) via `SHOW` queries, and by observing fewer processes in
`ps` during test runs.

## Constraints

- Do not change public APIs without explicit approval.
- Do not add new external dependencies.
- Keep configuration scoped to embedded/ephemeral clusters unless the codebase
  clearly uses `postgresql_embedded` for all environments.
- Preserve existing configuration and defaults; merge new settings rather than
  replacing existing ones.
- Follow repository validation gates (format, lint, tests) before any commit.
- Markdown changes must obey repository formatting rules (80-column wrapping).

## Tolerances (exception triggers)

- Scope: more than 6 files changed or more than 300 net lines of code.
- Interface: any public API signature change.
- Dependencies: any new crate or feature flag.
- Iterations: more than 2 failed attempts for a single validation command.
- Time: any single milestone taking more than 2 hours.
- Ambiguity: if it is unclear whether the configuration applies to production
  clusters, stop and ask for clarification.

## Risks

- Risk: applying the settings to non-test clusters could reduce performance or
  disable required parallelism. Severity: high Likelihood: medium Mitigation:
  confirm where `postgresql_embedded` is used and scope changes to
  ephemeral/test cluster setup.
- Risk: disabling autovacuum may cause bloat or slow tests that rely on vacuum
  behaviour. Severity: medium Likelihood: medium Mitigation: gate autovacuum
  changes to short-lived clusters and document the trade-off.
- Risk: existing code already sets server configuration, and new inserts could
  override required settings. Severity: medium Likelihood: medium Mitigation:
  inspect current settings and merge carefully; add comments documenting intent.

## Progress

- [x] (2026-01-28 00:00Z) Drafted initial ExecPlan.
- [x] (2026-01-28 18:35Z) Located bootstrap settings creation and cluster
  startup paths.
- [x] (2026-01-28 18:40Z) Added worker-related configuration defaults to
  `Settings.configuration` during settings creation.
- [x] (2026-01-28 19:35Z) Validated settings via formatting, lint, and test
  gates.
- [x] (2026-01-28 18:45Z) Updated documentation to reflect the new defaults.

## Surprises & discoveries

- Observation: `make test` initially failed because `cargo nextest` was
  missing. Evidence: error: no such command: `nextest` during `make test`.
  Impact: Installed `cargo-nextest` via `cargo install --locked cargo-nextest`
  before rerunning the test suite.

## Decision log

- Decision: use `Settings.configuration: HashMap<String, String>` to set server
  configuration for embedded Postgres. Rationale: this is the supported knob
  for server configuration in `postgresql_embedded` and matches the upstream
  examples.[^1] Date/Author: 2026-01-28 / Codex
- Decision: apply worker limits during `bootstrap_for_tests` only, keeping the
  defaults scoped to ephemeral test clusters. Rationale: the CLI bootstrap path
  is used for long-lived setups and should not disable autovacuum or
  replication unless explicitly configured. Date/Author: 2026-01-28 / Codex.

## Outcomes & retrospective

Default embedded PostgreSQL settings now include worker and parallelism limits
in the test bootstrap path, with autovacuum disabled to keep ephemeral test
clusters lightweight. Added tests to verify defaults are applied without
overriding existing configuration, updated user-facing documentation, and ran
formatting, lint, and test gates. Future work could add explicit configuration
overrides if production-style settings are needed.

## Context and orientation

`postgresql_embedded` allows configuring server settings through
`Settings.configuration`, a `HashMap<String, String>` passed into
`PostgreSQL::new(settings)`.[^1][^2] The repository already provisions embedded
Postgres clusters during tests or local runs; the exact location must be
identified in the codebase. This plan expects to find a module that creates
`Settings::default()` or similar, then instantiates `PostgreSQL::new` and calls
`setup`/`start`. The change will add server configuration entries at cluster
setup time, keeping the configuration scoped to ephemeral/test environments if
possible.

## Plan of work

Stage A: discovery and scoping. Locate where embedded Postgres is configured.
Search for `postgresql_embedded`, `PostgreSQL::new`, or `Settings::default`.
Identify whether there are separate paths for test/ephemeral clusters versus
long-lived or production-like clusters. Confirm whether configuration is
already injected from user settings or environment variables.

Stage B: design the configuration update. Decide whether to create a small
helper that returns a `Settings` instance (or updates an existing one) to keep
all Postgres settings in one place. The configuration should include the
worker-related knobs described in the prompt: `max_connections`,
`max_worker_processes`, `max_parallel_workers`,
`max_parallel_workers_per_gather`, `max_parallel_maintenance_workers`, and
`max_wal_senders`/`max_replication_slots`, plus `autovacuum` if the cluster is
explicitly ephemeral. Keep the configuration additive and avoid overwriting any
existing settings without justification.

Stage C: implementation. Update the identified cluster setup path to insert the
configuration values into `Settings.configuration` before calling
`PostgreSQL::new(settings)` and `setup`. If there is a user-configurable
override, merge with precedence rules that preserve explicit user intent.

Stage D: validation and documentation. Run the full validation gates. If
behaviour changes are user-visible (for example, fewer worker processes or
changed autovacuum behaviour), update the most relevant documentation in
`docs/` (likely `docs/users-guide.md` or any embedded Postgres guidance) and
re-run Markdown checks.

## Concrete steps

1. Discover the embedded Postgres setup code.

    rg "postgresql_embedded|PostgreSQL::new|Settings::default" src tests

2. Open the files identified in step 1 and confirm the cluster setup flow.
   Note any existing configuration map usage.

3. Implement the configuration updates where the embedded cluster is created.
   Keep changes small and local. Example configuration keys to insert:
   - `max_connections = 20`
   - `max_worker_processes = 2`
   - `max_parallel_workers = 0`
   - `max_parallel_workers_per_gather = 0`
   - `max_parallel_maintenance_workers = 0`
   - `autovacuum = off` (only for ephemeral/test clusters)
   - `max_wal_senders = 0`
   - `max_replication_slots = 0`

4. (Optional) If a manual verification path exists, add a quick `SHOW` query in
   a test or a debug-only path to assert the settings are applied.

5. Run validation commands from the repository root with logging to `tee`:

    set -o pipefail
    make check-fmt 2>&1 | tee /tmp/make-check-fmt.log
    make lint 2>&1 | tee /tmp/make-lint.log
    make test 2>&1 | tee /tmp/make-test.log

6. If documentation files in `docs/` were updated, also run:

    set -o pipefail
    make fmt 2>&1 | tee /tmp/make-fmt.log
    make markdownlint 2>&1 | tee /tmp/make-markdownlint.log
    make nixie 2>&1 | tee /tmp/make-nixie.log

## Validation and acceptance

The change is accepted when all of the following are true:

- The embedded cluster setup path inserts the specified server configuration
  into `Settings.configuration` before calling `PostgreSQL::new`.
- Running `make check-fmt`, `make lint`, and `make test` passes without errors.
- If a verification query is added, it confirms the expected values (for
  example, `SHOW max_worker_processes` returns `2`).
- The number of extra worker processes is reduced during test runs, while core
  Postgres background processes still exist as expected.

## Idempotence and recovery

All steps are safe to rerun. If a validation command fails, fix the reported
issue and rerun the same command. If a configuration change causes tests to
fail, revert only that setting and rerun the tests to confirm the regression is
isolated before escalating.

## Artifacts and notes

Keep a short record of key outputs (for example, `SHOW` results or process
counts) in the PR description or commit message body to demonstrate the change
had the intended effect.

## Interfaces and dependencies

- Use `postgresql_embedded::Settings` and its
  `configuration: HashMap<String, String>` field for server settings.[^1]
- Instantiate Postgres with `postgresql_embedded::PostgreSQL::new(settings)` in
  the existing cluster setup path.[^2]
- Do not introduce new crates or external services.

## Revision note

Initial draft created from the user-provided configuration guidance and
repository constraints.

Revision 2026-01-28: marked the plan in progress, recorded implementation
location decisions, and updated progress to reflect configuration and
documentation changes. Remaining work is limited to validation.

Revision 2026-01-28: marked the plan complete after implementing the settings
defaults, validating via the standard gates, and documenting the new behaviour.

[^1]: https://docs.rs/postgresql_embedded/latest/postgresql_embedded/struct.Settings.html
[^2]: https://docs.rs/postgresql_embedded/latest/postgresql_embedded/struct.PostgreSQL.html
