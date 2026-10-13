# Checkpoint

**Slice:** #7 Phase 2, slice 4 (the uses guard) on `feat/ownership-guard`, from
`feat/ownership` — where slices 1–3 landed as PR #50 (`5e6ad4dc`) and PR #51
(`6196810c`). Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–3 merged into `feat/ownership` (PRs #50, #51): registry, claim,
  uses, prune by ownership.
- Slice 4 (red `6476dc39`, `f1e8ad39`; impl `9c91df5b`, `968c1293`): the uses
  guard — `dependents` walks pg_depend (as a CASCADE reaches) + dbd.uses
  (routine bodies); reconcile refuses a dropped column/constraint/index/enum
  value another owner depends on, a prune keeps such a table named; apply
  refuses a pending migration's DROP / DROP COLUMN / DROP CONSTRAINT of it.
- Embedded 222/222, `cli_live` 48/48, workspace 2002/0, clippy, fmt, doc clean.

## Next

    # PR for feat/ownership-guard into feat/ownership; focused review; fix
    # test-first; CI green; merge locally; then slice 5: reset rules (drops
    # only own; --schemas/--extensions/roles skip what others use).

## Open

- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Docs slice (end of Phase 2) must cover `--prune-unowned` in the guides.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
