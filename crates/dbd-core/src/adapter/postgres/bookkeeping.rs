//! dbd's own bookkeeping storage (`_dbd_meta`, `_dbd_migrations`) for the
//! Postgres adapter.
//!
//! Extracted from `postgres::mod` as a pure relocation (no behavior change):
//! bookkeeping tables still live in `public._dbd_*`, resolved via the same
//! catalog-based `bookkeeping_schema` / `ensure_public_bookkeeping` logic that
//! lived on `PostgresAdapter` before. A later task moves the tables to a
//! dedicated schema; this step only relocates the code that manages them.

use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::adapter::ProjectMeta;
use crate::error::{DbdError, Result};

/// Owns dbd's bookkeeping tables for one project. Holds a pool clone (not a
/// batch transaction) — callers thread an open batch transaction through
/// `set_meta` / `record_migration` explicitly, mirroring how `PostgresAdapter`
/// threads `self.batch` through its own trait methods.
pub(super) struct Bookkeeping {
    pool: PgPool,
    project: String,
}

impl Bookkeeping {
    pub(super) fn new(pool: PgPool, project: String) -> Self {
        Self { pool, project }
    }

    /// Execute raw DDL directly against the pool — deliberately, not through any
    /// open batch transaction (unlike `PostgresAdapter::exec_raw`). This DDL is
    /// idempotent (`CREATE TABLE IF NOT EXISTS`, `ADD COLUMN IF NOT EXISTS`, and a
    /// schema-relocation that no-ops once the table is already in `public`), so
    /// when it fires mid-batch (`set_meta` → `ensure_meta_table` during the
    /// `SetVersion` apply step) it is a no-op in practice: `heal_bookkeeping` ran
    /// at the start of the operation, before the batch transaction opened, so the
    /// tables already exist and are already in `public`.
    async fn exec_raw(&self, sql: &str) -> Result<()> {
        sqlx::raw_sql(sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("SQL execution failed: {e}")))?;
        Ok(())
    }

    /// Schema that currently holds bookkeeping table `table` per the catalog,
    /// preferring `public` when copies exist in several schemas. `None` when it
    /// exists nowhere.
    ///
    /// Bookkeeping tables (`_dbd_meta`, `_dbd_migrations`) are resolved via the
    /// catalog rather than an unqualified name because a scoped apply can leave a
    /// stray copy in a non-`public` schema (e.g. `dojo._dbd_meta`): pooled
    /// connections don't share `search_path`, so an unqualified read or write can
    /// resolve to a different schema than the one the table actually lives in —
    /// which surfaces as `relation "_dbd_meta" does not exist`.
    async fn bookkeeping_schema(&self, table: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT n.nspname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = $1 AND c.relkind = 'r' \
             ORDER BY (n.nspname = 'public') DESC LIMIT 1",
        )
        .bind(table)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            DbdError::Config(format!("bookkeeping schema lookup for {table} failed: {e}"))
        })?;
        Ok(row.map(|r| r.get::<String, _>("nspname")))
    }

    /// Ensure bookkeeping table `table` lives in `public`, relocating a stray
    /// copy a scoped apply may have left in another schema. These tables hold only
    /// dbd's own version bookkeeping, so `ALTER TABLE … SET SCHEMA public` is safe
    /// and preserves their rows. `create_public_sql` must create `public.<table>`.
    async fn ensure_public_bookkeeping(&self, table: &str, create_public_sql: &str) -> Result<()> {
        match self.bookkeeping_schema(table).await? {
            Some(ref s) if s == "public" => {} // already home
            Some(s) => {
                // `bookkeeping_schema` prefers `public`, so a non-public result
                // means there is no `public` copy to collide with the relocation.
                let quoted = s.replace('"', "\"\"");
                self.exec_raw(&format!(
                    "ALTER TABLE \"{quoted}\".\"{table}\" SET SCHEMA public"
                ))
                .await?;
            }
            None => {} // doesn't exist anywhere yet — created below
        }
        // Idempotent: creates `public.<table>` on a fresh DB, no-op once present.
        self.exec_raw(create_public_sql).await
    }

    /// Inner body for the migrations-table half of `heal`. Kept as a private
    /// method so it can be composed with `ensure_meta_table` inside `heal`.
    async fn ensure_migrations_table(&self) -> Result<()> {
        self.ensure_public_bookkeeping(
            "_dbd_migrations",
            "CREATE TABLE IF NOT EXISTS public._dbd_migrations ( \
                project     varchar NOT NULL, \
                version     integer NOT NULL, \
                applied_at  timestamptz NOT NULL DEFAULT now(), \
                description text, \
                checksum    text, \
                PRIMARY KEY (project, version) \
            )",
        )
        .await
    }

    /// Inner body for the meta-table half of `heal`. Kept as a private method
    /// so it can be composed with `ensure_migrations_table` inside `heal`, and
    /// called directly by `set_meta`.
    async fn ensure_meta_table(&self) -> Result<()> {
        self.ensure_public_bookkeeping(
            "_dbd_meta",
            "CREATE TABLE IF NOT EXISTS public._dbd_meta ( \
                project     varchar NOT NULL PRIMARY KEY, \
                env         varchar NOT NULL DEFAULT 'dev', \
                version     integer NOT NULL DEFAULT 0, \
                scope       varchar, \
                created_at  timestamptz NOT NULL DEFAULT now(), \
                updated_at  timestamptz NOT NULL DEFAULT now() \
            )",
        )
        .await?;
        // Backfill `scope` on databases whose `_dbd_meta` predates the column.
        self.exec_raw("ALTER TABLE public._dbd_meta ADD COLUMN IF NOT EXISTS scope varchar")
            .await
    }

    /// Ensure bookkeeping storage exists and is at the current layout, healing
    /// any legacy/mislocated bookkeeping in place. Idempotent; safe to call at
    /// the start of every ownership operation (apply/deploy/reconcile/reset/migrate).
    pub(super) async fn heal(&self) -> Result<()> {
        self.ensure_meta_table().await?;
        self.ensure_migrations_table().await
    }

    /// Read `_dbd_meta`'s recorded version for this project (authoritative
    /// version source). Resolve its schema via the catalog rather than an
    /// unqualified SELECT so a stray copy left by a scoped apply (e.g.
    /// `dojo._dbd_meta`) is read correctly instead of silently missed as version 0.
    pub(super) async fn version(&self) -> Result<u32> {
        let Some(schema) = self.bookkeeping_schema("_dbd_meta").await? else {
            return Ok(0); // no `_dbd_meta` anywhere → fresh/unmanaged DB
        };
        let quoted = schema.replace('"', "\"\"");
        let result = sqlx::query(&format!(
            "SELECT version FROM \"{quoted}\"._dbd_meta WHERE project = $1"
        ))
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await;

        match result {
            Ok(Some(row)) => {
                let version: i32 = row.get("version");
                Ok(version as u32)
            }
            // `bookkeeping_schema` already confirmed the table exists, so a read
            // error here is real (transient failure, permission) — surface it
            // rather than masking a live DB's version as 0 and misplanning apply.
            Ok(None) => Ok(0),
            Err(e) => Err(DbdError::Config(format!("read _dbd_meta version failed: {e}"))),
        }
    }

    /// Read the full `_dbd_meta` row (env/version/scope/applied_at) for this
    /// project. Resolve `_dbd_meta`'s schema via the catalog (a scoped apply may
    /// have left it outside `public`) rather than reading an unqualified name.
    pub(super) async fn get_meta(&self) -> Result<Option<ProjectMeta>> {
        let Some(schema) = self.bookkeeping_schema("_dbd_meta").await? else {
            return Ok(None); // no `_dbd_meta` anywhere yet
        };
        let quoted = schema.replace('"', "\"\"");
        // Preferred read includes `scope`. On a database whose `_dbd_meta`
        // predates the column this SELECT errors; fall back to the legacy shape
        // (scope = None) so env/version — and the prod guard — still work.
        let with_scope = sqlx::query(&format!(
            "SELECT project, env, version, scope, updated_at::text as applied_at FROM \"{quoted}\"._dbd_meta WHERE project = $1"
        ))
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await;

        match with_scope {
            Ok(Some(row)) => Ok(Some(ProjectMeta {
                project: row.get("project"),
                env: row.get("env"),
                version: row.get::<i32, _>("version") as u32,
                scope: row.try_get("scope").ok(),
                applied_at: row.try_get("applied_at").ok(),
            })),
            Ok(None) => Ok(None),
            // A legacy `_dbd_meta` (which `bookkeeping_schema` above confirmed
            // exists) lacks the `scope` column → SQLSTATE 42703 (undefined_column):
            // read the legacy shape in that one case. Any OTHER error is real (a
            // transient failure, permission issue, etc.) and must surface —
            // swallowing it to `None` would silently disable the scope AND prod
            // guards.
            Err(e) => {
                let undefined_column =
                    e.as_database_error().and_then(|db| db.code()).as_deref() == Some("42703");
                if !undefined_column {
                    return Err(DbdError::Config(format!("read _dbd_meta failed: {e}")));
                }
                let row = sqlx::query(&format!(
                    "SELECT project, env, version, updated_at::text as applied_at FROM \"{quoted}\"._dbd_meta WHERE project = $1"
                ))
                .bind(&self.project)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("read _dbd_meta failed: {e}")))?;
                Ok(row.map(|row| ProjectMeta {
                    project: row.get("project"),
                    env: row.get("env"),
                    version: row.get::<i32, _>("version") as u32,
                    scope: None,
                    applied_at: row.try_get("applied_at").ok(),
                }))
            }
        }
    }

    /// Upsert `_dbd_meta` for this project. `tx` is the caller's open batch
    /// transaction (if any) — when `Some`, the write routes through it so it
    /// commits/rolls back atomically with the rest of the apply batch; when
    /// `None`, it runs directly against the pool.
    ///
    /// Calls `ensure_meta_table` (not the full `heal`) first, matching the
    /// original `set_project_meta` behavior — no redundant migrations-table
    /// ensure on every meta write. This also pins `_dbd_meta` to `public`
    /// (relocating a stray copy a scoped apply may have left in another schema).
    /// The insert is schema-qualified, so it resolves deterministically
    /// regardless of the ambient search_path or which pooled connection runs
    /// it — no `RESET` dance required, which never worked across the non-batch
    /// pool anyway.
    pub(super) async fn set_meta(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        env: &str,
        version: u32,
        scope: Option<&str>,
    ) -> Result<()> {
        self.ensure_meta_table().await?;
        let q = sqlx::query(
            "INSERT INTO public._dbd_meta (project, env, version, scope) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project) DO UPDATE \
             SET env = EXCLUDED.env, version = EXCLUDED.version, scope = EXCLUDED.scope, updated_at = now()"
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i32)
        .bind(scope);
        match tx {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;

        Ok(())
    }

    /// Record a migration in `_dbd_migrations`. `tx` is the caller's open batch
    /// transaction (if any) — same routing convention as `set_meta`. Does NOT
    /// run the migration's own SQL — that stays with the adapter's
    /// `execute_script`, since this struct owns only bookkeeping storage.
    ///
    /// `_dbd_migrations` is pinned to `public` (see `ensure_migrations_table`),
    /// so the qualified insert lands in the right table regardless of the
    /// ambient search_path or which pooled connection runs it.
    pub(super) async fn record_migration(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        version: u32,
        description: &str,
        checksum: &str,
    ) -> Result<()> {
        let q = sqlx::query(
            "INSERT INTO public._dbd_migrations (project, version, description, checksum) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project, version) DO NOTHING"
        )
        .bind(&self.project)
        .bind(version as i32)
        .bind(description)
        .bind(checksum);
        match tx {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Migration(format!("Record migration failed: {e}")))?;

        Ok(())
    }

    pub(super) async fn clear_migrations(&self) -> Result<()> {
        sqlx::query("DELETE FROM public._dbd_migrations WHERE project = $1")
            .bind(&self.project)
            .execute(&self.pool)
            .await
            .ok(); // Ignore if table doesn't exist
        Ok(())
    }

    /// Reverse-engineering safety: `Some(version)` if this DB is dbd-managed (a
    /// `_dbd_meta` table exists in ANY schema), reading the applied version for
    /// `self.project` — `0` if the table exists but has no matching row. `None`
    /// for a foreign DB (no `_dbd_meta`).
    ///
    /// Note: `_dbd_meta` has `PRIMARY KEY (project)` — exactly one row per
    /// project. `env` records the *last-applied* environment and is not part of
    /// the key, so this read is keyed on `project` only (env-agnostic).
    pub(super) async fn detect_managed_version(&self) -> Result<Option<u32>> {
        // 1. Find the schema that holds `_dbd_meta` via the catalog (not an
        //    unqualified SELECT) — it commonly lives off the search_path
        //    (e.g. `staging._dbd_meta`). No row → foreign DB.
        let Some(schema) = self.bookkeeping_schema("_dbd_meta").await? else {
            return Ok(None);
        };

        // 2. Read the applied version for this project from that schema's
        //    `_dbd_meta`. `schema` comes from the catalog (not user input) but is
        //    still quoted defensively. `_dbd_meta` has PRIMARY KEY (project), so
        //    there is exactly one row per project; `env` records the *last-applied*
        //    environment and is NOT part of the key. Row → Some(version); no row →
        //    Some(0) (the table exists, so the DB is managed, just no row yet).
        let quoted = schema.replace('"', "\"\"");
        let query = format!(
            "SELECT version FROM \"{quoted}\"._dbd_meta WHERE project = $1"
        );
        let version_row = sqlx::query(&query)
            .bind(&self.project)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("detect_managed_version read failed: {e}")))?;

        match version_row {
            Some(row) => {
                let version: i32 = row.get("version");
                Ok(Some(version as u32))
            }
            None => Ok(Some(0)),
        }
    }
}
