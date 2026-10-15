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
- Slice 6 merged (PRs #67, #68): the workspace transfer rule (T22, T24) and
  `dbd split --from-scopes` (T23), whole or not at all, with `--undo`.
- Slice 7 merged (PR #69): docs on every surface; CHANGELOG Unreleased.

## Next

Phase 3 complete on `feat/workspaces`. Next: PR to main, CI, then with the
user's OK the merge and the 0.27.0 release checklist.

## Open

- main needs the rustls 0.23.46 bump: feat/workspaces carries it.
- `dbd init --dry-run` writes design.yaml and the scaffold (not #7; unfiled).
- Issue #60: reconcile re-emits a mixed-case `nextval('App.Seq')` default.
- SQLite `deploy --module shared` with core's keys to it fails on a bare FK
  error while emptying (Postgres names the keys) — slice 2, unfiled.
- Delete by hand: `/tmp/dbd-*` scratch and review dirs; `git worktree remove
  /tmp/dbd-review-tests-wt` (and `git worktree prune`).

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
