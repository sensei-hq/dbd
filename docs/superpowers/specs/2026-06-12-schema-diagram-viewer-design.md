# Schema Diagram Viewer (`dbd diagram`) — Design

**Goal:** `dbd diagram` produces a single self-contained HTML file that interactively explores a project's schema — a sidebar schema→table tree, markdown-style column detail per table, and an SVG ER diagram with a full overview and a per-entity focus view (references from/to). No external services, no `dbdocs.io` publish step, no runtime network dependencies.

**Architecture:** dbd emits a **dbd-native `SchemaModel` JSON** (built from its entities + dependency graph, *not* from DBML), and a framework-agnostic JS **viewer** renders that JSON. The CLI inlines the model JSON + an embedded viewer bundle into an HTML template. The same viewer module is reusable by the website later (Phase 1) and by versioned-storage hosting (Phase 2).

**Tech stack:** Rust (`dbd-core` + `dbd-cli`); a standalone `viewer/` TypeScript package built with esbuild/bun to one minified bundle (CSS inlined); SVG rendering with `d3-zoom` for pan/zoom (no other D3, no layout library); tests via Rust unit/snapshot + vitest/jsdom for the viewer.

---

## Why a JSON model, not DBML

DBML cannot represent views, procedures, or functions. Coupling the viewer to DBML would force a rewrite when those are added. The `SchemaModel` is a dbd-native superset: tables/columns/FKs in v1, with a `kind` discriminator on nodes and edges that extends to views/functions/procedures (already first-class dbd entities with `refers`/`reads`/`writes` edges) without changing the model or the viewer's contract. DBML generation (`dbd dbml`) remains a separate, parallel output.

## Non-goals (deferred)

- View / function / procedure nodes (the model is *designed* for them, but v1 emits only tables/schemas/FK edges).
- Website embedding of the viewer (Phase 1) and versioned per-project/user DBML storage + history (Phase 2).
- PNG/SVG image export.
- Any layout library (elk/dagre) — kept out to keep the embedded bundle small.

---

## Data flow

```
Design (entities + TableDefs + dependency graph)
   └─ schema_model::build(&Design, scope) ──> SchemaModel  (serde)
                                                  │
   dbd diagram ───────────────────────────────────┤
     --json   → <out>.json   (raw SchemaModel, for the site / tooling)
     (default)→ <out>.html   (self-contained):
                  diagram.html template (include_str!)
                  + <script id="dbd-model">…model JSON…</script>
                  + <script>…embedded viewer bundle (include_str!)…</script>

viewer/ (vanilla TS) ── build (bun/esbuild) ──> viewer bundle (JS + inlined CSS)
   ├─ committed to crates/dbd-core/assets/diagram_viewer.js  (embedded in the binary)
   └─ imported by the SvelteKit site later (Phase 1) — same source
```

The viewer's single input is a `SchemaModel`. It never parses DBML or SQL.

---

## `SchemaModel` JSON (v1)

```jsonc
{
  "project": "MyProject",
  "schemas": [{ "name": "config" }, { "name": "app" }],
  "nodes": [
    {
      "id": "config.lookups",
      "schema": "config",
      "name": "lookups",
      "kind": "table",                 // extension point: view | function | procedure | enum
      "note": "optional comment text or null",
      "columns": [
        { "name": "id",        "type": "uuid", "pk": true,  "nullable": false, "fk": null },
        { "name": "parent_id", "type": "uuid", "pk": false, "nullable": true,
          "fk": { "to": "config.lookups", "column": "id" } }
      ]
    }
  ],
  "edges": [
    { "from": "config.lookup_values", "to": "config.lookups",
      "kind": "fk", "columns": [["lookup_id", "id"]] }
  ]
}
```

Rust types (in `crates/dbd-core/src/schema_model.rs`), all `#[derive(Serialize, Deserialize, PartialEq, Debug)]`:

