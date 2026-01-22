# Promote pg_worker to first-class binary

This Execution Plan (ExecPlan) is a living document. The sections
`Constraints`, `Tolerances`, `Risks`, `Progress`, `Surprises & discoveries`,
`Decision log`, and `Outcomes & retrospective` must be kept up to date as work
proceeds.

Status: COMPLETED

## Purpose / big picture

Promote `pg_worker` from a test-only helper to a first-class production binary.
Currently, `pg_worker` lives under `tests/support/pg_worker.rs` and is gated
behind the `dev-worker` feature, making it unavailable for standard
installation with `cargo install --path .`. This creates friction for library
consumers who need to run the library in root mode.

After this change:

- `pg_worker` is built by default (no feature gate).
- `cargo install --path .` installs both `pg_embedded_setup_unpriv` and
  `pg_worker`.
- PATH-based autodiscovery allows root users to skip explicit
  `PG_EMBEDDED_WORKER` configuration.
- Documentation clearly explains root usage with copy-paste examples.
- Error messages guide users to install and configure the worker.

## Constraints

- **C1: Backward compatibility**: Existing test infrastructure must continue to
  work. Tests using `CARGO_BIN_EXE_pg_worker` and build directory discovery
  must not break.

- **C2: No feature gate on production binary**: `pg_worker` must build by
  default for all users. The `dev-worker` feature should only apply to
  test-only tools like `pg_worker_hang`.

- **C3: Explicit config takes precedence**: If `PG_EMBEDDED_WORKER` is set, it
  must override PATH-based autodiscovery. This allows custom worker binaries
  for development and testing.

- **C4: Code style**: Per `AGENTS.md`: files must stay under 400 lines, use
  en-GB-oxendict spelling, modules must have `//!` doc comments.

- **C5: Missing docs lint**: All public items require documentation
  (`#![deny(missing_docs)]`).

- **C6: Platform handling**: PATH search must work on both Unix (using `:`) and
  Windows (using `;`). Windows binaries should use the `.exe` suffix.

## Tolerances (exception triggers)

- **Scope**: If implementation requires changes to more than 10 files, stop and
  escalate.
- **Interface**: No changes to public library API surface except the new binary.
- **Dependencies**: If a new external dependency is required, stop and escalate.
- **Iterations**: If tests still fail after 3 attempts at fixing a particular
  issue, stop and escalate.
- **Ambiguity**: If behaviour of PATH search vs explicit config is unclear,
  escalate for user input.

## Risks

- **Risk**: Moving `pg_worker.rs` may break test discovery if imports are not
  updated. Severity: high. Likelihood: medium. Mitigation: Verify all imports
  and module references are updated. Run full test suite after move.

- **Risk**: PATH-based autodiscovery might find the wrong binary if the user has
  multiple `pg_worker` binaries in `PATH`. Severity: low. Likelihood: low.
  Mitigation: Document that `PG_EMBEDDED_WORKER` provides explicit control over
  which binary is used.

- **Risk**: Test code has its own worker discovery logic that might conflict
  with production autodiscovery. Severity: medium. Likelihood: low. Mitigation:
  test discovery uses `CARGO_BIN_EXE_pg_worker` and searches the build
  directory; production uses `PATH`. These are independent paths.

## Progress

- [x] (2026-01-22) Stage A: Preparation - read all affected files
- [x] (2026-01-22) Stage B: Move `pg_worker` to `src/bin/pg_worker.rs`
- [x] (2026-01-22) Stage C: Update `Cargo.toml` to remove feature gate
- [x] (2026-01-22) Stage D: Implement PATH-based autodiscovery
- [x] (2026-01-22) Stage E: Update error messages
- [x] (2026-01-22) Stage F: Update documentation (`docs/users-guide.md`)
- [x] (2026-01-22) Stage G: Update documentation (`README.md`)
- [x] (2026-01-22) Stage H: Final validation and testing

## Surprises & discoveries

- **Discovery**: The `src/bin/` directory does not exist yet. Impact: Create
  directory structure first, then move the file.

