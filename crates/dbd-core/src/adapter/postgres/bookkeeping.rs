//! dbd's own version bookkeeping for the Postgres/Supabase adapter.
//!
//! Bookkeeping lives in a dedicated, PostgREST-invisible `dbd` schema as
//! `dbd.meta` / `dbd.migrations`. `heal()` creates that layout and folds any
//! legacy `public._dbd_*` copy (plus scoped-apply strays in other schemas) into
//! it in one transaction, so every subsequent read/write in an ownership op
//! targets `dbd.*` directly. The read-only detection path stays both-names aware
//! (recognises `dbd.meta` OR a legacy `_dbd_meta` in any schema) because the CLI
//! scope/prod guards read meta BEFORE the core op runs `heal_bookkeeping()` — on
//! a not-yet-healed legacy DB a read hardcoded to `dbd.meta` would find nothing
//! and silently disable the guard on the first post-upgrade write.

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
    /// DDL for the `dbd` schema and its tables. Idempotent (`IF NOT EXISTS`), so
    /// it runs both against the pool (`ensure_dbd_layout`) and inside `heal`'s
    /// transaction from the same source of truth.
    const LAYOUT_DDL: &'static str = "CREATE SCHEMA IF NOT EXISTS dbd; \
         CREATE TABLE IF NOT EXISTS dbd.meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         CREATE TABLE IF NOT EXISTS dbd.migrations ( \
            project varchar NOT NULL, version integer NOT NULL, applied_at timestamptz NOT NULL DEFAULT now(), \
            description text, checksum text, PRIMARY KEY (project, version) );";

    pub(super) fn new(pool: PgPool, project: String) -> Self {
        Self { pool, project }
    }

    /// Create the `dbd` schema + tables if missing. Idempotent and
    /// NON-destructive (no fold, no drop). Runs against the pool, so it is safe
    /// to call anytime — including as a no-op mid-batch, since `heal` already ran
    /// at op entry and the tables already exist by then.
    async fn ensure_dbd_layout(&self) -> Result<()> {
        sqlx::raw_sql(Self::LAYOUT_DDL)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("ensure dbd layout failed: {e}")))?;
        Ok(())
    }

    /// Resolve the meta relation to read: `dbd.meta` if present, else a legacy
    /// `_dbd_meta` in any schema (a not-yet-healed DB). Returns (schema, relname).
    /// `None` = no bookkeeping anywhere (foreign/fresh DB). Only matches
    /// `dbd.meta` or `_dbd_meta` — never an unrelated user table named `meta`.
    async fn resolve_meta(&self) -> Result<Option<(String, String)>> {
        let row = sqlx::query(
            "SELECT n.nspname, c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind = 'r' \
               AND ((c.relname = 'meta' AND n.nspname = 'dbd') OR c.relname = '_dbd_meta') \
             ORDER BY (n.nspname = 'dbd') DESC, (n.nspname = 'public') DESC, n.nspname LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("resolve meta relation failed: {e}")))?;
        Ok(row.map(|r| (r.get::<String, _>("nspname"), r.get::<String, _>("relname"))))
    }

    /// Create the `dbd` schema + tables, fold every legacy `_dbd_*` copy (in
    /// `public` or a scoped-apply stray schema) into them, and drop the legacy
    /// copies. The fold + drop run in one transaction; idempotent (a fresh DB
    /// yields empty `dbd.*`, an already-migrated DB has no legacy copies → no-op).
    ///
    /// The legacy copies are discovered up front on the pool (a `Send`-safe
    /// `fetch`), then all DDL/DML runs inside the transaction via
    /// `Executor::execute(&str)` — not `raw_sql`, whose future is not `Send`
    /// against `&mut PgConnection` under `#[async_trait]` (see
    /// `PostgresAdapter::exec_raw`). dbd runs are serial per DB, so the catalog
    /// cannot shift between the discovery read and the fold.
    pub(super) async fn heal(&self) -> Result<()> {
        use sqlx::Executor as _;

        // `public` first so the canonical `_dbd_meta` row wins the meta conflict.
        let meta_schemas = self.legacy_schemas("_dbd_meta", true).await?;
        let migration_schemas = self.legacy_schemas("_dbd_migrations", false).await?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("heal: begin failed: {e}")))?;

        (&mut *tx)
            .execute(Self::LAYOUT_DDL)
            .await
            .map_err(|e| DbdError::Config(format!("heal: create dbd layout failed: {e}")))?;

        // Fold `_dbd_meta` → `dbd.meta` (canonical row wins on conflict).
        for s in meta_schemas {
            let q = s.replace('"', "\"\"");
            let sql = format!(
                "ALTER TABLE \"{q}\"._dbd_meta ADD COLUMN IF NOT EXISTS scope varchar; \
                 INSERT INTO dbd.meta (project, env, version, scope, created_at, updated_at) \
                   SELECT project, env, version, scope, created_at, updated_at FROM \"{q}\"._dbd_meta \
                   ON CONFLICT (project) DO NOTHING; \
                 DROP TABLE IF EXISTS \"{q}\"._dbd_meta;"
            );
            (&mut *tx)
                .execute(sql.as_str())
                .await
                .map_err(|e| DbdError::Config(format!("heal: fold {s}._dbd_meta failed: {e}")))?;
        }

        // Fold `_dbd_migrations` → `dbd.migrations` (composite PK unions copies).
        for s in migration_schemas {
            let q = s.replace('"', "\"\"");
            let sql = format!(
                "INSERT INTO dbd.migrations (project, version, applied_at, description, checksum) \
                   SELECT project, version, applied_at, description, checksum FROM \"{q}\"._dbd_migrations \
                   ON CONFLICT (project, version) DO NOTHING; \
                 DROP TABLE IF EXISTS \"{q}\"._dbd_migrations;"
            );
            (&mut *tx)
                .execute(sql.as_str())
                .await
                .map_err(|e| {
                    DbdError::Config(format!("heal: fold {s}._dbd_migrations failed: {e}"))
                })?;
        }

        tx.commit()
            .await
            .map_err(|e| DbdError::Config(format!("heal: commit failed: {e}")))?;
        Ok(())
    }

    /// Schemas (excluding `dbd`) holding a table named `relname`, read on the
    /// pool. `public_first` orders `public` ahead of strays so its row wins a
    /// meta conflict.
    async fn legacy_schemas(&self, relname: &str, public_first: bool) -> Result<Vec<String>> {
        let order = if public_first {
            "ORDER BY (n.nspname = 'public') DESC, n.nspname"
        } else {
            "ORDER BY n.nspname"
        };
        let rows = sqlx::query(&format!(
            "SELECT n.nspname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = $1 AND c.relkind = 'r' AND n.nspname <> 'dbd' {order}"
        ))
        .bind(relname)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("heal: list legacy {relname} failed: {e}")))?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("nspname")).collect())
    }

    /// Read the recorded version for this project (authoritative version source).
    /// Catalog-resolved (both-names aware) so a not-yet-healed legacy DB is read
    /// correctly instead of silently missed as version 0. `0` when there is no
    /// bookkeeping anywhere or no row for this project yet.
    pub(super) async fn version(&self) -> Result<u32> {
        Ok(self.detect_managed_version().await?.unwrap_or(0))
    }

    /// Read the full meta row (env/version/scope/applied_at) for this project.
    /// Catalog-resolved (both-names aware): reads `dbd.meta` if present, else a
    /// legacy `_dbd_meta` in any schema, so the CLI scope/prod guards still work
    /// on a legacy DB that hasn't been healed yet (the guards read before the core
    /// op heals).
    pub(super) async fn get_meta(&self) -> Result<Option<ProjectMeta>> {
        let Some((schema, rel)) = self.resolve_meta().await? else {
            return Ok(None); // no bookkeeping anywhere yet
        };
        let (sq, rq) = (schema.replace('"', "\"\""), rel.replace('"', "\"\""));
        // Preferred read includes `scope`. On a legacy `_dbd_meta` that predates
        // the column this SELECT errors with 42703; fall back to the legacy shape
        // (scope = None) so env/version — and the prod guard — still work.
        let with_scope = sqlx::query(&format!(
            "SELECT project, env, version, scope, updated_at::text AS applied_at \
             FROM \"{sq}\".\"{rq}\" WHERE project = $1"
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
            // A legacy `_dbd_meta` lacking the `scope` column → SQLSTATE 42703
            // (undefined_column): read the legacy shape in that one case. Any
            // OTHER error is real (transient failure, permission, etc.) and must
            // surface — swallowing it to `None` would silently disable the scope
            // AND prod guards.
            Err(e) => {
                let undefined_column =
                    e.as_database_error().and_then(|db| db.code()).as_deref() == Some("42703");
                if !undefined_column {
                    return Err(DbdError::Config(format!("read meta failed: {e}")));
                }
                let row = sqlx::query(&format!(
                    "SELECT project, env, version, updated_at::text AS applied_at \
                     FROM \"{sq}\".\"{rq}\" WHERE project = $1"
                ))
                .bind(&self.project)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("read meta failed: {e}")))?;
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

    /// Upsert `dbd.meta` for this project. `tx` is the caller's open batch
    /// transaction (if any) — when `Some`, the write routes through it so it
    /// commits/rolls back atomically with the rest of the apply batch; when
    /// `None`, it runs directly against the pool.
    ///
    /// Calls the NON-destructive `ensure_dbd_layout` first (never the destructive
    /// `heal`): `set_meta` runs inside the open batch via the `SetVersion` apply
    /// step, and re-firing heal's fold/drop mid-batch would be unsafe. `heal`
    /// already created the layout at op entry, so this ensure is a no-op there.
    pub(super) async fn set_meta(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        env: &str,
        version: u32,
        scope: Option<&str>,
    ) -> Result<()> {
        self.ensure_dbd_layout().await?;
        let q = sqlx::query(
            "INSERT INTO dbd.meta (project, env, version, scope) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project) DO UPDATE \
             SET env = EXCLUDED.env, version = EXCLUDED.version, scope = EXCLUDED.scope, updated_at = now()",
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i32)
        .bind(scope);
        match tx {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Config(format!("set dbd.meta failed: {e}")))?;
        Ok(())
    }

    /// Record a migration in `dbd.migrations`. `tx` is the caller's open batch
    /// transaction (if any) — same routing convention as `set_meta`. Runs
    /// post-heal (the `dbd` layout already exists), so no ensure is needed. Does
    /// NOT run the migration's own SQL — that stays with the adapter's
    /// `execute_script`, since this struct owns only bookkeeping storage.
    pub(super) async fn record_migration(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        version: u32,
        description: &str,
        checksum: &str,
    ) -> Result<()> {
        let q = sqlx::query(
            "INSERT INTO dbd.migrations (project, version, description, checksum) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (project, version) DO NOTHING",
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

    /// Delete this project's migration rows from `dbd.migrations`. The only
    /// caller (`reset`) heals first, so `dbd.migrations` is guaranteed to exist
    /// here — errors (permission/transient) are surfaced, not swallowed.
    pub(super) async fn clear_migrations(&self) -> Result<()> {
        sqlx::query("DELETE FROM dbd.migrations WHERE project = $1")
            .bind(&self.project)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("clear dbd.migrations failed: {e}")))?;
        Ok(())
    }

    /// Read-only detection: `Some(version)` if this DB is dbd-managed — resolving
    /// `dbd.meta` OR a legacy `_dbd_meta` in ANY schema — reading the applied
    /// version for this project (`0` if the relation exists but has no matching
    /// row). `None` for a foreign DB (no bookkeeping anywhere). Never mutates;
    /// used by init/merge, which must not heal.
    ///
    /// Note: meta has `PRIMARY KEY (project)` — exactly one row per project. `env`
    /// records the *last-applied* environment and is not part of the key, so this
    /// read is keyed on `project` only (env-agnostic).
    pub(super) async fn detect_managed_version(&self) -> Result<Option<u32>> {
        let Some((schema, rel)) = self.resolve_meta().await? else {
            return Ok(None); // no bookkeeping anywhere → foreign DB
        };
        let (sq, rq) = (schema.replace('"', "\"\""), rel.replace('"', "\"\""));
        let vrow = sqlx::query(&format!(
            "SELECT version FROM \"{sq}\".\"{rq}\" WHERE project = $1"
        ))
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("detect managed version read failed: {e}")))?;
        Ok(Some(vrow.map(|r| r.get::<i32, _>("version") as u32).unwrap_or(0)))
    }
}
