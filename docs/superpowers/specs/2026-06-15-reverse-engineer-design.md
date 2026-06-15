# Reverse-engineer a dbd project from a database — design

- **Date:** 2026-06-15
- **Status:** approved (design); pending implementation plan
- **Target release:** minor (v0.4.11 → v0.5.0)

## Goal

Let a user generate a dbd project (a `design.yaml` + a `ddl/<kind>/<schema>/<name>.sql`
tree) from an existing database, instead of authoring DDL by hand. Two entry points:

- `dbd init --from-db <conn>` — create a **new** project from a DB.
- `dbd merge <conn>` — sync a DB into an **existing** project.

## Scope

**This cut (the v0.5.0 minor):**

- Sources: **Postgres + Supabase** (Supabase uses the Postgres adapter + a platform-schema denylist).
- Entity coverage (the "data model"): **schemas, extensions, enums, tables**
  (columns, defaults, identity, PK/FK/unique/check constraints, indexes, table +
  column comments), **views**.
- Commands: `init --from-db` and `merge`.

**Roadmap (out of scope here, each its own spec/release):**

- **Patches after v0.5.0:** functions & procedures (full bodies), roles, sequences.
- **Later phases (ordered):** DBML file source (needs a new DBML *parser* — today
  DBML is export-only) → SQLite (`pragma` introspection) → Convex (no SQL surface;
  different mechanism).

## Commands & flags

```
dbd init --from-db <conn> [--version N] [--name NAME]
         [--schema S]... [--exclude-schema S]... [--all-schemas]
         [--force-overwrite] [--dry-run]

dbd merge <conn>
         [--schema S]... [--exclude-schema S]... [--all-schemas]
         [--force-overwrite] [--dry-run]
```

- `<conn>` resolves from the flag value, then `$DATABASE_URL`. `init` cannot read a
  config yet, so connection is flag/env only (no config fallback).
- `--version N` — base `project.version` in the generated `design.yaml`. Default **1**.
  (`init` only; `merge` keeps the existing config and ignores it.)
- `--name NAME` — `project.name`. Default: the database name parsed from `<conn>`.
- `--schema` (repeatable) — limit to exactly these schemas.
- `--exclude-schema` (repeatable) — add to the exclusion set.
- `--all-schemas` — bypass the Supabase platform denylist (Postgres internals are
  still always excluded).
- `--force-overwrite` — on conflict, back up the existing file to `.bak` and write
  the new one (see Write-plan). Without it, conflicts abort the run.
- `--dry-run` — print the plan and exit; touch nothing.

## Architecture — one shared engine

```
source → introspect → Vec<Entity> → emit DDL → write-plan → apply / report
```

A new orchestration module in `dbd-core` (working name `reverse`) owns the pipeline.
`init` and `merge` are thin CLI wrappers over it:

- `init` runs with `write_config = true` (generates `design.yaml`, uses `--version`/`--name`).
- `merge` runs with `write_config = false` (keeps the existing `design.yaml`; requires the project to exist). `merge` **never mutates `design.yaml`** — if it writes files for a schema not present in the existing `schemas:` list, it **warns** ("schema `x` written but not listed in design.yaml; add it to include those files") rather than editing config.

Otherwise identical: same introspection, emitter, write-plan, conflict/orphan rules.

**DDL-emitter approach (the one real design choice):** we **reconstruct canonical
`CREATE …` text from the introspected `TableDef`/entity**, then run it through the
existing `formatter` for consistent style. Postgres has no built-in CREATE-statement
emitter, and shelling out to `pg_dump` is unreliable / often unavailable — so
capturing "raw" DDL isn't an option. Rejected: raw-DDL capture.

## Postgres / Supabase introspection (new adapter capability)

Add an `introspect()` method to the `DatabaseAdapter` trait (Postgres impl first),
returning `Vec<Entity>` built via `sqlx` catalog queries:

- **schemas** — `pg_namespace`, filtered (see Schema selection).
- **extensions** — `pg_extension` (+ the schema each is installed into).
- **enums** — `pg_type` (typtype = 'e') + `pg_enum` (ordered values).
- **tables** — `information_schema.columns` / `pg_attribute` for columns (type,
  nullability, default, identity/serial), `pg_constraint` for PK / FK (with on-delete
  action) / unique / check, `pg_index` + `pg_indexes` for indexes (columns, order,
  uniqueness, name), `pg_description` for table + column comments.
- **views** — `pg_views` (definition text).

Reuses the internal-schema filtering already present in `postgres.rs::list_entities()`.

### Schema selection

- **Always excluded** (both Postgres and Supabase): `pg_catalog`, `information_schema`,
  `pg_toast*`, `pg_temp*`.
- **Supabase platform denylist** (excluded by default, included with `--all-schemas`):
  `auth`, `storage`, `realtime`, `_realtime`, `extensions`, `graphql`, `graphql_public`,
  `vault`, `pgsodium`, `pgsodium_masks`, `supabase_functions`, `supabase_migrations`,
  `cron`, `net`, `pgbouncer`, `_analytics`, `_supavisor`, `pgtle`. Maintained as a
  named constant.
