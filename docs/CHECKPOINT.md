# Checkpoint

**Slice:** #7 Phases 1 and 2 merged into `develop` (Phase 2: PR #58, `1ec0e311`),
unreleased. Phase 3 (workspaces) not started.

## Done

- Entity ownership for projects sharing a database: registry and claim, uses,
  prune by ownership (`--prune-unowned`), uses guard, reset rules, the dbd write
  lock, name reuse (`--allow-ownership-change`), `--adopt-from`, `inspect
  --shared`, onboarding beside other projects, SQLite parity; guide 14 and the
  references (04, 06, 08, 12, llms, both SKILL.md, README, agent).
- Embedded 299/299, `cli_live` 61/61, workspace 2017/0, clippy, fmt, doc, site.

## Next

    # waiting on the user: a release (Phases 1+2 → 0.26.0, minor), Phase 3
    # (workspaces), or the nextval drift issue

## Open

- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Delete by hand: `/tmp/dbd-mut` (+ `git worktree prune`), `/tmp/dbd-mut-target`,
  `/tmp/dbd-idx.*`, `/tmp/dbd-review2*`, `/tmp/dbd-lockreview*`.
- Phases 1 and 2 are releasable together as 0.26.0 (minor: library API changes) when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
