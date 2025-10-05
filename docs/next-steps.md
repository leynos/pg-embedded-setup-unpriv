# Next Steps For Unprivileged PostgreSQL Setup

## What Has Been Achieved

- Dropped the process real and effective user and group IDs to `nobody`,
  including supplementary groups, so `getuid()` and friends observe the
  unprivileged identity.
- Forced embedded PostgreSQL paths away from `/root` and into
  `/var/tmp/pg-embed-<uid>` (or caller-supplied directories) before the
  privilege drop.
- Normalised ownership and permissions of the installation and data trees so
  PostgreSQL sees `0700` on its data directory and `nobody:nogroup` on cached
  artefacts.
- Added regression tests to cover privilege dropping, directory modes, and the
  default-path selection logic.
- Verified `make fmt`, `make lint`, and `make test` all pass with the new
  behaviour.

## What Still Fails

- `postgresql_embedded::setup()` still aborts with `EACCES` while running as
  `nobody`. The trace shows `open(..., O_WRONLY|O_CREAT|O_TRUNC)` calls failing
  inside the installation directory structure, even after the recursive `chown`.

## Recommended Next Actions

1. Inspect the failing path from the strace (the argument to the failing
   `openat`) and confirm its parent directories inherit `0700` or `0755` with
   the correct ownership. There may be a remaining subdirectory that was
   created *after* the recursive `chown` ran.
2. Consider running the setup once as root, capture the filesystem state, and
   compare against the state after the failure to isolate which files are left
   owned by root.
3. If the culprit is a file created post-drop, add another targeted `chown`
   (or run `ensure_tree_owned_by_user` immediately before invoking
   `pg.setup()`) to reclaim ownership of freshly unpacked artefacts.
4. Re-run the privileged integration check (`sudo strace -f -yy -s 256` on the
   binary) to validate that PostgreSQL completes successfully without
   permission errors.
