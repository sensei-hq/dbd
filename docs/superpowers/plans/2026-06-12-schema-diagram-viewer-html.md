# Schema Diagram Viewer v1 — Plan 2: Svelte+Rokkit viewer + self-contained HTML

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement task-by-task. For every `.svelte`/`.svelte.ts` file use the **svelte (svelte-file-editor / Svelte MCP)** skill, and the **semantic-styles-rokkit** skill for tokens. Steps use checkbox (`- [ ]`).

**Goal:** Make `dbd diagram` produce a single **self-contained interactive HTML** schema explorer (sidebar schema→table list, markdown entity detail, SVG ER diagram with overview + per-entity focus). Plan 1 already ships the `SchemaModel` JSON + `dbd diagram --json`; this plan adds the viewer and makes HTML the default output.

**Architecture:** A Svelte 5 + Rokkit viewer lives in `site/src/lib/viewer/`, reusing the site's `rokkit.config.js` tokens and `@rokkit/ui` `List`. A pure `layout.ts` (ported from `docs/mockup/designs/diagram-layout.js`) computes ER geometry; `Diagram.svelte` renders it as SVG with `d3-zoom`. A Vite "lib" build emits ONE self-contained JS (CSS inlined) committed to `crates/dbd-core/assets/diagram_viewer.js`; the Rust CLI embeds it (`include_str!`) into `diagram.html` with the model JSON inlined. The site imports the same components for Phase 1.

**Tech Stack:** Svelte 5.55 (`mount` from 'svelte'), Vite 8 lib build, UnoCSS `presetRokkit`, `@rokkit/ui` `List`, `@rokkit/app` `ThemeSwitcherToggle`, `@rokkit/states` `vibe` + `@rokkit/actions` `themable`, `d3-zoom`, vitest + @testing-library/svelte. npm. Rust `include_str!`. Spec: `docs/superpowers/specs/2026-06-12-schema-diagram-viewer-design.md`. Visual reference: `docs/mockup/designs/` (recreate, mapping its `--bg/--surface/--accent/--edge` tokens to Rokkit `paper/paper-soft/primary/ink-faint` etc.).

**Release gate:** This plan is what makes `dbd diagram` user-facing; `make bump` only after it lands.

---

## Contract: the viewer's input

The viewer consumes the Plan-1 `SchemaModel` JSON verbatim:
```ts
type SchemaModel = {
  project: { name: string; db: string; note?: string };
  schemas: { name: string; tables: number; enums: number }[];
  tables: { schema: string; name: string; kind: string; note?: string; noteMd?: string;
            columns: { name: string; type: string; pk?: boolean; nn?: boolean; en?: boolean; def?: string; note?: string }[] }[];
  refs: { from: { s: string; t: string; c: string }; to: { s: string; t: string; c: string }; action?: string }[];
};
```
The mockup's `diagram-layout.compute(data, …)` wants `data = { tables: [{schema,name,columns:[{name,pk,fk,type}]}], refs: [{from:{s,t,c},to:{s,t,c}}] }`. So an **adapter** maps `SchemaModel → layout data`, deriving a per-column `fk: boolean` (a column is FK if it appears as a `from.c` for that table in `refs`). This adapter is the only shape glue.

## File structure

- `site/src/lib/viewer/model.ts` — `SchemaModel` TS types + `toLayoutData(model)` adapter (+ helpers: `nodeId`, `neighborsOf`).
- `site/src/lib/viewer/layout.ts` — pure port of `diagram-layout.js` (`compute`, `edgePath`), typed.
- `site/src/lib/viewer/state.svelte.ts` — runes state: `selected: string|null`, `mode: 'overview'|'focus'`, `density`, `arrange`, `filter`.
- `site/src/lib/viewer/Diagram.svelte` — SVG cards/columns/edges + `d3-zoom`; selection highlight.
- `site/src/lib/viewer/Sidebar.svelte` — Rokkit `List` schema→table + filter input.
- `site/src/lib/viewer/Detail.svelte` — markdown column/entity detail + mini focused ERD.
- `site/src/lib/viewer/Viewer.svelte` — shell (header: logo + `ThemeSwitcherToggle` + density/arrange; `NavContent`-style sidebar+main; detail slide-over).
- `site/src/lib/viewer/index.ts` — `export { Viewer }` + `export function mountViewer(target, model)`.
- `site/src/lib/viewer/styles.css` — structural CSS (card/row sizing, edge stroke vars) using Rokkit tokens.
- `site/vite.viewer.config.ts` — lib build → `crates/dbd-core/assets/diagram_viewer.js`.
- `crates/dbd-core/assets/diagram.html` — HTML template (`__DBD_MODEL__`, `__DBD_VIEWER__`).
- `crates/dbd-core/assets/diagram_viewer.js` — committed built bundle.
- `crates/dbd-core/src/diagram.rs` (or extend `schema_model.rs`) — `render_html(&SchemaModel) -> String`.
- Modify `crates/dbd-cli/src/commands/diagram.rs` (HTML default, `--json` opt-in), `cli.rs` help text, `Makefile`, docs.