- `--schema` (allowlist) and `--exclude-schema` (extra denies) compose with the above:
  allowlist wins when present; otherwise everything not internal/denied is included.

## DDL emitter (new, `dbd-core`)

`emit_ddl(&Entity) -> String` for `Schema`, `Extension`, `Enum`, `Table`, `View` — the
inverse of the parser. Extends `script.rs::ddl_from_entity` (already handles
Schema/Extension/Role). Emits identifier-quoted, schema-qualified `CREATE` statements;
table emit covers columns/defaults/identity, table-level PK/FK/unique/check, `CREATE
INDEX` statements, and `COMMENT ON` for table + columns. Output is normalized via the
existing `formatter`.

## File layout, write-plan, reporting

- Paths follow the existing convention `ddl/<kind>/<schema>/<name>.sql`; `init` also
  writes `design.yaml`. Directory scaffolding reuses `init.rs`.
- `design.yaml` `target.<dialect>.url` is written as **`$DATABASE_URL`** (an env
  reference) — never the literal connection string, so no secrets land on disk.
- **Write-plan** (computed entirely before touching disk) classifies each target file:
  - **create** — no file at the path.
  - **skip** — file exists and is **byte-identical** to the generated content (makes
    re-runs idempotent).
  - **conflict** — file exists and differs.
  - **orphan** — an existing `.sql` of a **managed kind** (schema/extension/enum/table/
    view — the kinds this run generates) **under a selected schema**, with no
    corresponding DB entity. Files of unmanaged kinds (e.g. `ddl/function/**`) and files
    under non-selected schemas are never flagged.
- **Apply:**
  - If there are conflicts and **not** `--force-overwrite` → **abort**, print the
    conflict list, write nothing.
  - With `--force-overwrite` → rename each conflicting file to `<name>.sql.bak`
    (`.bak.1`, `.bak.2`, … on collision), then write the new file.
  - **Orphans are reported, never deleted** — the user handles deletes.
  - `--dry-run` prints the plan and exits before any writes.
- **Report:** `N created · M unchanged · K overwritten (.bak) · J orphans (left as-is)`,
  with the orphan paths listed.

## Errors / edge cases

- Connection failure → clear, actionable message (no stack dump).
- Zero schemas after filtering → warn and exit non-error (nothing to do).
- Unsupported / exotic column types → emit the raw type string verbatim (lossless
  passthrough; never silently drop a column).
- Reserved-word / mixed-case identifiers → always quote in the emitter.
- `.bak` name collisions → `.bak`, then `.bak.1`, `.bak.2`, …
- `init` in a dir that already has `design.yaml`/`ddl/` → refuse, suggest `merge`.
- `merge` in a dir with no project → refuse, suggest `init --from-db`.

## Testing

- **Emitter** — round-trip unit tests per entity kind: parse a fixture DDL → `TableDef`
  → `emit_ddl` → re-parse → assert structural equality. No DB required.
- **Write-plan / apply** — pure unit tests over a temp dir: create / skip (identical) /
  conflict (abort) / conflict (`--force-overwrite` → `.bak`) / orphan reporting /
  `--dry-run` (no writes).
- **Schema selection** — unit tests over the filter (internal always out; Supabase
  denylist; `--schema` / `--exclude-schema` / `--all-schemas`).
- **Introspection** — exercised against a real Postgres, gated the same way the
  existing adapter DB tests are (confirm the repo's current pattern when implementing;
  if they need a live DB they're `#[ignore]`/feature-gated, not run in the default
  `cargo test`).

## Key files / new modules

- New: `crates/dbd-core/src/reverse.rs` (engine: orchestration + write-plan + apply).
- New: DDL emitter — extend `crates/dbd-core/src/script.rs` (or a sibling `emit.rs`).
- Extend: `crates/dbd-core/src/adapter/mod.rs` (`introspect()` on the trait) +
  `adapter/postgres.rs` (impl + catalog queries).
- Extend: `crates/dbd-core/src/config.rs` (write a `design.yaml` from introspection) +
  reuse `init.rs` scaffolding.
- CLI: `crates/dbd-cli/src/cli.rs` (`--from-db`/`--version`/`--name`/schema/force/
  dry-run flags on `init`; new `merge` command), `commands/mod.rs` dispatch,
  `commands/` impl for both (thin wrappers over `reverse`).
- Reuse: `Entity`/`TableDef`/`ColumnDef`/`IndexDef`, the internal-schema filter in
  `list_entities()`, `get_adapter()`/`connect()`, the `formatter`.

## Risks / notes

- DDL-emitter fidelity is the main risk; the round-trip tests bound it, and the
  raw-type passthrough keeps it lossless for types we don't model.
- Introspection completeness (esp. constraint/index edge cases) — covered incrementally;
  the data-model scope keeps the surface bounded.
