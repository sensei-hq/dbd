# Checkpoint

**Slice:** #7 Phase 2, slice 4 (the uses guard) on `feat/ownership-guard`, from
`feat/ownership` — where slices 1–3 landed as PR #50 (`5e6ad4dc`) and PR #51
(`6196810c`). Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–3 merged into `feat/ownership` (PRs #50, #51): registry, claim,
  uses, prune by ownership.
- Slice 4 = PR #52 (impl `9c91df5b`, `968c1293`; fixes `3612a8cf`, `6ebdf47d`,
  `8092df1a`, `a17e3e56`, `c08bf8d9`):
  the guard walks pg_depend as a CASCADE reaches, part by part (inherited
  columns, arrays, domain checks) + dbd.uses (recorded whoever owns the
  target; routine bodies in any file); reconcile refuses dropped parts, a prune
  keeps used tables; apply refuses a migration's drops, or an unreadable one.
- Embedded 238/238, `cli_live` 48/48, workspace 2005/0, clippy, fmt, doc clean.

## Next

    # PR #52: regression check + CI green → merge locally into feat/ownership;
    # then slice 5: reset rules (drops only own; --schemas/--extensions/roles
    # skip what others use; reset's CASCADE behind the same guard).

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
