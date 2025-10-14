# Zero-config RAII Postgres test fixture roadmap

## Phase 1: Stabilize zero-configuration bootstrap

### Step: Detect and adapt to execution privileges

- [ ] **Task:** Ship runtime privilege detection that branches between root and
  unprivileged flows, verified by integration tests covering both contexts.
- [ ] **Task:** Introduce mandatory privilege-dropping logic for root executions
  that provisions directories under the `nobody` user and proves idempotence
  across two consecutive cluster initializations.

### Step: Harden bootstrap orchestration

- [ ] **Task:** Deliver a `bootstrap_for_tests()` helper that wraps
  configuration discovery, handles privilege checks, and prepares directories,
  returning a structured settings object exercised in sample tests.
- [ ] **Task:** Automate environment variable preparation (for example `TZDIR`
  and `PGPASSFILE`) with traceable defaults validated through assertions in the
  bootstrap tests.

## Phase 2: Provide an ergonomic RAII fixture

### Step: Implement the `TestCluster` lifecycle

- [ ] **Task:** Create the `TestCluster` RAII struct with `new` and `Drop`
  implementations that guarantee cluster startup and teardown, evidenced by
  tests ensuring no processes remain after scope exit.
- [ ] **Task:** Expose connection metadata and helper methods (for example a
  Diesel connection constructor) with documentation examples and smoke tests
  that execute simple SQL statements.

### Step: Integrate with test frameworks

- [ ] **Task:** Publish an `rstest` fixture (or equivalent) that yields a ready
  `TestCluster`, demonstrated in example tests showing zero explicit setup.
- [ ] **Task:** Document usage patterns in README excerpts and doctests so the
  fixture becomes the default path for new integration tests.

## Phase 3: Enhance visibility and platform coverage

### Step: Instrument setup for observability

- [ ] **Task:** Add `tracing` spans and logs around privilege changes, directory
  mutations, environment variable injection, and PostgreSQL lifecycle events,
  verified by log assertions in integration tests.
- [ ] **Task:** Surface the chosen settings (ports, directories, version) via
  debug logging with sanitized output, ensuring sensitive values are redacted.

### Step: Validate cross-platform behaviour

- [ ] **Task:** Confirm Linux root and unprivileged paths through CI matrix
  jobs and document expected outcomes for macOS and Windows in the roadmap
  appendix.
- [ ] **Task:** Establish guardrails that fail fast on unsupported root
  scenarios on non-Linux systems, including unit coverage for the error
  messaging.
