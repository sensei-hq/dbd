# Checkpoint

**Slice:** #7 Phase 3 (workspaces and modules), integration branch
`feat/workspaces`. Phases 1 and 2 shipped in v0.26.0.

## Done

- Slice 1 merged (PR #61): `Workspace::load` — a module is a `Design` owned by
  `<project>/<module>`; the `modules:` block validated; T19/T20 refuse the load.
- Slice 2 merged (PR #62): a workspace runs module by module as each module's
  owner (T21); `--module`; manifest hooks around the run; every module checked
  before any writes; whole-or-nothing reset; deploy schema then data.
- Slice 3 merged (PR #63): `scopes.<name>.modules`, each scope projected per
  module (`Workspace::under_scope`/`under_scope_of`, `check_scope_gaps`);
  walking up from a module folder (`Workspace::locate`).
- Slice 4 merged (PR #64): per-module history — `snapshot`, `release`,
  `format`, `doctor`; reconcile leaves released modules out.
- Slice 5 merged (PR #66): `Reading` — dbml, diagram, graph, combine, emit
  over a workspace; the viewer groups by module.

## Next

Slice 6a merged (PR #67, the transfer rule). Slice 6b on
`feat/workspaces-split-scopes`: `dbd split --from-scopes` (40210d4c), T23/T24
green. Next: PR, review, CI, merge; then slice 7 (docs).

## Open

- main needs the rustls 0.23.46 bump too (cargo audit fails there).
- `dbd init --dry-run` writes design.yaml and the scaffold (not #7; unfiled).
- Issue #60: reconcile re-emits a mixed-case `nextval('App.Seq')` default.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- Delete by hand: `/tmp/dbd-*` scratch and review dirs; `git worktree remove
  /tmp/dbd-review-tests-wt` (and `git worktree prune`).

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
