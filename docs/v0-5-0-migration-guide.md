# v0.5.0 migration guide (from v0.4.0)

This guide covers migration from tag `v0.4.0` to current `HEAD` (`34b86ac`) and
is prioritized for:

1. breaking changes;
2. fixes for outstanding issues; and
3. new features that require usage changes to realize their benefit.

## Scope and audience

This guide is for teams already using `pg_embedded_setup_unpriv` in test
harnesses, Continuous Integration (CI) agents, or root-constrained environments.

For straightforward unprivileged local tests, the default configuration
typically suffices, with focused review of the breaking-changes section.

## Quick upgrade checklist

- Update crate version in `Cargo.toml`.
- Review `PG_TEST_BACKEND` usage in CI and local scripts.
- Review any assumptions that dropped clusters leave data on disk.
- Migrate send-bound shared fixtures to the handle/guard split APIs.
- For custom bootstrap settings, prefer test-specific settings constructors.
- Run the test suite once and validate cleanup modes explicitly.

## Breaking changes

### Behavioural break: `TestCluster` now cleans up data directories on drop by default

Background:

In `v0.4.0`, dropping `TestCluster` stopped PostgreSQL, but did not reliably
remove all cluster artefacts. In `HEAD`, drop-time cleanup is now explicit and
configurable via `CleanupMode`, with `DataOnly` as the default.

Benefits:

- Prevents persistent disk growth in repeated test runs.
- Makes Resource Acquisition Is Initialization (RAII) semantics match practical
  expectations: cluster resources are reclaimed when guards are dropped.

Adoption:

- If dropped cluster files were previously inspected for debugging, opt into:
  `CleanupMode::None`.
- For full filesystem hygiene (data + installation dirs), opt into:
  `CleanupMode::Full`.
- Otherwise, keep default `CleanupMode::DataOnly`.

Before (`v0.4.0`):

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?;
// On drop, cluster stop was best-effort, but directory cleanup was not the
// deterministic default behaviour.
drop(cluster);
# Ok(())
# }
```

After (`HEAD`):

```rust,no_run
use pg_embedded_setup_unpriv::{CleanupMode, TestCluster};

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let cluster = TestCluster::new()?.with_cleanup_mode(CleanupMode::Full);
drop(cluster);
# Ok(())
# }
```

### Behavioural break: strict `PG_TEST_BACKEND` validation in test bootstrap

Background:

`bootstrap_for_tests()` and `TestCluster` now validate `PG_TEST_BACKEND`. The
only accepted values are unset/empty and `postgresql_embedded`. Other values
produce a `SKIP-TEST-CLUSTER` error.

Benefits:

- Removes ambiguous backend selection in mixed environments.
- Makes intentional backend skipping explicit and detectable.

Adoption:

- Set `PG_TEST_BACKEND=postgresql_embedded` where embedded PostgreSQL is
  required.
- Unset the variable when embedded should remain default.
- Treat `SKIP-TEST-CLUSTER` as an intentional skip signal in harness logic.

Before (`v0.4.0`):

```bash
# Non-standard values could flow through without explicit backend validation.
export PG_TEST_BACKEND=sqlite
```

After (`HEAD`):

```bash
# Explicit and supported.
export PG_TEST_BACKEND=postgresql_embedded
# or
unset PG_TEST_BACKEND
```

### Behavioural break for root workflows: malformed `PATH` now hard-fails worker discovery

Background:

In root mode on Unix, worker discovery now rejects non-UTF-8 `PATH` entries
instead of silently tolerating malformed entries.

Benefits:

- Deterministic worker lookup and clearer diagnostics.
- Avoids surprising runtime failures where worker detection appears flaky.

Adoption:

- Prefer explicit `PG_EMBEDDED_WORKER` in root CI.
- Sanitize `PATH` in root containers/agents.

Recommended root setup:

```bash
export PG_EMBEDDED_WORKER=/absolute/path/to/pg_worker
```

## Fixes for outstanding issues

### Issue #66: RAII cleanup leak fixed

Background:

Dropped clusters previously left artefacts in common temp/runtime locations,
especially in repeated test executions.

Benefits:

- Stable disk usage over long-running CI cycles.
- Less manual cleanup scripting.

Adoption:

- Remove external cleanup wrappers that compensated for the old leak.
- Use `CleanupMode::None` only for explicit forensic/debug sessions.

### Issue #81 and issue #101: partial data-directory recovery now automatic

Background:

Interrupted setup could leave an invalid data directory, causing follow-up
setup runs to fail until users manually removed the directory.

`pg_worker` now detects invalid partial state and performs recovery before
re-running setup.

Benefits:

- Retries are resilient after interrupted bootstrap.
- Less brittle CI recovery logic.

Adoption:

- Remove manual `rm -rf` fallback steps for partially initialized data dirs.
- Allow setup retries to use built-in recovery path.

### Issue #105: worker discovery and compile-path robustness improvements

Background:

Worker-discovery and tests were adjusted to fix a rustc `E0277` regression and
stabilize the root worker path handling.

Benefits:

- More robust worker bootstrap behaviour under root test agents.
- Better confidence in worker discovery tests across environments.

Adoption:

- No API migration required.
- Prefer explicit worker path configuration in constrained CI environments.

## New features that require usage-style changes

### Handle/guard split for send-safe cluster access

Background:

`TestCluster` remains convenient but is `!Send` due to environment guard
semantics. New split constructors expose:

- `TestCluster::new_split()`
- `TestCluster::start_async_split()`
- `ClusterHandle` (send-safe access)
- `ClusterGuard` (lifecycle and environment ownership)

Benefits:

- Enables `OnceLock`, timeout-enabled `rstest`, and other send-bound patterns
  without unsafe fixture workarounds.

Adoption:

Use split constructors for shared fixtures and cross-thread use:

```rust,no_run
use std::sync::OnceLock;
use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};

