//! dbd's own version bookkeeping for the Postgres/Supabase adapter.
//!
//! Bookkeeping lives in a dedicated, PostgREST-invisible `dbd` schema as
//! `dbd.meta` / `dbd.migrations`. `heal()` creates that layout and folds this
//! project's rows from any legacy `public._dbd_*` copy (plus scoped-apply strays
//! in other schemas) into it in one transaction, so every subsequent read/write
//! in an ownership op targets `dbd.*` directly. Another project's rows stay in
//! the legacy copy, which stays while it holds any: that project may still be on
//! an older dbd (#7, H5). So reads are layered — a project's record is its
//! `dbd.meta` row, else its row in a legacy `_dbd_meta` — because the CLI
//! scope/prod guards read meta BEFORE the core op runs `heal_bookkeeping()`: on
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
            description text, checksum text, PRIMARY KEY (project, version) ); \
         CREATE TABLE IF NOT EXISTS dbd.matview_stamps ( \
            project varchar NOT NULL, matview varchar NOT NULL, stamp text NOT NULL, \
            stamped_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (project, matview) );";

    pub(super) fn new(pool: PgPool, project: String) -> Self {
        Self { pool, project }
    }

    /// The project this bookkeeping is kept for.
    pub(super) fn project(&self) -> &str {
        &self.project
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

    /// The meta relations to read, in the order a project's record is looked
    /// for: `dbd.meta` if present, then each legacy `_dbd_meta` — `public`
    /// first, then strays by schema — which hold the rows of projects not yet
    /// healed. Empty = no bookkeeping anywhere (foreign/fresh DB). Only matches
    /// `dbd.meta` or `_dbd_meta` — never an unrelated user table named `meta`.
    /// One this role cannot read is left out: projects sharing a database may
    /// run as different roles, and another's legacy table holds nothing of this
    /// project's.
    async fn meta_relations(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT n.nspname, c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind = 'r' \
               AND ((c.relname = 'meta' AND n.nspname = 'dbd') OR c.relname = '_dbd_meta') \
               AND has_schema_privilege(n.oid, 'USAGE') AND has_table_privilege(c.oid, 'SELECT') \
             ORDER BY (n.nspname = 'dbd') DESC, (n.nspname = 'public') DESC, n.nspname",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("resolve meta relations failed: {e}")))?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<String, _>("nspname"), r.get::<String, _>("relname")))
            .collect())
    }

    /// Create the `dbd` schema + tables, and fold this project's rows from every
    /// legacy `_dbd_*` copy (in `public` or a scoped-apply stray schema) into
    /// them. Only this project's: another project sharing the database may still
    /// be on an older dbd, reading and writing the legacy copy, so its rows stay
    /// and the copy is neither altered nor dropped while it holds any (#7, H5).
    /// A copy left empty is dropped. The fold + drop run in one transaction;
    /// idempotent (a fresh DB yields empty `dbd.*`, an already-migrated DB has no
    /// rows of this project's to fold → no-op).
    ///
    /// The legacy copies are discovered up front on the pool (a `Send`-safe
    /// `fetch`), then all DDL/DML runs inside the transaction via
    /// `Executor::execute(&str)` — not `raw_sql`, whose future is not `Send`
    /// against `&mut PgConnection` under `#[async_trait]` (see
    /// `PostgresAdapter::exec_raw`). Projects sharing a database run at once,
    /// so each copy is locked before it is read — see [`Self::lock_legacy`] —
    /// and one gone since the discovery is passed over.
    ///
    /// The transaction reads `READ COMMITTED`, whatever the database's default:
    /// a lock waits for a writer in flight, and only a fresh snapshot per
    /// statement then sees what it committed. One heal runs at a time per
    /// database — two first runs each creating the `dbd` schema collided on it
    /// — and none waits past [`HEAL_LOCK_TIMEOUT`] for a lock.
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
            .execute(
                format!(
                    "SET TRANSACTION ISOLATION LEVEL READ COMMITTED; \
                     SET LOCAL lock_timeout = '{HEAL_LOCK_TIMEOUT}'"
                )
                .as_str(),
            )
            .await
            .map_err(|e| DbdError::Config(format!("heal: begin failed: {e}")))?;
        if let Err(e) = (&mut *tx)
            .execute("SELECT pg_advisory_xact_lock(hashtext('dbd:heal'))")
            .await
        {
            return Err(DbdError::Config(if sqlstate(&e).as_deref() == Some("55P03") {
                format!(
                    "heal: another dbd run is healing this database's bookkeeping and did not \
                     finish within {HEAL_LOCK_TIMEOUT}; run again"
                )
            } else {
                format!("heal: waiting for another heal failed: {e}")
            }));
        }

        (&mut *tx)
            .execute(Self::LAYOUT_DDL)
            .await
            .map_err(|e| DbdError::Config(format!("heal: create dbd layout failed: {e}")))?;

        // Lift each materialized view's legacy drift stamp — a dbd before 0.25
        // kept it as the view's whole comment — into `dbd.matview_stamps`,
        // before anything re-runs the view's file. That file's own
        // `COMMENT ON` would erase the stamp, leaving the view untracked. A
        // recorded stamp wins; the comment itself is left for its author.
        sqlx::query(
            "INSERT INTO dbd.matview_stamps (project, matview, stamp) \
               SELECT $1, n.nspname || '.' || c.relname, obj_description(c.oid, 'pg_class') \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE c.relkind = 'm' AND obj_description(c.oid, 'pg_class') LIKE 'dbd:hash=%' \
             ON CONFLICT (project, matview) DO NOTHING",
        )
        .bind(&self.project)
        .execute(&mut *tx)
        .await
        .map_err(|e| DbdError::Config(format!("heal: lift matview stamps failed: {e}")))?;

        // Fold this project's `_dbd_migrations` rows → `dbd.migrations`
        // (composite PK unions copies) — before `_dbd_meta`, the order a dbd
        // before the `dbd` schema writes them in one batch. Locked the other
        // way round, the heal and such a batch each waited for the other.
        for s in migration_schemas {
            if !self.lock_legacy(&mut tx, &s, "_dbd_migrations").await? {
                continue;
            }
            let q = s.replace('"', "\"\"");
            sqlx::query(&format!(
                "INSERT INTO dbd.migrations (project, version, applied_at, description, checksum) \
                   SELECT project, version, applied_at, description, checksum FROM \"{q}\"._dbd_migrations \
                   WHERE project = $1 ON CONFLICT (project, version) DO NOTHING"
            ))
            .bind(&self.project)
            .execute(&mut *tx)
            .await
            .map_err(|e| DbdError::Config(format!("heal: fold {s}._dbd_migrations failed: {e}")))?;
            self.release_legacy_rows(&mut tx, &s, "_dbd_migrations").await?;
        }

        // Fold this project's `_dbd_meta` row → `dbd.meta` (canonical row wins
        // on conflict). A copy that predates the `scope` column is read as it
        // is, not given one: an older dbd may still write it.
        for s in meta_schemas {
            if !self.lock_legacy(&mut tx, &s, "_dbd_meta").await? {
                continue;
            }
            let q = s.replace('"', "\"\"");
            let has_scope: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute \
                   WHERE attrelid = format('%I._dbd_meta', $1::text)::regclass \
                     AND attname = 'scope' AND NOT attisdropped)",
            )
            .bind(&s)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| DbdError::Config(format!("heal: read {s}._dbd_meta columns failed: {e}")))?;
            let scope = if has_scope { "scope" } else { "NULL::varchar" };
            sqlx::query(&format!(
                "INSERT INTO dbd.meta (project, env, version, scope, created_at, updated_at) \
                   SELECT project, env, version, {scope}, created_at, updated_at FROM \"{q}\"._dbd_meta \
                   WHERE project = $1 ON CONFLICT (project) DO NOTHING"
            ))
            .bind(&self.project)
            .execute(&mut *tx)
            .await
            .map_err(|e| DbdError::Config(format!("heal: fold {s}._dbd_meta failed: {e}")))?;
            self.release_legacy_rows(&mut tx, &s, "_dbd_meta").await?;
        }

        tx.commit()
            .await
            .map_err(|e| DbdError::Config(format!("heal: commit failed: {e}")))?;
        Ok(())
    }

    /// Lock the legacy `schema.relname` for the heal, so a project on an older
    /// dbd writing it at the same moment is waited for and what it commits is
    /// seen: a row it inserts unseen would otherwise go with a table the heal
    /// found empty and dropped. `SHARE ROW EXCLUSIVE` lets readers through and
    /// holds writers until the heal commits.
    ///
    /// `false`, with the transaction as it was, for a table this role cannot
    /// lock, read or delete from — another project's role owns it — unless it
    /// holds this project's own row, which would go on being read there: that
    /// is refused, naming the table. `false` too for one dropped since it was
    /// found, by another project's heal. A writer that holds it past
    /// [`HEAL_LOCK_TIMEOUT`] fails the heal, naming the table.
    async fn lock_legacy(&self, tx: &mut Transaction<'static, Postgres>, schema: &str, relname: &str) -> Result<bool> {
        use sqlx::Executor as _;
        let table = quoted_table(schema, relname);
        (&mut **tx)
            .execute("SAVEPOINT dbd_heal_lock")
            .await
            .map_err(|e| DbdError::Config(format!("heal: savepoint failed: {e}")))?;
        let locked = async {
            (&mut **tx)
                .execute(format!("LOCK TABLE {table} IN SHARE ROW EXCLUSIVE MODE").as_str())
                .await?;
            sqlx::query_scalar::<_, bool>(
                "SELECT has_table_privilege(format('%I.%I', $1::text, $2::text), 'SELECT') \
                    AND has_table_privilege(format('%I.%I', $1::text, $2::text), 'DELETE')",
            )
            .bind(schema)
            .bind(relname)
            .fetch_one(&mut **tx)
            .await
        }
        .await;
        match locked {
            Ok(true) => {
                (&mut **tx)
                    .execute("RELEASE SAVEPOINT dbd_heal_lock")
                    .await
                    .map_err(|e| DbdError::Config(format!("heal: release savepoint failed: {e}")))?;
                Ok(true)
            }
            Ok(false) => {
                self.undo_to(tx, "dbd_heal_lock").await?;
                self.refuse_if_holding_own_row(tx, schema, relname).await?;
                Ok(false)
            }
            Err(e) if sqlstate(&e).as_deref() == Some("42P01") => {
                self.undo_to(tx, "dbd_heal_lock").await?;
                Ok(false)
            }
            Err(e) if sqlstate(&e).as_deref() == Some("42501") => {
                self.undo_to(tx, "dbd_heal_lock").await?;
                self.refuse_if_holding_own_row(tx, schema, relname).await?;
                Ok(false)
            }
            Err(e) if sqlstate(&e).as_deref() == Some("55P03") => Err(DbdError::Config(format!(
                "heal: {schema}.{relname} is held by another session's open write, which did not finish \
                 within {HEAL_LOCK_TIMEOUT} — let it finish, or end it, and run again"
            ))),
            Err(e) => Err(DbdError::Config(format!("heal: lock {schema}.{relname} failed: {e}"))),
        }
    }

    /// Refuse a legacy `schema.relname` this role can read but not take that
    /// holds this project's row: passed over, the row would never move to
    /// `dbd.*` and would go on being read — after a reset forgot the project,
    /// its old record would come back.
    async fn refuse_if_holding_own_row(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        schema: &str,
        relname: &str,
    ) -> Result<()> {
        // By oid, not by name: looking a name up in a schema the role cannot use
        // raises, where this must only answer no.
        let readable: bool = sqlx::query_scalar(
            "SELECT coalesce(bool_and(has_schema_privilege(n.oid, 'USAGE') \
                                      AND has_table_privilege(c.oid, 'SELECT')), false) \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(schema)
        .bind(relname)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| DbdError::Config(format!("heal: read {schema}.{relname} privileges failed: {e}")))?;
        if !readable {
            return Ok(());
        }
        let holds: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {} WHERE project = $1)",
            quoted_table(schema, relname)
        ))
        .bind(&self.project)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| DbdError::Config(format!("heal: read {schema}.{relname} failed: {e}")))?;
        if holds {
            return Err(DbdError::Config(format!(
                "heal: {schema}.{relname} holds this project's record, but this role cannot lock and delete \
                 from it, so the record cannot move to dbd.*; grant it DELETE on {schema}.{relname}, or run \
                 as its owner"
            )));
        }
        Ok(())
    }

    /// Roll the heal back to `savepoint` and release it.
    async fn undo_to(&self, tx: &mut Transaction<'static, Postgres>, savepoint: &str) -> Result<()> {
        use sqlx::Executor as _;
        (&mut **tx)
            .execute(format!("ROLLBACK TO SAVEPOINT {savepoint}; RELEASE SAVEPOINT {savepoint}").as_str())
            .await
            .map_err(|e| DbdError::Config(format!("heal: rollback to savepoint failed: {e}")))?;
        Ok(())
    }

    /// Delete this project's rows from the legacy `schema.relname`, folded
    /// already, and drop the table if that leaves it empty — no other project's
    /// rows, so no older dbd still using it. The drop is housekeeping — a reader
    /// finds no rows in an empty table either — so one Postgres refuses (a view
    /// over the table, a role that does not own it) leaves the table, empty,
    /// rather than failing the heal and every one after it.
    async fn release_legacy_rows(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        schema: &str,
        relname: &str,
    ) -> Result<()> {
        use sqlx::Executor as _;
        let table = quoted_table(schema, relname);
        sqlx::query(&format!("DELETE FROM {table} WHERE project = $1"))
            .bind(&self.project)
            .execute(&mut **tx)
            .await
            .map_err(|e| DbdError::Config(format!("heal: release {schema}.{relname} failed: {e}")))?;
        let emptied: bool = sqlx::query_scalar(&format!("SELECT NOT EXISTS (SELECT 1 FROM {table})"))
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| DbdError::Config(format!("heal: read {schema}.{relname} failed: {e}")))?;
        if emptied {
            (&mut **tx)
                .execute("SAVEPOINT dbd_heal_drop")
                .await
                .map_err(|e| DbdError::Config(format!("heal: savepoint failed: {e}")))?;
            match (&mut **tx).execute(format!("DROP TABLE {table}").as_str()).await {
                Ok(_) => {
                    (&mut **tx)
                        .execute("RELEASE SAVEPOINT dbd_heal_drop")
                        .await
                        .map_err(|e| DbdError::Config(format!("heal: release savepoint failed: {e}")))?;
                }
                Err(_) => self.undo_to(tx, "dbd_heal_drop").await?,
            }
        }
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
    /// Catalog-resolved (both-names aware): its `dbd.meta` row, else its row in
    /// a legacy `_dbd_meta` in any schema, so the CLI scope/prod guards still
    /// work on a project not yet healed (the guards read before the core op
    /// heals) — on a legacy DB, or beside a project that healed first.
    pub(super) async fn get_meta(&self) -> Result<Option<ProjectMeta>> {
        for (schema, rel) in self.meta_relations().await? {
            if let Some(row) = self
                .read_meta_in(&schema, &rel, "project = $1")
                .await?
                .into_iter()
                .next()
            {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    /// Every other project's meta row, by project — what the guards read when
    /// this project has none (see `DatabaseAdapter::other_projects_meta`).
    /// Resolved as [`Self::get_meta`] is, so a project not yet healed reads too.
    pub(super) async fn other_projects(&self) -> Result<Vec<ProjectMeta>> {
        let mut rows = self.read_meta("project <> $1").await?;
        rows.sort_by(|a, b| a.project.cmp(&b.project));
        Ok(rows)
    }

    /// The meta rows `filter` selects — a `WHERE` clause over `project`,
    /// bound to this project's name as `$1` — one per project: the first
    /// found in [`Self::meta_relations`] order.
    async fn read_meta(&self, filter: &str) -> Result<Vec<ProjectMeta>> {
        let mut found: Vec<ProjectMeta> = Vec::new();
        for (schema, rel) in self.meta_relations().await? {
            for row in self.read_meta_in(&schema, &rel, filter).await? {
                if !found.iter().any(|m| m.project == row.project) {
                    found.push(row);
                }
            }
        }
        Ok(found)
    }

    /// The rows `filter` selects in one meta relation, `schema.rel` — none if
    /// another project's heal dropped it since it was listed.
    async fn read_meta_in(&self, schema: &str, rel: &str, filter: &str) -> Result<Vec<ProjectMeta>> {
        let (sq, rq) = (schema.replace('"', "\"\""), rel.replace('"', "\"\""));
        // Preferred read includes `scope`. On a legacy `_dbd_meta` that predates
        // the column this SELECT errors with 42703; fall back to the legacy shape
        // (scope = None) so env/version — and the prod guard — still work.
        let with_scope = sqlx::query(&format!(
            "SELECT project, env, version, scope, updated_at::text AS applied_at \
             FROM \"{sq}\".\"{rq}\" WHERE {filter}"
        ))
        .bind(&self.project)
        .fetch_all(&self.pool)
        .await;

        let read = |row: &sqlx::postgres::PgRow, with_scope: bool| ProjectMeta {
            project: row.get("project"),
            env: row.get("env"),
            version: row.get::<i32, _>("version") as u32,
            scope: if with_scope { row.try_get("scope").ok() } else { None },
            applied_at: row.try_get("applied_at").ok(),
        };
        match with_scope {
            Ok(rows) => Ok(rows.iter().map(|row| read(row, true)).collect()),
            // A legacy `_dbd_meta` lacking the `scope` column → SQLSTATE 42703
            // (undefined_column): read the legacy shape in that one case. Any
            // OTHER error is real (transient failure, permission, etc.) and must
            // surface — swallowing it to `None` would silently disable the scope
            // AND prod guards.
            Err(e) if sqlstate(&e).as_deref() == Some("42P01") => Ok(Vec::new()),
            Err(e) => {
                let undefined_column = sqlstate(&e).as_deref() == Some("42703");
                if !undefined_column {
                    return Err(DbdError::Config(format!("read meta failed: {e}")));
                }
                let rows = sqlx::query(&format!(
                    "SELECT project, env, version, updated_at::text AS applied_at \
                     FROM \"{sq}\".\"{rq}\" WHERE {filter}"
                ))
                .bind(&self.project)
                .fetch_all(&self.pool)
                .await;
                match rows {
                    Ok(rows) => Ok(rows.iter().map(|row| read(row, false)).collect()),
                    // Dropped between the two reads, by another project's heal.
                    Err(e) if sqlstate(&e).as_deref() == Some("42P01") => Ok(Vec::new()),
                    Err(e) => Err(DbdError::Config(format!("read meta failed: {e}"))),
                }
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

    /// Record `stamp` as the drift stamp of materialized view `matview`
    /// (`schema.name`). `tx` routes through the caller's open batch, as
    /// [`Self::set_meta`] does.
    ///
    /// The stamp lives here, not in the view's comment: that comment is the
    /// author's (`COMMENT ON MATERIALIZED VIEW` in the DDL file), and sharing it
    /// meant the first apply overwrote the author's text and the next erased the
    /// stamp.
    pub(super) async fn stamp_matview(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        matview: &str,
        stamp: &str,
    ) -> Result<()> {
        self.ensure_dbd_layout().await?;
        let q = sqlx::query(
            "INSERT INTO dbd.matview_stamps (project, matview, stamp) VALUES ($1, $2, $3) \
             ON CONFLICT (project, matview) DO UPDATE SET stamp = EXCLUDED.stamp, stamped_at = now()",
        )
        .bind(&self.project)
        .bind(matview)
        .bind(stamp);
        match tx {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Config(format!("stamp {matview} failed: {e}")))?;
        Ok(())
    }

    /// Delete this project's drift stamps for `matviews` (`schema.name`). The
    /// only caller (`reset`) heals first, so `dbd.matview_stamps` exists here —
    /// errors (permission/transient) are surfaced, not swallowed.
    pub(super) async fn forget_matview_stamps(&self, matviews: &[String]) -> Result<()> {
        if matviews.is_empty() {
            return Ok(());
        }
        sqlx::query("DELETE FROM dbd.matview_stamps WHERE project = $1 AND matview = ANY($2)")
            .bind(&self.project)
            .bind(matviews)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("forget matview stamps failed: {e}")))?;
        Ok(())
    }

    /// Every drift stamp this project recorded, `schema.name` → stamp. Empty on
    /// a database not yet healed (no `dbd.matview_stamps`): read-only commands
    /// call this without healing, and must not fail for it.
    pub(super) async fn matview_stamps(&self) -> Result<std::collections::HashMap<String, String>> {
        let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass('dbd.matview_stamps')::text")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("matview stamps lookup failed: {e}")))?;
        if exists.is_none() {
            return Ok(Default::default());
        }
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT matview, stamp FROM dbd.matview_stamps WHERE project = $1")
                .bind(&self.project)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("matview stamps read failed: {e}")))?;
        Ok(rows.into_iter().collect())
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

    /// Delete this project's `dbd.meta` row. Like [`Self::clear_migrations`],
    /// called only by `reset`, which heals first.
    pub(super) async fn forget_meta(&self) -> Result<()> {
        sqlx::query("DELETE FROM dbd.meta WHERE project = $1")
            .bind(&self.project)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("clear dbd.meta failed: {e}")))?;
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
        let relations = self.meta_relations().await?;
        if relations.is_empty() {
            return Ok(None); // no bookkeeping anywhere → foreign DB
        }
        for (schema, rel) in relations {
            let (sq, rq) = (schema.replace('"', "\"\""), rel.replace('"', "\"\""));
            let vrow = match sqlx::query(&format!("SELECT version FROM \"{sq}\".\"{rq}\" WHERE project = $1"))
                .bind(&self.project)
                .fetch_optional(&self.pool)
                .await
            {
                // Dropped since it was listed, by another project's heal.
                Err(e) if sqlstate(&e).as_deref() == Some("42P01") => continue,
                read => read.map_err(|e| DbdError::Config(format!("detect managed version read failed: {e}")))?,
            };
            if let Some(row) = vrow {
                return Ok(Some(row.get::<i32, _>("version") as u32));
            }
        }
        Ok(Some(0))
    }
}

/// How long a heal waits for a lock — another heal, or a writer on a legacy
/// table — before it gives up and says which.
const HEAL_LOCK_TIMEOUT: &str = "5s";

/// `"schema"."relname"`, each part quoted.
fn quoted_table(schema: &str, relname: &str) -> String {
    format!(
        "\"{}\".\"{}\"",
        schema.replace('"', "\"\""),
        relname.replace('"', "\"\"")
    )
}

/// The SQLSTATE of a database error, if `e` is one.
fn sqlstate(e: &sqlx::Error) -> Option<String> {
    e.as_database_error().and_then(|db| db.code()).map(|c| c.into_owned())
}
