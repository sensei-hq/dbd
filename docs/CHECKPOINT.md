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

## Next

Slice 5 on `feat/workspaces-viewer`: decisions in the plan; red tests in
ede5e71d (core `Reading`, CLI artifacts, site `modules.test.ts`). Next: the
implementation — core, then CLI, then the viewer.
    cargo test -p dbd-core --test workspace_reading; cargo test --test workspace_artifacts
    (cd site && bun run test -- src/lib/design/modules.test.ts)

## Open

- `dbd init --dry-run` writes design.yaml and the scaffold (not #7; unfiled).
- Issue #60: reconcile re-emits a mixed-case or quoted `nextval('App.Seq')`
  default every run — probe pg_get_expr before fixing.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- One-transaction apply runs `apply.before` before its claim (in the spec).
- Delete by hand: `/tmp/dbd-mut` (+ `git worktree prune`), `/tmp/dbd-mut-target`,
  `/tmp/dbd-idx.*`, `/tmp/dbd-review2*`, `/tmp/dbd-lockreview*`.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