static SHARED: OnceLock<ClusterHandle> = OnceLock::new();

fn shared_handle() -> &'static ClusterHandle {
    SHARED.get_or_init(|| {
        let (handle, guard) = TestCluster::new_split()
            .expect("cluster bootstrap failed");
        std::mem::forget(guard);
        handle
    })
}
```

`std::mem::forget(guard)` is intentional in this pattern. It leaks the guard,
so the shared cluster remains available for process-lifetime fixtures. Use this
approach only when process-lifetime cluster ownership is desired.

For deterministic shutdown, retain the guard and drop it explicitly at the end
of the test scope:

```rust,no_run
use pg_embedded_setup_unpriv::TestCluster;

# fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
let (handle, guard) = TestCluster::new_split()?;
assert!(handle.database_exists("postgres")?);
drop(guard); // deterministic shutdown and environment restoration
# Ok(())
# }
```

### New shared fixture for send-bound test contexts

Background:

`test_support::shared_test_cluster_handle()` was added to provide an
out-of-the-box shared fixture returning `&'static ClusterHandle`.

Benefits:

- Better fit for test frameworks/features that require `Send + Sync` handles.
- Cleaner migration path from custom shared-singleton implementations.

Adoption:

Use the handle fixture where previously `&'static TestCluster` caused send
constraints:

```rust,no_run
use pg_embedded_setup_unpriv::test_support::shared_test_cluster_handle;
use pg_embedded_setup_unpriv::ClusterHandle;
use rstest::rstest;

#[rstest]
fn uses_shared_handle(shared_test_cluster_handle: &'static ClusterHandle) {
    assert!(shared_test_cluster_handle.database_exists("postgres").unwrap());
}
```

### Test-focused settings constructors

Background:

`PgEnvCfg` now provides:

- `to_settings_for_tests()`
- `to_settings_with_context(for_tests: bool)`

These apply worker/process limits suitable for ephemeral test clusters.

Benefits:

- Lower background process overhead in ephemeral test environments.
- Better defaults for resource-constrained CI workers.

Adoption:

For test-specific manual derivation of `postgresql_embedded::Settings`, switch
from `to_settings()` to `to_settings_for_tests()` unless explicitly required
production-style concurrency settings.

## API changes at a glance

| Category             | Added/changed surface                     | Migration note                                    |
| -------------------- | ----------------------------------------- | ------------------------------------------------- |
| Cleanup control      | `CleanupMode::{DataOnly, Full, None}`     | Default now removes data dir on drop              |
| Cleanup control      | `TestCluster::with_cleanup_mode(...)`     | Set explicit teardown policy per suite            |
| Cluster lifecycle    | `ClusterHandle`                           | Use for send-safe shared access                   |
| Cluster lifecycle    | `ClusterGuard`                            | Owns lifecycle; dropping guard shuts down cluster |
| Constructors         | `TestCluster::new_split()`                | Preferred for shared/send-bound fixtures          |
| Constructors         | `TestCluster::start_async_split()`        | Async split constructor (`async-api`)             |
| Test support         | `shared_test_cluster_handle()`            | Send-safe shared `rstest` fixture                 |
| Settings             | `PgEnvCfg::to_settings_for_tests()`       | Use for ephemeral test clusters                   |
| Settings             | `PgEnvCfg::to_settings_with_context(...)` | Use when toggling test/non-test defaults          |
| Environment contract | `PG_TEST_BACKEND` validation              | Only empty/unset or `postgresql_embedded`         |

## Suggested migration validation

Run these checks after upgrading:

1. `PG_TEST_BACKEND` contract

   - Set `PG_TEST_BACKEND=postgresql_embedded` and confirm bootstrap succeeds.
   - Set an unsupported value and confirm `SKIP-TEST-CLUSTER` is emitted.

2. Cleanup-mode behaviour

   - Default mode: confirm data directory is removed on drop.
   - `CleanupMode::Full`: confirm install and data directories are removed.
   - `CleanupMode::None`: confirm directories are retained for inspection.

3. Send-safe fixture behaviour

   - Use `new_split()` or `shared_test_cluster_handle()` in one timeout-enabled
     or cross-thread test path.
   - Confirm no send-bound fixture errors remain.

4. Recovery path

   - Simulate interrupted setup (partial data directory) and verify rerun
     succeeds without manual cleanup.

5. Manual settings path

   - If using custom `PgEnvCfg` conversion for tests, confirm migration from
     `to_settings()` to `to_settings_for_tests()` keeps expected behaviour.

## Notes for CI maintainers

If CI sometimes runs tests as `root`, keep worker configuration explicit and
deterministic:

- set `PG_EMBEDDED_WORKER` to an absolute path;
- keep `PATH` UTF-8 clean; and
- keep `PG_TEST_BACKEND` to supported values.

These settings make root and unprivileged runs behave consistently while
retaining the new cleanup and recovery benefits.
