# Entity-level `dbd reset` + opt-in schema/extension drops — design

- **Date:** 2026-06-17
- **Status:** approved (design)
- **Target release:** minor (new flags) — v0.8.0

## Problem

`dbd reset` is **schema-granular** (`DROP SCHEMA … CASCADE` per managed schema). Two issues:

1. **Aborts on `public`.** `script::build_reset_script` returns `Err` if any managed schema is in
   `SYSTEM_PROTECTED` (`pg_catalog`, `information_schema`, `pg_toast`, **`public`**), and
   `reset()` propagates it with `?` — so a project with entities in `public` (common) makes
   `dbd reset --force` abort and drop **nothing**. `--force` overrides the prod/migrations guards
   but never reaches this one.
2. **Coarse + churns extensions.** `DROP SCHEMA … CASCADE` would nuke everything in the schema,
   including extensions (e.g. pgvector) installed there.

`public` is only special on **Supabase** (PostgREST-exposed/managed); on plain Postgres it's a
normal user schema.

## Design — entity-level reset

Default `dbd reset` drops the project's **own managed objects individually**, never `DROP SCHEMA`,
never `DROP EXTENSION`. Opt-in flags extend it.

### Default behavior
- Drop the data-model entities in **reverse dependency order** (dependents first): functions /
  procedures → views → tables → sequences → enums. (Roles still dropped on a full reset only.)
- **Tables / views / enums / sequences:** `DROP TABLE|VIEW|TYPE|SEQUENCE IF EXISTS "s"."n" CASCADE;`
- **Functions / procedures** (overloads need signatures): one `DO $$ … $$` block per `schema.name`
  that drops every overload via `regprocedure` + `DROP ROUTINE` (handles functions *and*
  procedures, PG ≥ 11), scoped to that exact `schema.name` so nothing else is touched:
  ```sql
  DO $$ DECLARE r record; BEGIN
    FOR r IN SELECT p.oid::regprocedure AS sig FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = '<schema>' AND p.proname = '<name>'
    LOOP EXECUTE format('DROP ROUTINE IF EXISTS %s CASCADE', r.sig); END LOOP;
  END $$;
  ```
  (`<schema>`/`<name>` are single-quote-escaped.)
- **No `DROP SCHEMA`, no `DROP EXTENSION`** → `public`, custom schemas (`config`/`staging`), the
  `extensions` schema, and all extensions survive. Empty custom schemas linger harmlessly; the
  next `dbd apply` repopulates them.
- **No abort on `public`** — the schema-protected guard becomes moot for the default path (we drop
  entities, not schemas). Reset only ever touches entities the project declares, so it inherently
  never affects Supabase `auth.*` or extension objects.

### `--schemas`
Additionally `DROP SCHEMA IF EXISTS "s" CASCADE;` for the project's managed schemas, **skipping**:
- Always: `pg_catalog`, `information_schema`, `pg_toast` (true system).
- When `target == "supabase"`: the full `SUPABASE_PROTECTED` set (`auth`, `storage`, …, **`public`**).
- On a `postgres` target, `public` **is** dropped by `--schemas` (it's the project's schema).
Skipped protected schemas are silently left (no abort) — their entities were still entity-dropped.

### `--extensions`
Additionally `DROP EXTENSION IF EXISTS "e" CASCADE;` for each extension in the active target's
`extensions` config (by bare name).

### `--clean`
Shorthand for `--schemas --extensions`.

### Unchanged
- The prod / applied-migrations guards in `reset()` (overridable by `--force`).
- Scope handling: `--scope` still restricts which entities/schemas the reset targets (entity set
  now filtered to the scope's working set, same as schemas were).
- `--dry-run` prints the generated script and exits.
- `clear_project_migrations()` after the drops.

## CLI

`Reset` clap variant gains `schemas: bool`, `extensions: bool`, `clean: bool`
(`clean` implies the other two). Thread through `commands/mod.rs` dispatch → `cmd_reset` →
`Design::reset(...)`. `reset()` gains `drop_schemas: bool, drop_extensions: bool` params (the CLI
ORs `clean` into both).

## Implementation

- Replace `script::build_reset_script(user_schemas, roles, target, skip)` with an entity-aware
  builder, e.g. `build_reset_script(entities: &[&Entity], roles, extensions, target, opts)` where
  `opts = { drop_schemas, drop_extensions }`. It emits: entity DROPs (reverse order) → optional
  schema DROPs (filtered by the protected rules above) → optional extension DROPs → role DROPs.
- `Design::reset` selects the managed entities (scope-filtered), passes them + flags.
- Keep `reset_target_schemas` (scope→schemas) for the `--schemas` path.

## Testing

- **Default reset** (unit, `script.rs`): a project with `public.users` + `config.lookups` +
  a view + a function → script contains `DROP TABLE IF EXISTS "public"."users" CASCADE`, the view
  DROP, the function `DO $$ … DROP ROUTINE … $$` block, NO `DROP SCHEMA`, NO `DROP EXTENSION`, and
  does **not** error on `public`.
- **Ordering**: functions/views before tables before enums.
- **`--schemas` on postgres**: includes `DROP SCHEMA "config"`, `DROP SCHEMA "public"`; on
  **supabase**: includes `config`, excludes `public`/`auth`; always excludes `pg_catalog`.
- **`--extensions`**: `DROP EXTENSION IF EXISTS "vector" CASCADE`.
- **`--clean`**: schemas + extensions both present.
- **CLI parse**: `dbd reset`, `--schemas`, `--extensions`, `--clean`, `--force`, `--dry-run`.
- **Embedded** (feature `embedded-tests`): apply the fixture, `reset` (default) → tables gone,
  schema still exists; `reset --schemas` → schema gone. Re-`apply` works after a default reset.

## Docs (all three surfaces)

- `docs/guide/04-commands.md` `dbd reset`: entity-level default + `--schemas`/`--extensions`/
  `--clean`; note `public`/extensions survive by default.
- `docs/llms/llms.txt` + `llms-full.txt`: the reset behavior change + flags.
