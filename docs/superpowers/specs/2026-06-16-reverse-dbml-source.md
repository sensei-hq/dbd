# Reverse-engineer from a DBML file — design

- **Date:** 2026-06-16
- **Status:** approved (design)
- **Target release:** patch v0.7.1
- **Builds on:** the reverse engine (init/merge/snapshot) + the DBML *exporter* (`dbml.rs`)

## Goal

Add DBML as a **source**: `dbd init --from-dbml <file>` / `dbd merge --from-dbml <file>` parse a
`.dbml` file into `Vec<Entity>` and feed the existing reverse engine (emit → write-plan →
snapshot). DBML is export-only today; this is the inverse parser.

## Architecture

The reverse engine is `source → Vec<Entity> → emit → write-plan → apply`. A DBML source plugs in
at the same point as the Postgres adapter: parse the file → entities, then reuse the unchanged
emit/write-plan/snapshot path. No DB connection → DBML is always a "foreign" source: the
version-safety gate (which needs `_dbd_meta`) does not apply — `init` generates + baseline
snapshot; `merge` uses the snapshot path directly.

## CLI

- `dbd init --from-dbml <FILE>` and `dbd merge --from-dbml <FILE>`. A file path, **mutually
  exclusive with `--from-db`** (error if both). No `-d`/`$DATABASE_URL`.
- Schema-selection flags (`--schema`/`--exclude-schema`/`--all-schemas`) still filter the parsed
  entities by schema. `--roles` is irrelevant (DBML has no roles).
- `init --from-dbml` honors `--name`/`--version` and the existing guards (refuse if `design.yaml`
  exists). `merge --from-dbml` refuses if no project.

## DBML parser (`dbd-core`, new `dbml_parse.rs`)

`pub fn parse_dbml(text: &str) -> Result<Vec<Entity>>` — hand-rolled (no mature Rust DBML crate;
scope is what `dbml.rs` emits + standard dbdiagram.io DBML). Mirror the exporter's constructs:

- **`Project "name" { database_type: '…' [Note: '…'] }`** → captured as project name/note (used by
  `init` for `design.yaml`; ignored by `merge`).
- **`Enum "schema"."name" { "v1" "v2" … }`** → `Entity(Enum)` with `enum_values` (order preserved).
- **`Table "schema"."name" { <cols> [indexes { … }] [Note: …] }`** → `Entity(Table)` with a
  `TableDef`:
  - **column** `"name" type [settings]` — settings (comma-separated, any order): `pk` →
    PrimaryKey constraint + `is_pk`; `unique` → `is_unique`; `not null` → `nullable=false`;
    `increment` → (type is already `serial`/`bigserial` in dbd's export, keep it); `default: <v>`
    where `<v>` is `'string'` | `` `expr` `` (backtick = raw expression) | number | `true/false/null`;
    `note: '…'` → column comment. Unknown settings: ignore (forward-compatible).
  - **`indexes { (a, b) [unique, name: '…'] \n  c [name: '…'] }`** → `IndexDef` (cols, `unique`,
    `name`); single bare column or parenthesized list.
  - **`Note: '…'`** (single) or **`Note: '''…'''`** (triple-quoted multiline) → table comment.
- **`Ref: "s"."t"."c" > "s"."t"."c" [delete: …, update: …]`** (standalone) → a `ForeignKey`
  constraint appended to the **source** table after all tables are parsed. Composite:
  `"s"."t".(c1, c2) > "s"."o".(c1, c2)`. Actions map cascade/restrict/set null/set default/no action.
  (Only the `>` many-to-one direction is emitted by dbd; accept `<` by swapping sides; `-`/`<>` →
  treat as a plain reference / skip the action.)
- **Identifiers**: accept quoted `"x"` and bare `x`; **schema-qualified** `"schema"."name"` and
  **unqualified** `name` (→ default schema `public`). **Synthesize a `Schema` entity** for each
  distinct non-`public` schema so the generated project is applyable.
- **`TableGroup … { }`** and stray top-level `Note` → ignored.
- Errors: a clear `DbdError` with the offending construct (no panic); unknown blocks → skip with
  no error (lenient, dbdiagram.io adds blocks over time).

## Emit / engine wiring

No emitter changes — parsed entities are `Table`/`Enum`/`Schema`, all already handled by
`emit_entity`. The CLI reads the file, calls `parse_dbml`, filters by schema selection, then runs
the existing init/merge path with those entities (no adapter).

## Inherent limitations (documented)

DBML represents only enums + tables + FK refs. `--from-dbml` therefore produces **schemas + enums
+ tables + foreign keys** only — no functions, procedures, views, standalone sequences, roles, or
check constraints (DBML cannot express them). `serial`/identity survive because dbd's DBML carries
`bigserial`/`[increment]`.

## Testing

- **Parser unit tests**: project, enum, table (each column setting), indexes block (unique +
  named, bare + parenthesized), multi-line `'''` note, standalone Ref (simple + composite + each
  action), unqualified vs schema-qualified names, schema synthesis, lenient skip of unknown blocks.
- **Round-trip test**: a `Vec<Entity>` → `dbml::generate_dbml` → `parse_dbml` → structural equality
  (the strongest guard — exporter and parser are inverses for the supported subset).
- **Live validation**: `dbd dbml` on sensei → `parse_dbml` the output → the parsed table/enum/FK
  set matches sensei's introspected entities (modulo the unrepresentable kinds).

## Documentation (all surfaces — required this cycle)

1. **`docs/guide/04-commands.md`** — `--from-dbml` on `init`/`merge` + the DBML-source limitations.
2. **`docs/llms/llms-full.txt` + `docs/llms/llms.txt`** — **backfill the entire reverse-engineer
   story** (currently absent): `init --from-db`, `merge`, `--roles`, sequences, functions, version
   safety, and `--from-dbml`. (These drifted across v0.5–v0.7; bring them current.)
3. **New guide page `docs/guide/06-reverse-engineering.md`** — a dedicated guide with a
   **greenfield vs brownfield workflow diagram** (inline SVG; the guide renderer `@html`s markdown):
   - **Greenfield**: `dbd init` (scaffold) → edit DDL → `dbd apply`.
   - **Brownfield**: existing DB → `dbd init --from-db <conn>` (or `--from-dbml <file>`) → review →
     `dbd apply`; and ongoing `dbd merge` to sync DB drift (with the version-safety gate).
   The site auto-publishes it at `/guide/reverse-engineering` (pages enumerate from
   `docs/guide/*.md`; `site/src/lib/content/` is gitignored + regenerated by `copy-content.mjs`).

## Out of scope

- Emitting DBML constructs dbd doesn't model (sticky notes, `TableGroup` settings, relationship
  cardinality beyond FK direction).
- The other sources (SQLite `pragma`, Convex) remain future patches.
