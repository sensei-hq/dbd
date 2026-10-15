# Checkpoint

**Slice:** none open. 0.27.1 (#65) released; `main` at fcc5868c.

## Done

- 0.27.0 — #7 phase 3, workspaces and modules (PRs #70, #71).
- 0.27.1 — #65 (PRs #74, #75), on crates.io, `main` CI green, issue closed.
  The installed binary re-keyed the issue's repro on PostgreSQL 18 through
  `reconcile` and through `dbd snapshot` + `apply` over existing rows, and
  replaced `app.stats()` with its new return type.
  - Reconcile: the foreign keys holding a dropped key go first, kept ones
    come back after the new key, replaced ones in their own step (one
    transaction, 10s lock timeout); a holder outside the run is refused.
  - Routines: a replace conflict is dropped and created again, grants, owner
    and comment kept; a file with its own `BEGIN`/`COMMIT` runs as before.
  - Migrations: the same ordering; on PostgreSQL (by URL) an unnamed key is
    dropped by catalog lookup.

## Next

Pick from the open issues: #72 (a rebuilt column loses its keys), #73 (a
migration's `DROP INDEX` names no schema), #60 (nextval spelling).

## Open

- Unfiled: `init --dry-run` writes files; SQLite scopes cannot name a table
  (a bare name reads as a schema, `main.` is dropped); a dry run with
  `--allow-destructive` still says to re-run with it; SQLite `deploy --module
  shared` fails on a bare FK error while emptying.
- Scratch to delete by hand: /tmp/dbd-verify-pg (stopped),
  /tmp/dbd-review-65-*, /tmp/dbd-r65-scratch, /tmp/dbd-65-*,
  /tmp/dbd-repro-65*, /tmp/dbd-0271-*, /tmp/dbd-verify-0.27.0*; worktree
  /tmp/dbd-review-tests-wt (`git worktree remove`, then `git worktree prune`).
