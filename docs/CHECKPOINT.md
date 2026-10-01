# Checkpoint

**Slice:** #25, #27, #28, #29 shipped — merged to `main` (`9c4a0e3`) and live
on https://dbd.sensei-hq.com/diagram. Nothing in flight. Not yet released.

## Done

- #25 (PR #26): viewer on `@rokkit/graph` 1.7 — ErDiagram/Neighborhood,
  controls, schema tint + legend, shared theme switcher.
- #27: Details tab — info, fields (notes, defaults), references, dependencies.
- #28: root opens on Overview — counts with icons, note, per schema, recent.
- #29: changelog from snapshots — `dbd_core::history`, `SchemaModel` v3,
  `graph.json` stage marker, `scope::admits`; site Changelog tab.
- PR #30 → develop, PR #31 → main; #25 and #27–#29 closed by their PRs.

## Verified

    main 9c4a0e3: CI, CodeQL, Cloudflare production build all green
    production: Overview, Changelog (v5…v1), Details sections checked live

## Next

    # cut 0.24.0 — a minor: SchemaModel gained a pub field (history)
    # follow the Release Checklist in ~/.claude/CLAUDE.md

## Open questions

- When to cut 0.24.0; the CHANGELOG [Unreleased] section is ready for it.

## Known-broken / upstream

- rokkit#170 off-centre neighbourhood · #171 zoom/pan extent · #172 style parity.
- History has no views/routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
