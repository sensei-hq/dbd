# Materialized View Support Design

**Date:** 2026-08-03  |  **Status:** Draft
**Scope:** First-class `materialized_view` entity type — folder discovery, parser fixes, emission/introspection, and pg_cron–driven scheduled refresh (global default + per-view overrides) declared in `design.yaml`, plus an on-demand `dbd refresh` command.

---

## Overview

dbd already treats **views** as first-class entities: discovered from `ddl/view/<schema>/<name>.ddl`, introspected from `pg_views`, emitted as `CREATE VIEW … AS <body>`, and "reconciled by idempotent re-apply rather than diffing" (`reconcile.rs:71`). Materialized views are the natural sibling, but they differ in three ways that drive this design:

1. **They store data** — created with `WITH [NO] DATA`; a plain `CREATE OR REPLACE` cannot alter an existing one.
2. **They must be refreshed** — `REFRESH MATERIALIZED VIEW [CONCURRENTLY] <name>` recomputes the stored rows.
3. **They can carry indexes** — unlike plain views. A **unique** index is a hard prerequisite for `REFRESH … CONCURRENTLY`.

The refresh lifecycle is handled **in-database via pg_cron**, not by dbd polling or refreshing on every apply. dbd declaratively **owns** the cron jobs: a single shared schedule applies to all matviews, with optional per-view overrides, all declared in `design.yaml`. An on-demand `dbd refresh` command covers initial population and CI/manual runs.

### Non-goals (v1)

- **SQLite / Convex** matview support — no such concept. They error on apply exactly as `Function`/`Procedure` do today.
- **Refresh engines other than pg_cron** (external schedulers, dbd-side polling). pg_cron is the one mechanism.
- **Managing arbitrary user cron jobs** — dbd only creates/updates/removes jobs whose name carries the reserved `dbd:refresh:` prefix. Hand-authored cron jobs are never touched.
- **Incremental / partial refresh** — Postgres has no native incremental matview refresh; out of scope.
- **DBML rendering of matviews** as a distinct node shape — for v1 a matview renders like a view in `dbml`/`diagram` (can be revisited).

---

## Architecture

### 1. Entity model & discovery

Add `MaterializedView` to `EntityType` (`entity.rs`) and to `TYPES_WITH_SCHEMA` (schema-scoped, same path shape as views/tables):

```
ddl/materialized_view/<schema>/<name>.ddl   →   entity  <schema>.<name>
```

- `EntityType::from_folder_name` accepts `materialized_view` / `materialized_views`, plus the short alias `matview` / `matviews`.
- `tag()` returns `"materializedview"` today via the derive-based lowercasing; we override the tag/reverse-folder mapping so the on-disk folder and reverse-engineering output are `materialized_view` (consistent, readable). Reverse (`reverse.rs`) maps the type to the `materialized_view` folder.
- The `.ddl` file contains the `CREATE MATERIALIZED VIEW … AS <query>;` statement **plus** any `CREATE [UNIQUE] INDEX … ON <mv>(…);` statements — identical to how table DDL already carries trailing index statements.

The matview's **body** is carried in `entity.writes[0]` (same contract as views); its **indexes** are parsed into `entity.table_def.indexes` (reusing the existing `IndexDef` machinery). `table_def` for a matview carries indexes/comments only — columns are derived from the query, not declared.

### 2. Parser (`parser/`)

Two changes, both localized to `preprocess_sql` in `parser/mod.rs` and the table/index extraction in `parser/tables.rs`:

- **`COMMENT ON MATERIALIZED VIEW` fix.** The existing workaround regex (`parser/mod.rs:54`) strips `COMMENT ON (view|function|procedure|trigger|index|schema|extension|type)` but omits `materialized view`, so `COMMENT ON MATERIALIZED VIEW … IS '…'` falls through to sqlparser and is rejected (observed in the wild). Add `materialized\s+view` to the alternation.
- **`CREATE MATERIALIZED VIEW` parsing.** If sqlparser's PG dialect does not parse `CREATE MATERIALIZED VIEW`, add a preprocess rewrite `CREATE MATERIALIZED VIEW` → `CREATE VIEW` for **AST extraction only** (mirrors the existing `PROCEDURE → FUNCTION` workaround at `parser/mod.rs:61`). The real keyword is preserved for emission; only reads/writes/body extraction uses the rewritten form. The trailing `CREATE INDEX` statements parse unchanged into `IndexDef`s. *(Implementation task: verify sqlparser's actual behaviour first; skip the rewrite if it already parses.)*

