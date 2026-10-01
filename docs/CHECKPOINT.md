# Checkpoint

**Slice:** #27 Details tab, #28 overview, #29 snapshot changelog — built,
reviewed and fixed; PR #30 (`feat/details-tab` → develop) open. #29 reaches Rust.

## Done

- PR #26 merged (0a7ac9e); #25 closed; production verified live.
- #27: Details = Table info · Fields (Name/Type/Settings/Default/References/
  Notes) · References in+out · Dependencies (v2 `deps`) · Indexes.
- #28: root opens on Overview — counts with diagram icons, note, per schema,
  Recent changes (latest three).
- #29: `dbd_core::history`; `SchemaModel` v3 `history`; `graph.json` carries
  `stage {index, of}`; scoped history via `scope::admits`. Site Changelog tab.
- Data-correctness review: 7 defects found, all fixed red-first (`c9035be`).
- Docs: guide 04/05, llms.txt, llms-full.txt, both SKILL.md copies, CHANGELOG.

## Verified

    cargo test --workspace 1632/0 · clippy 1.98 + 1.99 clean · fmt clean
    vitest 85/85 · svelte-check 0/0 · CF_PAGES=1 build exit 0
    release binary: snapshot writes stage 1/1; history v1–v3 on the fixture

## Next

    gh pr checks 30     # then merge #30 into develop
    # #27–#29 close only via a develop → main PR that names them

## Open questions

- Next release is a minor (0.24.0): `SchemaModel` gained a pub field.

## Known-broken / upstream

- rokkit#170 off-centre neighbourhood · #171 zoom/pan extent · #172 style parity.
- History has no views/routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
