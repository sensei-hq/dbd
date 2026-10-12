# Checkpoint

**Slice:** #7 Phase 2, slice 3 (prune rules, `--prune-unowned`, release) on
`feat/ownership-prune`, from `feat/ownership` — where slices 1–2 landed as
PR #50 (`5e6ad4dc`). Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–2 merged into `feat/ownership` (PR #50): registry, claim, uses.
- Slice 3 (red `131689a3`/`6a3543ea`, impl `f8c09c35`/`78030d9f`): reconcile and diff hide
  other owners' entities; `--prune` drops own undeclared (a) and unowned in an
  unshared schema (b); unowned in a shared schema, or while a recorded owner
  has not upgraded (no `owner` row in dbd.uses), kept and named unless
  `--prune-unowned`; a successful run releases what is undeclared and gone.
  Review: prune claims candidates first, quotes its DROP, refuses an enum
  swap another owner's column holds.
- Embedded 209/209, `cli_live` 48/48, workspace 2002/0, clippy, fmt, doc clean.

## Next

    git push -u origin feat/ownership-prune   # PR into feat/ownership
    # focused review; fix test-first; CI green; merge locally into feat/ownership;
    # then slice 4: the uses guard (refuse dropping/altering what another uses).

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
