# dbd Bookkeeping Schema Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move Postgres bookkeeping into a dedicated, PostgREST-invisible `dbd` schema (`dbd.meta` / `dbd.migrations`), healing existing `public._dbd_*` layouts automatically, with the logic isolated in a `Bookkeeping` unit.

**Architecture:** A single `heal_bookkeeping()` trait method runs first in every ownership op (apply/deploy/reconcile/reset/migrate). On Postgres it creates the `dbd` schema + tables, folds every legacy `_dbd_*` copy (canonical + scoped strays) into them in one transaction, and drops the originals — so all later reads/writes hit `dbd.*` directly. Read-only detection (init/merge) keeps both-names awareness. SQLite/Convex are behavior-unchanged.

**Tech Stack:** Rust, `sqlx` (Postgres), `postgresql_embedded` integration tests.

**Spec:** `docs/superpowers/specs/2026-08-18-dbd-bookkeeping-schema-design.md`

**Commands:**
- Unit tests: `cargo test --workspace`
- Postgres integration tests: `cargo test --features embedded-tests --test embedded_test <name>` (run from `crates/dbd-core`)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/dbd-core/src/adapter/postgres/mod.rs` | (renamed from `postgres.rs`) adapter; bookkeeping methods delegate to `self.bookkeeping` |
| `crates/dbd-core/src/adapter/postgres/bookkeeping.rs` | **new** — `Bookkeeping` struct: names, `heal`, `version`, `get_meta`, `set_meta`, `record_migration`, `clear_migrations`, `detect_managed_version` |
| `crates/dbd-core/src/adapter/mod.rs` | trait: replace `ensure_meta_table`+`ensure_migrations_table` with `heal_bookkeeping` |
| `crates/dbd-core/src/adapter/sqlite.rs` | implement `heal_bookkeeping` (= old ensure bodies) |
| `crates/dbd-core/src/adapter/convex.rs` / `mock.rs` | implement `heal_bookkeeping` (no-op) |
| `crates/dbd-core/src/reverse.rs` | add `"dbd"` to `ALWAYS_EXCLUDED` |
| `crates/dbd-core/src/script.rs` | add `"dbd"` to `ALWAYS_PROTECTED` |
| `crates/dbd-core/src/adapter/postgres/mod.rs::schema_filter_column` | keep in sync with `reverse::ALWAYS_EXCLUDED` |
| `crates/dbd-core/src/design/apply.rs`, `reconcile.rs`, `reset.rs`, migrate path | call `heal_bookkeeping()` at entry |
| `crates/dbd-core/tests/embedded_test.rs` | retarget legacy-meta tests; add heal/exclusion tests |
| `docs/llms/llms-full.txt`, `docs/llms/llms.txt`, dbd skill | document the `dbd` layout + per-adapter split |

---

## Task 1: Trait seam — `heal_bookkeeping()` (behavior-preserving refactor)

Establish the new trait method and heal-first call order **without** changing where data lives yet (Postgres still uses `public._dbd_*`). This keeps the diff green and isolates the risky behavior change to Task 3.

**Files:**
- Modify: `crates/dbd-core/src/adapter/mod.rs:229,249`
- Modify: `crates/dbd-core/src/adapter/postgres.rs` (impls `ensure_migrations_table` ~1622, `ensure_meta_table` ~1721)
- Modify: `crates/dbd-core/src/adapter/sqlite.rs`, `convex.rs`, `mock.rs`
- Modify: `crates/dbd-core/src/design/apply.rs:80`, `reconcile.rs:306`, `reset.rs:43`

- [ ] **Step 1: Change the trait**

In `adapter/mod.rs`, delete the two lines:
```rust
async fn ensure_migrations_table(&self) -> Result<()>;
```
```rust
async fn ensure_meta_table(&self) -> Result<()>;
```
Add (in the `Migration tracking` section):
```rust
/// Ensure bookkeeping storage exists and is at the current layout, healing any
/// legacy layout in place. Idempotent; safe to call at the start of every
/// ownership operation (apply/deploy/reconcile/reset/migrate). On Postgres this
/// relocates legacy `public._dbd_*` bookkeeping into the `dbd` schema.
async fn heal_bookkeeping(&self) -> Result<()>;
```

- [ ] **Step 2: Postgres impl (temporary passthrough)**

In `postgres.rs`, keep the existing `ensure_migrations_table` / `ensure_meta_table` bodies but make them **private inherent methods** (move them out of the trait `impl` into `impl PostgresAdapter`), and add the trait method:
```rust
async fn heal_bookkeeping(&self) -> Result<()> {
    self.ensure_meta_table_inner().await?;
    self.ensure_migrations_table_inner().await
}
```
(Rename the moved fns to `*_inner`. Task 3 replaces this whole block.)

- [ ] **Step 3: SQLite / Convex / Mock impls**

SQLite (`sqlite.rs`): add, keeping the current `_dbd_*` bodies:
```rust
async fn heal_bookkeeping(&self) -> Result<()> {
    self.ensure_meta_table_inner().await?;
    self.ensure_migrations_table_inner().await
}
```
(Move its existing `ensure_*` bodies to `*_inner` inherent methods, same as Postgres.)

Convex (`convex.rs`) and Mock (`mock.rs`): replace both `ensure_*` impls with:
```rust
async fn heal_bookkeeping(&self) -> Result<()> {
    Ok(())
}
```

- [ ] **Step 4: Move the calls to heal-first**

`design/apply.rs`: at the very top of the apply function (before the first `get_db_version` at line 44), add:
```rust
adapter.heal_bookkeeping().await?;
```
Delete the `adapter.ensure_migrations_table().await?;` call at line 80.

`design/reconcile.rs:306`: replace `adapter.ensure_meta_table().await?;` with `adapter.heal_bookkeeping().await?;` and move it to the top of the reconcile body (before any meta read).

`design/reset.rs`: before the `get_project_meta` read at line 43, add `adapter.heal_bookkeeping().await?;`.

Grep for any remaining callers: `rg -n "ensure_meta_table|ensure_migrations_table" crates/` — the migrate command path and any test helper must call `heal_bookkeeping` instead.

- [ ] **Step 5: Build + existing tests green**

Run: `cargo test --workspace` then `cargo test --features embedded-tests --test embedded_test` (from `crates/dbd-core`)
Expected: PASS (pure refactor — Postgres still uses `public._dbd_*`).

- [ ] **Step 6: Commit**
```bash
git add crates/dbd-core/src/adapter crates/dbd-core/src/design
git commit -m "refactor(adapter): replace ensure_*_table with heal_bookkeeping seam"
```

---

## Task 2: Extract the `Bookkeeping` module (behavior-preserving)

Convert `postgres.rs` into a module directory and move all bookkeeping into `Bookkeeping`, **still against `public._dbd_*`**. No behavior change; embedded tests stay green.

**Files:**
- Rename: `crates/dbd-core/src/adapter/postgres.rs` → `crates/dbd-core/src/adapter/postgres/mod.rs`
- Create: `crates/dbd-core/src/adapter/postgres/bookkeeping.rs`

- [ ] **Step 1: Convert to a module dir**
```bash
cd crates/dbd-core/src/adapter
mkdir postgres
git mv postgres.rs postgres/mod.rs
```
At the top of `postgres/mod.rs` add:
```rust
mod bookkeeping;
use bookkeeping::Bookkeeping;
```

- [ ] **Step 2: Add the struct + field**

Create `postgres/bookkeeping.rs` with the struct and the *current-layout* logic lifted verbatim from `mod.rs` (still `public._dbd_meta` / `public._dbd_migrations`, still using `bookkeeping_schema` / `ensure_public_bookkeeping`). Expose `pub(super)` methods: `heal` (= old `ensure_meta_table_inner` + `ensure_migrations_table_inner`), `version` (= `get_db_version` body), `get_meta`, `set_meta(tx, …)`, `record_migration(tx, …)`, `clear_migrations`, `detect_managed_version` (= `reverse_managed_version` body).

In `postgres/mod.rs`, add the field to `struct PostgresAdapter`:
```rust
    bookkeeping: Bookkeeping,
