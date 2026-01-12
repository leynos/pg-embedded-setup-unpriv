# Zero-config RAII Postgres test fixture roadmap

## 1. Stabilize zero-configuration bootstrap

### 1.1 Detect and adapt to execution privileges

- [x] 1.1.1. Ship runtime privilege detection that branches between root and
  unprivileged flows, verified by integration tests covering both contexts.
- [x] 1.1.2. Introduce mandatory privilege-dropping logic for root executions
  that provisions directories under the `nobody` user and proves idempotence
  across two consecutive cluster initializations.

### 1.2 Harden bootstrap orchestration

- [x] 1.2.1. Deliver a `bootstrap_for_tests()` helper that wraps configuration
  discovery, handles privilege checks, and prepares directories, returning a
  structured settings object exercised in sample tests.
- [x] 1.2.2. Automate environment variable preparation (for example `TZDIR` and
  `PGPASSFILE`) with traceable defaults validated through assertions in the
  bootstrap tests.

## 2. Provide an ergonomic RAII fixture

### 2.1 Implement the `TestCluster` lifecycle

- [x] 2.1.1. Create the `TestCluster` RAII struct with `new` and `Drop`
  implementations that guarantee cluster startup and teardown, evidenced by
  tests ensuring no processes remain after scope exit.
- [x] 2.1.2. Expose connection metadata and helper methods (for example a
  Diesel connection constructor) with documentation examples and smoke tests
  that execute simple SQL statements.

### 2.2 Integrate with test frameworks

- [x] 2.2.1. Publish an `rstest` fixture (or equivalent) that yields a ready
  `TestCluster`, demonstrated in example tests showing zero explicit setup.
- [x] 2.2.2. Document usage patterns in README excerpts and doctests, so the
  fixture becomes the default path for new integration tests.

## 3. Enhance visibility and platform coverage

### 3.1 Instrument setup for observability

- [x] 3.1.1. Add `tracing` spans and logs around privilege changes, directory
  mutations, environment variable injection, and PostgreSQL lifecycle events,
  verified by log assertions in integration tests.
- [x] 3.1.2. Surface the chosen settings (ports, directories, version) via
  debug logging with sanitized output, ensuring sensitive values are redacted.

### 3.2 Strengthen concurrency guarantees

- [ ] 3.2.1. Add Loom-based concurrency tests for the `ScopedEnv` mutex, gated
  behind a feature flag, and document how to run them.

### 3.3 Validate cross-platform behaviour

- [ ] 3.3.1. Confirm Linux root and unprivileged paths through Continuous
  Integration (CI) matrix jobs, and document expected outcomes for macOS and
  Windows in the roadmap appendix.
- [ ] 3.3.2. Establish guardrails that fail fast on unsupported root scenarios
  on non-Linux systems, including unit coverage for the error messaging.
