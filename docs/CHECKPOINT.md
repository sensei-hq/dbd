# Checkpoint

**Slice:** #7 Phase 2, slice 3 (prune rules, `--prune-unowned`, release) on
`feat/ownership-prune`, from `feat/ownership` — where slices 1–2 landed as
PR #50 (`5e6ad4dc`). Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–2 merged into `feat/ownership`: `dbd.objects`/`dbd.uses`; keys and
  uses as Postgres makes and resolves them; claim in the batch, or first for a
  run that writes as it goes. Five review rounds, all fixed test-first; CI runs
  `cli_live`. Embedded 198/198, `cli_live` 47/47, workspace 2002/0.

## Next

    # slice 3: prune rule (a) owned + undeclared, rule (b) unowned in a schema
    # no other owner uses; --prune-unowned naming each; release on a successful
    # run of what is no longer declared and no longer exists; rule (b)
    # suspended while an owner has not upgraded (meta row, no uses).

## Open

- Slice 3: an empty design's meta has no uses — the not-upgraded marker.
- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
