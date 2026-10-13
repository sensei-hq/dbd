# Checkpoint

**Slice:** #7 Phase 2, slice 5 (reset rules) on `feat/ownership-reset`, from
`feat/ownership` (slices 1–4: PRs #50–#52). Phase 1 is on `develop`, unreleased.

## Done

- Slice 5 (PR #53): reset keeps/refuses for other projects, `ResetComplete.kept`,
  `reset_preview`. Two review rounds fixed test-first: the dbd write lock
  (`dbd:write`; exclusive for reset/reconcile/migrating apply, shared for
  apply/import/policies/grants; polls, survives idle timeouts, gives way on
  cancel); the guard sees what nobody owns (prune's rule), composites, a
  schema's collations, arrays and row types; supabase; kept roles' grants.
- Embedded 278/278, `cli_live` 49/49, workspace 2006/0, clippy, fmt, doc clean.

## Next

    gh pr checks 53 --watch
    # CI green → local --no-ff merge into feat/ownership; then slice 6:
    # name reuse (--allow-ownership-change) + transfer (--adopt-from)

## Open

- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Docs slice (end of Phase 2) must cover `--prune-unowned`, reset's kept
  lines and the write lock's waits in the guides.
- Delete by hand: `/tmp/dbd-mut` (+ `git worktree prune`), `/tmp/dbd-mut-target`,
  `/tmp/dbd-idx.*`, `/tmp/dbd-review2*`, `/tmp/dbd-lockreview*`.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
