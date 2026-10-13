# Checkpoint

**Slice:** #7 Phase 3 (workspaces and modules), integration branch
`feat/workspaces`. Phases 1 and 2 shipped in v0.26.0.

## Done

- Slice 1 merged (PR #61): `Workspace::load` — a module is a `Design` owned by
  `<project>/<module>`; the `modules:` block validated; T19 overlaps and T20
  gaps refuse the load; `dbd inspect` per module; other commands refuse a
  workspace until they run one.

## Next

Slice 2 (running a workspace): per-module runs and adapters for apply, deploy,
reconcile, diff, import, export, policies, refresh, migrate --status, reset
(reverse), inspect --shared; `--module` (+ dependencies); manifest `apply:`
hooks around the run; T21. Branch `feat/workspaces-run`, red tests first.

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