- **Discovery**: Tests have sophisticated worker discovery in
  `src/test_support/worker_env.rs` that searches the build directory and stages
  binaries in `/tmp` with hash-based paths. Impact: Test discovery is
  independent of production code and will continue working after the move.

- **Discovery**: `pg_worker_hang` is a test-only binary for timeout testing.
  Impact: Should remain under the `dev-worker` feature gate and in
  `tests/support/`.

- **Discovery**: Current error message in `worker_invoker/mod.rs:113` only
  mentions `PG_EMBEDDED_WORKER`. Impact: Must update to mention PATH discovery
  and installation instructions.

## Decision log

- **Decision**: Create `src/bin/` directory for production binaries.
  Rationale: Standard Rust project structure separates library code (`src/`)
  from binaries (`src/bin/`). Aligns with community conventions and makes
  intent clear. Date/Author: 2026-01-22 / Plan author.

- **Decision**: Remove `required-features = ["dev-worker"]` from `pg_worker`
  only. Rationale: Production users need `pg_worker` available by default. Keep
  the `dev-worker` feature for `pg_worker_hang`, which is purely for testing.
  Date/Author: 2026-01-22 / Plan author.

- **Decision**: Implement three-tier discovery strategy.
  Rationale: Explicit config → PATH search → error. Respects user preferences
  while providing sensible defaults. Matches user preference to mention both in
  error messages. Date/Author: 2026-01-22 / Plan author.

- **Decision**: Add "Root usage" section to `docs/users-guide.md` after "Quick
  start". Rationale: Users' guide is authoritative documentation for library
  usage. Placing after quick start makes it accessible but not overwhelming at
  the start. Date/Author: 2026-01-22 / Plan author (based on user preference).

## Outcomes & retrospective

### What was achieved

Successfully promoted `pg_worker` from a test-only helper to a first-class
production binary:

- Moved `pg_worker` from `tests/support/pg_worker.rs` to `src/bin/pg_worker.rs`.
- Removed feature gate from `pg_worker` in `Cargo.toml`, making it available by
  default.
- Implemented three-tier worker binary discovery (explicit config → PATH search
  → error).
- Updated error messages to guide users on installation and PATH discovery.
- Added "Root usage" sections to `docs/users-guide.md` and `README.md`.

All tests pass with no regressions, and the binary builds and installs
correctly.

### Metrics

- Files modified: 5 (below tolerance of 10)
- Test iterations: All tests pass on first run
- Linting: Zero clippy warnings, formatting compliant
- Documentation: Two sections added with clear examples

### Lessons learned

- Test discovery (`CARGO_BIN_EXE_pg_worker`) and production discovery (`PATH`)
  are independent and can coexist without conflict.
- Using `concat!()` for long string literals improves readability compared to
  backslash continuations.
- Platform-specific code (`cfg(unix)`) requires careful handling to ensure
  cross-platform compatibility.

## Context and orientation

### Key files

- `tests/support/pg_worker.rs` (374 lines): Current location of worker binary.
  Contains command-line interface (CLI) argument parsing, worker operation
  execution, and privilege dropping. Lines 1-38: module documentation. Lines
  98-135: `run_worker()` main function.

- `Cargo.toml` (226 lines): Package configuration. Lines 20-28: binary
  definitions. Lines 128-137: feature flags. Line 135: `dev-worker` feature
  definition.

- `src/bootstrap/env.rs` (330 lines): Environment variable parsing and worker
  binary discovery. Lines 103-129: `worker_binary_from_env()` checks
  `PG_EMBEDDED_WORKER` only. Lines 131-154: `validate_worker_binary()`
  validates the binary is a regular executable file.

- `src/cluster/worker_invoker/mod.rs` (456 lines): Dispatches PostgreSQL
  lifecycle operations. Lines 111-115: error when no worker binary is
  configured.

- `src/test_support/worker_env.rs` (330 lines): Test worker discovery. Lines
  31-44: `worker_binary()` searches build directory. Lines 287-305:
  `locate_worker_binary()` walks directory tree to find worker.

