# Checkpoint

**Slice:** #7 Phase 3 (workspaces and modules), integration branch
`feat/workspaces`. Phases 1 and 2 shipped in v0.26.0.

## Done

- Slice 1 merged (PR #61): `Workspace::load` — a module is a `Design` owned by
  `<project>/<module>`; the `modules:` block validated; T19/T20 refuse the load.
- Slice 2 merged (PR #62): a workspace runs module by module as each module's
  owner (T21); `--module`; the manifest's hooks around the run; every module
  checked before any writes; reset whole-or-nothing in reverse; deploy schema
  then data; import empties together; `export` per module.

## Next

Slice 3 on `feat/workspaces-scopes`: red tests committed (1bd5201a) —
`crates/dbd-core/tests/workspace_scopes.rs`, CLI scopes and walking up, a live
scope-pin test. Implement: `ScopeSpec.modules`, each manifest scope projected
onto each module at load, `Workspace::under_scope`, the guard no longer
refusing `--scope`, the walk up from a module folder.

    cargo test -p dbd-core --test workspace_scopes && cargo test --test workspace_inspect

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
