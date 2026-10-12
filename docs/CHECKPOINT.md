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
- Keys/uses fixes `31bd657e`; red `bfffee35`; docs `b4d0708b`.
- CI now runs `cli_live` (`37c91e51`) — it had compiled to no tests.
- Embedded 186/186, `cli_live` 45/45, workspace 1997/0, clippy and fmt clean.

## Next

    # PR #50 pushed; update its body; focused re-verification of
    # bfffee35..b4d0708b; CI green; merge #50 locally into feat/ownership;
    # then slice 3: prune rules (a)/(b), --prune-unowned, release.

## Open

- Slice 3: an empty design's meta has no uses — the not-upgraded marker.
- Declined: the batch rollback error stays best-effort (pre-existing, no
  state survives it); dropping the old shared `dbd.import_tmp`.
- A bare type of another project's (search path, unqualified) is no use:
  bare types are dropped as built-ins; pg_depend still guards the column.
- `dbd.objects` still allows namespace `type`, unused.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