```rust
pub struct SchemaModel { pub project: String, pub schemas: Vec<SchemaRef>, pub nodes: Vec<Node>, pub edges: Vec<Edge> }
pub struct SchemaRef { pub name: String }
pub struct Node { pub id: String, pub schema: Option<String>, pub name: String, pub kind: NodeKind, pub note: Option<String>, pub columns: Vec<Column> }
pub enum NodeKind { Table /* future: View, Function, Procedure, Enum */ }   // serde rename_all = "snake_case"
pub struct Column { pub name: String, #[serde(rename="type")] pub ty: String, pub pk: bool, pub nullable: bool, pub fk: Option<Fk> }
pub struct Fk { pub to: String, pub column: String }
pub enum EdgeKind { Fk }                                                    // future: Depends, Reads, Writes
pub struct Edge { pub from: String, pub to: String, pub kind: EdgeKind, pub columns: Vec<(String, String)> }
```

**Builder** — `schema_model::build(design: &Design, scope: Option<&ResolvedScope>) -> SchemaModel`:
- Nodes: each scoped `EntityType::Table` with a `TableDef` → a `Node` with `columns` from `TableDef.columns` (name, type, `is_pk`, `nullable`) and per-column `fk` from the table's foreign keys (single-column FKs map to `Fk { to, column }`; composite FKs populate the column pairs on the corresponding `Edge`).
- Schemas: the distinct schemas of the included nodes.
- Edges: derived from each `TableDef`'s foreign-key constraints (column-level — the viewer needs the local↔remote column pairs, which the node-level dependency graph does not carry). One `Edge { kind: Fk, columns: [(local, remote), …] }` per FK constraint between two in-scope tables. FKs whose target is out of scope or external are omitted from `edges` (the originating column still carries its `fk` marker in the node). The node-level dependency graph is not used in v1; the focus view computes a node's neighbors directly from `edges`.
- Scope-aware: uses `Design::scoped_entities` so `--scope` filters the model exactly like the other commands.

## CLI: `dbd diagram`

New subcommand in `crates/dbd-cli/src/cli.rs` + `crates/dbd-cli/src/commands/`:

```sh
dbd diagram                  # writes schema.html (self-contained, default)
dbd diagram -f db.html        # custom output path
dbd diagram --json -f m.json  # raw SchemaModel JSON instead of HTML
dbd diagram --scope hub       # scope-aware (Design::scoped_entities)
```

Flags: `-f/--file` (default `schema.html`), `--json` (emit raw model JSON; default output name `schema.json` if `-f` omitted), plus the global `--scope`/`--deps`. Writing uses the existing `safe_write` within the project root. HTML build: `diagram::render_html(&model) -> String` = `include_str!("../assets/diagram.html")` template with two placeholders replaced — the model JSON (`__DBD_MODEL__`) and the viewer bundle (`include_str!("../assets/diagram_viewer.js")`). The model JSON is injected inside a `<script type="application/json" id="dbd-model">` tag (not interpolated into JS) to avoid escaping issues.

## Viewer package

Location: top-level `viewer/` (standalone, framework-agnostic, no dependency on SvelteKit).

```
viewer/
  package.json          # build script (bun/esbuild), vitest
  src/
    index.ts            # export mount(el: HTMLElement, model: SchemaModel): void
    model.ts            # SchemaModel TS types (mirror of the Rust JSON)
    layout/
      overview.ts       # schema-grouped grid layout → positioned cards + edge paths
      focus.ts          # centered selected node + referencers/referenced neighbors (1 hop)
    render/
      diagram.ts        # SVG: cards, columns, FK edge paths; d3-zoom pan/zoom
      sidebar.ts        # schema→table tree + filter box
      detail.ts         # markdown-style column table + note + mini focused ERD
    state.ts            # selection / mode (overview|focus) / filter state
    styles.css          # inlined into the bundle at build
  dist/                 # build output (committed mirror lives in crates/dbd-core/assets/)
```

- **Entry contract:** `mount(el, model)` renders the whole UI into `el`. The self-contained HTML calls `mount(document.body, JSON.parse(document.getElementById('dbd-model').textContent))`. The website (Phase 1) calls the same `mount` with a model fetched/decoded however it likes.
- **Build:** `bun build src/index.ts --minify --bundle` → a single self-contained `viewer.js`. `styles.css` is imported as text (esbuild `loader: { '.css': 'text' }`) and injected into a `<style>` element by `mount()` on first render. `d3-zoom` is bundled (tree-shaken). No other runtime dependencies.
- **No layout library.** Overview = schema regions (labeled boxes) with cards grid-packed inside; FK edges are SVG `<path>` (orthogonal or cubic-bezier) between card column anchors. Focus = selected node centered, referencers left, referenced-by right (1 hop, click a neighbor to re-focus). Pan/zoom via `d3-zoom` on the SVG root `<g>`.

