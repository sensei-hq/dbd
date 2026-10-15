# Checkpoint

**Slice:** release 0.27.0 (#7 phase 3, workspaces and modules), then the
patch 0.27.1 for #65.

## Done

- #7 phase 3, slices 1–7, merged into `feat/workspaces` (PRs #61–#69) and
  from there into `develop` (PR #70, 3bdb2e4f).
- The 0.27.0 release notes: CHANGELOG `[0.27.0] — 2026-10-14`.

## Next

1. `make bump minor` on `develop` — tests, tag v0.27.0, push, publish
   (`gh run list --workflow=release.yml`), install.
2. PR `develop` → `main`, CI green, merge locally.
3. Check the installed binary: a SQLite project → `dbd split --from-scopes
   --write` → `dbd deploy`.
4. #65: reconcile drops the foreign keys referencing a primary key it
   changes, and re-adds them; a function whose return type changes is dropped
   and created. Then 0.27.1.

## Open

- Issue #60: reconcile re-emits a mixed-case `nextval('App.Seq')` default.
- `dbd init --dry-run` writes design.yaml and the scaffold (unfiled).
- SQLite `deploy --module shared` with core's keys to it fails on a bare FK
  error while emptying (Postgres names the keys) — unfiled.
- Delete by hand: `/tmp/dbd-*` scratch and review dirs; `git worktree remove
  /tmp/dbd-review-tests-wt` (and `git worktree prune`).
- Known limit: a type in a schema that needs quoting (`"Ext"`) reads as drift.