```
In `PostgresAdapter::new`, before the `Ok(Self { … })`:
```rust
        let bookkeeping = Bookkeeping::new(pool.clone(), project.to_string());
```
and add `bookkeeping,` to the struct literal.

- [ ] **Step 3: Delegate the trait methods**

Replace the bodies in `postgres/mod.rs`:
```rust
async fn heal_bookkeeping(&self) -> Result<()> { self.bookkeeping.heal().await }
async fn get_db_version(&self) -> Result<u32> { self.bookkeeping.version().await }
async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> { self.bookkeeping.get_meta().await }
async fn clear_project_migrations(&self) -> Result<()> { self.bookkeeping.clear_migrations().await }
async fn reverse_managed_version(&self) -> Result<Option<u32>> { self.bookkeeping.detect_managed_version().await }

async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()> {
    let mut guard = self.batch.lock().await;
    self.bookkeeping.set_meta(guard.as_mut(), env, version, scope).await
}

async fn apply_migration(&self, version: u32, sql: &str, description: &str, checksum: &str) -> Result<()> {
    if !sql.is_empty() { self.execute_script(sql).await?; }
    let mut guard = self.batch.lock().await;
    self.bookkeeping.record_migration(guard.as_mut(), version, description, checksum).await
}
```
Delete the now-orphaned `ensure_meta_table_inner` / `ensure_migrations_table_inner` from `mod.rs` (their logic now lives in `Bookkeeping`).

- [ ] **Step 4: Build + tests green**

Run: `cargo test --features embedded-tests --test embedded_test` (from `crates/dbd-core`) and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS — identical behavior, code relocated.

- [ ] **Step 5: Commit**
```bash
git add crates/dbd-core/src/adapter/postgres
git commit -m "refactor(postgres): extract Bookkeeping into its own module"
```

---

## Task 3: Move to the `dbd` schema + rename + heal (the behavior change)

Now rewrite `Bookkeeping` to own `dbd.meta` / `dbd.migrations` and heal legacy layouts. TDD.

**Files:**
- Modify: `crates/dbd-core/src/adapter/postgres/bookkeeping.rs`
- Test: `crates/dbd-core/tests/embedded_test.rs`

- [ ] **Step 1: Write failing heal tests**

Add to `embedded_test.rs` (helpers `start_pg`, `connect`, `assert_table_exists`, `assert_table_absent` already exist). Add one helper:
```rust
/// Assert `dbd.meta.version` for `project` equals `expected`.
async fn assert_dbd_meta_version(
    adapter: &dyn dbd_core::DatabaseAdapter, project: &str, expected: i32,
) {
    let sql = format!(
        "DO $$ DECLARE v integer; BEGIN \
           SELECT version INTO v FROM dbd.meta WHERE project = '{project}'; \
           IF v IS DISTINCT FROM {expected} THEN \
             RAISE EXCEPTION 'dbd.meta[{project}].version = %, expected {expected}', v; \
           END IF; END $$"
    );
    adapter.execute_script(&sql).await
        .unwrap_or_else(|e| panic!("assert_dbd_meta_version({project}) failed: {e}"));
}
```

```rust
#[tokio::test]
async fn heal_fresh_db_creates_dbd_schema() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "fresh").await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    assert_table_exists(&*adapter, "dbd", "meta").await;
    assert_table_exists(&*adapter, "dbd", "migrations").await;
    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "public", "_dbd_migrations").await;
}

