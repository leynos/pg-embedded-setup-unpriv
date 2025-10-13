# Zero-Config RAII Postgres Test Fixture Design

To evolve **pg-embedded-setup-unpriv** into a seamless, zero-configuration RAII
test fixture, we will create a high-level testing helper that automatically
handles environment differences (root vs non-root) and integrates with Rust
test frameworks. The goal is for developers to **write tests that spin up an
embedded PostgreSQL instance with no manual setup**, whether running as root
(e.g. Codex sandbox) or as an unprivileged user (e.g. GitHub CI), using the
same code. Below we outline the design steps and features:

## Automatic Privilege Detection (Root vs. Unprivileged)

We will make the fixture **auto-detect the execution context** and adjust
accordingly, so the developer “shouldn’t have to do anything”
extra([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
 This can be done at runtime by checking the effective user ID:

- **If running as root on Linux:** the helper will perform the necessary
  privilege drop to a safe user (such as `"nobody"`) before initializing
  PostgreSQL. This uses the existing logic to temporarily drop `EUID` and
  `EGID` to `"nobody"` for filesystem
  operations([2](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/src/lib.rs#L50-L58))([2](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/src/lib.rs#L75-L83)).
   This ensures all Postgres files/directories are owned by a non-root user,
  since PostgreSQL will refuse to run as root.

- **If running as a normal user or on non-Linux platforms:** no privilege
  dropping is needed. The helper will simply use the `postgresql_embedded`
  crate in its normal mode (which runs as the current user). On MacOS/BSD, we
  treat the helper as a **no-op pass-through** to `postgresql_embedded`
  (privileged setup is only targeted on Linux) – meaning the library will just
  use the embedded Postgres defaults without special permission handling.

This runtime detection makes the behavior “fixed per environment” but
**transparent to the developer** – your test code calls the same API in all
cases, and the library decides whether to invoke the privileged path or not.
(Compile-time detection is less practical for root vs non-root, so a runtime
check of UID is simplest and robust.)

## RAII `TestCluster` Implementation

We will introduce a `TestCluster` RAII struct that encapsulates an embedded
Postgres instance’s
lifecycle([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L7)).
 This struct will hide all setup/teardown details:

- **On creation/start:** it will configure and launch a PostgreSQL instance
  (using `postgresql_embedded`) appropriate to the environment. If running as
  root, `TestCluster` will internally drop privileges to `"nobody"` and
  initialize the database directories using our current helper
  logic([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
   If not root, it will directly initialize under the current user. In either
  case, it calls the embedded Postgres **`.setup()` and `.start()`** routines
  (async or blocking) to actually download/init the database and launch the
  server process.

- **On drop:** `TestCluster` will implement `Drop` to automatically **stop the
  database and clean up**. Upon going out of scope (e.g. end of a test), it
  will call `postgresql_embedded.stop()` to shut down the Postgres subprocess
  gracefully([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L40-L49))([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L59-L67)).
   This ensures no orphan processes or locks remain, giving true RAII semantics.

- **Connection info and utilities:** `TestCluster` can hold the connection
  parameters (port, host, credentials) or even provide convenience methods. For
  example, we can include a method like `get_diesel_connection()` to quickly
  obtain a Diesel `PgConnection` to the test database, and perhaps methods to
  execute SQL or apply fixture
  scripts([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L7)).
   (While schema loading or SQL fixtures are a *“day 2”* feature, designing
  `TestCluster` with extension points for running setup SQL is wise for future
  needs.)

- **Zero configuration defaults:** To keep it zero-config, `TestCluster` will
  choose sensible defaults if the user doesn’t specify any. We’ll leverage
  `PgEnvCfg::load()` (from the existing OrthoConfig integration) to gather any
  env/file configs, but if none are provided, use
  `postgresql_embedded::Settings::default()` with smart
  tweaks([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
   This means using the latest PostgreSQL version by default, a random free
  port, and temporary directories for data. All of this happens behind the
  scenes, so in most cases **the developer simply creates a `TestCluster` with
  no arguments**.

## Bootstrapping and Configuration Handling

We will provide a high-level **`bootstrap_for_tests()` helper** as a one-call
setup for
tests([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
 This function will:

- Load the configuration from environment or config files via
  `PgEnvCfg::load()` (using ortho_config), applying any overrides like
  `PG_VERSION_REQ`, `PG_PORT`, etc., but **the developer need not set
  anything** for defaults.

- Internally handle the privilege drop and call our existing
  `pg_embedded_setup_unpriv::run()` logic to prepare directories if
  needed([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
   In root mode, this means creating/chowning the data and installation
  directories to `"nobody"` and initializing the DB cluster files (essentially
  performing an
  `initdb`)([2](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/src/lib.rs#L363-L371)).
   In unprivileged mode, this step can be bypassed or simplified since no chown
  is required.

- Return the resulting configuration (e.g. a `PgSettings` or our own struct)
  and paths that were
  used([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
   This gives visibility into where the data directory and binaries are, and
  the connection info (like the chosen port).

In practice, **`TestCluster::start()` can wrap `bootstrap_for_tests()`**
internally. For example, `TestCluster::start()` would call
`bootstrap_for_tests()` to do all the config+init work, then launch the
Postgres server process and yield a `TestCluster` instance that holds the
running server. The idea is to encapsulate all “boilerplate” (timezone env,
password file handling, etc.) so that tests do not repeat those
steps([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
 This makes starting a test database **trivial** – essentially one line in the
test setup.

### Ephemeral Ports and Isolation

To allow the same tests to run concurrently (especially under `nextest` which
runs tests in parallel), our fixture should avoid fixed resources like static
ports. We will configure the cluster to use **ephemeral ports by default**,
unless a specific `PG_PORT` is given. The `postgresql_embedded` crate supports
running on ephemeral
ports([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L22-L25)),
 so by setting the port to 0 in the Settings (or leaving it unspecified and
letting the crate choose), each `TestCluster` will get a free port assigned at
runtime. The chosen port can be obtained from the returned settings or directly
via the `PostgreSQL` handle after startup.

Similarly, we should ensure each cluster uses a unique data directory (if not
explicitly configured). For example, we can generate a temp directory (in
`/var/tmp/pg-embed-<uid>/...` or using `tempfile::tempdir`) for each test
instance. The current default is to use `/var/tmp/pg-embed-<uid>/data` and
`.../install` for a given
UID([2](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/src/lib.rs#L90-L98))
 – we may tweak this so that if multiple clusters are initialized in one run,
they don’t all target the exact same path. One approach is to include a random
suffix or use the OS tempdir for isolation. This way, two tests running in
parallel won’t conflict over the data directory or lock files. The
**installation binaries cache** could still be shared (so we don’t re-download
Postgres multiple times), but the database **data directory will be distinct
per cluster**.

## Integration with Test Frameworks (Sync and Async Tests)

We will ensure the library works smoothly in both synchronous and asynchronous
Rust tests:

- **Synchronous tests (`cargo test`):** We can enable the `postgresql_embedded`
  crate’s `"blocking"` feature to use its blocking
  API([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L53-L61)).
   This allows calling `PostgreSQL::setup()` and `start()` in a normal #[test]
  function without needing an async runtime. For example,
  `TestCluster::start()` can internally call the blocking setup/start and
  return once the DB is running. Developers can then use Diesel or any blocking
  client directly.

- **Asynchronous tests (tokio):** In async test contexts, we’ll use the async
  API of the
  crate([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L33-L42)).
   We might provide an `async fn start_async() -> TestCluster` that awaits the
  `.setup().await` and `.start().await` internally. This yields a running
  cluster that async tests can interact with (e.g. using `sqlx` or other async
  DB clients).

- **Uniform API:** To keep things ergonomic, `TestCluster` could be made such
  that calling it in a sync test will do the blocking startup, but the same
  struct can be used in an async test by calling an async constructor.
  Internally both share the same logic, just different execution (one uses an
  internal Tokio runtime or the blocking feature). This way, the **same
  `TestCluster` type works for both sync and async tests**, fulfilling the
  “foundation for sync and async tests” requirement. For example:

```rust
rustCopy code`// Synchronous test
#[test]
fn test_something() {
    let cluster = TestCluster::start().expect("PG start failed");
    // ... use cluster (Diesel, etc.)
}

// Asynchronous test
#[tokio::test]
async fn test_async_thing() {
    let cluster = TestCluster::start_async().await.expect("PG start failed");
    // ... use cluster with async client
}
`
```

Under the hood, both will configure and launch Postgres appropriately (dropping
privileges if root, etc.). The underlying crate supports both patterns, so
we’ll leverage
that([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L22-L30))([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L40-L49)).

- **rstest fixtures:** We plan to publish built-in *fixtures* for the rstest
  framework([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L8-L10)).
  For example, the library can expose:

```rust
rustCopy code`#[fixture]
pub fn test_cluster() -> TestCluster {
    TestCluster::start().unwrap()
}
`
```

This allows tests to simply declare `fn my_test(cluster: TestCluster) { ... }`
and get a running DB instance injected automatically. This will make
integration tests highly concise and consistent across projects, since everyone
can use the same fixture name and behavior. The fixture will handle both root
and non-root cases identically (abstracted behind `TestCluster`).

- **Parallel test runners:** Using `cargo nextest` (or even running
  `cargo test -- --test-threads=n`), multiple tests may run concurrently. We
  have addressed this by using ephemeral ports and separate data directories as
  noted. We’ll also implement any necessary synchronization to avoid race
  conditions on initial download (for example, one test could call
  `ensure_pg_binaries_cached()` at the start of the suite – see below). With
  these measures, our fixture will be **nextest-ready**, and tests can run in
  parallel without interfering with each other.

## Caching and CI-Friendly Features

To optimize performance and reliability in CI or rapid local testing, we will
add a couple of helper functions:

- **Binary Cache Preloading:** Provide an `ensure_pg_binaries_cached()`
  function([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L7-L8))
  that pre-fetches the PostgreSQL binaries archive (using the configured
  version). This would essentially invoke the download logic of
  `postgresql_embedded` ahead of time. In a busy CI environment, this avoids
  each test run (or each parallel test) hitting the GitHub releases API and
  potentially running into rate limits. We can make this function automatically
  use a `GITHUB_TOKEN` from the environment to authenticate and increase rate
  limits during the
  download([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L7-L8)).
   Developers could call this in a `build.rs` or a test setup hook (or we might
  integrate it into `bootstrap_for_tests` to run once). After caching, all test
  clusters can reuse the local archive, resulting in faster startup and a
  *flakeless* experience even with many tests.

- **Prerequisite checks (tzdata, etc.):** The helper will detect common
  environment issues and either fix or emit clear
  errors([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L7-L8)).
   A primary example is the TimeZone data requirement – if the host is missing
  the `tzdata` package, PostgreSQL may fail to start with a timezone
  error([4](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/README.md#L70-L73)).
   We can proactively check for the presence of the system timezone database
  (e.g. check if `/usr/share/zoneinfo` exists) and **guide the user to install
  it** if not. For instance, if not found, we can return an error like *“Time
  zone database not found (tzdata missing). Please install tzdata (e.g.
  `apt-get install tzdata`) on this
  system.”*([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L7-L8)).
   Similar checks could be done for other prerequisites (though tzdata is the
  main one encountered). By doing this, we prevent puzzling failures and make
  the developer experience smoother.

- **Environment setup (TZDIR, etc.):** The library can also set up any required
  environment variables automatically. For example, if we need to set `TZDIR`
  or `TZ` environment for the embedded Postgres to find timezone info, or
  `PGPASSFILE` for the generated password file, the `bootstrap_for_tests()`
  should handle that
  internally([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L6)).
   This encapsulates all those nitty-gritty details so tests don’t need to
  worry about them.

## Logging and Visibility

For a pleasant developer experience, we will add **logging instrumentation** to
the helper’s
operations([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L9-L10)).
 Using the `tracing` crate, we can emit debug/info logs for steps like:

- Dropping privileges (e.g. “Dropping from root to nobody for Postgres setup”).

- Directory creation and ownership changes (e.g. “Chowning
  /var/tmp/pg-embed-1000 to user nobody”).

- Setting environment variables (like informing if we override `HOME`,
  `XDG_CACHE_HOME`, `PGPASSFILE` paths for the embedded process).

- Postgres startup events (downloading binaries, starting server on port XYZ,
  etc.).

These logs (visible when tests are run with `RUST_LOG` configured) will help
troubleshoot any issues in the setup process without the developer having to
guess what the helper is
doing([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L9-L10)).
 By surfacing directory paths and config values in logs, users can verify that
the auto-detection picked up the right settings (for example, confirming it
used an ephemeral port or the expected PG version).

## Platform Support and Limitations

Initially, our focus is **Linux** for the root-dropping functionality. On
Linux, running tests as root will trigger the privileged path (drop to
“nobody”) so that Postgres can be initialized safely. On other Unix-like OSes
(macOS, FreeBSD), we will not attempt any special privilege management –
typically tests on those platforms run as normal user anyway. If someone did
run as root on macOS, we might simply error out or treat it as unprivileged
(since our dropping logic is primarily tested on Linux). The embedded Postgres
crate itself supports Mac/Windows, but those environments won’t need our extra
helper – effectively the fixture will just call `PostgreSQL::setup()` and
`start()` directly on those platforms.

We are **leveraging the `postgresql-embedded` crate** as the core engine for
cross-platform
support([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L11-L19)),
 so all OS-specific nuances of downloading and running PostgreSQL are handled
by that library. Our layer is an orchestration on top to handle permission and
configuration in a test-friendly way. This means as `postgresql-embedded` gains
features or support, our fixture inherits them. (For example, Windows support
is out-of-scope for now, but could be considered in the future through that
crate.)

## Summary: Developer Experience

Once these improvements are in place, using the library in tests will be
extremely easy and consistent:

- **No manual setup:** The test writer does not need to manually create users,
  directories, or call out to `sudo` scripts. Just calling our fixture is
  enough to get a working empty PostgreSQL instance.

- **Same code runs anywhere:** The *same test code* runs on a root CI (where
  behind the scenes we chown and drop privileges) and on an unprivileged
  machine (where it just uses the current user). There’s no need for
  conditional logic in the test depending on environment – the library handles
  it.

- **Integration with frameworks:** Using the provided rstest fixture or
  similar, tests can be written in a clean style without repetitive
  setup/teardown code. For example, a test function can simply accept a
  `test_cluster: TestCluster`
  parameter([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L8-L10))
   and immediately proceed to execute queries against the database, relying on
  RAII for cleanup.

- **Defaults with escape hatches:** By default, everything is auto-chosen
  (ports, temp dirs, latest PG version). If needed, developers can still
  override via env vars or config files (`pg.toml`, etc.) using the OrthoConfig
  system([4](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/README.md#L20-L28)),
   but this is purely optional. In most cases “it just works” with zero config.

In conclusion, **pg-embedded-setup-unpriv** will evolve from a low-level
bootstrap tool into a robust test fixture foundation. By combining automatic
root handling, an RAII `TestCluster` struct, and helper utilities (for caching
and prerequisites), we provide a **smooth, seamless developer experience** for
Postgres integration testing. Developers can write tests once and run them
anywhere with confidence that the embedded database will spin up and tear down
correctly, whether under root in a container or as a normal user on a laptop.
These changes align with the planned ergonomic
improvements([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L10))
 and will make PostgreSQL integration tests in Rust as effortless as using an
in-memory database, but with full PostgreSQL fidelity.

**Sources:**

- pg_embedded_setup_unpriv – *Next Steps for Root-Oriented Postgres
  Testing*([1](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/docs/next-steps.md#L5-L10))
  (design goals for fixtures, RAII cluster, caching, etc.)

- pg_embedded_setup_unpriv – *README (Usage and
  prerequisites)*([4](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/README.md#L62-L71))([4](https://github.com/leynos/pg-embedded-setup-unpriv/blob/2faace459329747e62fa4cd479318aa1bfb07628/README.md#L70-L73))
  (need for root and tzdata for embedded Postgres)

- theseus-rs/postgresql_embedded – *README (features and
  examples)*([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L20-L28))([3](https://github.com/theseus-rs/postgresql-embedded/blob/b907b245e3f6aa22d6f1009ffd6af1a3a2e56635/postgresql_embedded/README.md#L40-L49))
  (capabilities of the underlying embedded Postgres crate, async vs blocking
  API, ephemeral ports support)