---

## Task 1: model types + `toLayoutData` adapter (pure TS)

**Files:** Create `site/src/lib/viewer/model.ts`; test `site/src/lib/viewer/model.test.ts`. Ensure `vitest` + `@testing-library/svelte` + `jsdom` + `d3-zoom` are dev-deps (Task 0 below if missing).

- [ ] **Step 0 (deps):** In `site/`, confirm/install dev-deps: `npm i -D vitest @testing-library/svelte jsdom @vitest/ui` and runtime `npm i d3-zoom @types/d3-zoom`. Add a `vitest.config.ts` with `environment: 'jsdom'` and the svelte plugin. Add `"test:viewer": "vitest run src/lib/viewer"` to `site/package.json`. (Use the site's package manager — check for `bun.lock`/`package-lock.json`.)

- [ ] **Step 1: failing test** `model.test.ts`:
```ts
import { describe, it, expect } from 'vitest';
import { toLayoutData, neighborsOf, type SchemaModel } from './model';

const model: SchemaModel = {
  project: { name: 'p', db: 'postgresql' },
  schemas: [{ name: 'config', tables: 2, enums: 0 }],
  tables: [
    { schema: 'config', name: 'lookups', kind: 'table', columns: [{ name: 'id', type: 'uuid', pk: true }] },
    { schema: 'config', name: 'lookup_values', kind: 'table',
      columns: [{ name: 'id', type: 'uuid', pk: true }, { name: 'lookup_id', type: 'uuid' }] },
  ],
  refs: [{ from: { s: 'config', t: 'lookup_values', c: 'lookup_id' }, to: { s: 'config', t: 'lookups', c: 'id' } }],
};

it('derives per-column fk flags from refs', () => {
  const data = toLayoutData(model);
  const lv = data.tables.find((t) => t.name === 'lookup_values')!;
  expect(lv.columns.find((c) => c.name === 'lookup_id')!.fk).toBe(true);
  expect(lv.columns.find((c) => c.name === 'id')!.fk).toBeFalsy();
  expect(data.refs).toHaveLength(1);
});

it('neighborsOf returns from+to connected tables', () => {
  const n = neighborsOf(model, 'config.lookup_values');
  expect(n.has('config.lookups')).toBe(true);
});
```
Run `npm run test:viewer` → FAIL (module missing).

- [ ] **Step 2: implement `model.ts`:**
```ts
export type Column = { name: string; type: string; pk?: boolean; nn?: boolean; en?: boolean; def?: string; note?: string };
export type Table = { schema: string; name: string; kind: string; note?: string; noteMd?: string; columns: Column[] };
export type RefEnd = { s: string; t: string; c: string };
export type Ref = { from: RefEnd; to: RefEnd; action?: string };
export type SchemaModel = {
  project: { name: string; db: string; note?: string };
  schemas: { name: string; tables: number; enums: number }[];
  tables: Table[];
  refs: Ref[];
};

export const nodeId = (schema: string, name: string) => `${schema}.${name}`;

/** Layout input: tables with a derived per-column `fk` flag + the raw refs. */
export function toLayoutData(model: SchemaModel) {
  const fkCols = new Set<string>();
  for (const r of model.refs) fkCols.add(`${r.from.s}.${r.from.t}.${r.from.c}`);
  const tables = model.tables.map((t) => ({
    schema: t.schema,
    name: t.name,
    columns: t.columns.map((c) => ({ ...c, fk: fkCols.has(`${t.schema}.${t.name}.${c.name}`) })),
  }));
  return { tables, refs: model.refs };
}

/** Tables connected to `id` via any ref (either direction). */
export function neighborsOf(model: SchemaModel, id: string): Set<string> {
  const out = new Set<string>();
  for (const r of model.refs) {
    const f = nodeId(r.from.s, r.from.t), t = nodeId(r.to.s, r.to.t);
    if (f === id) out.add(t);
    if (t === id) out.add(f);
  }
  return out;
}
```
Run → PASS. Commit: `feat(viewer): model types + toLayoutData adapter`.

## Task 2: port `layout.ts` (pure geometry)

**Files:** Create `site/src/lib/viewer/layout.ts`; test `layout.test.ts`.

- [ ] **Step 1: failing test** — assert determinism + shape:
```ts
import { it, expect } from 'vitest';
import { compute, edgePath } from './layout';
import { toLayoutData, type SchemaModel } from './model';
// (reuse a 2-table model with one ref)
it('compute returns cards positioned and one edge', () => {
  const data = toLayoutData(MODEL);
  const a = compute(data, 'keys', 'a-z');
  expect(Object.keys(a.cards)).toContain('config.lookups');
  expect(a.cards['config.lookups'].w).toBe(248);
  expect(a.edges).toHaveLength(1);
  // determinism: same input → same geometry
  const b = compute(data, 'keys', 'a-z');
  expect(b.cards['config.lookups'].x).toBe(a.cards['config.lookups'].x);
  expect(typeof edgePath(a.edges[0], 'curved')).toBe('string');
});
```
- [ ] **Step 2: port** `docs/mockup/designs/diagram-layout.js` to TypeScript in `layout.ts` — copy the algorithm verbatim (constants, `visibleCols`, `compute`, `pack`, cluster ordering, barycenter, `anchorY`, edges, `edgePath`), converting `window.DiagramLayout = (function(){…})()` into named `export function compute(data, density, arrange='untangle')` + `export function edgePath(e, style)`. Add TS types for the return (`{clusters, cards, edges, size, consts}`). Keep it a pure module (no DOM, no `window`). Run → PASS. Commit: `feat(viewer): port deterministic ER layout to TS`.

## Task 3: `Diagram.svelte` (SVG + d3-zoom)

**Files:** Create `site/src/lib/viewer/Diagram.svelte`, `state.svelte.ts`, `styles.css`; test `Diagram.test.ts`. **Use the svelte skill.**

Recreate `docs/mockup/designs/diagram.jsx` as a Svelte 5 component. Requirements:
- Props: `{ model: SchemaModel, state }` (state = the runes store from `state.svelte.ts`).
- Renders, from `compute(toLayoutData(model), state.density, state.arrange)`: schema-cluster regions, table cards (header = `schema.name`; rows = visible columns with PK/FK markers + type), and FK edges as SVG `<path d={edgePath(e, 'curved')}>`.
- Pan/zoom via `d3-zoom` on the root `<g transform>`; expose fit-to-view on mount.
- Selection: clicking a card sets `state.selected` and `state.mode='focus'`; in focus mode show only the selected node + `neighborsOf`; in overview, dim unrelated (`.dim`) and highlight selected (`.sel`)/related (`.rel`).
- **Styling: Rokkit tokens only** — cards `bg-paper-soft border-paper-edge text-ink`, edges `stroke: var(--ink-faint)` (highlighted `var(--primary)`), per-schema tint from an OKLCH hue (inline `--cl-h`) over `paper`. Map mockup tokens per the spec table. No raw hexes.
- `state.svelte.ts`: `export const createViewerState = () => $state({ selected: null, mode: 'overview', density: 'keys', arrange: 'untangle', filter: '' })`.

- [ ] Test (`@testing-library/svelte`): mount `Diagram` with the 2-table model → renders 2 `[data-card]` elements and ≥1 `<path>` edge; clicking a card sets focus (only neighbors visible). Commit: `feat(viewer): Diagram.svelte (SVG cards/edges, d3-zoom, focus)`.

## Task 4: `Sidebar.svelte` (Rokkit `List`)

**Files:** Create `site/src/lib/viewer/Sidebar.svelte`; test `Sidebar.test.ts`. **Use the svelte + semantic-styles-rokkit skills.**

- Use `@rokkit/ui` `List` with `collapsible={false}` so schemas are fixed group headers and tables are leaf items. Build `items` as groups: `model.schemas.map(s => ({ name: s.name, children: model.tables.filter(t => t.schema === s.name).map(t => ({ id: `${s.name}.${t.name}`, name: t.name })) }))` (adapt to List's `fields`/`items` API per `@rokkit/ui` `List.svelte`).
- A filter `<input>` bound to `state.filter` narrows the visible tables (case-insensitive substring on table name).
- `onselect` → set `state.selected = id`, `state.mode = 'focus'`.
- `groupContent` snippet: schema name + `tables`/`enums` count chips; `itemContent`: table name.

- [ ] Test: mount with the model → renders a `[data-list-group]` per schema and `[data-list-item]` per table; typing in the filter narrows items; selecting an item calls the select handler with the right id. Commit: `feat(viewer): Sidebar.svelte (Rokkit List schema→table nav + filter)`.

## Task 5: `Detail.svelte` (markdown + mini ERD)

**Files:** Create `site/src/lib/viewer/Detail.svelte`; test `Detail.test.ts`. **Use the svelte skill.** Recreate `docs/mockup/designs/entity-page.jsx`'s detail view.

- Props: `{ model, selected }`. When `selected` is a table id, show: the table `noteMd` rendered as markdown (reuse `marked` — the site already depends on it), then a columns table (Column · badges PK/FK/NN/ENUM from `pk`/derived-fk/`nn`/`en` · Type · Default · Note), then a **mini focused ERD** = `Diagram` restricted to the selected node + `neighborsOf` (reuse the focus layout).
- Rokkit tokens for all styling; badges as `bg-accent-soft text-primary`/etc.

- [ ] Test: mount with `selected='config.lookup_values'` → shows a row for `lookup_id` with an `FK` badge and a row for `id` with a `PK` badge; renders the note markdown. Commit: `feat(viewer): Detail.svelte (markdown column detail + mini ERD)`.

## Task 6: `Viewer.svelte` shell + `index.ts` mount

**Files:** Create `site/src/lib/viewer/Viewer.svelte`, `index.ts`; test `Viewer.test.ts`. **Use the svelte + semantic-styles-rokkit skills.** Recreate `docs/mockup/designs/project-view.jsx` layout.

- `Viewer.svelte` props: `{ model: SchemaModel }`. Creates the viewer state, lays out: **header** (`site/static/dbd.svg` logo + project name + db badge + schema/table/ref counts + `ThemeSwitcherToggle variant="triad"` + density (`names|keys|full`) and arrange (`untangle|a-z`) controls bound to state), **sidebar** (`Sidebar`), **main** (`Diagram`), and a **detail slide-over** (`Detail`, shown when `state.selected`). Use a `NavContent`-style or CSS grid layout; Rokkit tokens throughout.
- For dark/light in the standalone HTML: include `<svelte:body use:themable={{ theme: vibe, storageKey: 'dbd-diagram-theme' }} />` so the toggle persists (no SvelteKit hook in the static file; acceptable — first paint defaults to system via the toggle).
- `index.ts`:
```ts
import { mount } from 'svelte';
import Viewer from './Viewer.svelte';
import type { SchemaModel } from './model';
export { Viewer };
export function mountViewer(target: HTMLElement, model: SchemaModel) {
  return mount(Viewer, { target, props: { model } });
}
```

- [ ] Test: mount `Viewer` with the model → header shows project name + counts; sidebar + diagram present; selecting a table opens the detail panel. Commit: `feat(viewer): Viewer.svelte shell + mountViewer`.

## Task 7: Vite lib build → embeddable bundle

**Files:** Create `site/vite.viewer.config.ts`; add `build:viewer` script; modify `Makefile`.

- [ ] `vite.viewer.config.ts`: a standalone Vite config (svelte plugin + `UnoCSS()` with the same `uno.config`) building `src/lib/viewer/index.ts` as `build.lib` (`formats: ['iife']`, `name: 'DbdDiagram'`, entry exposes `mountViewer`), `build.cssCodeSplit: false`, output to a temp dir, then a small post-step concatenates JS + emitted CSS into ONE self-contained file at `crates/dbd-core/assets/diagram_viewer.js` (CSS injected via a `<style>` the IIFE appends on load, OR use `vite-plugin-css-injected-by-js` — add it as a dev-dep). The IIFE must expose `window.DbdDiagram.mountViewer`.
- [ ] `site/package.json`: `"build:viewer": "vite build --config vite.viewer.config.ts"`.
- [ ] `Makefile`: a `viewer` target: `cd site && <pm> install && <pm> run build:viewer` (writes the committed bundle).
- [ ] **Verify:** run `make viewer`; confirm `crates/dbd-core/assets/diagram_viewer.js` exists, is non-empty, references no remote URLs, and defines `mountViewer`. Commit the config + scripts + the built bundle: `build(viewer): standalone bundle → crates/dbd-core/assets`.

## Task 8: Rust HTML render + `dbd diagram` default

**Files:** Create `crates/dbd-core/assets/diagram.html`; add `render_html` (new `crates/dbd-core/src/diagram.rs`, `pub mod diagram;` in lib.rs); modify `crates/dbd-cli/src/commands/diagram.rs` + `cli.rs` help.

- [ ] `crates/dbd-core/assets/diagram.html`:
```html
<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>__DBD_TITLE__ — schema</title></head>
<body><div id="app"></div>
<script type="application/json" id="dbd-model">__DBD_MODEL__</script>
<script>__DBD_VIEWER__</script>
<script>window.DbdDiagram.mountViewer(document.getElementById('app'),
  JSON.parse(document.getElementById('dbd-model').textContent));</script>
</body></html>
```
- [ ] `crates/dbd-core/src/diagram.rs`:
```rust
use crate::schema_model::SchemaModel;
const TEMPLATE: &str = include_str!("../assets/diagram.html");
const VIEWER: &str = include_str!("../assets/diagram_viewer.js");
/// Render a self-contained HTML schema explorer embedding the model + viewer.
pub fn render_html(model: &SchemaModel) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(model)?;
    Ok(TEMPLATE
        .replace("__DBD_TITLE__", &model.project.name)
        .replace("__DBD_MODEL__", &json)        // inside a <script type=application/json>: safe except </script>
        .replace("__DBD_VIEWER__", VIEWER))
}
```
  - Guard the `</script>` edge case: escape `</` in the JSON as `<\/` before injecting (one line: `let json = json.replace("</", "<\\/");`). Add a unit test asserting the rendered HTML contains `id="dbd-model"`, contains `mountViewer`, and has no `</script>` inside the model script payload.
- [ ] `lib.rs`: `pub mod diagram;`.
- [ ] `crates/dbd-cli/src/commands/diagram.rs`: when `json` is true → write JSON (current behavior); else → `let html = dbd_core::diagram::render_html(&model)?;` write to `file` (default now `schema.html`). Update the default in `cli.rs` `file` to `"schema.html"`, and reword the `--json` help to "Emit the raw SchemaModel JSON instead of the HTML diagram".
- [ ] **Tests:** Rust unit test for `render_html` (self-contained: no `src="http`/`href="http`; embeds model + viewer). A CLI smoke (manual): `dbd diagram -f /tmp/s.html` from fixtures → open, confirm it renders (or at least the file contains the model + `mountViewer`). Commit: `feat: dbd diagram renders self-contained HTML (default); --json opt-in`.

## Task 9: docs + CI

**Files:** `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt`, `README.md`, `.github/workflows/ci.yml`.

- [ ] Flip the `dbd diagram` docs: HTML is the default (`dbd diagram` → `schema.html`), `--json` emits the model. Mention it's a self-contained interactive diagram replacing the dbdocs.io step. Update README row + llms entries accordingly.
- [ ] CI: add a **viewer bundle freshness** check — a job (or step in the existing `test` job, with Node) that runs `make viewer` and `git diff --exit-code crates/dbd-core/assets/diagram_viewer.js` so the committed bundle can't drift from source. (Node/npm is available on `ubuntu-latest`.)
- [ ] Commit: `docs+ci: dbd diagram HTML default; viewer bundle freshness check`.

---

## Self-review checklist (run before handoff)
- Spec coverage: viewer in `site/src/lib/viewer` (Rokkit tokens + `List`) ✓ (T3–6); reuse mockup `diagram-layout.js` ✓ (T2); `SchemaModel` input + `fk`-from-refs adapter ✓ (T1); overview + focus + sidebar + markdown detail ✓ (T3–5); self-contained HTML via `include_str!` + `render_html`, HTML default + `--json` ✓ (T7–8); bundle pre-built/committed + `make viewer` + CI freshness ✓ (T7,T9); no-remote-URL guarantee ✓ (T8). Deferred (per spec): views/funcs/procs nodes, website embedding, versioned storage.
- Placeholder scan: infra/contract tasks (T1,2,7,8) carry full code; the visual Svelte components (T3–6) specify exact props, required rendered elements, Rokkit token usage, and tests, and point at the authoritative mockup files to recreate — recreation via the svelte skill is the intended method, not a placeholder.
- Type consistency: `SchemaModel`/`toLayoutData`/`neighborsOf`/`compute`/`edgePath`/`mountViewer`/`render_html` names are used identically across tasks; the viewer's `state.svelte.ts` shape (`selected/mode/density/arrange/filter`) is referenced consistently by Diagram/Sidebar/Detail/Viewer.

## Done when
`dbd diagram` (no flags) writes a self-contained `schema.html` that opens to an interactive explorer (sidebar schema→table, click a table → markdown detail + focused ERD, overview ER diagram with pan/zoom, dark/light toggle), with zero network requests; `--json` still emits the model. `cargo test --workspace` + clippy green; `npm run test:viewer` green; `make viewer` reproducible + CI freshness check passing. Then the feature is usable → release (`make bump`).
