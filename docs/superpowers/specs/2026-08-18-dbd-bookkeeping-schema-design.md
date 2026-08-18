# dbd Bookkeeping Schema Design

**Date:** 2026-08-18
**Status:** Approved
**Scope:** Move Postgres bookkeeping (`_dbd_meta` / `_dbd_migrations`) out of `public` into a dedicated `dbd` schema, renamed `dbd.meta` / `dbd.migrations`. Heal existing databases automatically on the next write. Extract the bookkeeping logic into an isolated `Bookkeeping` unit. SQLite/Convex are unchanged this cycle.

---

## Overview

dbd's own version bookkeeping currently lives in `public._dbd_meta` and `public._dbd_migrations` on Postgres/Supabase. Because they sit in `public`, they are **exposed by PostgREST** (whose default `db-schemas` is `public`) and are swept up by Supabase's "auto-enable RLS on new public tables" behaviour — which then makes the platform linter flag `rls_enabled_no_policy` on dbd-internal tables. The current mitigation (uncommitted, `postgres.rs:281-297`) is to attach a **deny-all RLS policy** to each bookkeeping table so the linter is satisfied.

A cleaner fix removes the problem at the root: put these tables in a schema PostgREST never exposes. A dedicated `dbd` schema is not in PostgREST's default `db-schemas`, is not in Supabase's exposed set (`public`, `graphql_public`, `storage`, …), and is not targeted by the auto-enable-RLS trigger (which fires on `public`). So the tables become **unreachable via the API and never linted** — and the deny-all policy is **deleted**, not maintained.

With a dedicated schema doing the namespacing, the `_dbd_` prefix is redundant, so the tables are renamed to the clean `dbd.meta` / `dbd.migrations`.

This is a **Postgres-only** change. SQLite has no schemas — its single namespace makes the `_dbd_` prefix *the* collision guard and the basis of its reverse-scan exclusion — so it keeps `_dbd_meta` / `_dbd_migrations`. Convex has no bookkeeping tables at all (a JSON sidecar next to `schema.ts` holds migrations + meta), so it is untouched. The `DatabaseAdapter` trait already hides *where* bookkeeping lives, so no non-Postgres caller changes.

### Model: heal-first, then act

Every operation that owns the database — `apply`, `deploy`, `reconcile`, `reset`, `migrate` — runs a single **`heal_bookkeeping()`** step **before any version read or guard**. On Postgres, heal:

1. Creates the `dbd` schema and `dbd.meta` / `dbd.migrations` if absent.
2. Folds **all** legacy copies found in **any** schema — the canonical `public._dbd_meta` *and* any stray copy a scoped apply leaked into another schema (e.g. `dojo._dbd_meta`) — into the new tables.
3. Drops every legacy copy.

All in one transaction, fully idempotent (a fresh DB yields empty `dbd.*`; an already-migrated DB is a no-op). Because heal runs first, **every subsequent read and write in the operation targets `dbd.meta` / `dbd.migrations` directly** — no catalog lookup, no dual-name resolution. The scoped-leak resolution that today smears `bookkeeping_schema` across every read collapses into this one up-front consolidation.

### Where heal-first cannot reach

Two paths are **read-only by contract and must never mutate the database**, so they cannot heal:

- `init --from-db` — detecting a managed DB in order to *refuse* (dbd is not taking ownership).
- `merge` — reading the applied version to choose reverse-vs-snapshot before deciding to proceed.

Both go through `reverse_managed_version`. These retain a **small both-names awareness**: they recognise `dbd.meta` *or* legacy `_dbd_meta` (in any schema) as "managed". This is the entire residual of the old dual-name logic — confined to one read-only detection function instead of spread across every read. If `merge` proceeds into reconcile, that is an ownership op → heal runs → new tables.

---

## Changes

### 1. New isolated `Bookkeeping` unit (Postgres)

