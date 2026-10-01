# Checkpoint

**Slice:** viewer docs — #27 Details tab and #28 overview done on local
`feat/details-tab` (unpushed); #29 snapshot changelog in design.

## Done

- #25 merged to `develop`; PR #26 (develop → main) green, awaiting merge.
- CI fix: clippy 1.99 `single_element_loop` in `tests/pre_commit_hook.rs`.
- #27: Details = Table info · Fields (Name/Type/Settings/Default/References/
  Notes) · References in+out · Dependencies (v2 `deps`) · Indexes. Full width.
- #28: root opens on Overview — counts with diagram-card icons, note, per
  schema. `OVERVIEW_ICONS` spread into the UnoCSS safelist.
- One `Markdown.svelte` renderer; fixed a duplicate-key crash on repeated
  inline code in comments (Details and Entities).

## Verified

    vitest 70/70 · svelte-check 0/0 · CF_PAGES=1 build exit 0
    all 10 overview glyphs present in the shipped CSS · browser 1024/1440

## Next

    # #29: map snapshot/diff/schema_model, then design the history field
    git push origin feat/details-tab   # only after PR #26 merges

## Open questions

- Merge PR #26 to `main` (deploys production)?
- #29: compute history in Rust (reusing `diff`) vs in the browser; URL
  size budget for share links carrying history.
- `CLAUDE.md` names a `docs_match_code` website-copies gate that does not exist.

## Known-broken / upstream

- rokkit#170 off-centre neighbourhood · #171 zoom/pan extent · #172
  Neighborhood style parity (dbd works around it).
- #28 "recent changes" waits on #29.
- Preview on :4317 — :4173 is shared with another session.
