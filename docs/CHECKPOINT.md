# Checkpoint

**Slice:** rokkit 1.8.2 (#32), per-entity changelog (#33) and every entity in
the sidebar (#34) — merged to `develop` (`234c784`), not yet on `main`.

## Done

- #32: rokkit 1.8.2; EntityDiagram is a plain Neighborhood (rokkit#172 fixed);
  header stays put on Overview (subtitle = note's first paragraph).
- #33: entity page Changelog tab — `entityHistory`, shared `FieldEdits`.
- #34: `SchemaModel` v3 `enums`; parser keeps `COMMENT ON TYPE`; sidebar lists
  every entity with a kind filter; ObjectView (views/routines) and EnumView.
- Docs: guide 04, llms.txt, llms-full.txt, both SKILL.md copies, CHANGELOG.

## Verified

    cargo test --workspace 1635/0 · clippy 1.98 + 1.99 · fmt
    vitest 114/114 · svelte-check 0/0 · CF build exit 0 · PR #35 CI green

## Next

    gh pr create --base main --head develop   # closes #33, #34; deploys
    # then cut 0.24.0 (a minor) per the Release Checklist

## Open questions

- Ship develop → main now? It carries rokkit#170's regression (below).
- rokkit#170 reopened + corrected: centre the drawn cards, not the focus.

## Known-broken / upstream

- rokkit#170: since 1.8.1, `sessions`/`order_items` neighbourhoods draw shifted
  right (card margins 461/41) — an empty column reserved to centre the focus.
- History has no views/routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