Split `adapter/postgres.rs` (~2000 lines) into `adapter/postgres/mod.rs` + `adapter/postgres/bookkeeping.rs`. All schema/table names, the heal, the fetches, and the writes live in one cohesive struct:

```rust
// adapter/postgres/bookkeeping.rs
pub(super) struct Bookkeeping {
    pool: PgPool,       // cheap Arc clone of the adapter's pool
    project: String,
}

impl Bookkeeping {
    const SCHEMA: &str = "dbd";
    const META: &str = "meta";
    const MIGRATIONS: &str = "migrations";
    const LEGACY_META: &str = "_dbd_meta";
    const LEGACY_MIGRATIONS: &str = "_dbd_migrations";

    // Write path — assumes it may run against a legacy DB; consolidates then owns dbd.*
    async fn heal(&self) -> Result<()>;

    // Post-heal reads/writes — target dbd.meta / dbd.migrations directly
    async fn version(&self) -> Result<u32>;
    async fn get_meta(&self) -> Result<Option<ProjectMeta>>;
    async fn set_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()>;
    async fn record_migration(&self, tx: Option<&mut PgTx<'_>>, v: u32, desc: &str, sum: &str) -> Result<()>;
    async fn clear_migrations(&self) -> Result<()>;

    // Read-only detection — both-names aware, never mutates (init / merge gate)
    async fn detect_managed_version(&self) -> Result<Option<u32>>;
}
```