#[tokio::test]
async fn heal_folds_legacy_public_meta_into_dbd() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "legacy").await.unwrap();
    adapter.execute_script(
        "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         CREATE TABLE public._dbd_migrations ( \
            project varchar NOT NULL, version integer NOT NULL, applied_at timestamptz NOT NULL DEFAULT now(), \
            description text, checksum text, PRIMARY KEY (project, version) ); \
         INSERT INTO public._dbd_meta (project, env, version, scope) VALUES ('legacy','prod',4,'public'); \
         INSERT INTO public._dbd_migrations (project, version, description, checksum) VALUES ('legacy',1,'init','abc');"
    ).await.unwrap();

    adapter.heal_bookkeeping().await.unwrap();

    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "public", "_dbd_migrations").await;
    assert_dbd_meta_version(&*adapter, "legacy", 4).await;
    let m = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m.env, "prod");
    assert_eq!(m.scope.as_deref(), Some("public"));
    assert_eq!(adapter.get_db_version().await.unwrap(), 4);
}

#[tokio::test]
async fn heal_folds_legacy_meta_without_scope_column() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "nolscope").await.unwrap();
    adapter.execute_script(
        "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('nolscope','prod',4);"
    ).await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    let m = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m.version, 4);
    assert_eq!(m.env, "prod");
    assert_eq!(m.scope, None);
    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
}

