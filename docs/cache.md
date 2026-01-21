# Shared binary cache

The `pg_embedded_setup_unpriv` crate provides a shared binary cache that stores
PostgreSQL binaries across test runs. When `TestCluster` creates a cluster, it
first checks the cache for a matching version. If binaries are found, they are
copied to the installation directory, avoiding the download cost. After a
successful download, the cache is populated so subsequent runs benefit.

## Cache location

The cache directory is resolved using the following priority:

1. `PG_BINARY_CACHE_DIR` environment variable (when set and non-empty)
2. `$XDG_CACHE_HOME/pg-embedded/binaries` (when `XDG_CACHE_HOME` is set)
3. `~/.cache/pg-embedded/binaries` (home directory fallback)
4. `/tmp/pg-embedded/binaries` (last resort when none of the above are
   available)

The `/tmp` fallback ensures the cache functions in restricted environments such
as CI containers where the home directory may be inaccessible.

## Cache structure

```plaintext
{cache_dir}/
  .locks/
    17.4.0.lock
    16.3.0.lock
  17.4.0/
    .complete
    bin/
      postgres
      pg_ctl
      ...
    lib/
      ...
    share/
      ...
  16.3.0/
    .complete
    bin/
    lib/
    share/
```

- **`.locks/`**: Contains per-version lock files for cross-process coordination.
- **`{version}/`**: Version-specific directories holding extracted binaries.
- **`.complete`**: Marker file indicating a valid, complete cache entry.

A cache entry is valid when both the `.complete` marker and the `bin/`
subdirectory exist. Incomplete entries (missing marker or binaries) are ignored
during cache lookups.

## Version matching

The cache supports semver version requirements. When `TestCluster` requests
`^17`, the cache scans for directories whose names parse as valid semver
versions matching the requirement. The highest matching version is selected.

For example, with cached versions `17.2.0` and `17.4.0`, a requirement of `^17`
matches `17.4.0`.

## Cross-process coordination

The cache uses file-based advisory locking (`flock(2)` on Unix) to coordinate
concurrent access:

- **Shared locks**: Acquired when reading from the cache. Multiple readers can
  access the same version concurrently.
- **Exclusive locks**: Acquired when downloading or populating the cache,
  preventing concurrent writes.

Locks are per-version, allowing different versions to be downloaded in parallel
without contention.

On non-Unix platforms, locking is not supported; concurrent tests may race.

## Integration with TestCluster

`TestCluster` integrates with the cache transparently:

1. Before setup, the cluster checks the cache for a matching version.
2. On cache hit, binaries are copied to the installation directory and
   `trust_installation_dir` is set to skip re-validation.
3. On cache miss, binaries are downloaded normally.
4. After a successful download, the cluster populates the cache for future use.

Cache operations are best-effort: failures log warnings and fall back to normal
downloads without blocking test execution.

## Environment variables

Table: Environment variables used by the binary cache.

| Variable              | Description                           |
| --------------------- | ------------------------------------- |
| `PG_BINARY_CACHE_DIR` | Override the cache directory location |
| `XDG_CACHE_HOME`      | Standard XDG cache base directory     |

## Startup sequence

Figure: `TestCluster` startup with binary cache integration.

```mermaid
sequenceDiagram
    actor TestAuthor
    participant TestCode
    participant TestCluster
    participant startup as startup module
    participant cache_integration as cache_integration module
    participant cache as cache module
    participant CacheLock
    participant installation as installation module
    participant PostgreSQL

    TestAuthor->>TestCode: run_tests()
    TestCode->>TestCluster: new()
    activate TestCluster

    TestCluster->>TestCluster: bootstrap_for_tests()
    Note right of TestCluster: Resolves settings from<br/>environment and config

    TestCluster->>startup: cache_config_from_bootstrap(bootstrap)
    startup-->>TestCluster: cache_config

    TestCluster->>startup: start_postgres(runtime, bootstrap, env_vars, cache_config)
    activate startup

    startup->>cache_integration: try_use_binary_cache(config, version_req, bootstrap)
    activate cache_integration

    cache_integration->>cache: find_matching_cached_version(cache_dir, version_req)

    alt cache_hit
        cache-->>cache_integration: (version, source_dir)
        cache_integration->>CacheLock: acquire_shared(cache_dir, version)
        CacheLock-->>cache_integration: lock

        Note right of cache_integration: Double-check after<br/>acquiring lock
        cache_integration->>cache: check_cache(cache_dir, version)
        cache-->>cache_integration: CacheLookupResult::Hit { source_dir }

        cache_integration->>cache: copy_from_cache(source_dir, target_dir)
        cache-->>cache_integration: Ok(())

        Note right of cache_integration: Update bootstrap settings
        cache_integration->>cache_integration: set installation_dir
        cache_integration->>cache_integration: set trust_installation_dir = true
        cache_integration->>cache_integration: set exact version requirement

        cache_integration-->>startup: true
    else cache_miss
        cache-->>cache_integration: None
        cache_integration-->>startup: false
    end
    deactivate cache_integration

    startup->>startup: handle_privilege_lifecycle(privileges, runtime, bootstrap, env_vars)

    alt root_privileges
        startup->>startup: invoke_lifecycle_root(runtime, bootstrap, env_vars)
        Note right of startup: Worker subprocess<br/>manages PostgreSQL
    else unprivileged
        startup->>PostgreSQL: new(settings)
        startup->>startup: invoke_lifecycle(runtime, bootstrap, env_vars, embedded)
        PostgreSQL->>PostgreSQL: setup()
        PostgreSQL->>PostgreSQL: start()
    end

    opt cache_miss
        startup->>cache_integration: try_populate_binary_cache(config, settings)
        activate cache_integration

        cache_integration->>installation: resolve_installed_dir(settings)
        installation-->>cache_integration: installed_dir

        cache_integration->>cache_integration: extract_version_from_path(installed_dir)

        Note right of cache_integration: Skip if already cached
        cache_integration->>cache: check_cache(cache_dir, version)
        cache-->>cache_integration: CacheLookupResult::Miss

        cache_integration->>CacheLock: acquire_exclusive(cache_dir, version)
        CacheLock-->>cache_integration: lock

        Note right of cache_integration: Double-check after<br/>acquiring exclusive lock
        cache_integration->>cache: check_cache(cache_dir, version)
        cache-->>cache_integration: CacheLookupResult::Miss

        cache_integration->>cache: populate_cache(source, cache_dir, version)
        cache-->>cache_integration: Ok(())

        deactivate cache_integration
    end

    startup-->>TestCluster: StartupOutcome
    deactivate startup

    TestCluster-->>TestCode: TestCluster instance
    deactivate TestCluster
```

## Cache maintenance

The cache does not perform automatic cleanup. To clear stale entries:

```bash
rm -rf ~/.cache/pg-embedded/binaries
```

Or remove specific versions:

```bash
rm -rf ~/.cache/pg-embedded/binaries/16.3.0
```

Lock files in `.locks/` may be safely deleted when no processes are actively
using the cache.