### 3. Emission & introspection

- **Emit** (`emit.rs`): new `emit_matview(entity)` producing
  `CREATE MATERIALIZED VIEW "s"."n" AS <body> WITH DATA;`
  followed by the entity's index statements (reuse the table index emitter). Wire `EntityType::MaterializedView => Some(emit_matview(entity))` into `emit_entity`.
- **Introspect** (`adapter/postgres.rs`): new `introspect_matviews()` reading `pg_matviews` (schemaname, matviewname, definition) with indexes joined from `pg_indexes`, building `EntityType::MaterializedView` entities (definition → `writes[0]`, indexes → `table_def`). Add it to the introspection fan-in alongside `introspect_views()` (`postgres.rs:1355`). This gives `dbd merge` and `init --from-db` round-tripping. Remove/adjust any blanket `cron`-schema ignore only for dbd-owned refresh jobs (see §5) — user objects in `cron` stay ignored.

### 4. Apply / diff / reconcile

- **Apply order** (`design.rs:579`): insert matviews **after views**, before functions/procedures — a matview may read tables and views. The apply tuple/sort in `design.rs:582` grows one slot.
- **Clean apply**: `CREATE MATERIALIZED VIEW IF NOT EXISTS … WITH DATA;` + indexes.
- **Reconcile / drift**: a matview definition cannot be `CREATE OR REPLACE`d. On definition drift, reconcile does `DROP MATERIALIZED VIEW … CASCADE` + recreate (repopulates). Like views, matviews are otherwise re-applied idempotently rather than ALTER-diffed. Index drift is handled by the same drop/recreate.

### 5. Scheduled refresh via pg_cron

**Config** — new `design.yaml` block mirroring the established `import.options` + per-entity-override house style:

```yaml
materialized_views:
  options:
    refresh: "0 2 * * *"      # shared cron schedule for ALL matviews
    concurrently: true        # shared default
  views:
    analytics.top_products:
      refresh: "*/30 * * * *" # override just this one
    analytics.realtime:
      concurrently: false     # override just the concurrently flag
```

- A matview with **no** resolved schedule (no global `options.refresh` and no override) gets **no** cron job — it is create-only and refreshed manually via `dbd refresh`.
- Effective settings = `options` defaults overlaid by the per-view entry in `views:`.

**Ownership model** — dbd manages a cron job per scheduled matview, named with a reserved prefix so user jobs are never touched:

```
job name:   dbd:refresh:<schema>.<name>
command:    REFRESH MATERIALIZED VIEW [CONCURRENTLY] "<schema>"."<name>"
schedule:   <resolved cron expression>
```

On `apply` / `reconcile`, dbd **syncs** these jobs against the declared set:
- **create** a job for a newly scheduled matview,
- **update** (`cron.unschedule` + `cron.schedule`, or `cron.alter_job`) when schedule/concurrently/name changes,
- **unschedule** jobs whose `dbd:refresh:` name no longer corresponds to a scheduled matview.
Jobs without the `dbd:refresh:` prefix are ignored entirely.

**Prerequisites & validation** (`inspect`):
- If any matview resolves a schedule but `pg_cron` is not listed in `target.postgres.extensions`, `inspect` **errors** with a clear message (matview *definitions* still work without pg_cron; only scheduling needs it).
- If `concurrently: true` (resolved) but the matview declares **no unique index**, `inspect` **errors** (`REFRESH … CONCURRENTLY` requires one).
- Invalid cron expressions are reported by `inspect` (basic 5-field validation).

### 6. On-demand refresh command