#[tokio::test]
async fn heal_folds_multiple_stray_copies_public_wins() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "p").await.unwrap();
    // public (canonical) v1 + stray dojo v2 for the same project.
    adapter.execute_script(
        "CREATE SCHEMA dojo; \
         CREATE TABLE public._dbd_meta (project varchar PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()); \
         CREATE TABLE dojo._dbd_meta (LIKE public._dbd_meta INCLUDING ALL); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('p','prod',1); \
         INSERT INTO dojo._dbd_meta   (project, env, version) VALUES ('p','prod',2); \
         CREATE TABLE public._dbd_migrations (project varchar, version integer, applied_at timestamptz DEFAULT now(), \
            description text, checksum text, PRIMARY KEY (project, version)); \
         CREATE TABLE dojo._dbd_migrations (LIKE public._dbd_migrations INCLUDING ALL); \
         INSERT INTO public._dbd_migrations (project, version) VALUES ('p',1); \
         INSERT INTO dojo._dbd_migrations   (project, version) VALUES ('p',2);"
    ).await.unwrap();

    adapter.heal_bookkeeping().await.unwrap();

    // Canonical public row wins (v1), matching today's read-prefers-public semantics.
    assert_dbd_meta_version(&*adapter, "p", 1).await;
    // Migrations union both copies (composite PK).
    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "dojo", "_dbd_meta").await;
    assert_table_absent(&*adapter, "dojo", "_dbd_migrations").await;
    let n: i64 = 2; // versions 1 and 2 present
    let sql = format!(
        "DO $$ DECLARE c bigint; BEGIN SELECT count(*) INTO c FROM dbd.migrations WHERE project='p'; \
         IF c <> {n} THEN RAISE EXCEPTION 'dbd.migrations count = %, expected {n}', c; END IF; END $$"
    );
    adapter.execute_script(&sql).await.unwrap();
}

#[tokio::test]
async fn heal_is_idempotent() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "idem").await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    adapter.set_project_meta("prod", 3, Some("public")).await.unwrap();
    adapter.heal_bookkeeping().await.unwrap(); // second heal — no-op
    let m = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m.version, 3);
    assert_eq!(m.scope.as_deref(), Some("public"));
}
```

- [ ] **Step 2: Run tests — verify they FAIL**

Run: `cargo test --features embedded-tests --test embedded_test heal_ ` (from `crates/dbd-core`)
Expected: FAIL (`dbd.meta` does not exist — still using `public._dbd_*`).

- [ ] **Step 3: Rewrite `bookkeeping.rs`**

Replace the file body with the `dbd`-schema implementation:
```rust
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::adapter::ProjectMeta;
use crate::error::{DbdError, Result};

/// Owns dbd's version bookkeeping for a Postgres/Supabase database: the `dbd`
/// schema and its `meta` / `migrations` tables, their creation, the one-time
/// heal from the legacy `public._dbd_*` layout, and all reads/writes. Kept in
/// its own module so the naming/heal knowledge lives in one cohesive place.
pub(super) struct Bookkeeping {
    pool: PgPool,
    project: String,
}

impl Bookkeeping {
    pub(super) fn new(pool: PgPool, project: String) -> Self {
        Self { pool, project }
    }

