# Checkpoint

**Slice:** #7 Phase 2, slices 1–2 (registry + claim, uses) on `feat/ownership-uses`
= PR #50 (into the `feat/ownership` integration branch). Phase 1 is on `develop`,
unreleased.

## Done

- Slices 1–2: `dbd.objects`/`dbd.uses`; keys from every `CREATE` (types a file
  creates too) as the loader reads the design; one namespace for relations
  and types, routines by name; uses namespaced, bare names along the path.
- Claim timing (`889a433f`): an apply in one transaction claims last, in it,
  and before a part-way enum-value commit; reconcile, reset and a DBD_NO_TX
  apply claim first and keep it. Closes the reset/reconcile race.
- Keys/uses `31bd657e`, red `bfffee35`, docs `b4d0708b`; CI runs `cli_live`.
- Re-verification round: red `f01dbfda`; DBD_NO_TX claims before hooks
  `780ece9d`; DO blocks, multiranges, nextval strings, bare names by catalog
  `0e8fedf3`; docs `75d7fb5e`. Race tests poll pg_locks.
- Embedded 192/192, `cli_live` 46/46, workspace 1997/0, clippy and fmt clean.

## Next

    # push PR #50, update its body; CI green; merge #50 locally into
    # feat/ownership;
    # then slice 3: prune rules (a)/(b), --prune-unowned, release.

## Open

- Slice 3: an empty design's meta has no uses — the not-upgraded marker.
- Declined: the batch rollback error stays best-effort (pre-existing, no
  state survives it); dropping the old shared `dbd.import_tmp`.
- A bare type of another project's is no use (dropped as a built-in).
- `dbd.objects` still allows namespace `type`, unused.
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec; claiming first would strand a killed run's claims).
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
