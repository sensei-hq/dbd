# Checkpoint

**Slice:** #7 Phase 2, slice 5 (reset rules) on `feat/ownership-reset`, from
`feat/ownership` — where slices 1–4 landed as PRs #50, #51, #52 (`64e09594`).
Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–4 in `feat/ownership`: registry, claim, uses; prune by ownership;
  the uses guard (pg_depend as a CASCADE reaches + dbd.uses) on reconcile's
  drops and apply's migrations.
- Embedded 238/238, `cli_live` 48/48, workspace 2005/0, clippy, fmt, doc clean.

## Next

    # slice 5: reset drops only own (claims first — done in slice 1); its
    # CASCADE behind the uses guard; --schemas only for a schema holding no
    # other owner's and no unowned entity; --extensions and roles skip what
    # another owner uses; the prod guard refuses while any owner is prod.

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