    /// Create the `dbd` schema + tables, fold every legacy `_dbd_*` copy (in
    /// `public` or a scoped-apply stray schema) into them, and drop the legacy
    /// copies. One transaction; idempotent.
    pub(super) async fn heal(&self) -> Result<()> {
        let mut tx = self.pool.begin().await
            .map_err(|e| DbdError::Config(format!("heal: begin failed: {e}")))?;

        sqlx::raw_sql(
            "CREATE SCHEMA IF NOT EXISTS dbd; \
             CREATE TABLE IF NOT EXISTS dbd.meta ( \
                project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
                version integer NOT NULL DEFAULT 0, scope varchar, \
                created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
             CREATE TABLE IF NOT EXISTS dbd.migrations ( \
                project varchar NOT NULL, version integer NOT NULL, applied_at timestamptz NOT NULL DEFAULT now(), \
                description text, checksum text, PRIMARY KEY (project, version) );"
        ).execute(&mut *tx).await
            .map_err(|e| DbdError::Config(format!("heal: create dbd layout failed: {e}")))?;

        // Fold `_dbd_meta`: `public` first so the canonical row wins on conflict.
        for s in Self::legacy_schemas(&mut tx, "_dbd_meta", true).await? {
            let q = s.replace('"', "\"\"");
            sqlx::raw_sql(&format!(
                "ALTER TABLE \"{q}\"._dbd_meta ADD COLUMN IF NOT EXISTS scope varchar; \
                 INSERT INTO dbd.meta (project, env, version, scope, created_at, updated_at) \
                   SELECT project, env, version, scope, created_at, updated_at FROM \"{q}\"._dbd_meta \
                   ON CONFLICT (project) DO NOTHING; \
                 DROP TABLE \"{q}\"._dbd_meta;"
            )).execute(&mut *tx).await
                .map_err(|e| DbdError::Config(format!("heal: fold {s}._dbd_meta failed: {e}")))?;
        }

        // Fold `_dbd_migrations`: union all copies (composite PK makes it safe).
        for s in Self::legacy_schemas(&mut tx, "_dbd_migrations", false).await? {
            let q = s.replace('"', "\"\"");
            sqlx::raw_sql(&format!(
                "INSERT INTO dbd.migrations (project, version, applied_at, description, checksum) \
                   SELECT project, version, applied_at, description, checksum FROM \"{q}\"._dbd_migrations \
                   ON CONFLICT (project, version) DO NOTHING; \
                 DROP TABLE \"{q}\"._dbd_migrations;"
            )).execute(&mut *tx).await
                .map_err(|e| DbdError::Config(format!("heal: fold {s}._dbd_migrations failed: {e}")))?;
        }

        tx.commit().await
            .map_err(|e| DbdError::Config(format!("heal: commit failed: {e}")))?;
        Ok(())
    }