`dbd refresh [entity]`:
- No arg → refresh **all** matviews, in dependency order.
- With an entity name (or `schema.*` wildcard, matching existing scope/selection conventions) → refresh that subset.
- Emits `REFRESH MATERIALIZED VIEW [CONCURRENTLY] <name>` honoring each view's resolved `concurrently` setting.
- Rationale: needed for the **first population** after an initial data load, and for CI / manual recompute independent of the pg_cron schedule.

### 7. Targets

| Target | Behaviour |
|--------|-----------|
| PostgreSQL | Full: define, apply, introspect, schedule (pg_cron extension required for scheduling), `dbd refresh`. |
| Supabase | Same as Postgres; pg_cron is available. |
| SQLite | No matviews — error on apply, like `Function`/`Procedure` today. `dbd refresh` is a no-op/error. |
| Convex | Codegen target — no matviews. Error on apply as with other unsupported entity kinds. |

---

## Behavioural decisions (confirmed)

- **`WITH DATA` on create** (populate immediately) rather than `WITH NO DATA`. Makes the matview usable right after `apply` and satisfies the "must be populated before first `CONCURRENTLY` refresh" rule, at the cost of computing the query once during `apply`.
- **`dbd refresh` command is in scope** (§6).
- **Schedule lives in `design.yaml`** (not a DDL header directive) — keeps `.ddl` files pure, portable SQL and matches the existing config surface.
- **Global shared schedule + per-view overrides** (§5).

---

## Touchpoints (files)

- `entity.rs` — `EntityType::MaterializedView`, `TYPES_WITH_SCHEMA`, `from_folder_name`, tag/folder mapping.
- `parser/mod.rs` — `COMMENT ON MATERIALIZED VIEW` regex; optional `CREATE MATERIALIZED VIEW → CREATE VIEW` extraction rewrite.
- `parser/tables.rs` — reuse index extraction for matview index statements.
- `emit.rs` — `emit_matview`, wire into `emit_entity`.
- `adapter/postgres.rs` — `introspect_matviews`, cron-job sync (create/update/unschedule).
- `adapter/sqlite.rs`, `adapter/convex.rs` — error on matview apply.
- `design.rs` — apply-order slot; load & resolve `materialized_views` config (options + overrides).
- `config.rs` — `materialized_views` config structs (serde).
- `reconcile.rs` — drop+recreate on matview definition/index drift; run cron sync.
- `reverse.rs` — map type to `materialized_view` folder; keep user `cron` objects ignored.
- `scope.rs` — include matviews in scope resolution/wildcards.
- `dbd-cli` — new `refresh` subcommand; `inspect` validations (pg_cron present, unique index for concurrently, cron syntax).
- `snapshot.rs` — decide whether matviews participate in snapshots (follow views' current treatment).

---

## Testing

Every change ships with a test (house rule). Coverage targets:

- **entity/discovery**: `from_folder_name` accepts `materialized_view`/`matview`; `from_file` resolves `ddl/materialized_view/analytics/daily_sales.ddl` → `analytics.daily_sales`.
- **parser**: `COMMENT ON MATERIALIZED VIEW … IS '…'` now parses; `CREATE MATERIALIZED VIEW … AS … ; CREATE UNIQUE INDEX …` yields body in `writes[0]` and one `IndexDef`.
- **emit**: `emit_matview` renders `CREATE MATERIALIZED VIEW "s"."n" AS … WITH DATA;` + index.
- **config**: `materialized_views` options + overrides resolve correctly (global applies; override wins; no-schedule → no job).
- **validation** (`inspect`): errors when concurrently+no-unique-index; errors when scheduled+no pg_cron extension; flags bad cron expression.
- **cron sync** (adapter/embedded PG): apply creates `dbd:refresh:<name>` job; changing schedule updates it; removing schedule unschedules it; a non-`dbd:refresh:` job is untouched.
- **refresh command**: `dbd refresh` all vs single vs wildcard emits correct `REFRESH … [CONCURRENTLY] …`.
- **round-trip**: introspect a matview (with a unique index) and re-emit produces equivalent DDL (`dbd merge` / `init --from-db`).
- **targets**: matview apply errors cleanly on SQLite and Convex.
```