- `docs/users-guide.md` (628 lines): User documentation. Lines 583-606:
  "Integrating with root-only test agents" section (will be moved and expanded).

- `README.md` (152 lines): Project overview. Lines 15-36: configuration model
  section.

### Current architecture

```text
Root user without PG_EMBEDDED_WORKER
└─> Error: "PG_EMBEDDED_WORKER must be set"
└─> No PATH search
└─> No autodiscovery

Test infrastructure
└─> Uses CARGO_BIN_EXE_pg_worker
└─> Falls back to build directory search
└─> Independent from production code
```

*Figure: Current architecture showing missing production autodiscovery.*

### Terms

- **PATH search**: Searching directories in the `PATH` environment variable for
  an executable binary.

- **Feature gate**: Conditional compilation using Cargo features. A binary is
  only built when its feature is enabled.

- **Privilege dropping**: Running code as a different user (typically `nobody`)
  after initially running as `root`.

- **Autodiscovery**: Automatic discovery of resources (like binaries) without
  requiring explicit configuration.

## Plan of work

### Stage A: Preparation

Read all files that will be modified to verify current implementation and
ensure no unintended side effects. No code changes.

### Stage B: Move `pg_worker` to `src/bin/pg_worker.rs`

1. Create `src/bin/` directory.

2. Move `tests/support/pg_worker.rs` to `src/bin/pg_worker.rs`.

3. Update the binary definition in `Cargo.toml` from:

   ```toml
   [[bin]]
   name = "pg_worker"
   path = "tests/support/pg_worker.rs"
   required-features = ["dev-worker"]
   ```

   to:

   ```toml
   [[bin]]
   name = "pg_worker"
   path = "src/bin/pg_worker.rs"
   ```

4. Verify `pg_worker_hang` remains test-only:

   ```toml
   [[bin]]
   name = "pg_worker_hang"
   path = "tests/support/pg_worker_hang.rs"
   required-features = ["dev-worker"]
   ```

5. Run tests to ensure test infrastructure still works:

   ```bash
   make test
   ```

Expected: All tests pass with no failures related to worker binary discovery.

### Stage C: Update `Cargo.toml` to remove feature gate

Verify `Cargo.toml` has no `required-features` on the `pg_worker` binary. The
`dev-worker` feature should only apply to `pg_worker_hang`.

Test that the binary builds without features:

```bash
cargo build --release --bin pg_worker
ls target/release/pg_worker
```

Expected: Binary exists in `target/release/`.

### Stage D: Implement PATH-based autodiscovery

In `src/bootstrap/env.rs`, add PATH-based autodiscovery.

1. Add helper function `discover_worker_from_path()`:

   ```rust
   #[cfg(unix)]
   const WORKER_BINARY_NAME: &str = "pg_worker";
   #[cfg(windows)]
   const WORKER_BINARY_NAME: &str = "pg_worker.exe";

   fn discover_worker_from_path() -> Option<Utf8PathBuf> {
       let path_var = env::var_os("PATH")?;
       for dir in env::split_paths(&path_var) {
           let worker_path = Utf8PathBuf::from_path_buf(
               PathBuf::from(dir).join(WORKER_BINARY_NAME),
           ).ok()?;

           if worker_path.is_file() && is_executable(&worker_path) {
               return Some(worker_path);
           }
       }

       None
   }

   #[cfg(unix)]
   fn is_executable(path: &Utf8Path) -> bool {
       use std::os::unix::fs::PermissionsExt;
       path.metadata()
           .map(|m| m.permissions().mode() & 0o111 != 0)
           .unwrap_or(false)
   }

   #[cfg(not(unix))]
   fn is_executable(_path: &Utf8Path) -> bool {
       true
   }
   ```

   Note: `env::split_paths` already handles platform separators, so there is no
   need to use `PATH_SEPARATOR`.