    /// Schemas (excluding `dbd`) holding a table named `relname`. `public_first`
    /// orders `public` ahead of strays so its row wins a meta conflict.
    async fn legacy_schemas(
        tx: &mut Transaction<'static, Postgres>, relname: &str, public_first: bool,
    ) -> Result<Vec<String>> {
        let order = if public_first { "ORDER BY (n.nspname = 'public') DESC, n.nspname" } else { "ORDER BY n.nspname" };
        let rows = sqlx::query(&format!(
            "SELECT n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = $1 AND c.relkind = 'r' AND n.nspname <> 'dbd' {order}"
        )).bind(relname).fetch_all(&mut **tx).await
            .map_err(|e| DbdError::Config(format!("heal: list legacy {relname} failed: {e}")))?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("nspname")).collect())
    }

    pub(super) async fn version(&self) -> Result<u32> {
        let row = sqlx::query("SELECT version FROM dbd.meta WHERE project = $1")
            .bind(&self.project).fetch_optional(&self.pool).await
            .map_err(|e| DbdError::Config(format!("read dbd.meta version failed: {e}")))?;
        Ok(row.map(|r| r.get::<i32, _>("version") as u32).unwrap_or(0))
    }

    pub(super) async fn get_meta(&self) -> Result<Option<ProjectMeta>> {
        let row = sqlx::query(
            "SELECT project, env, version, scope, updated_at::text AS applied_at \
             FROM dbd.meta WHERE project = $1"
        ).bind(&self.project).fetch_optional(&self.pool).await
            .map_err(|e| DbdError::Config(format!("read dbd.meta failed: {e}")))?;
        Ok(row.map(|r| ProjectMeta {
            project: r.get("project"),
            env: r.get("env"),
            version: r.get::<i32, _>("version") as u32,
            scope: r.try_get("scope").ok(),
            applied_at: r.try_get("applied_at").ok(),
        }))
    }

    pub(super) async fn set_meta(
        &self, tx: Option<&mut Transaction<'static, Postgres>>,
        env: &str, version: u32, scope: Option<&str>,
    ) -> Result<()> {
        let q = sqlx::query(
            "INSERT INTO dbd.meta (project, env, version, scope) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project) DO UPDATE \
             SET env = EXCLUDED.env, version = EXCLUDED.version, scope = EXCLUDED.scope, updated_at = now()"
        ).bind(&self.project).bind(env).bind(version as i32).bind(scope);
        match tx {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }.map_err(|e| DbdError::Config(format!("set dbd.meta failed: {e}")))?;
        Ok(())
    }

    pub(super) async fn record_migration(
        &self, tx: Option<&mut Transaction<'static, Postgres>>,
        version: u32, description: &str, checksum: &str,
    ) -> Result<()> {
        let q = sqlx::query(
            "INSERT INTO dbd.migrations (project, version, description, checksum) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (project, version) DO NOTHING"
        ).bind(&self.project).bind(version as i32).bind(description).bind(checksum);
        match tx {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }.map_err(|e| DbdError::Migration(format!("Record migration failed: {e}")))?;
        Ok(())
    }

    pub(super) async fn clear_migrations(&self) -> Result<()> {
        sqlx::query("DELETE FROM dbd.migrations WHERE project = $1")
            .bind(&self.project).execute(&self.pool).await.ok();
        Ok(())
    }

    /// Read-only: recognise `dbd.meta` OR a legacy `_dbd_meta` (any schema) as a
    /// managed DB and return its version for this project. Never mutates. `None`
    /// for a foreign DB. Used by init/merge, which must not heal.
    pub(super) async fn detect_managed_version(&self) -> Result<Option<u32>> {
        let row = sqlx::query(
            "SELECT n.nspname, c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind = 'r' \
               AND ((c.relname = 'meta' AND n.nspname = 'dbd') OR c.relname = '_dbd_meta') \
             ORDER BY (n.nspname = 'dbd') DESC, (n.nspname = 'public') DESC, n.nspname LIMIT 1"
        ).fetch_optional(&self.pool).await
            .map_err(|e| DbdError::Config(format!("detect managed lookup failed: {e}")))?;
        let Some(row) = row else { return Ok(None) };
        let ns: String = row.get("nspname");
        let rel: String = row.get("relname");
        let (nq, rq) = (ns.replace('"', "\"\""), rel.replace('"', "\"\""));
        let vrow = sqlx::query(&format!("SELECT version FROM \"{nq}\".\"{rq}\" WHERE project = $1"))
            .bind(&self.project).fetch_optional(&self.pool).await
            .map_err(|e| DbdError::Config(format!("detect managed version read failed: {e}")))?;
        Ok(Some(vrow.map(|r| r.get::<i32, _>("version") as u32).unwrap_or(0)))
    }
}
```

- [ ] **Step 4: Run heal tests — verify PASS**

Run: `cargo test --features embedded-tests --test embedded_test heal_` (from `crates/dbd-core`)
Expected: PASS (all five heal tests).

- [ ] **Step 5: Commit**
```bash
git add crates/dbd-core/src/adapter/postgres/bookkeeping.rs crates/dbd-core/tests/embedded_test.rs
git commit -m "feat(postgres): move bookkeeping to dbd schema with heal-on-write"
```

---

## Task 4: Retarget the legacy relocation tests

The two old tests assert relocation into `public._dbd_meta` via `set_project_meta`; that path no longer heals location. Update them to the `dbd` layout.

**Files:**
- Modify: `crates/dbd-core/tests/embedded_test.rs` (`meta_table_heals_from_stray_schema_into_public` ~2318, `legacy_meta_without_scope_reads_and_backfills` ~2372, `assert_meta_version` helper ~2290)

- [ ] **Step 1: Update `meta_table_heals_from_stray_schema_into_public`**

Rename to `heal_relocates_stray_meta_and_preserves_rows`. Keep the `dojo._dbd_meta` seed (project `meta_heal_test` v7 + `other_project` v99). Replace the body after the seed with:
```rust
    // Before heal the new layout doesn't exist; detection still sees it as managed.
    assert_eq!(adapter.reverse_managed_version().await.unwrap(), Some(7));

    adapter.heal_bookkeeping().await.unwrap();

    assert_table_exists(&*adapter, "dbd", "meta").await;
    assert_table_absent(&*adapter, "dojo", "_dbd_meta").await;
    assert_dbd_meta_version(&*adapter, "meta_heal_test", 7).await;   // this project rode along
    assert_dbd_meta_version(&*adapter, "other_project", 99).await;   // unrelated row too → data moved

    adapter.set_project_meta("prod", 8, None).await.unwrap();
    assert_eq!(adapter.get_db_version().await.unwrap(), 8);
    let meta = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(meta.version, 8);
    assert_eq!(meta.env, "prod");
