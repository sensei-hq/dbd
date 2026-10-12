# Checkpoint

**Slice:** #7 Phase 2, slice 4 (the uses guard) on `feat/ownership-guard`, from
`feat/ownership` — where slices 1–3 landed as PR #50 (`5e6ad4dc`) and PR #51
(`6196810c`). Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–2 (PR #50): registry, claim, uses. Slice 3 (PR #51): prune by
  ownership — other owners' entities hidden, rules (a)/(b), `--prune-unowned`,
  release; prune claims and drops in one READ COMMITTED transaction.
- Embedded 214/214, `cli_live` 48/48, workspace 2002/0, clippy, fmt, doc clean.

## Next

    # slice 4: before an owner drops or destructively alters an entity, check
    # dbd.uses and pg_depend; refuse when another owner depends on it, naming
    # the use — reconcile's prune (incl. CASCADE reaching another owner's
    # child/view), its destructive alters, apply's migration drops.

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
