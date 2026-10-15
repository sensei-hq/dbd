# Checkpoint

**Slice:** #65 — a key a foreign key holds, and a routine whose return type
changes; then the patch 0.27.1. Branch `fix/65-key-change` (from `develop`).

## Done

- 0.27.0 released; `develop` merged to `main` (PR #71); the shipped binary
  split and deployed a scratch Postgres project.
- Reconcile: the foreign keys holding a dropped key (PK, UNIQUE, unique index,
  a key column, one turned generated) go first, kept ones come back after the
  new key, replaced ones in their own step — those steps one transaction, a
  10s lock timeout; a holder outside the run (read from `pg_constraint`) is
  refused by name.
- Routines: `CREATE OR REPLACE` refused as a replace conflict → dropped and
  created again (one routine of the name, no dependents), grants/owner/comment
  put back; a file with its own `BEGIN`/`COMMIT` runs as before.
- Migrations: the same ordering per version; on PostgreSQL (by URL) an unnamed
  PK/UNIQUE/FK dropped by catalog lookup.
- Two adversarial review rounds, every finding fixed red-first; docs on every
  surface; CHANGELOG `[Unreleased]`. Filed #72, #73.

## Next

1. PR `fix/65-key-change` → `develop`, CI green, merge locally.
2. `develop`: CHANGELOG `[0.27.1]` heading; `make bump` (patch); release.yml;
   PR `develop` → `main`, CI, merge.
3. Installed 0.27.1: the #65 repro on the scratch Postgres, and a released key
   change through `dbd snapshot` + `apply`.

## Open

- #60 (nextval spelling), #72, #73; unfiled: `init --dry-run` writes files;
  SQLite scopes cannot name a table; a dry run with `--allow-destructive`
  still says to re-run with it; SQLite `deploy --module shared` FK error.
- Scratch to delete: /tmp/dbd-verify-pg (stop: `pg_ctl -D
  /tmp/dbd-verify-pg/data stop`), /tmp/dbd-review-65-*, /tmp/dbd-r65-scratch,
  /tmp/dbd-65-*, /tmp/dbd-repro-65*, /tmp/dbd-verify-0.27.0*.