```
(`connect` still creates the adapter for project `meta_heal_test`; `reverse_managed_version` reads that project's row.)

- [ ] **Step 2: Update `legacy_meta_without_scope_reads_and_backfills`**

This behavior is now covered by `heal_folds_legacy_meta_without_scope_column` (Task 3). Delete this test to avoid duplication, OR convert it to call `heal_bookkeeping()` before the read. Prefer delete (DRY):
```bash
# remove the legacy_meta_without_scope_reads_and_backfills test function
```

- [ ] **Step 3: Retire the `assert_meta_version` helper if now unused**

Run: `rg -n "assert_meta_version" crates/dbd-core/tests/embedded_test.rs` — if only its definition remains, delete it (superseded by `assert_dbd_meta_version`).

- [ ] **Step 4: Run + green**

Run: `cargo test --features embedded-tests --test embedded_test` (from `crates/dbd-core`)
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/dbd-core/tests/embedded_test.rs
git commit -m "test(postgres): retarget legacy-meta relocation tests to dbd schema"
```

---

## Task 5: Keep `dbd` invisible — reverse + reset exclusions

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs:6`
- Modify: `crates/dbd-core/src/adapter/postgres/mod.rs::schema_filter_column` (~406)
- Modify: `crates/dbd-core/src/script.rs:53`
- Test: `crates/dbd-core/src/reverse.rs` (unit), `crates/dbd-core/src/script.rs` (unit), `crates/dbd-core/tests/embedded_test.rs`

- [ ] **Step 1: Failing unit test for reverse exclusion**

In `reverse.rs` tests module:
```rust
#[test]
fn dbd_schema_is_internal() {
    assert!(is_internal("dbd"));
}
```
Run: `cargo test -p dbd-core dbd_schema_is_internal` → FAIL.

- [ ] **Step 2: Add `"dbd"` to `ALWAYS_EXCLUDED`**

`reverse.rs:6`:
```rust
pub const ALWAYS_EXCLUDED: &[&str] = &["pg_catalog", "information_schema", "dbd"];
```
Run the test → PASS.

- [ ] **Step 3: Keep `schema_filter_column` in sync**

`postgres/mod.rs::schema_filter_column` — add `dbd` to the NOT-IN list so introspection SQL skips it:
```rust
format!(
    "{col} NOT IN ('pg_catalog', 'information_schema', 'dbd') \
     AND {col} NOT LIKE 'pg_toast%' \
     AND {col} NOT LIKE 'pg_temp%'"
)
```

- [ ] **Step 4: Failing unit test for reset protection**

In `script.rs` tests:
```rust
#[test]
fn dbd_schema_is_protected_on_all_targets() {
    assert!(schema_is_protected("dbd", "postgres"));
    assert!(schema_is_protected("dbd", "supabase"));
}
```
Run: `cargo test -p dbd-core dbd_schema_is_protected_on_all_targets` → FAIL.

- [ ] **Step 5: Add `"dbd"` to `ALWAYS_PROTECTED`**

`script.rs:53`:
```rust
const ALWAYS_PROTECTED: &[&str] = &["pg_catalog", "information_schema", "pg_toast", "dbd"];
```
Run the test → PASS.

- [ ] **Step 6: Integration test — `dbd` not reverse-engineered, survives reset**

In `embedded_test.rs`:
```rust
#[tokio::test]
async fn dbd_schema_excluded_from_introspect_and_reset() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "excl").await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    // introspect() must not surface dbd.meta / dbd.migrations
    let ents = adapter.introspect().await.unwrap();
    assert!(ents.iter().all(|e| !e.name.starts_with("dbd.")),
        "dbd.* leaked into introspect: {:?}", ents.iter().map(|e| &e.name).collect::<Vec<_>>());
    // dbd schema survives a --clean reset (assert table still present after reset script runs)
    assert_table_exists(&*adapter, "dbd", "meta").await;
}
```
(If a helper to run reset end-to-end exists in the test file, invoke it with `--clean`; otherwise the introspect assertion is the core check and the `schema_is_protected` unit test covers the drop-script exclusion.)

Run: `cargo test --features embedded-tests --test embedded_test dbd_schema_excluded` → PASS.

- [ ] **Step 7: Commit**
```bash
git add crates/dbd-core/src/reverse.rs crates/dbd-core/src/script.rs crates/dbd-core/src/adapter/postgres/mod.rs crates/dbd-core/tests/embedded_test.rs
git commit -m "feat(schema): exclude dbd schema from reverse-engineering and reset"
```

---

## Task 6: Confirm the deny-all policy is gone

The deny-all RLS block lived in `ensure_public_bookkeeping`, deleted in Task 2/3. Verify nothing reintroduces it.

**Files:**
- Verify: `crates/dbd-core/src/adapter/postgres/`

- [ ] **Step 1: Grep for residue**

Run: `rg -n "dbd_internal|USING \(false\)|rls_enabled_no_policy|ensure_public_bookkeeping|bookkeeping_schema" crates/dbd-core/src`
Expected: **no matches** in `src` (comments in tests/docs are fine). If any remain, delete them.

- [ ] **Step 2: Leave the unrelated ddl change alone**

`crates/dbd-core/src/internal/import_jsonb_to_table.ddl` is modified in the working tree by unrelated WIP — do **not** stage or revert it as part of this feature.

- [ ] **Step 3: Full green + lint**

Run (from repo root): `cargo test --workspace` , then from `crates/dbd-core`: `cargo test --features embedded-tests --test embedded_test` , then `cargo clippy --workspace --all-targets -- -D warnings`
Expected: all PASS, zero warnings.

- [ ] **Step 4: Commit (only if Step 1 required deletions)**
```bash
git add crates/dbd-core/src/adapter/postgres
git commit -m "chore(postgres): drop obsolete deny-all RLS policy on bookkeeping tables"
```

---

## Task 7: Docs

**Files:**
- Modify: `docs/llms/llms-full.txt`, `docs/llms/llms.txt`, `/Users/Jerry/.claude/skills/dbd/SKILL.md` (or the repo's dbd skill source if tracked)

- [ ] **Step 1: Update the layout references**

Change descriptions of `_dbd_meta` / `_dbd_migrations` to state the per-adapter split:
- Postgres/Supabase → `dbd.meta` / `dbd.migrations` in a dedicated, unexposed `dbd` schema (not in PostgREST `db-schemas`; excluded from reverse-engineering and reset).
- SQLite → `_dbd_meta` / `_dbd_migrations` (no schemas; prefix is the namespace).
- Convex → JSON sidecar next to `schema.ts`.

Touch the lines noted in the spec §8 (`llms-full.txt` ~378, 821-823, 875; `llms.txt` ~125). Keep the "managed DB detected in ANY schema" wording for the init/merge gate — detection now also recognises `dbd.meta`.

- [ ] **Step 2: Verify no stale claims remain**

Run: `rg -n "public\._dbd|_dbd_meta table in|deny-all" docs/`
Fix any that now misdescribe the layout.

- [ ] **Step 3: Commit**
```bash
git add docs
git commit -m "docs: describe dbd bookkeeping schema and per-adapter split"
```

---

## Self-Review

**Spec coverage:**
- §1 Bookkeeping unit → Task 2 (extract) + Task 3 (rewrite). ✔
- §2 `heal_bookkeeping` trait → Task 1. ✔
- §3 heal SQL (fold + drop, public-wins, migrations union) → Task 3 Step 3 + tests T1/T2/T4. ✔
- §4 post-heal reads/writes target `dbd.*` → Task 3 Step 3 delegations. ✔
- §5 read-only detection both-names → Task 3 `detect_managed_version` + Task 4 Step 1 assertion. ✔
- §6 reverse + reset exclusions → Task 5. ✔
- §7 revert deny-all → Task 6. ✔
- §8 docs → Task 7. ✔
- Test scenarios T1-T10 → Task 3 (heal_*), Task 4 (relocation), Task 5 (exclusion); T10 SQLite covered by existing SQLite tests staying green (Task 1 Step 5). T11 (live Supabase) is a manual validation, noted below. ✔

**Placeholder scan:** No TBD/TODO; all code blocks concrete. The migrate-path call site (Task 1 Step 4) is resolved by the `rg` sweep for `ensure_*` callers rather than a hardcoded line — intentional, since that path wasn't pinned in exploration.

**Type consistency:** `Bookkeeping` method names (`heal`, `version`, `get_meta`, `set_meta`, `record_migration`, `clear_migrations`, `detect_managed_version`) are identical across Task 2 and Task 3. Trait method `heal_bookkeeping` consistent across Tasks 1-5. `assert_dbd_meta_version` defined once (Task 3 Step 1), reused in Task 4.

**Manual validation (not automatable here):** T11 — deploy to a real Supabase project and confirm (a) the REST API does not expose `dbd.*`, (b) the linter no longer flags `rls_enabled_no_policy`. Required by the mandatory "done means verified against live data" rule before calling the feature complete.
