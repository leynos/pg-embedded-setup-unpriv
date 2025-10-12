# Next Steps for Root-Oriented Postgres Testing

## Ergonomic Improvements

- Provide a high-level `bootstrap_for_tests()` helper that wraps `PgEnvCfg::load()`, runs `pg_embedded_setup_unpriv::run()`, and returns the derived `PgSettings` plus useful paths. Encapsulate timezone (`TZDIR`, `TZ`) and password-file scaffolding so agent tests do not repeat boilerplate.
- Expose a `TestCluster` RAII struct that drops to `nobody`, starts `postgresql_embedded`, and stops the cluster via `Drop`. Include convenience methods for creating Diesel connections and applying SQL fixtures.
- Offer a `ensure_pg_binaries_cached()` function that pre-populates the Theseus archive, avoiding repeated downloads and GitHub rate-limit failures in busy CI environments. Allow it to read `GITHUB_TOKEN` automatically.
- Provide helper APIs to install missing prerequisites (e.g. detect missing `tzdata` and emit actionable errors pointing to the package manager command).
- Publish rstest fixtures (e.g. `#[fixture] pub fn test_cluster()`) that leverage these helpers, making root-based integration tests concise and consistent across crates.
- Add logging instrumentation (using `tracing`) to surface directory ownership changes, timezone configuration, and any environment overrides applied by the helpers.

## Recommended Workflow Enhancements

1. Add `docs/testing-playbook-root.md` containing copy-pastable recipes for starting a cluster, seeding data, and cleaning up in root-required workflows.
2. Update CI images to install `tzdata` (and optionally cache the PostgreSQL archive) to prevent the `TimeZone` regression from resurfacing.
3. Provide a `Makefile` target (e.g. `make e2e-root`) that runs the privileged Diesel e2e with the right environment variables, enabling quick validation for contributors.
4. Add smoke tests and clippy lints covering the proposed helper functions to guarantee their behaviour under both root and unprivileged invocations.
