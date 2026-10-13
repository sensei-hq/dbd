# Checkpoint

**Slice:** #7 Phase 3 (workspaces and modules), integration branch
`feat/workspaces` from `develop`. Phases 1 and 2 shipped in v0.26.0.

## Done

- v0.26.0 released: crates.io published, `main` merged (PR #59). Issue #60 filed
  for the reconcile `nextval()` drift.
- Phase 3 plan: `docs/superpowers/plans/2026-10-13-workspaces-phase-3.md` — a
  module is a `Design` owned by `<project>/<module>`, rooted at its folder.

## Next

Slice 1 PR #61 (`feat/workspaces-load` → `feat/workspaces`): two review rounds
fixed (2534f002, a34dc1b3), plan/spec updated (9ceacd0f). Waiting on CI, then
merge into `feat/workspaces` locally; then slice 2 (running a workspace:
per-module runs and adapters, `--module`, manifest hooks around the run, T21).

    gh pr checks 61

## Open

- Issue #60: reconcile re-emits a mixed-case or quoted `nextval('App.Seq')`
  default every run — probe pg_get_expr before fixing.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Delete by hand: `/tmp/dbd-mut` (+ `git worktree prune`), `/tmp/dbd-mut-target`,
  `/tmp/dbd-idx.*`, `/tmp/dbd-review2*`, `/tmp/dbd-lockreview*`.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