`PostgresAdapter` gains a `bookkeeping: Bookkeeping` field, built in `new()` from a pool clone (`PgPool` is `Arc`-backed; clone is cheap) and the project name. Every bookkeeping trait method collapses to a one-line delegation. The scattered `bookkeeping_schema` and `ensure_public_bookkeeping` helpers are **deleted** — their catalog dance is absorbed into `heal()` (write path) and `detect_managed_version()` (read-only path). `record_migration` takes the optional batch transaction so migration recording stays atomic with the DDL apply (today's `self.batch` routing).

**Forward-compatibility (tracked debt).** This cycle only Postgres gets an isolated unit; SQLite keeps its bookkeeping inline and Convex keeps its sidecar `State`. That is a deliberate, temporary divergence — "two different ways will bite us later". To keep next cycle's consolidation mechanical, the `Bookkeeping` method set above is chosen to **be** the future `BookkeepingStore` trait surface (`heal` / `version` / `get_meta` / `set_meta` / `record_migration` / `clear_migrations` / `detect_managed_version`). Next cycle: lift that as a trait, move SQLite's inline logic and Convex's sidecar behind it. Recorded in Open/Deferred so it is not lost.

### 2. Trait: replace two `ensure_*` with one `heal_bookkeeping()`

`adapter/mod.rs` currently declares `ensure_meta_table()` and `ensure_migrations_table()` (lines 229, 249). Replace both with a single:

```rust
/// Ensure bookkeeping storage exists and is at the current layout, healing any
/// legacy layout in place. Idempotent; safe to call at the start of every
/// ownership operation.
async fn heal_bookkeeping(&self) -> Result<()>;
```

Per-adapter impl:

- **Postgres** → `self.bookkeeping.heal()` (schema move + fold + drop; §3).
- **SQLite** → today's `ensure_meta_table` + `ensure_migrations_table` bodies, unchanged (creates/keeps `_dbd_meta` / `_dbd_migrations`).
- **Convex** → no-op (the sidecar is created on first write).
- **Mock** (`mock.rs`) → no-op.

### 3. Postgres `heal()` — the migration itself

Run in one transaction:

```sql
CREATE SCHEMA IF NOT EXISTS dbd;

CREATE TABLE IF NOT EXISTS dbd.meta (
    project     varchar NOT NULL PRIMARY KEY,
    env         varchar NOT NULL DEFAULT 'dev',
    version     integer NOT NULL DEFAULT 0,
    scope       varchar,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS dbd.migrations (
    project     varchar NOT NULL,
    version     integer NOT NULL,
    applied_at  timestamptz NOT NULL DEFAULT now(),
    description text,
    checksum    text,
    PRIMARY KEY (project, version)
);
```

Then, for every legacy copy discovered via the catalog (all schemas, `relname IN ('_dbd_meta','_dbd_migrations')`):

- **`_dbd_meta` → `dbd.meta`:** normalise shape first (`ALTER TABLE <s>._dbd_meta ADD COLUMN IF NOT EXISTS scope varchar` — legacy pre-scope-guard tables lack it), then
  `INSERT INTO dbd.meta (project, env, version, scope, created_at, updated_at) SELECT … FROM <s>._dbd_meta ON CONFLICT (project) DO NOTHING`.
  **Preference on conflict:** apply copies in canonical order — `public` first, then others — so the canonical row wins, matching today's `bookkeeping_schema` `ORDER BY (nspname='public') DESC`. Semantics do not shift.
- **`_dbd_migrations` → `dbd.migrations`:** `INSERT … SELECT … FROM <s>._dbd_migrations ON CONFLICT (project, version) DO NOTHING` — the composite PK makes unioning every copy safe and complete.
- **Drop:** `DROP TABLE IF EXISTS <s>._dbd_meta` / `<s>._dbd_migrations` for every legacy copy found.

Rename-in-place (`ALTER TABLE … SET SCHEMA dbd; … RENAME TO meta`) is rejected: it cannot fold **two** stray copies into one target (the second collides). The `INSERT … SELECT` + `DROP` path is uniform and handles the multi-copy leak.

**Concurrency:** each statement is individually idempotent; the `ALTER`/`DROP` take `ACCESS EXCLUSIVE` locks, serialising concurrent dbd runs naturally. dbd runs are effectively serial per DB, so no advisory lock is added.

### 4. Post-heal reads/writes target `dbd.*` directly

Because `heal_bookkeeping()` runs first in every ownership op, these simplify (no `bookkeeping_schema` catalog lookup, no legacy fallback):

- `get_db_version` → `SELECT version FROM dbd.meta WHERE project = $1`.
- `get_project_meta` → `SELECT … FROM dbd.meta WHERE project = $1` (the `42703` legacy-no-`scope` fallback is no longer needed — heal guarantees the column exists).
- `set_project_meta` → UPSERT into `dbd.meta`.
- `apply_migration` → `INSERT INTO dbd.migrations …` (via the batch tx).
- `clear_project_migrations` → `DELETE FROM dbd.migrations WHERE project = $1`.

Heal call sites (audit every caller of the removed `ensure_*` and every pre-write meta read):

| Op | Today | After |
|----|-------|-------|
| `apply` | `get_db_version` (`apply.rs:44,61`), `ensure_migrations_table` (`apply.rs:80`) | `heal_bookkeeping()` at the top of `apply`, before line 44; drop the line-80 ensure |
| `reconcile` | `ensure_meta_table` (`reconcile.rs:306`) | `heal_bookkeeping()` at the top of `reconcile` |
| `reset` | `get_project_meta` (`reset.rs:43`) | `heal_bookkeeping()` before the meta read |
| `deploy` | delegates to `apply` | covered by `apply`'s heal |
| `migrate` | reads version | `heal_bookkeeping()` at entry |

### 5. Read-only detection keeps both-names awareness

`reverse_managed_version` (used by `init --from-db` and `merge` to detect a managed DB without mutating it) delegates to `Bookkeeping::detect_managed_version()`, which resolves `dbd.meta` **or** legacy `_dbd_meta` in any schema (prefer `dbd.meta`) and reads the project's version. No write. This preserves the `init` refuse-on-managed gate (`commands/reverse.rs:97`) and `merge`'s path choice across the transition.

### 6. Keep `dbd` invisible: reverse-scan + reset exclusions

- **Reverse/merge/DBML/diagram:** add `"dbd"` to `reverse::ALWAYS_EXCLUDED` (`reverse.rs:6`) and keep `postgres.rs::schema_filter_column` in sync (it references that constant). This stops the whole `dbd` schema — not just the two table names — from being reverse-engineered into a project. (The existing `table_name NOT IN ('_dbd_meta','_dbd_migrations')` filter at `postgres.rs:518` becomes redundant but stays as belt-and-suspenders.)
- **Reset:** add `"dbd"` to `script.rs::ALWAYS_PROTECTED` (currently `pg_catalog`, `information_schema`, `pg_toast`). dbd's own bookkeeping schema must never be dropped by `reset --schemas` / `--clean`, on any target.

### 7. Revert the deny-all RLS policy

Delete the uncommitted deny-all block in `ensure_public_bookkeeping` (`postgres.rs:281-297`). It exists solely to satisfy the Supabase linter on `public` bookkeeping tables; once they live in the unexposed `dbd` schema, they are neither exposed nor linted, so the policy is obsolete. `ensure_public_bookkeeping` itself is deleted with the rest of the old helper set (§1).

### 8. Docs

Update references to `_dbd_meta` / `_dbd_migrations` that describe the Postgres layout: `docs/llms/llms-full.txt` (e.g. lines 378, 821-823, 875), `docs/llms/llms.txt`, and the dbd skill. Clarify the split: Postgres → `dbd.meta` / `dbd.migrations`; SQLite → `_dbd_meta` / `_dbd_migrations`; Convex → sidecar. Note the `dbd` schema is dbd-internal, unexposed, and excluded from reverse/reset.

---

## Files Modified

| File | Change |
|------|--------|
| `crates/dbd-core/src/adapter/postgres.rs` → `postgres/mod.rs` | Convert to a module dir; delegate bookkeeping methods to `self.bookkeeping`; delete `bookkeeping_schema` / `ensure_public_bookkeeping` and the deny-all block; add `bookkeeping` field + `new()` init |
| `crates/dbd-core/src/adapter/postgres/bookkeeping.rs` (new) | `Bookkeeping` struct: names, `heal`, `version`, `get_meta`, `set_meta`, `record_migration`, `clear_migrations`, `detect_managed_version` |
| `crates/dbd-core/src/adapter/mod.rs` | Replace `ensure_meta_table` + `ensure_migrations_table` with `heal_bookkeeping`; update the default `reverse_managed_version` doc |
| `crates/dbd-core/src/adapter/sqlite.rs` | Implement `heal_bookkeeping` (= old ensure bodies); names unchanged |
| `crates/dbd-core/src/adapter/convex.rs` | Implement `heal_bookkeeping` (no-op) |
| `crates/dbd-core/src/adapter/mock.rs` | Implement `heal_bookkeeping` (no-op) |
| `crates/dbd-core/src/reverse.rs` | Add `"dbd"` to `ALWAYS_EXCLUDED` (and `is_internal`) |
| `crates/dbd-core/src/script.rs` | Add `"dbd"` to `ALWAYS_PROTECTED` |
| `crates/dbd-core/src/design/apply.rs` | `heal_bookkeeping()` at entry; drop `ensure_migrations_table` call |
| `crates/dbd-core/src/design/reconcile.rs` | `heal_bookkeeping()` at entry (replaces `ensure_meta_table`) |
| `crates/dbd-core/src/design/reset.rs` | `heal_bookkeeping()` before the meta read |
| `crates/dbd-core/src/design/*` (migrate path) | `heal_bookkeeping()` at entry |
| `crates/dbd-core/tests/embedded_test.rs` | Retarget legacy-`_dbd_meta` seeding tests as heal-path tests (§ Test Scenarios) |
| `docs/llms/llms-full.txt`, `docs/llms/llms.txt`, dbd skill | Document the `dbd` schema layout and the per-adapter split |

---

## Test Scenarios

### T1: Fresh Postgres DB creates `dbd.*` directly
```
Given: empty DB
When:  apply (heal_bookkeeping runs first)
Then:  dbd.meta and dbd.migrations exist; no public._dbd_meta / public._dbd_migrations
```

### T2: Legacy `public._dbd_meta` folds into `dbd.meta`
```
Given: seeded public._dbd_meta (env=prod, version=2, with scope) + public._dbd_migrations rows
When:  apply
Then:  dbd.meta has the row (env/version/scope preserved); dbd.migrations has the rows;
       public._dbd_meta and public._dbd_migrations are dropped
```

### T3: Legacy `_dbd_meta` WITHOUT the `scope` column
```
Given: seeded public._dbd_meta lacking the scope column (pre-scope-guard shape)
When:  apply
Then:  heal adds scope, folds the row (scope = NULL), drops the legacy table; prod guard still reads env=prod
```

### T4: Multi-copy heal (scoped-leak) — the case rename cannot handle
```
Given: public._dbd_meta (project p, version 1) AND dojo._dbd_meta (project p, version 2)
When:  apply
Then:  dbd.meta has exactly one row for p (canonical public copy wins, version 1);
       both public._dbd_meta and dojo._dbd_meta are dropped;
       dbd.migrations is the union of both copies' migration rows
```

### T5: Read-before-heal is unnecessary — guards read `dbd.meta` after heal
```
Given: DB with only legacy public._dbd_meta (env=prod)
When:  reset (heal runs, then get_project_meta)
Then:  prod guard fires from dbd.meta; nothing stranded in public
```

### T6: Idempotent re-run
```
Given: already-migrated DB (dbd.meta present, no legacy copies)
When:  apply again
Then:  heal is a no-op; no errors; row unchanged
```

### T7: `reverse` / `merge` recognise `dbd.meta` without mutating
```
Given: managed DB with dbd.meta (no legacy tables)
When:  init --from-db  (and merge)
Then:  init refuses as managed; merge reads the version; dbd.meta is not mutated and not emitted as a project entity
```

### T8: `dbd` schema excluded from reverse-engineering
```
Given: managed DB with dbd.meta + dbd.migrations
When:  merge / reverse / dbml
Then:  no dbd.* entity appears in the output
```

### T9: `reset --clean` leaves `dbd` intact
```
Given: managed DB
When:  reset --clean (drops project schemas)
Then:  dbd schema, dbd.meta, dbd.migrations survive
```

### T10: SQLite unchanged
```
Given: SQLite DB
When:  apply (heal_bookkeeping = old ensure bodies)
Then:  _dbd_meta / _dbd_migrations created as today; no schema concept involved
```

### T11 (validation, live Supabase — see mandatory "verify against live data"): unexposed + unlinted
```
Given: a Supabase project deployed with dbd.meta / dbd.migrations
Then:  the REST API does not expose dbd.* (not in db-schemas);
       the platform linter does NOT flag rls_enabled_no_policy on dbd.* (no deny-all policy present)
```

---

## Open / Deferred

- **Adapter-consistency debt (next cycle).** Only Postgres gets an isolated `Bookkeeping` unit now. SQLite (inline) and Convex (sidecar) still do bookkeeping their own way. The `Bookkeeping` method set is deliberately shaped to become the `BookkeepingStore` trait; next cycle lift it and move SQLite/Convex behind it. Tracked so the divergence does not calcify.
- **PostgREST exposure is config-dependent.** The premise holds for default PostgREST/Supabase config. A user who *explicitly* adds `dbd` to their exposed `db-schemas` re-exposes the tables — their choice, out of scope. T11 verifies the default.
- **No down-migration.** Heal is one-way (`public._dbd_meta` → `dbd.meta`). A user pinning an older dbd against a healed DB would find no `public._dbd_meta`; the read-only detection recognises `dbd.meta`, but an old binary predates that. Acceptable: forward-only, documented.
- **`ensure_import_procedure`** still creates a `staging` schema for the import procedure — unrelated to bookkeeping, left as-is.
