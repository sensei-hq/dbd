# Checkpoint

**Slice:** #7 Phase 2, slices 1–2 (registry + claim, uses) on `feat/ownership-uses`
= PR #50 (into the `feat/ownership` integration branch). Phase 1 is on `develop`,
unreleased.

## Done

- Slice 1: `dbd.objects` in `LAYOUT_DDL`; the whole design's entities keyed by
  every `CREATE` in their files (folded names, the search path for unqualified
  ones, the path only where nothing parses); tables, views, matviews,
  sequences and enums share the `relation` namespace, routines are keyed by name.
- Slice 2: `dbd.uses` — schemas, extensions, roles, and references to another
  owner's entities (every overload, dotted names split as dbd splits them).
- Claim-when-built (red `c1d7df8f`, green `9307a6e5`): every command checks
  first (a dry run, diff, import, refresh and policies only read); apply claims
  in its batch, reconcile after its writes, reset only checks. A failed, killed
  or concurrent run takes nothing, so there is no give-back. Registry waits are
  capped at 5s (55P03/40001 name the table).
- CHANGELOG and spec describe claim-when-built (`7548ff03`).
- Embedded 171/171, `cli_live` 44/44, workspace 1997/0, clippy and fmt clean.

## Next

    git push origin feat/ownership-uses:feat/ownership-registry   # PR #50
    # update the PR #50 body; focused review; merge #50 locally into feat/ownership;
    # then slice 3: prune rules (a)/(b), --prune-unowned, release.

## Open

- Slice 3: an empty design's meta has no uses — the not-upgraded marker.
- Reconcile writes before it claims: a rival claim in between is refused late.
- Slice 5: a reset claims nothing, so "drops only what it owns" must also
  take what nobody owns and the design declares.
- Phase 1 is releasable as 0.25.2 when asked.
- Declined: dropping the old shared `dbd.import_tmp`.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