2. Modify `worker_binary_from_env()` to implement three-tier discovery:

   ```rust
   pub(super) fn worker_binary_from_env() -> BootstrapResult<Option<Utf8PathBuf>> {
       // Tier 1: Explicit PG_EMBEDDED_WORKER
       if let Some(raw) = env::var_os("PG_EMBEDDED_WORKER") {
           let path = Utf8PathBuf::from_path_buf(PathBuf::from(&raw))
               .map_err(|_| {
                   let invalid_value = raw.to_string_lossy().to_string();
                   BootstrapError::from(color_eyre::eyre::eyre!(
                       "PG_EMBEDDED_WORKER contains a non-UTF-8 value: \
                        {invalid_value:?}. Provide a UTF-8 encoded \
                        absolute path to the worker binary."
                   ))
               })?;

           validate_worker_path(&path)?;
           return Ok(Some(path));
       }

       // Tier 2: PATH search
       if let Some(worker) = discover_worker_from_path() {
           validate_worker_path(&worker)?;
           return Ok(Some(worker));
       }

       // Tier 3: Not found
       Ok(None)
   }

   fn validate_worker_path(path: &Utf8PathBuf) -> BootstrapResult<()> {
       if path.as_str().is_empty() {
           return Err(BootstrapError::from(color_eyre::eyre::eyre!(
               "Worker binary path must not be empty"
           )));
       }
       if path.as_str() == "/" {
           return Err(BootstrapError::from(color_eyre::eyre::eyre!(
               "Worker binary path must not point at filesystem root"
           )));
       }

       validate_worker_binary(path)?;
       Ok(())
   }
   ```

3. Run tests to verify autodiscovery works:

   ```bash
   make test
   ```

Expected: Tests pass; root operations can find a worker in `PATH`.

### Stage E: Update error messages

In `src/cluster/worker_invoker/mod.rs`, update the error message on lines
111-115:

```rust
let worker = bootstrap.worker_binary.as_ref().ok_or_else(|| {
    BootstrapError::from(eyre!(
        "pg_worker binary not found. Install it with 'cargo install --path . --bin pg_worker' \
         and ensure it is in PATH, or set PG_EMBEDDED_WORKER to its absolute path"
    ))
})?;
```

In `src/bootstrap/env.rs`, ensure validation errors are clear and actionable.

### Stage F: Update documentation (`docs/users-guide.md`)

1. Move "Integrating with root-only test agents" section (lines 583-606) earlier
   in the document, after "Quick start" (around line 62).
2. Rename the section to "Root usage" and expand it (proposed content below).
3. Update the existing "Integrating with root-only test agents" reference (line
   622) to point to the new "Root usage" section.

Proposed content for `docs/users-guide.md`:

```markdown
## Root usage

When running as `root`, the library uses a privileged worker binary
(`pg_worker`) to execute PostgreSQL lifecycle operations as the `nobody` user.
This ensures that all filesystem mutations occur with the correct user and
group identifiers (UID/GID).

### Installing the worker

The worker binary is installed alongside the main library:

```bash
cargo install --path .
```

This installs both `pg_embedded_setup_unpriv` and `pg_worker` to the Cargo
binary directory. Verify installation:

```bash
which pg_embedded_setup_unpriv pg_worker
```

To install only the worker:

```bash
cargo install --path . --bin pg_worker
```

### Worker discovery

The library automatically discovers the worker using a two-tier strategy:

1. **Explicit configuration**: If `PG_EMBEDDED_WORKER` is set, the library uses
   that absolute path. Use this for custom worker binaries or testing.

2. **PATH search**: If `PG_EMBEDDED_WORKER` is not set, the library searches all
   directories in the `PATH` environment variable for `pg_worker` (or
   `pg_worker.exe` on Windows).

If the worker cannot be found, the library returns a helpful error with
installation instructions.

### Configuration examples

**Autodiscovery (recommended for most users)**:

```bash
# Install the worker
cargo install --path . --bin pg_worker

# Ensure it is in PATH (cargo install does this by default)
export PATH="$HOME/.cargo/bin:$PATH"

# Run the application as root
sudo -E your_application
```

**Explicit configuration (for custom workers)**:

```bash
export PG_EMBEDDED_WORKER="/custom/path/to/pg_worker"