## UX

- **Left sidebar:** collapsible tree grouped schema → tables; a filter input narrows the tree (and the diagram highlight). 
- **Main canvas:** SVG diagram; overview ⇄ focus toggle; pan/zoom; click a card (or sidebar item) to select.
- **Detail panel:** on selection — a markdown-style column table (columns: name · type · PK · null · FK→target) + the node `note`, plus the mini focused ERD (the table + its direct FK neighbors).
- **Selection semantics:** selecting an entity highlights it and its FK edges and switches the diagram to its focus view (refs from/to).

## Self-contained guarantee

The output HTML references no external URLs (no CDN, no `<link>`/`<script src=…>` to remote, no web-font fetches — system font stack). A test asserts the rendered HTML contains no `src="http`, `href="http`, or `@import url(http` and embeds both the model and the viewer inline.

## Build & maintenance

- `cargo build`/`cargo install` must NOT require a JS toolchain. Therefore the viewer bundle is **pre-built and committed** to `crates/dbd-core/assets/diagram_viewer.js` and embedded with `include_str!`.
- A `make viewer` target builds `viewer/` and copies the bundle to `crates/dbd-core/assets/diagram_viewer.js`.
- CI adds a **bundle-freshness check**: rebuild the viewer and `git diff --exit-code` the committed bundle, so the embedded artifact can't silently drift from source. (Runs in the `test + clippy` job with bun available, or a dedicated job.)

## File structure

**Create:**
- `crates/dbd-core/src/schema_model.rs` — `SchemaModel` types + `build()`; unit/snapshot tests.
- `crates/dbd-core/assets/diagram.html` — HTML template with `__DBD_MODEL__` / `__DBD_VIEWER__` placeholders.
- `crates/dbd-core/assets/diagram_viewer.js` — committed pre-built viewer bundle.
- `crates/dbd-cli/src/commands/` — `cmd_diagram` (wire into `cli.rs` `Commands::Diagram` + `commands/mod.rs` dispatch).
- `viewer/` — the TypeScript viewer package (src, build, vitest).

**Modify:**
- `crates/dbd-core/src/lib.rs` — `pub mod schema_model;` + re-exports.
- `crates/dbd-cli/src/cli.rs`, `crates/dbd-cli/src/commands/mod.rs` — add the `Diagram` command + dispatch (scope/deps already threaded through `run`).
- `Makefile` — `viewer` target.
- Docs: `docs/guide/04-commands.md` (new `dbd diagram` section), `docs/guide/03-design-yaml.md` (none needed), `docs/llms/llms.txt` + `llms-full.txt` (command entry), README command table. Mark `dbd diagram` in the landing page later.

## Testing

- **Rust (`schema_model`)**: from fixture entities, `build()` produces expected schemas, nodes with correct columns + PK/nullable/FK flags, and FK edges with column pairs; composite FK; scope filtering drops out-of-scope nodes/edges; snapshot of the JSON via `insta`.
- **CLI (`dbd diagram`)**: emits a non-empty self-contained HTML containing the model `<script>` and the viewer; `--json` emits parseable `SchemaModel`; self-contained assertion (no remote URLs); `--scope` reflected in the model.
- **Viewer (vitest + jsdom)**: `mount(el, model)` renders N table cards + M edge paths; sidebar tree lists schemas/tables; selecting a node enters focus mode and shows only neighbors; filter narrows the tree. Pure `model → DOM`, no network.
- **Bundle freshness** (CI): committed bundle equals a fresh build.

## Scope (v1)

**In:** tables + schemas + FK relationships; overview + focus + sidebar + detail; `dbd diagram` (HTML + `--json`), scope-aware; viewer module structured for site reuse (`mount(el, model)`).

**Deferred:** view/function/procedure node kinds (model + viewer extension points in place); website embedding (Phase 1); versioned storage/history (Phase 2); image export.
