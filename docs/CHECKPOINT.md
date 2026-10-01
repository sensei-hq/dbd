# Checkpoint

**Slice:** #25 — the viewer on `@rokkit/graph` 1.7's bare canvas. Site-only,
merged to `develop`; `main` and production untouched.

## Done

- Root diagram composes `ErDiagram`: `flow`, schema tint + legend, density /
  edge-style / zoom controls. `arrange` removed with `cluster`.
- Entity diagram composes `Neighborhood` with depth / edge / zoom controls, in
  the root's style — dbd owns the `GraphState` (tint on, no selection highlight).
- `/diagram` and `/projects` use the home page's `ThemeSwitcherToggle`.
- `auth.users` on one line in the entity header and the entities list.
- Hint pill moved to the canvas top. CHANGELOG `[Unreleased]` written.

## Verified

    vitest 44/44 · svelte-check 0/0 · CF_PAGES=1 build exit 0
    bun install --frozen-lockfile: no changes · make _check-ci green per commit
Production build driven in a browser, light and dark, 1024–1440 wide.

## Next

    git push origin develop          # CI's `site` job is the gate
    gh run list --branch develop

Then: Details tab dbdocs-style, project overview, snapshot changelog (an issue each).

## Open questions

- Ship to production now (merge `main`), or ride with the next release?
- `CLAUDE.md` names a `docs_match_code` website-copies gate that does not exist.

## Known-broken / upstream

- rokkit#170 — a one-sided focus is drawn off-centre in its neighbourhood.
- rokkit#171 — past fit, zoom anchors top-left; right/bottom unreachable.
- rokkit#172 — `Neighborhood` lacks `groupTint`; dbd works around it.
- Preview on :4317 — :4173 is shared with another session's rokkit checks.