# Run the application as root
sudo -E your_application
```

### Common issues

**Worker not found error**: Ensure `pg_worker` is installed and in PATH, or set
`PG_EMBEDDED_WORKER` to its absolute path.

```bash
# Check if worker is installed
which pg_worker

# If not found, install it
cargo install --path . --bin pg_worker
```

**Permission denied**: Ensure the worker binary is executable.

```bash
chmod +x ~/.cargo/bin/pg_worker
```

### Example: Using TestCluster as root

```rust
use pg_embedded_setup_unpriv::TestCluster;

fn bootstrap_as_root() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    // Ensure pg_worker is installed and in PATH before running as root
    let cluster = TestCluster::new()?;
    // … use cluster …
    Ok(())
}
```

Unprivileged users do not need to install the worker binary and can run
`TestCluster` directly without any additional setup.

```

### Stage G: Update documentation (`README.md`)

Add a "Root usage" section after "Configuration model" (around line 36), and
update the troubleshooting section to include worker-related issues.

Proposed content for `README.md`:

```markdown
## Root usage and worker binary

When running as `root`, the library requires a privileged worker binary
(`pg_worker`) to execute PostgreSQL operations safely while dropping privileges
to `nobody`.

### Installation

```bash
cargo install --path .
```

This installs both `pg_embedded_setup_unpriv` and `pg_worker`. Verify
installation:

```bash
which pg_embedded_setup_unpriv pg_worker
```

### Worker discovery

The library automatically discovers `pg_worker` from your PATH. If you need to
use a custom worker, set the `PG_EMBEDDED_WORKER` environment variable:

```bash
export PG_EMBEDDED_WORKER=/path/to/custom/worker
```

### Common issues

- **Worker not found**: Ensure `pg_worker` is installed and in PATH.
- **Permission errors**: Verify the binary is executable (`chmod +x`).

Unprivileged users do not need to install the worker binary.

```

### Stage H: Final validation and testing

1. Run full test suite:

   ```bash
   make test
   ```

   Expected: All tests pass with no failures.

2. Run linting and formatting checks:

   ```bash
   make check-fmt && make lint
   ```

   Expected: No warnings or formatting issues.

3. Test binary builds:

   ```bash
   cargo build --release --bin pg_worker --bin pg_embedded_setup_unpriv
   ls target/release/pg_worker target/release/pg_embedded_setup_unpriv
   ```

   Expected: Both binaries exist.

4. Test PATH-based autodiscovery (requires root):

   ```bash
   # Ensure pg_worker is in PATH
   export PATH="$HOME/.cargo/bin:$PATH"
   which pg_worker

   # Run a test that uses root mode
   cargo test --lib root
   ```

   Expected: Tests find worker without explicit `PG_EMBEDDED_WORKER`.

## Concrete steps

### After each stage (validation)

```bash
make check-fmt && make lint && make test
```

Expected: All pass with no warnings.

### After Stage B (file move)

```bash
ls -la src/bin/pg_worker.rs
cargo build --bin pg_worker
```

Expected: File exists in `src/bin/`, binary builds successfully.

### After Stage D (PATH discovery)

Create a test script to verify PATH discovery works:

```bash
#!/bin/bash
set -euo pipefail

# Build and install pg_worker to a custom location
cargo build --release --bin pg_worker
mkdir -p /tmp/test-path
cp target/release/pg_worker /tmp/test-path/

# Test PATH discovery
export PATH="/tmp/test-path:$PATH"
which pg_worker  # Should find our test binary

# Verify library can find it (run a simple test)
# … (implementation-specific test)
```

### Final validation (acceptance)

```bash
make check-fmt && make lint && make test
cargo build --release --bin pg_worker --bin pg_embedded_setup_unpriv
ls target/release/pg_worker target/release/pg_embedded_setup_unpriv
```

Expected: All checks pass, both binaries are built.

## Validation and acceptance

Quality criteria (acceptance):

- **Tests**: All existing tests pass. No regressions in worker binary discovery.
- **Lint/typecheck**: `make lint` passes with no warnings. `make check-fmt`
  passes.
