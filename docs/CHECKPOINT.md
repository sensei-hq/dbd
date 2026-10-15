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

## Next

1. Review slices 1 and 2 (failure modes, data correctness).
2. Slice 3: snapshot migrations — an unnamed key dropped by catalog lookup;
   the foreign keys holding a dropped key dropped first, kept ones re-added.
3. Docs (guide reconcile/migrations, CHANGELOG), `make bump` patch 0.27.1.

## Open

- Issue #60: reconcile re-emits a mixed-case `nextval('App.Seq')` default.
- `dbd init --dry-run` writes files (unfiled). SQLite scopes cannot name a
  table (bare names read as schemas; `main.` is dropped) — unfiled.
- A dry run with `--allow-destructive` still says "re-run with
  --allow-destructive" (unfiled).
- SQLite `deploy --module shared` with core's keys fails on a bare FK error.
- Scratch Postgres at /tmp/dbd-verify-pg (port 55432, socket) — stop with
  `pg_ctl -D /tmp/dbd-verify-pg/data stop` when done.
