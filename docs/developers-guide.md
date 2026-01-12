# pg_embedded_setup_unpriv developer guide

This guide captures contributor-focused notes for maintaining the library. It
complements the user guide and omits consumer-facing usage details.

If you are integrating the library, start with `docs/users-guide.md` for
consumer-facing guidance.

## Test coverage notes

- Unit and behavioural tests assert that `postmaster.pid` disappears after
  `TestCluster` teardown, demonstrating that no orphaned processes remain.
- Behavioural tests driven by `rstest-bdd` exercise both privilege branches to
  guard against regressions in ownership or permission handling.

## Feature coverage in CI

The default feature set keeps Diesel optional for consumers, while `make test`
enables `--all-features` so the Diesel helpers are exercised by smoke tests.

## Loom concurrency tests

Loom-based checks for `ScopedEnv` are opt-in and only compile when the
`loom-tests` feature is enabled. The Loom tests are marked `#[ignore]` so
`make test --all-features` stays deterministic unless you opt in explicitly.
Run the Loom suite with:

```sh
cargo test --features "loom-tests,dev-worker" --lib -- --ignored
```

## Further reading

- `tests/e2e_postgresql_embedded_diesel.rs` – example of combining the helper
  with Diesel-based integration tests while running under `root`.
