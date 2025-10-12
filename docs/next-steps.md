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
- Rebased the PostgreSQL password file into the runtime tree and exported
  `PGPASSFILE` so the file is created by `nobody` rather than inside a
  root-owned temporary directory.
- Added regression tests to cover privilege dropping, directory modes, and the
  default-path selection logic.
- Verified `make fmt`, `make lint`, and `make test` all pass with the new
  behaviour.

## What Still Fails

- No failing behaviour is currently known after relocating the password file,
  but we still need to confirm the fix by observing a clean PostgreSQL setup
  while running as `nobody`.

## Recommended Next Actions

1. Re-run the privileged integration check (for example run
   `sudo strace -f -yy -s 256 ./target/debug/pg_embedded_setup_unpriv` or use
   `strace -f -yy -e trace=file -s 256 ./target/debug/pg_embedded_setup_unpriv 2>&1`
    followed by piping to `less -R`) to validate that PostgreSQL completes
   successfully without permission errors.
2. If any `EACCES` remains, capture the failing path from strace so we can
   inspect its ownership and mode before introducing additional corrective
   `chown` calls.
