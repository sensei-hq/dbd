# Schema Viewer — Faithful UI Rebuild Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. For every `.svelte`/`.svelte.ts` use the **svelte (svelte-file-editor / Svelte MCP)** skill + **semantic-styles-rokkit**. Steps use `- [ ]`.

**Goal:** Rebuild the `/diagram` viewer UI to faithfully match the mockup **layout/structure** (`docs/mockup/designs/*.jsx` + `app-styles.css`, see `docs/mockup/screenshots/`) — app header, design header with **Diagram/Entities tabs**, search + collapsible **schema-tree** sidebar, a **div-world** ER diagram with **colored schema tint** + **zoom toolbar** + hint pill, and an **entity page** (Details/Diagram + columns table) — while using a **small, consistent Rokkit/Tailwind type scale** (no scattered rems) and Rokkit named color tokens throughout.

**Scope (v2):** the single-design view only. Defer the multi-design Designs-listing, Share, avatar, auth (v3). Header = logo + design name + theme toggle.

**Architecture change:** the diagram becomes a `.dg-viewport` div containing a CSS-`transform`ed `.dg-world` with **absolutely-positioned `<div>` cards** + an **SVG overlay for edges only** + manual pointer/wheel pan-zoom + a `.dg-tools` zoom toolbar (mirrors `docs/mockup/designs/diagram.jsx`). This replaces the SVG/`foreignObject` + `d3-zoom` approach (`d3-zoom`/`d3-selection` deps become unused → remove). `layout.ts` (`compute`/`edgePath`) is unchanged — same geometry.

---

## CONTRACT A — Type scale (consistent; the only sizes allowed)

Use ONLY these Tailwind steps; never arbitrary `text-[…rem]`. Build hierarchy with **weight + color + font-family**, not new sizes.

| Role | Class |
|------|-------|
| Design title (h1) | `font-display text-lg font-semibold tracking-tight` |
| Section/body, sidebar items, buttons, tab labels, card titles | `text-sm` (card title adds `font-mono font-semibold`; tabs `font-medium`) |
| Meta, counts, badges, column rows, column types, group heads, "+N more", hint pill | `text-xs` (mono contexts add `font-mono`) |

That's **three sizes** total (`text-lg`, `text-sm`, `text-xs`). Group heads/labels: `text-xs font-mono font-semibold uppercase tracking-wide`. Differentiate row name vs type by **color** (`text-ink` vs `text-ink-soft`), not size.

## CONTRACT B — Color tokens (Rokkit named tokens only; no raw hex/oklch)

Map the mockup's vars → Rokkit tokens:

| mockup var | Rokkit token |
|---|---|
| `--bg` (canvas) | `bg-paper` |
| `--bg-deep` (diagram canvas) | `bg-paper` (use the dotted bg via `.dg-dots`) |
| `--surface` (header, cards, sidebar) | `bg-paper-soft` |
| `--surface-2` (hover, badges, card head) | `bg-paper-mute` |
| `--line` / `--line-soft` | `border-paper-edge` |
| `--fg` | `text-ink` |
| `--muted` | `text-ink-mute` |
| `--faint` | `text-ink-soft` |
| `--accent` / `--accent-2` | `text-primary` / `bg-primary` |
| `--on-accent` | `text-on-primary` |
| `--accent-soft` | `bg-accent-soft` |
| `--accent-line` | `border-primary` |
| edges | `var(--paper-edge)` (stroke), highlighted `var(--primary)` |

**Sole exception (sanctioned raw OKLCH):** the per-schema **tint** regions + tinted card headers, which key off an inline `--cl-h` hue — keep the OKLCH formulas (light + `[data-mode=dark]`) as in the current `styles.css`. Fonts come from the site layout (`@fontsource`), families via `font-display`/`font-mono`/`font-sans`.

