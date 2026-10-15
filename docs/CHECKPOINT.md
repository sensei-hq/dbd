# Checkpoint

**Slice:** #65 — a key change a foreign key holds, and a function whose
return type changes; then the patch 0.27.1. Branch `fix/65-key-change`
(from `develop`).

## Done

- 0.27.0 released (crates.io), `develop` merged to `main` (PR #71), CI green;
  the installed binary split and deployed a scratch Postgres project.
- Slice 1 (reconcile): `plan_key_holders` drops the foreign keys holding a key
  the plan drops before it, adds back the ones the design keeps, refuses one
  on a table outside the run; `execute_reconcile_writes` runs the key change
  and the step re-adding a replaced foreign key in one transaction.
- Slice 2 (functions): Postgres `apply_routine` drops and re-creates a routine
  `CREATE OR REPLACE` refuses (42P13/42809) when it is the only one of its
  name and nothing depends on it — reconcile and apply (savepoint in a batch).
- Slice 3 (migrations): `MigrationDialect` + `create_snapshot_in`; a version's
  migration drops the foreign keys holding a key first, re-adds kept ones; on
  PostgreSQL an unnamed key is dropped by catalog lookup. Review fixes: a
  unique index and a column turned generated are held keys.

## Next

1. Failure-mode review fixes (red first): `SET LOCAL lock_timeout` in the
   key-change span; holders from `pg_constraint` (a table the role cannot see
   in information_schema refuses). Then review slices 2 and 3.
2. Docs (guide reconcile/migrations, CHANGELOG), `make bump` patch 0.27.1.

## Open

- Issue #60: reconcile re-emits a mixed-case `nextval('App.Seq')` default.
- `dbd init --dry-run` writes files (unfiled). SQLite scopes cannot name a
  table (bare names read as schemas; `main.` is dropped) — unfiled.
- A dry run with `--allow-destructive` still says "re-run with
  --allow-destructive" (unfiled).
- SQLite `deploy --module shared` with core's keys fails on a bare FK error.
- Scratch Postgres at /tmp/dbd-verify-pg (port 55432, socket) — stop with
  `pg_ctl -D /tmp/dbd-verify-pg/data stop` when done.