- **Installation**: `cargo install --path .` installs both
  `pg_embedded_setup_unpriv` and `pg_worker`.
- **Autodiscovery**: Library finds `pg_worker` in `PATH` when
  `PG_EMBEDDED_WORKER` is not set.
- **Documentation**: "Root usage" section exists in both `docs/users-guide.md`
  and `README.md` with clear installation instructions.
- **Error messages**: Error when worker not found mentions both PATH discovery
  and `PG_EMBEDDED_WORKER` configuration.

Quality method (validation):

```bash
make check-fmt && make lint && make test
cargo install --path .
which pg_embedded_setup_unpriv pg_worker
```

Observable behaviour (verification):

1. Running `cargo install --path .` installs both binaries.
2. Running tests with root privileges finds worker without explicit
   configuration.
3. Running `which pg_worker` after installation shows the binary is in `PATH`.
4. Documentation clearly explains installation and configuration steps.

## Idempotence and recovery

All stages can be re-run safely. Each stage builds on the previous but does not
destroy intermediate state. If a stage fails partway:

1. Discard uncommitted changes: `git checkout -- .`
2. Re-read the affected files to understand the current state.
3. Resume from the beginning of the failed stage.

## Artefacts and notes

### Test discovery vs production autodiscovery

The test infrastructure has its own worker discovery mechanism in
`src/test_support/worker_env.rs`:

- **Tests**: Use `CARGO_BIN_EXE_pg_worker` environment variable set by Cargo,
  then search the build directory for the binary. This ensures tests find the
  freshly built binary in `target/debug/` or `target/release/`.

- **Production**: Check `PG_EMBEDDED_WORKER` first, then search `PATH`. This
  assumes the worker is installed (via `cargo install`) or in a known location.

These are independent and serve different purposes:

*Table: Worker discovery mechanisms by context*

| Context    | Discovery mechanism             | Purpose                   |
| ---------- | ------------------------------- | ------------------------- |
| Tests      | Build directory search          | Find freshly built binary |
| Production | Explicit config → PATH search   | Find installed binary     |

### Binary location structure

After Stage B, directory structure will be:

```text
src/
├── bin/
│   └── pg_worker.rs           # Production worker binary
├── lib.rs
├── main.rs                    # pg_embedded_setup_unpriv binary
└── …

tests/
├── support/
│   ├── pg_worker.rs           # Removed (moved to src/bin/)
│   ├── pg_worker_hang.rs      # Test-only timeout helper
│   └── …
└── …
```

### PATH search algorithm

The PATH search iterates through directories in order and returns the first
valid executable found:

1. Split `PATH` using the platform rules (via `env::split_paths`).
2. For each directory:

   1. Append `pg_worker` (or `pg_worker.exe` on Windows).
   2. Check if the path exists and is a regular file.
   3. Check if the file is executable (Unix only: mode has the execute bits
      set).
   4. If all checks pass, return this path.
3. If no valid binary is found, return `None`.

This matches user preference to mention both `PATH` and explicit configuration
in error messages.

## Interfaces and dependencies

### Binary interface

New production binary at `src/bin/pg_worker.rs`:

- **CLI**: `pg_worker <operation> <config-path>`
- **Operations**: `setup`, `start`, `stop`
- **Input**: JSON configuration file with `WorkerPayload` structure
- **Exit codes**: `0` on success, `1` on error

No changes to binary interface or behaviour from previous implementation.

### Library interface changes

**New function in `src/bootstrap/env.rs`**:

```rust
fn discover_worker_from_path() -> Option<Utf8PathBuf>
```

Internal helper for PATH-based worker discovery. Not public API.

**Modified function in `src/bootstrap/env.rs`**:

```rust
pub(super) fn worker_binary_from_env() -> BootstrapResult<Option<Utf8PathBuf>>
```

Now implements three-tier discovery (explicit → PATH → not found).

### Dependencies

No new external dependencies. Uses existing:

- `std::env` for environment variable access
- `camino` for UTF-8 path handling
- Existing error handling infrastructure
