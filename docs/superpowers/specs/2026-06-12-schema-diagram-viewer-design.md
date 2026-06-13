# Schema Diagram Viewer (`dbd diagram`) — Design

**Goal:** `dbd diagram` produces a single self-contained HTML file that interactively explores a project's schema — a sidebar schema→table hierarchy, markdown-style column/entity detail, and an SVG ER diagram with a full overview and a per-entity focus view (references from/to). No external services, no `dbdocs.io` publish step, no runtime network dependencies.

**Architecture:** dbd emits a **dbd-native `SchemaModel` JSON** (built from its entities + dependency graph, *not* from DBML). A **Svelte 5 + Rokkit viewer** renders that JSON; it is compiled to a single self-contained bundle the CLI embeds into an HTML template. The same Svelte components are imported directly by the marketing site for Phase 1, and back the versioned-storage product in Phase 2.

**Tech stack:** Rust (`dbd-core` + `dbd-cli`); the viewer is **Svelte 5** components in `site/src/lib/viewer/` styled with the **Rokkit** token + component system (reusing the site's `rokkit.config.js`); SVG diagram with `d3-zoom` for pan/zoom; the ER layout reuses the mockup's deterministic `diagram-layout.js` (ported to TS). The viewer is built to an embeddable bundle via a dedicated Vite/UnoCSS build. Tests: Rust unit/snapshot + Svelte component tests (vitest + @testing-library/svelte).

**Visual reference:** `docs/mockup/designs/` (the "Project View" handoff). The layout algorithm (`diagram-layout.js`), screen structure (`project-view.jsx`, `diagram.jsx`, `entity-page.jsx`), and data shape (`schema-data.js` → `window.DBD_SCHEMA`) are reused/recreated; the mockup's hand-rolled tokens are **replaced by Rokkit named tokens**, and its hand-rolled components by **Rokkit components** where they exist (the sidebar uses Rokkit `List`).

---

## Why a JSON model, not DBML

DBML cannot represent views, procedures, or functions. Coupling the viewer to DBML would force a rewrite when those are added. The `SchemaModel` is a dbd-native superset: tables/columns/refs in v1, extending to views/functions/procedures (already first-class dbd entities with `refers`/`reads`/`writes`) by adding node kinds — no model or viewer-contract rewrite. DBML generation (`dbd dbml`) remains a separate, parallel output.

## Roadmap (phases)

This spec covers **v1 only**. Later phases are recorded so v1's boundaries (the `SchemaModel` JSON + the Svelte+Rokkit viewer components) are drawn to support them; each later phase gets its own spec.

- **v1 — local HTML (this spec).** `dbd diagram` renders a self-contained single-project HTML schema explorer. No auth, no network, no storage.
- **v2 — hosted login + storage + published designs.** A Supabase instance with magic-link login (via **kavach**, `~/Developer/kavach`) backs the website. Sketch schema:
  - `projects (id, user_id, name, target, version, json)` — current published model per project.
  - `project_history (id, project_id, version, json, created_at)` — prior versions + models.

  The website lists the signed-in user's projects and renders each project's diagram with the *same* Svelte viewer from v1. A light **daily keep-alive** job pings the DB so a low-traffic Supabase instance isn't paused.
- **v3 — CLI auth + publish.** `dbd` authenticates (kavach) and publishes a project's `SchemaModel` JSON to Supabase (`projects` + append `project_history`) — e.g. `dbd diagram --publish`.
- **v4 — sharing designs via the UI.** Share links / visibility controls for published designs.

## Non-goals (deferred)

- View / function / procedure node kinds (model + viewer are *designed* for them; v1 emits tables/schemas/refs only).
- Website embedding (Phase 1), versioned storage/history (Phase 2), CLI publish (v3), sharing (v4).
- PNG/SVG image export.
- Embedding the mockup's display/mono web fonts in the self-contained HTML (v1 uses a system-font stack via Rokkit typography to stay offline; font embedding is later polish).

---

## Data flow

```
Design (entities + TableDefs + dependency graph)
   └─ schema_model::build(&Design, scope) ──> SchemaModel  (serde → the DBD_SCHEMA JSON shape)
                                                  │
   dbd diagram ───────────────────────────────────┤
     --json   → <out>.json   (raw SchemaModel, for the site / tooling)
     (default)→ <out>.html   (self-contained):
                  diagram.html template (include_str!)
                  + <script type="application/json" id="dbd-model">…model…</script>
                  + <script>…embedded viewer bundle (include_str!)…</script>  // Svelte+Rokkit, CSS inlined

site/src/lib/viewer/  (Svelte 5 + Rokkit components)
   ├─ vite build (lib entry + presetRokkit) → self-contained bundle (JS w/ inlined CSS)
   │     → committed to crates/dbd-core/assets/diagram_viewer.js → embedded in the dbd binary
   └─ imported directly by the SvelteKit site (Phase 1) — same components, native Rokkit
```

The viewer's single input is a `SchemaModel`. It never parses DBML or SQL. The self-contained HTML mounts the viewer with Svelte 5 `mount(Viewer, { target, props: { model } })`, reading the model from the inert `<script id="dbd-model">` tag.

## `SchemaModel` JSON (v1) — serializes to the mockup's `DBD_SCHEMA` contract

```jsonc
{
  "project": { "name": "sensei", "db": "postgresql", "note": "…" },
  "schemas": [ { "name": "config", "tables": 3, "enums": 1 } ],
  "tables": [
    {
      "schema": "config", "name": "lookup_values", "kind": "table",   // kind extends: view|function|procedure
      "note": "short note", "noteMd": "markdown note\nmultiple lines",
      "columns": [
        { "name": "id",        "type": "uuid", "pk": 1, "nn": 1, "def": "gen_random_uuid()" },
        { "name": "lookup_id", "type": "uuid", "nn": 1 }
      ]
    }
  ],
  "refs": [
    { "from": { "s": "config", "t": "lookup_values", "c": "lookup_id" },
      "to":   { "s": "config", "t": "lookups",       "c": "id" }, "action": "cascade" }
  ]
}
```

Rust types in `crates/dbd-core/src/schema_model.rs` (serde, field names/casing matching the JSON above; booleans emitted as `1`/omitted to match the mockup contract via `skip_serializing_if`). Column flags: `pk`, `nn` (not null), `en` (enum type), `def` (default), `note`. FK relationships live in `refs` (column-level `{s,t,c}` + `action`). `kind` defaults to `"table"`; future view/function/procedure entries reuse the same `tables`/`refs` arrays with other `kind`s (or a future `nodes` alias).

**Builder** — `schema_model::build(design: &Design, scope: Option<&ResolvedScope>) -> SchemaModel`:
- `tables`: each scoped `EntityType::Table` with a `TableDef` → columns (name/type/pk/nn/en/def/note) + table `note`/`noteMd` (from the entity's comment).
- `schemas`: distinct schemas of the included tables, with table/enum counts.
- `refs`: one entry per FK constraint between two in-scope tables, column-level (`from`/`to` `{s,t,c}` + `action`). FKs to out-of-scope/external targets are omitted from `refs`.
- Scope-aware via `Design::scoped_entities`, exactly like the other commands.

## CLI: `dbd diagram`

New subcommand (`cli.rs` `Commands::Diagram` + `commands/`):

```sh
dbd diagram                  # writes schema.html (self-contained, default)
dbd diagram -f db.html        # custom output path
dbd diagram --json -f m.json  # raw SchemaModel JSON (for the site / tooling)
dbd diagram --scope hub       # scope-aware (Design::scoped_entities)
```

Flags: `-f/--file` (default `schema.html`; `schema.json` when `--json`), `--json`, plus global `--scope`/`--deps`. `diagram::render_html(&model) -> String` = `include_str!("../assets/diagram.html")` with two placeholders replaced: the model JSON (inside `<script type="application/json" id="dbd-model">`) and the viewer bundle (`include_str!("../assets/diagram_viewer.js")`). Output via `safe_write` within the project root.

## Viewer (Svelte 5 + Rokkit) — `site/src/lib/viewer/`

```
site/src/lib/viewer/
  index.ts             # export Viewer (Svelte component) + a mount(target, model) helper
  Viewer.svelte        # shell: header (logo + ThemeSwitcherToggle + density/arrange controls), sidebar, canvas, detail
  Sidebar.svelte       # Rokkit `List` (from @rokkit/ui): schema = group header, tables = leaf items; + filter input
  Diagram.svelte       # SVG: schema-cluster cards, columns, FK edge paths; d3-zoom pan/zoom; selection highlight
  Detail.svelte        # selected entity: markdown-style column table (name·type·PK·null·FK) + noteMd + mini focused ERD
  layout.ts            # ported from docs/mockup/designs/diagram-layout.js (pure: model+density+arrange → geometry)
  model.ts             # SchemaModel TS types (mirror the Rust JSON)
  state.svelte.ts      # selection / mode (overview|focus) / density / arrange / filter (Svelte 5 runes)
```

- **Sidebar**: Rokkit `List` with `groupContent` (schema header, table/enum counts) + `itemContent` (table name); the filter input narrows it. Selecting an item selects the entity.
- **Diagram**: bespoke SVG (Rokkit has no ERD component). Uses `layout.ts` (the mockup's algorithm: schema clusters with per-schema tint, density `names|keys|full`, `untangle` connectivity ordering + barycenter crossing reduction, column-anchored edge routing — orthogonal/curved/self-loop). `d3-zoom` drives pan/zoom on the SVG root `<g>`. Cards/edges styled with Rokkit tokens (`bg-paper-soft`, `border-paper-edge`, `text-ink`, edges `stroke` from `--primary`/`--ink-faint`). Per-schema tints derived in OKLCH from the accent hue.
- **Focus view**: selecting an entity (sidebar or card) switches to focus — the node centered with its `refs` neighbors (from/to), computed directly from `refs`; click a neighbor to re-focus. Same view powers the **mini ERD** in the detail panel.
- **Theme**: `ThemeSwitcherToggle` (`@rokkit/app`) + `themable` on the root → dark/light via the site's dual-palette skin; persisted to `localStorage` (works in the static file).

## Tokens — Rokkit, not the mockup's hand-rolled vars

The mockup's `:root` vars and `tw-config.js` are **dropped**; the viewer uses Rokkit named tokens via the site's `rokkit.config.js`. Mapping:

| Mockup token | Rokkit token |
|---|---|
| `--bg` / `--bg-deep` | `paper` / `paper` (canvas) |
| `--surface` / `--surface-2` | `paper-soft` / `paper-mute` |
| `--line` / `--line-soft` | `paper-edge` |
| `--fg` / `--muted` / `--faint` | `ink` / `ink-mute` / `ink-faint` |
| `--accent` / `--on-accent` / `--accent-soft` | `primary` / `on-primary` / `accent-soft` |
| `--font-display` / sans / mono | Rokkit `typography` (`font-heading` / `font-body` / `font-mono`) |
| `--radius*` | Rokkit `shape.radius` (`rounded-md/-lg/-sm`) |

Per-schema cluster tints (mockup's 8 hue angles) are kept as a visual affordance, derived in OKLCH so they read in both light/dark — not raw mockup hexes.

## Self-contained guarantee

Output HTML references no external URLs (no CDN/`<script src=…>`/`<link>` to remote, no remote web fonts — system font stack via Rokkit typography for v1). A test asserts the HTML embeds the model + viewer inline and contains no `src="http`, `href="http`, or `@import url(http`.

## Build & maintenance

- `cargo build`/`cargo install` must NOT require a JS toolchain → the viewer bundle is **pre-built and committed** to `crates/dbd-core/assets/diagram_viewer.js` and embedded via `include_str!`.
- A dedicated Vite build in `site/` (`vite.viewer.config.ts`, `build.lib` entry = `src/lib/viewer/index.ts`, `presetRokkit`, CSS inlined into the JS, IIFE/ESM exposing the mount) → `site/dist-viewer/` → copied to `crates/dbd-core/assets/diagram_viewer.js`. `make viewer` runs it (bun). The HTML template + a `bun run build:viewer` script live in `site/`.
- CI **bundle-freshness check**: rebuild the viewer and `git diff --exit-code` the committed bundle so it can't drift from source.
- The marketing site imports the viewer components directly (no embed step) for Phase 1.
- Implementation uses the **Svelte MCP / svelte skills** (the `svelte-file-editor` agent) for all `.svelte` work, and the **semantic-styles-rokkit** skill for tokens.

## File structure

**Create:**
- `crates/dbd-core/src/schema_model.rs` — `SchemaModel` types + `build()`; unit/snapshot tests.
- `crates/dbd-core/assets/diagram.html` — HTML template with `__DBD_MODEL__` / `__DBD_VIEWER__` placeholders.
- `crates/dbd-core/assets/diagram_viewer.js` — committed pre-built viewer bundle.
- `crates/dbd-cli/src/commands/` — `cmd_diagram` (+ `cli.rs` `Commands::Diagram` + `commands/mod.rs` dispatch).
- `site/src/lib/viewer/` — Svelte 5 + Rokkit viewer components + `layout.ts` + tests.
- `site/vite.viewer.config.ts` + `package.json` `build:viewer` script.

**Modify:**
- `crates/dbd-core/src/lib.rs` — `pub mod schema_model;` + re-exports.
- `crates/dbd-cli/src/cli.rs`, `commands/mod.rs` — add `Diagram` (scope/deps already threaded).
- `Makefile` — `viewer` target.
- Docs: `docs/guide/04-commands.md` (new `dbd diagram` section), `docs/llms/llms*.txt` + README command table (entry). Landing page mention later.

## Testing

- **Rust (`schema_model`)**: fixture entities → expected schemas (+counts), tables with correct columns + pk/nn/en/def/note, table `noteMd`, refs with `{s,t,c}` + action, composite FK; scope filtering drops out-of-scope tables/refs; `insta` JSON snapshot.
- **CLI (`dbd diagram`)**: emits non-empty self-contained HTML containing the model `<script>` + viewer; `--json` emits parseable `SchemaModel`; no-remote-URL assertion; `--scope` reflected.
- **Viewer (vitest + @testing-library/svelte, jsdom)**: `mount` renders N cards + M edges from a model; Rokkit `List` shows schema groups + table items; selecting a node enters focus and shows only neighbors; filter narrows the sidebar; `layout.ts` is unit-tested as a pure function (deterministic geometry, edge anchors).
- **Bundle freshness** (CI): committed bundle equals a fresh build.

## Scope (v1)

**In:** tables + schemas + FK refs; overview + focus + sidebar (Rokkit `List`) + markdown detail; `dbd diagram` (HTML + `--json`), scope-aware; Svelte+Rokkit viewer in `site/src/lib/viewer/` built to an embeddable bundle and importable by the site.

**Deferred:** view/function/procedure kinds (extension points in place); website embedding (Phase 1); versioned storage (Phase 2); CLI publish (v3); sharing (v4); image export; embedded web fonts.