## Files
- Rewrite `site/src/lib/viewer/styles.css` — structural classes (`.dg-viewport/.dg-world/.dg-card*/.dg-row/.dg-tools/.dg-cluster*`, `.tree-*`, tab underline, `.col-badge`) using Rokkit tokens; geometry px (card 248, row 24, head 40) stays; text sizing lives in markup per Contract A.
- Rewrite `site/src/lib/viewer/Diagram.svelte` — div-world architecture (per `diagram.jsx`): clusters (tinted), SVG edges overlay, absolute `.dg-card` divs, pointer/wheel pan-zoom, `.dg-tools` (zoom in/out/fit), tint default on.
- Rewrite `site/src/lib/viewer/Sidebar.svelte` — search input + collapsible schema groups (chevron + name + count) + table items (icon + name) + `enums` subgroup (per `SchemaTree` in `project-view.jsx`). Drop the Rokkit `List` if it fights the design; hand-build with `.tree-*` classes.
- Rewrite `site/src/lib/viewer/Viewer.svelte` — `ProjectViewPage`/`ProjectRoot`: app header (logo + `dbd`/`designs` badge + design-name crumb + theme toggle) → design header (title + db badge + note + counts/meta) → **Diagram/Entities tabs** → diagram (with hint pill) or `EntitiesList`. Sidebar (project-name button + Sidebar tree). Drop the standalone vibe wiring (already done).
- Create `site/src/lib/viewer/EntitiesList.svelte` — the Entities tab: a table (Entity · Cols · Refs · Comment) listing tables (+enums), row click → select entity (per `EntitiesList` referenced in project-view.jsx).
- Create `site/src/lib/viewer/EntityView.svelte` — entity page (per `entity-page.jsx`): `schema.name` eyebrow + name + `N columns` badge + **Details/Diagram tabs**; Details = comment (markdown) + columns table (COLUMN/PROPS[PK,FK,NN]/TYPE/REFS); Diagram = the focused mini-ERD (reuse `Diagram` restricted to the entity + neighbors).
- Modify `site/src/lib/viewer/state.svelte.ts` — add `tab: 'diagram'|'entities'`, `lines: 'curved'|'orthogonal'`, `tint: boolean`, keep `selected/density/arrange/filter`; selecting a table opens the entity page.
- Modify `site/src/routes/diagram/+page.svelte` if needed (it just mounts `<Viewer model>` — keep).
- Remove `d3-zoom`/`d3-selection` + `@types/d3-*` deps if no longer used after the Diagram rewrite (confirm by grep).
- Icons: add a tiny local `Ic` (inline SVG paths from `app-shell.jsx` `ICONS`: table, enumI, key, link, search, chevR/chevD, plus, minus, fit, grid, rows, sun/moon) as `site/src/lib/viewer/Icon.svelte` — stroke 1.8, 24 viewBox, `currentColor`.

---

## Task FU.1 — `styles.css` + `Icon.svelte` foundation
**Files:** rewrite `site/src/lib/viewer/styles.css`; create `site/src/lib/viewer/Icon.svelte`.
- Port the structural classes from `app-styles.css` (`.dg-viewport`, `.dg-dots`, `.dg-world`, `.dg-cluster`/`.dg-cluster-label`, `.dg-card`/`.dg-card.sel/.rel/.dim/.headonly`, `.dg-card-head`, `.dg-row`/`.iskey`/`.cname`/`.ctype`, `.dg-more`, `.dg-keyicon`/`.dg-fkicon`, `g.dg-edge*`, `.dg-tools`, `.tree-group-head`, `.tree-item`/`.sel`/`.ti-name`, `.col-badge`/`.pk`/`.fk`, the `.tinted` tint rules) — but every color via Rokkit tokens (Contract B) and **no font-size declarations in CSS** (text sizing is done with Tailwind classes in markup per Contract A; keep only geometry: heights/padding/widths). Keep the `[data-mode='dark']` tint blocks.
- `Icon.svelte`: `{ name, size=16, class }` → inline `<svg viewBox="0 0 24 24" stroke="currentColor" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round">` with the path(s) from `ICONS` (copy the exact `d` strings from `app-shell.jsx`).
- [ ] Build green (`bun run build`), `bun run test:viewer` still green. Commit `feat(viewer): faithful styles.css (Rokkit tokens, 3-size scale) + Icon`.

## Task FU.2 — `Diagram.svelte` div-world rebuild
**Files:** rewrite `Diagram.svelte`; update `state.svelte.ts` (add `lines`,`tint`).
- Mirror `diagram.jsx`: `.dg-viewport.dg-dots` (+`.tinted` when `state.tint`), `.dg-world` with `transform: translate(tx,ty) scale(s)`, clusters as tinted divs with `--cl-h`, an absolutely-positioned `<svg>` overlay (size = layout.size) for edges only (`pointer-events:none`), and `.dg-card` **divs** (head: table icon + title + col-count; rows: key/link icon + cname + ctype; `+N more`). Pan: pointer drag; zoom: ctrl/⌘+wheel (plain wheel pans); `.dg-tools` buttons (zoom in 1.25 / out 0.8 / fit). Selection states sel/rel/dim + edge hl/dim. Clicking a card → `onSelect(key)` (opens entity page); clicking empty space → clear.
- Card text per Contract A: title `text-sm font-mono font-semibold text-ink`; col-count `text-xs font-mono text-ink-soft`; rows `text-xs font-mono` (cname `text-ink`, ctype `text-ink-soft`).
- jsdom-safe (guard `clientWidth` 0). Keep `[data-card]` attr for tests.
- [ ] Test: renders ≥N `[data-card]` divs + edges; click selects. `bun run test:viewer` green. Commit `feat(viewer): div-world diagram (tint, zoom toolbar) matching mockup`.

