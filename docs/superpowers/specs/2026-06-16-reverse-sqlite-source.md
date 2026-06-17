# Reverse-engineer from SQLite — design

- **Date:** 2026-06-16
- **Status:** approved (design)
- **Target release:** patch v0.7.2
- **Builds on:** the reverse engine + the existing `SqliteAdapter`

## Goal

`dbd init --from-db sqlite://app.db` / `dbd merge --from-db sqlite://app.db` reverse-engineer a
SQLite database. SQLite has no schemas/enums/functions/sequences/roles, but `sqlite_master.sql`
stores the **verbatim** `CREATE` text for every object — so capture is lossless and simple.

## Scope (what SQLite has)

Captured: **tables**, **views**, and **user indexes** — all verbatim from `sqlite_master.sql`
(preserves CHECK constraints, type affinity, `AUTOINCREMENT`, `WITHOUT ROWID`, etc.). **Triggers
are skipped** (dbd has no `EntityType::Trigger`; documented limitation, consistent with Postgres
triggers also not captured). No schemas/enums/functions/sequences/roles (SQLite has none).

## CLI / engine wiring

- **Compile SQLite into the CLI**: add `"sqlite"` to `crates/dbd-cli/Cargo.toml`'s `dbd-core`
  features (`features = ["postgres", "sqlite"]`). `dbd_core::connect` already routes
  `sqlite://`/`file:`/`sqlite::memory:` → `SqliteAdapter`.
- **Dialect from the URL scheme** (not `--target`): the reverse handler picks the `design.yaml`
  target dialect from the connection — `sqlite:`/`file:` → `"sqlite"`, else `"postgres"`. Add a
  small helper `fn dialect_for_conn(conn: &str) -> &str`. The generated `design.yaml` gets a
  `sqlite` target with `url: $DATABASE_URL`. **Remove the old `--target sqlite` rejection** in
  `cmd_init_from_db` (SQLite is now a real source; dialect comes from the URL).

## Introspection — `SqliteAdapter::introspect()`

Override the trait method (currently the default `Err(unsupported)`):
- **Tables**: `SELECT name, sql FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'
  AND name NOT IN ('_dbd_meta','_dbd_migrations') ORDER BY name`. For each, also gather its user
  indexes: `SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL
  ORDER BY name` (auto-indexes from PK/UNIQUE have `sql IS NULL` and ride inside the table DDL —
  skip them). Build `Entity::new(Table, name)` with `schema=None` and the **verbatim DDL** =
  the table's `sql` + each index `sql`, joined with `;\n\n`, stored in the new `raw_ddl` field.
- **Views**: `type='view'` → `Entity::new(View, name)`, `schema=None`, `raw_ddl` = the view's `sql`.
- Errors → `DbdError`, no panic. `sql` is `NULL` only for internal/auto objects we already exclude.

## Verbatim emit — new `Entity.raw_ddl`

Add `pub raw_ddl: Option<String>` to `Entity` (`#[serde(default, skip_serializing_if = "Option::is_none")]`
— grep existing `Entity` constructors; it defaults to `None` everywhere, Postgres/DBML paths never
set it). In `emit::emit_entity`, **before** the kind dispatch: `if let Some(raw) = &entity.raw_ddl
{ return Some(raw.trim_end().trim_end_matches(';').to_string() + ";") }` (or join-aware if it
already holds multiple `;`-separated statements — emit verbatim, ensuring a trailing `;`). This is
a general "exact DDL provided" path; the Postgres/DBML reconstructing emitters are untouched.
`entity_path` already yields flat `ddl/table/<name>.ddl` / `ddl/view/<name>.ddl` when `schema=None`.

## Schema-less handling (required — SQLite entities have `schema=None`)

The reverse run derives `db_schemas` from `entity.schema` and bails "No user schemas … nothing to
do" when empty — which would break SQLite (all `schema=None`). Fix in the CLI run/plan path:
- The "nothing to do" guard fires only when there are **no entities at all**, not when there are
  no schemas.
- `select_and_keep`: schema-less entities (`schema=None`) are always kept (they already pass
  `owning.is_none_or(...)`); for the SQLite case `selected_schemas` is empty and that's fine.
- `design_yaml`: emit an empty `schemas:` list (or omit it) for a schema-less source.
- `plan_from_entities` orphan scan walks `ddl/<kind>/<schema>/` per selected schema; with no
  schemas it simply finds no orphans — acceptable v1 limitation for SQLite (note in docs).

## Version safety — `SqliteAdapter::reverse_managed_version()`

Override the default: check `sqlite_master` for a `_dbd_meta` table; if present read
`SELECT version FROM _dbd_meta WHERE project=?1` (project = `self.project`) → `Some(version)` (0 if
no row); else `None` (foreign). So the managed-DB gate (init-refuse / merge D<Y refuse / D≥Y
snapshot) works for SQLite exactly as for Postgres. (SQLite `_dbd_meta` is in the single namespace,
so no cross-schema lookup needed.)

## Testing

- **Adapter introspection** (a `#[cfg(test)]` test using an in-memory/temp SQLite DB via the
  existing sqlite test harness): create a table (with PK, FK, a CHECK, an explicit index), a view,
  and a trigger; assert `introspect()` returns the table (raw_ddl contains the CREATE TABLE + the
  user index, NOT the auto PK index), the view, and NOT the trigger / NOT `_dbd_meta`.
- **emit verbatim unit**: an `Entity` with `raw_ddl: Some("CREATE TABLE x(a)")` → `emit_entity`
  returns it verbatim (`;`-terminated), regardless of kind; an entity without `raw_ddl` still uses
  the structured emitter.
- **reverse_managed_version**: fresh SQLite DB → `None`; after creating `_dbd_meta` with a row →
  `Some(version)`.
- **End-to-end CLI** (manual + a test if feasible): `dbd init --from-db sqlite://<tmp>.db` on a
  seeded SQLite DB → generates `ddl/table/*.ddl` + `ddl/view/*.ddl` (flat, no schema dirs) + a
  `sqlite`-target `design.yaml` + baseline snapshot; `dbd inspect` on the result is clean.

## Documentation (all three surfaces — required)

1. `docs/guide/04-commands.md`: note `init --from-db`/`merge` accept a `sqlite://`/`file:` URL;
   SQLite captures tables + views + indexes (verbatim), no schemas/enums/functions/etc., triggers
   skipped.
2. `docs/llms/llms.txt` + `llms-full.txt`: add SQLite to the reverse-engineer source list + the
   captured/not-captured note.
3. `docs/guide/06-reverse-engineering.md`: add SQLite to the brownfield sources (a line/box —
   keep the SVG diagram coherent; a short prose subsection "Brownfield — from SQLite").

## Out of scope

- Triggers, and any structured (pragma-based) modeling — verbatim is lossless and sufficient.
- Convex remains the final planned source.
