# Reverse-engineer functions & procedures — design

- **Date:** 2026-06-16
- **Status:** approved (design)
- **Target release:** minor (v0.5.1 → v0.6.0)
- **Builds on:** `2026-06-15-reverse-engineer-design.md` (the reverse engine, version-safety gate, snapshot unification)

## Goal

Extend `dbd init --from-db` / `dbd merge` to reverse-engineer **functions and procedures**
(full bodies), the largest remaining gap in the data-model coverage. Roles and sequences
remain separate later patches (roles are cluster-global with heavy platform-role filtering;
sequences need a new `EntityType::Sequence` threaded through the model).

## Approach — capture the canonical definition verbatim

Unlike tables (no `pg_get_tabledef` exists, so we reconstruct), Postgres exposes
`pg_get_functiondef(oid)` which returns the complete, canonical
`CREATE OR REPLACE FUNCTION|PROCEDURE …` text. We capture that **verbatim** (lossless —
handles `LANGUAGE`, `SECURITY DEFINER`, `STRICT`, `SET`, arbitrary `$$` bodies in any
language) and store it on the entity. No reconstruction.

Emitted text is run through the existing `formatter` before writing (same pipeline as every
other reverse-engineered file → idempotent with `dbd format`). The formatter preserves
`$$ … $$` bodies verbatim, so only the surrounding `CREATE` clause is normalized; a body it
can't reparse falls back to the original text. Lossless either way.

## Introspection (new `pg_proc` query in `postgres.rs`)

```sql
SELECT n.nspname              AS schema,
       p.proname              AS name,
       p.prokind              AS kind,            -- 'f' function, 'p' procedure
       pg_get_functiondef(p.oid) AS definition
FROM pg_proc p
JOIN pg_namespace n ON n.oid = p.pronamespace
WHERE p.prokind IN ('f', 'p')                     -- skip aggregates ('a') / window ('w')
  AND <schema_filter on n.nspname>                -- reuse Self::schema_filter_column
  AND NOT EXISTS (                                -- exclude extension-owned routines
      SELECT 1 FROM pg_depend d
      WHERE d.objid = p.oid AND d.deptype = 'e'
  )
ORDER BY n.nspname, p.proname, p.prokind, p.oid   -- prokind keeps each (schema,name,kind) contiguous for grouping
```

- **Extension-owned routines excluded** (`pg_depend deptype = 'e'`) — e.g. `uuid-ossp`'s
  functions belong to the extension, not the project (mirrors the existing extension-object
  detection in `list_entities`).
- **`prokind` restricted to `f`/`p`**; aggregates/window functions are out of scope for v1.
- Schema selection / Supabase denylist apply exactly as for other kinds (a routine in a
  denied schema is dropped with its schema).

## Entity model + overload grouping

- Each row becomes part of an `Entity` with `entity_type = Function` (`prokind 'f'`) or
  `Procedure` (`prokind 'p'`), `schema = Some(nspname)`, `name = "<schema>.<proname>"`.
- **Overloads** (same `(schema, name, kind)`, different argument signatures) are **grouped
  into one entity** whose `writes` holds each overload's definition. This avoids
  `ddl/<kind>/<schema>/<name>.ddl` path collisions and keeps overloads together. (A name used
  by both a function and a procedure does not collide — different `ddl/function` vs
  `ddl/procedure` directories.)

## Emitter (`emit.rs`)

- `emit_routine(entity) -> String` joins `entity.writes` (one per overload) with `;\n\n`,
  ensuring each statement ends in a single `;` (`pg_get_functiondef` output omits the
  trailing semicolon).
- Wire `EntityType::Function | EntityType::Procedure` into `emit_entity` → `emit_routine`.

## Reverse-engine wiring (`reverse.rs`)

- Add `Function` and `Procedure` to `MANAGED_KINDS` → `merge` now manages them
  (orphan detection covers stale `ddl/function|procedure/**` files under selected schemas;
  still reported, never deleted).
- `entity_path` already handles them (`has_schema()` is true →
  `ddl/function/<schema>/<name>.ddl`, `ddl/procedure/<schema>/<name>.ddl`).
- No ordering work: `design.rs` apply order already places functions/procedures after views.

## Testing

- **Emit unit** — a `Function` entity with two `writes` (overloads) → `emit_routine` returns
  both, each `;`-terminated, separated by a blank line.
- **Embedded** (feature `embedded-tests`) — create in a `revtest`-style schema: a plain
  function, a procedure, an **overloaded** function (two signatures), and a function provided
  by an extension. Introspect and assert: the function + procedure + both overloads (grouped
  into one entity, two `writes`) are captured; the extension function is **excluded**;
  `prokind` maps to the right entity type.
- **Round-trip apply** — emit the captured routines and `execute_script` them into a fresh
  schema on embedded Postgres → succeeds (proves the emitted DDL is valid, like the existing
  index-DDL apply test).

## Edge cases / notes

- `pg_get_functiondef` is valid for both functions and procedures (PG ≥ 11). Aggregates and
  window functions (`prokind a/w`) error under it and are excluded by the `prokind` filter.
- Default-argument expressions, `OUT`/`INOUT` params, polymorphic types, and non-SQL
  languages (`plpgsql`, `plpython`, …) are all preserved because the body is captured
  verbatim.
- Trigger functions are ordinary `prokind 'f'` functions and are captured; the triggers
  themselves are not a managed kind in v1 (no `CREATE TRIGGER` reverse yet — future).

## Out of scope (future patches)

- **Roles** (cluster-global; platform-role filtering) — next patch.
- **Sequences** (needs `EntityType::Sequence`) — following patch.
- **Triggers**, event triggers, aggregates, operators, domains, composite types.