## Task FU.3 — `Sidebar.svelte` schema tree
**Files:** rewrite `Sidebar.svelte`.
- `SchemaTree` per `project-view.jsx`: search `<input class="ds-input … text-sm">` (bound to `state.filter`) with a search icon; per schema a `.tree-group-head` (chevron `Icon` + name + count) toggling open; `.tree-item`s (table `Icon` + `.ti-name`) → select; an `enums` subgroup (collapsible). Selected item highlights (`.sel`).
- Sizing per Contract A (`text-sm` items, `text-xs` group heads).
- [ ] Test: groups + items render; filter narrows; select fires. `bun run test:viewer` green. Commit `feat(viewer): schema-tree sidebar (search + collapsible groups)`.

## Task FU.4 — `Viewer.svelte` shell (app header + design header + tabs)
**Files:** rewrite `Viewer.svelte`.
- Layout per `ProjectViewPage` + `ProjectRoot`: header row (logo `dbdLogo` + `dbd` + `designs` `.ds-badge` + `/` + design name; right: theme toggle via `ThemeSwitcherToggle` or an `Icon` sun/moon button); body = sidebar (project-name button + `Sidebar`) + main; main = design header (`h1 text-lg` name + db `.ds-badge-accent` + note `text-sm text-ink-mute` + meta `text-xs font-mono text-ink-soft` counts) + tab bar (Diagram/Entities, underline-active) + content (`Diagram` with hint pill, or `EntitiesList`). When `state.selected` → render `EntityView` in main instead of ProjectRoot (sidebar stays).
- A small controls cluster for density/lines/arrange/tint — a compact popover/segmented control (keep minimal; `text-xs`), OR reuse a simple inline segmented control near the tabs. Use `state`.
- [ ] Test: header shows name+counts; tabs switch; selecting a sidebar item shows EntityView. Commit `feat(viewer): app/design header + Diagram/Entities tabs shell`.

## Task FU.5 — `EntitiesList.svelte`
**Files:** create `EntitiesList.svelte`.
- A table: columns Entity (`schema.name`), Cols (count), Refs (count), Comment (first line of `noteMd`/`note`). Sticky header (`.ds-th`). Rows `text-sm`; header `text-xs font-mono uppercase text-ink-soft`. Row click → select entity. Group/sort by schema.
- [ ] Test: lists all tables; row click selects. Commit `feat(viewer): Entities tab list`.

## Task FU.6 — `EntityView.svelte`
**Files:** create `EntityView.svelte`; remove the old `Detail.svelte` slide-over usage (replace with EntityView in main).
- Per `entity-page.jsx`: eyebrow `schema.` (`text-xs font-mono text-ink-soft`) + name (`text-lg font-display font-semibold`) + `N columns` `.ds-badge`; **Details/Diagram tabs**. Details = comment markdown (`marked`) + columns table (COLUMN `text-sm` + note under it `text-xs text-ink-mute`; PROPS = `.col-badge` PK/FK/NN; TYPE `text-xs font-mono text-ink-soft`; REFS). Diagram tab = `<Diagram>` restricted to this entity + neighbors (focused).
- [ ] Test: shows columns with PK/FK badges; comment renders. Commit `feat(viewer): entity page (Details/Diagram + columns table)`.

## Task FU.7 — cleanup + live verify + ship
- Remove `d3-zoom`/`d3-selection`/`@types/d3-*` from `site/package.json` if unused (grep first); delete the old `Detail.svelte` if fully replaced.
- `bun run check` (no new errors vs baseline), `bun run test:viewer` green, `bun run build` green, `cargo test --workspace` green.
- Push; after redeploy, open `https://dbd.sensei-hq.com/diagram#1.<fixture>` in Playwright, screenshot, and **compare against `docs/mockup/screenshots/`** — layout, tabs, sidebar tree, colored regions, zoom toolbar, typography. Iterate until it matches.
- [ ] Commit `chore(viewer): drop d3 deps; faithful-UI cleanup` and `make bump`.

## Done when
`/diagram` visually matches the mockup screenshots (app/design header, Diagram/Entities tabs, search+collapsible schema-tree sidebar, colored-tint div-world diagram with zoom toolbar + hint pill, entity Details/Diagram page), uses **only** `text-lg/text-sm/text-xs` + Rokkit color tokens (no scattered rems, no raw hex), tests/build/clippy green, deployed and verified against the screenshots.
