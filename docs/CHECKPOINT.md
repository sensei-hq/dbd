# Checkpoint

**Slice:** #27 Details tab, #28 overview, #29 snapshot changelog — all built
on local `feat/details-tab` (unpushed). #29 reaches Rust: SchemaModel v3.

## Done

- #27: Details = Table info · Fields (Name/Type/Settings/Default/References/
  Notes) · References in+out · Dependencies (v2 `deps`) · Indexes.
- #28: root opens on Overview — counts with diagram icons, note, per schema,
  Recent changes (latest three).
- #29: `dbd_core::history` (baseline; multi-stage grouped; canonical types;
  sorted; scoped under `--scope`); `SchemaModel.history`, version 3; `dbd
  diagram` attaches it, warns and omits on an unreadable snapshot. Site
  Changelog tab, newest first.
- Docs: guide, llms.txt, llms-full.txt, both SKILL.md copies → v3 + history.
  CHANGELOG [Unreleased] says the next release is a minor (new pub field).

## Verified

    cargo test --workspace 1622+ · clippy 1.98 and 1.99 clean · fmt clean
    vitest 85/85 · svelte-check 0/0 · CF_PAGES=1 build exit 0
    release binary on tests/fixtures/embedded → 751-byte link → Changelog renders

## Next

    # after PR #26 merges: push feat/details-tab, PR into develop
    git push origin feat/details-tab

## Open questions

- Merge PR #26 (develop → main) — deploys production.
- Push + PR `feat/details-tab` now, or after #26?

## Known-broken / upstream

- rokkit#170 off-centre neighbourhood · #171 zoom/pan extent · #172 style parity.
- History has no views/routines: snapshots hold tables and enums only.
- Preview on :4317 — :4173 is shared with another session.
