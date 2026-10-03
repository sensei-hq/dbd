# Checkpoint

**Slice:** shipped — rokkit 1.9.0, per-entity changelog (#33), every entity in
the sidebar (#34). `main` `66c8779`, live on https://dbd.sensei-hq.com/diagram.

## Done

- rokkit 1.9.0 (#32, #36): rokkit#170/#171/#172 all fixed upstream; plain
  Neighborhood; header stable across tabs. Guard test: no reserved column.
- #33: Changelog tab per table and enum (`entityHistory`, `FieldEdits`).
- #34: `SchemaModel` v3 `enums`; parser keeps `COMMENT ON TYPE`; sidebar of
  every entity with a kind filter; ObjectView and EnumView.
- PR #37 (develop → main) merged; #33, #34 closed.

## Verified

    main 66c8779: CI, CodeQL, Cloudflare production build all green
    live: 9 entities + 4 chips; one- and two-sided neighbourhoods centred;
    view uses, enum values, shop.orders' changelog (v5, v2)

## Next

    # cut 0.24.0 — a minor: SchemaModel gained pub fields (history, enums)
    # follow the Release Checklist in ~/.claude/CLAUDE.md

## Open questions

- When to cut 0.24.0; CHANGELOG [Unreleased] is ready for it.

## Known-broken / upstream

- History has no views/routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
