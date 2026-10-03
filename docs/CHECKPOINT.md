# Checkpoint

**Slice:** v0.24.0 released — tagged `v0.24.0` (`fb08bef`), published to
crates.io (`dbd-core`, `dbd-cli` 0.24.0), merged to `main` (`d36902c`).

## Done

- 0.24.0 = #25, #27–#29, #33, #34, rokkit 1.9.0 (#32, #36). CHANGELOG cut with
  linked issue refs; `make bump minor` bumped all version-bearing files.
- release.yml: tag/manifest match, test, clippy, fmt, doc links, publish — green.
- Shipped artifact verified: `cargo install dbd-cli --version 0.24.0` (from the
  registry) → `dbd snapshot` writes `stage {1,1}`; `dbd diagram --json` emits
  v3 with `enums` and `history` (baseline + a real column addition).

## Verified

    main d36902c: CI, CodeQL, Cloudflare build green · PR #38 green
    crates.io: dbd-core 0.24.0 11:58Z, dbd-cli 0.24.0 11:59Z

## Next

    # nothing in flight. Open: #7 (multi-tenant schema isolation).

## Open questions

- None.

## Known-broken / upstream

- History has no views/routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
- `CLAUDE.md` names a `docs_match_code` website-copies gate that does not exist.
