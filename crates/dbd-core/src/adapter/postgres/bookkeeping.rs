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
    /// The database's dbd write lock while this adapter holds it.
    write_lock: tokio::sync::Mutex<WriteLock>,
}

/// The database's dbd write lock as an adapter holds it: how many runs nested
/// in it, the session holding it, and whether exclusive.
#[derive(Default)]
struct WriteLock {
    depth: usize,
    session: Option<sqlx::PgConnection>,
    exclusive: bool,
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
            stamped_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (project, matview) ); \
         CREATE TABLE IF NOT EXISTS dbd.objects ( \
            namespace varchar NOT NULL CHECK (namespace IN ('relation', 'type', 'routine')), \
            schema varchar NOT NULL, name varchar NOT NULL, kind varchar NOT NULL, \
            owner varchar NOT NULL, claimed_at timestamptz NOT NULL DEFAULT now(), \
            PRIMARY KEY (namespace, schema, name) ); \
         CREATE TABLE IF NOT EXISTS dbd.uses ( \
            owner varchar NOT NULL, \
            kind varchar NOT NULL CHECK (kind IN ('schema', 'extension', 'role', 'entity', 'owner')), \
            target varchar NOT NULL, via varchar NOT NULL DEFAULT '', \
            PRIMARY KEY (owner, kind, target, via) );";

    pub(super) fn new(pool: PgPool, project: String) -> Self {
        Self {
            pool,
            project,
            write_lock: tokio::sync::Mutex::new(WriteLock::default()),
        }
    }

    /// Take the database's dbd write lock (#7) — the advisory lock on
    /// `dbd:write`, shared or exclusive — in a session of its own, held until
    /// [`Self::unlock_writes`]. A session lock, not a transaction's: a run
    /// commits as it goes, and holds it across. The session is out of the
    /// pool, so dropping it — [`Self::abandon_writes`] — releases the lock;
    /// and it sits idle through the run, so no `idle_session_timeout` may end
    /// it. A nested call shares the lock already held, and is refused one
    /// exclusive inside a shared one. It waits by asking again, not in one
    /// long statement — a statement holds a snapshot, which the run holding
    /// the lock waits out to `CREATE INDEX CONCURRENTLY` — saying so in its
    /// `application_name`, for at most [`WRITE_LOCK_TIMEOUT`].
    pub(super) async fn lock_writes(&self, exclusive: bool) -> Result<()> {
        use sqlx::Connection;
        let mut held = self.write_lock.lock().await;
        if held.depth > 0 {
            if exclusive && !held.exclusive {
                return Err(DbdError::Config(
                    "a run holding the dbd write lock shared cannot take it exclusive".to_string(),
                ));
            }
            held.depth += 1;
            return Ok(());
        }
        let failed = |e: sqlx::Error| DbdError::Config(format!("taking the dbd write lock failed: {e}"));
        let mut session = self.pool.acquire().await.map_err(failed)?.detach();
        for prepare in [
            "SELECT set_config('idle_session_timeout', '0', false) \
               WHERE current_setting('idle_session_timeout', true) IS NOT NULL",
            "SET application_name = 'dbd: waiting for the write lock'",
        ] {
            if let Err(e) = sqlx::query(prepare).execute(&mut session).await {
                let _ = session.close().await;
                return Err(failed(e));
            }
        }
        let try_lock = if exclusive {
            "SELECT pg_try_advisory_lock(hashtext('dbd:write'))"
        } else {
            "SELECT pg_try_advisory_lock_shared(hashtext('dbd:write'))"
        };
        let deadline = std::time::Instant::now() + WRITE_LOCK_TIMEOUT;
        loop {
            match sqlx::query_scalar::<_, bool>(try_lock).fetch_one(&mut session).await {
                Ok(true) => break,
                Ok(false) if std::time::Instant::now() < deadline => tokio::time::sleep(WRITE_LOCK_POLL).await,
                Ok(false) => {
                    let _ = session.close().await;
                    return Err(DbdError::Config(format!(
                        "waited {} minutes for another dbd run writing to this database to finish; try again \
                         once it has",
                        WRITE_LOCK_TIMEOUT.as_secs() / 60
                    )));
                }
                Err(e) => {
                    let _ = session.close().await;
                    return Err(failed(e));
                }
            }
        }
        if let Err(e) = sqlx::query("SET application_name = 'dbd: holding the write lock'")
            .execute(&mut session)
            .await
        {
            let _ = session.close().await;
            return Err(failed(e));
        }
        *held = WriteLock {
            depth: 1,
            session: Some(session),
            exclusive,
        };
        Ok(())
    }

    /// Release the lock [`Self::lock_writes`] took, once the outermost run
    /// that took it is done. A lock its session no longer holds — the session
    /// ended mid-run — fails the run: another project's may have written
    /// beside it.
    pub(super) async fn unlock_writes(&self) -> Result<()> {
        use sqlx::Connection;
        let mut held = self.write_lock.lock().await;
        match held.depth {
            0 => return Ok(()),
            1 => held.depth = 0,
            _ => {
                held.depth -= 1;
                return Ok(());
            }
        }
        let Some(mut session) = held.session.take() else {
            return Ok(());
        };
        let unlock = if held.exclusive {
            "SELECT pg_advisory_unlock(hashtext('dbd:write'))"
        } else {
            "SELECT pg_advisory_unlock_shared(hashtext('dbd:write'))"
        };
        let released = sqlx::query_scalar::<_, bool>(unlock).fetch_one(&mut session).await;
        let _ = session.close().await;
        match released {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(DbdError::Config(
                "the dbd write lock was lost during the run — its session ended — so another project's run \
                 may have written beside it; check the database before running again"
                    .to_string(),
            )),
        }
    }

    /// Give up the lock without a word to the server, for a run dropped
    /// part-way: one level of nesting, and at the outermost the session —
    /// dropped, it closes, and the server releases the lock.
    pub(super) fn abandon_writes(&self) {
        if let Ok(mut held) = self.write_lock.try_lock() {
            match held.depth {
                0 => {}
                1 => *held = WriteLock::default(),
                _ => held.depth -= 1,
            }
        }
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

    /// Claim `objects` for `owner` in `dbd.objects`, or with `write` false only
    /// read who holds them; the ones another owner holds come back, and when
    /// there are any the caller fails the run. In `tx` — the run's open batch —
    /// when there is one, so the claim commits with the run's work or rolls
    /// back with it; else in a transaction of its own, rolled back when any is
    /// held. Either waits at most [`HEAL_LOCK_TIMEOUT`] for another session's
    /// write to the registry. The primary key settles two runs at once: the
    /// second waits on the first's uncommitted rows, then finds them held.
    ///
    /// A read with no registry yet finds nothing held, and writes nothing.
    pub(super) async fn claim_objects(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        owner: &str,
        objects: &[crate::adapter::OwnedObject],
        write: bool,
    ) -> Result<Vec<crate::adapter::HeldObject>> {
        if !write {
            let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass('dbd.objects')::text")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("ownership lookup failed: {e}")))?;
            if exists.is_none() {
                return Ok(Vec::new());
            }
            let mut conn = self
                .pool
                .acquire()
                .await
                .map_err(|e| DbdError::Config(format!("ownership read failed: {e}")))?;
            return held_objects(&mut conn, owner, objects).await;
        }
        match tx {
            Some(tx) => self.claim_in(tx, owner, objects).await,
            None => {
                let mut own = self.registry_tx("claim").await?;
                let held = self.claim_in(&mut own, owner, objects).await?;
                if held.is_empty() {
                    own.commit()
                        .await
                        .map_err(|e| DbdError::Config(format!("claim: commit failed: {e}")))?;
                } else {
                    own.rollback()
                        .await
                        .map_err(|e| DbdError::Config(format!("claim: rollback failed: {e}")))?;
                }
                Ok(held)
            }
        }
    }

    /// A transaction of the registry's own, for a write outside any batch:
    /// `READ COMMITTED`, so a wait ends in a read of what the other session
    /// committed.
    async fn registry_tx(&self, what: &str) -> Result<Transaction<'static, Postgres>> {
        use sqlx::Executor as _;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("{what}: begin failed: {e}")))?;
        (&mut *tx)
            .execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .await
            .map_err(|e| DbdError::Config(format!("{what}: begin failed: {e}")))?;
        Ok(tx)
    }

    /// The claim's statements, in `tx`, waiting at most [`HEAL_LOCK_TIMEOUT`]
    /// for the registry and then leaving the batch's own lock timeout as it
    /// was.
    async fn claim_in(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        owner: &str,
        objects: &[crate::adapter::OwnedObject],
    ) -> Result<Vec<crate::adapter::HeldObject>> {
        let column = |f: fn(&crate::adapter::OwnedObject) -> &str| -> Vec<String> {
            objects.iter().map(|o| f(o).to_string()).collect()
        };
        bounded_wait(tx, "dbd.objects", true).await?;
        sqlx::query(
            "INSERT INTO dbd.objects (namespace, schema, name, kind, owner) \
               SELECT d.namespace, d.schema, d.name, d.kind, $5 \
               FROM unnest($1::text[], $2::text[], $3::text[], $4::text[]) AS d(namespace, schema, name, kind) \
             ON CONFLICT (namespace, schema, name) DO UPDATE SET kind = EXCLUDED.kind \
               WHERE dbd.objects.owner = EXCLUDED.owner",
        )
        .bind(column(|o| &o.namespace))
        .bind(column(|o| &o.schema))
        .bind(column(|o| &o.name))
        .bind(column(|o| &o.kind))
        .bind(owner)
        .execute(&mut **tx)
        .await
        .map_err(|e| registry_error(&e, "dbd.objects", "claim"))?;
        let held = held_objects(tx, owner, objects).await?;
        bounded_wait(tx, "dbd.objects", false).await?;
        Ok(held)
    }

    /// Replace `owner`'s rows in `dbd.uses` with `uses` — in `tx`, the run's
    /// open batch, when there is one, else in a transaction of its own; either
    /// waits at most [`HEAL_LOCK_TIMEOUT`] for the registry. A shared use —
    /// schema, extension, role — is recorded as given. An entity use names its
    /// target `namespace:schema.name`, the name split at its first dot as dbd
    /// splits one, and is recorded whoever owns it — or nobody yet: the uses
    /// guard asks the registry who owns a target when it runs, so a use of
    /// an entity its owner claims later still guards it. One named bare is
    /// looked up along its search path as Postgres looks it up: a read is of
    /// the first relation on the path that exists; a call, of any routine on
    /// it that could be the one its arguments reach.
    pub(super) async fn record_uses(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        owner: &str,
        uses: &[crate::adapter::Use],
    ) -> Result<()> {
        match tx {
            Some(tx) => self.record_uses_in(tx, owner, uses).await,
            None => {
                let mut own = self.registry_tx("record uses").await?;
                self.record_uses_in(&mut own, owner, uses).await?;
                own.commit()
                    .await
                    .map_err(|e| DbdError::Config(format!("record uses: commit failed: {e}")))
            }
        }
    }

    async fn record_uses_in(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        owner: &str,
        uses: &[crate::adapter::Use],
    ) -> Result<()> {
        let (shared, entities): (Vec<&crate::adapter::Use>, Vec<&crate::adapter::Use>) =
            uses.iter().partition(|u| u.kind != "entity");
        let column = |list: &[&crate::adapter::Use], f: fn(&crate::adapter::Use) -> &str| -> Vec<String> {
            list.iter().map(|u| f(u).to_string()).collect()
        };
        // Each entity use's candidates, in the order Postgres tries them: the
        // target's own schema, or each schema on its search path.
        let mut candidates = Candidates::default();
        for (i, u) in entities.iter().enumerate() {
            let (namespace, qualified) = u.target.split_once(':').unwrap_or_default();
            let (schema, name) = crate::entity::split_qualified_name(qualified);
            let bare = !u.search_path.is_empty();
            let schemas = if bare {
                u.search_path.clone()
            } else {
                vec![schema.unwrap_or_default()]
            };
            for (rank, schema) in schemas.into_iter().enumerate() {
                candidates.bare.push(bare);
                candidates.used.push(i as i32);
                candidates.rank.push(rank as i32);
                candidates.namespace.push(namespace.to_string());
                candidates.schema.push(schema);
                candidates.name.push(name.clone());
                candidates.via.push(u.via.clone());
            }
        }
        bounded_wait(tx, "dbd.uses", true).await?;
        sqlx::query("DELETE FROM dbd.uses WHERE owner = $1")
            .bind(owner)
            .execute(&mut **tx)
            .await
            .map_err(|e| registry_error(&e, "dbd.uses", "record uses"))?;
        sqlx::query(
            "INSERT INTO dbd.uses (owner, kind, target, via) \
               SELECT $1, d.kind, d.target, d.via \
               FROM unnest($2::text[], $3::text[], $4::text[]) AS d(kind, target, via) \
             ON CONFLICT DO NOTHING",
        )
        .bind(owner)
        .bind(column(&shared, |u| &u.kind))
        .bind(column(&shared, |u| &u.target))
        .bind(column(&shared, |u| &u.via))
        .execute(&mut **tx)
        .await
        .map_err(|e| registry_error(&e, "dbd.uses", "record uses"))?;
        // A bare name's candidates are those that exist, as Postgres finds
        // them. A read is of the first relation on the path; a call is
        // resolved by its arguments too, which dbd does not weigh, so it is a
        // use of every routine on the path it could reach.
        sqlx::query(
            "WITH d AS ( \
               SELECT * FROM unnest($2::int4[], $3::int4[], $4::text[], $5::text[], $6::text[], $7::text[], $8::bool[]) \
                 AS d(used, rank, namespace, schema, name, via, bare) \
             ), relations AS ( \
               SELECT DISTINCT ON (d.used) d.via, d.namespace, d.schema, d.name FROM d \
               WHERE d.namespace <> 'routine' AND (NOT d.bare OR EXISTS ( \
                 SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = d.schema AND c.relname = d.name AND c.relkind IN ('r', 'v', 'm', 'f', 'p', 'S'))) \
               ORDER BY d.used, d.rank \
             ), routines AS ( \
               SELECT d.via, d.namespace, d.schema, d.name FROM d \
               WHERE d.namespace = 'routine' AND (NOT d.bare OR EXISTS ( \
                 SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                 WHERE n.nspname = d.schema AND p.proname = d.name)) \
             ) \
             INSERT INTO dbd.uses (owner, kind, target, via) \
               SELECT $1, 'entity', f.namespace || ':' || f.schema || '.' || f.name, f.via \
               FROM (SELECT * FROM relations UNION ALL SELECT * FROM routines) f \
             ON CONFLICT DO NOTHING",
        )
        .bind(owner)
        .bind(&candidates.used)
        .bind(&candidates.rank)
        .bind(&candidates.namespace)
        .bind(&candidates.schema)
        .bind(&candidates.name)
        .bind(&candidates.via)
        .bind(&candidates.bare)
        .execute(&mut **tx)
        .await
        .map_err(|e| registry_error(&e, "dbd.uses", "record uses"))?;
        bounded_wait(tx, "dbd.uses", false).await
    }

    /// What the registry says of `objects` for `owner`'s prune: each one's
    /// owner, the schemas other owners use, and the owners that record their
    /// uses. Reads only; a database with no registry yet has none of it.
    pub(super) async fn ownership(
        &self,
        owner: &str,
        objects: &[crate::adapter::OwnedObject],
    ) -> Result<crate::adapter::Ownership> {
        let read = |e: sqlx::Error| DbdError::Config(format!("ownership read failed: {e}"));
        let mut found = crate::adapter::Ownership {
            owners: vec![None; objects.len()],
            ..Default::default()
        };
        let registry: Option<String> = sqlx::query_scalar("SELECT to_regclass('dbd.objects')::text")
            .fetch_one(&self.pool)
            .await
            .map_err(read)?;
        if registry.is_none() {
            return Ok(found);
        }
        let column = |f: fn(&crate::adapter::OwnedObject) -> &str| -> Vec<String> {
            objects.iter().map(|o| f(o).to_string()).collect()
        };
        found.owners = sqlx::query_scalar(
            "SELECT o.owner \
             FROM unnest($1::text[], $2::text[], $3::text[]) WITH ORDINALITY AS d(namespace, schema, name, i) \
             LEFT JOIN dbd.objects o ON o.namespace = d.namespace AND o.schema = d.schema AND o.name = d.name \
             ORDER BY d.i",
        )
        .bind(column(|o| &o.namespace))
        .bind(column(|o| &o.schema))
        .bind(column(|o| &o.name))
        .fetch_all(&self.pool)
        .await
        .map_err(read)?;
        let shared: Vec<(String, Vec<String>)> = sqlx::query_as(
            "SELECT target, array_agg(DISTINCT owner ORDER BY owner) FROM dbd.uses \
             WHERE kind = 'schema' AND owner <> $1 GROUP BY target",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await
        .map_err(read)?;
        found.schema_users = shared.into_iter().collect();
        let recording: Vec<String> = sqlx::query_scalar("SELECT DISTINCT owner FROM dbd.uses WHERE kind = 'owner'")
            .fetch_all(&self.pool)
            .await
            .map_err(read)?;
        found.recording = recording.into_iter().collect();
        Ok(found)
    }

    /// Release `owner`'s claims of what is neither among `declared` nor in
    /// the catalog any more — in `tx`, the run's open batch, when there is
    /// one, so it sees the batch's own drops; else in a transaction of its
    /// own. Waits at most [`HEAL_LOCK_TIMEOUT`] for the registry.
    pub(super) async fn release_objects(
        &self,
        tx: Option<&mut Transaction<'static, Postgres>>,
        owner: &str,
        declared: &[crate::adapter::OwnedObject],
    ) -> Result<()> {
        match tx {
            Some(tx) => release_in(tx, owner, declared).await,
            None => {
                let mut own = self.registry_tx("release").await?;
                release_in(&mut own, owner, declared).await?;
                own.commit()
                    .await
                    .map_err(|e| DbdError::Config(format!("release: commit failed: {e}")))
            }
        }
    }

    /// Other owners' dependences on `guarded`, from the catalog's dependency
    /// graph — whatever a `CASCADE` would reach, a dependent's own dependents
    /// included — and from `dbd.uses`, which sees into routine bodies the
    /// catalog does not. Reads only; a database with no registry has none.
    pub(super) async fn dependents(
        &self,
        owner: &str,
        guarded: &[crate::adapter::Guarded],
        mine: &[crate::adapter::OwnedObject],
    ) -> Result<Vec<crate::adapter::Dependence>> {
        use crate::adapter::GuardedPart;
        let read = |e: sqlx::Error| DbdError::Config(format!("uses guard read failed: {e}"));
        let registry: Option<String> = sqlx::query_scalar("SELECT to_regclass('dbd.objects')::text")
            .fetch_one(&self.pool)
            .await
            .map_err(read)?;
        if registry.is_none() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for (i, g) in guarded.iter().enumerate() {
            let (part, part_name) = match &g.part {
                GuardedPart::Whole => ("whole", ""),
                GuardedPart::Column(c) => ("column", c.as_str()),
                GuardedPart::Constraint(c) => ("constraint", c.as_str()),
                GuardedPart::Index(n) => ("index", n.as_str()),
                GuardedPart::Truncate => ("truncate", ""),
            };
            // Each dependent once, by owner and name, with its schema.
            let rows: Vec<(Option<String>, String, String)> = sqlx::query_as(DEPENDENTS_SQL)
                .bind(&g.object.namespace)
                .bind(&g.object.schema)
                .bind(&g.object.name)
                .bind(part)
                .bind(part_name)
                .bind(owner)
                .bind(mine.iter().map(|o| o.namespace.clone()).collect::<Vec<_>>())
                .bind(mine.iter().map(|o| o.schema.clone()).collect::<Vec<_>>())
                .bind(mine.iter().map(|o| o.name.clone()).collect::<Vec<_>>())
                .fetch_all(&self.pool)
                .await
                .map_err(read)?;
            let mut seen: std::collections::BTreeMap<(Option<String>, String), String> = rows
                .into_iter()
                .map(|(owner, via, schema)| ((owner, via), schema))
                .collect();
            // A schema's uses are its entities' uses.
            let uses: Vec<(Option<String>, String)> = sqlx::query_as(
                "SELECT owner, via FROM dbd.uses WHERE kind = 'entity' AND owner <> $2 \
                   AND (target = $1 OR ($4 AND (starts_with(target, 'relation:' || $5 || '.') \
                                                OR starts_with(target, 'routine:' || $5 || '.')))) \
                   AND ($3 OR via LIKE 'function %' OR via LIKE 'procedure %')",
            )
            .bind(format!("{}:{}.{}", g.object.namespace, g.object.schema, g.object.name))
            .bind(owner)
            .bind(part == "whole")
            .bind(g.object.namespace == "schema")
            .bind(&g.object.schema)
            .fetch_all(&self.pool)
            .await
            .map_err(read)?;
            // A truncate drops nothing a use could name.
            if part != "truncate" {
                for key in uses {
                    seen.entry(key).or_default();
                }
            }
            found.extend(
                seen.into_iter()
                    .map(|((owner, via), schema)| crate::adapter::Dependence {
                        guarded: i,
                        owner,
                        via,
                        schema,
                    }),
            );
        }
        Ok(found)
    }

    /// The other owners that record a shared use of `target` of `kind`.
    pub(super) async fn users_of(&self, owner: &str, kind: &str, target: &str) -> Result<Vec<String>> {
        let registry: Option<String> = sqlx::query_scalar("SELECT to_regclass('dbd.uses')::text")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("uses read failed: {e}")))?;
        if registry.is_none() {
            return Ok(Vec::new());
        }
        sqlx::query_scalar(
            "SELECT DISTINCT owner FROM dbd.uses WHERE kind = $1 AND target = $2 AND owner <> $3 ORDER BY 1",
        )
        .bind(kind)
        .bind(target)
        .bind(owner)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("uses read failed: {e}")))
    }

    /// What `schema` holds that is not `owner`'s, `mine` counted as its own.
    pub(super) async fn not_owned_in_schema(
        &self,
        owner: &str,
        schema: &str,
        mine: &[crate::adapter::OwnedObject],
    ) -> Result<Vec<crate::adapter::SchemaMember>> {
        let read = |e: sqlx::Error| DbdError::Config(format!("schema contents read failed: {e}"));
        let registry: Option<String> = sqlx::query_scalar("SELECT to_regclass('dbd.objects')::text")
            .fetch_one(&self.pool)
            .await
            .map_err(read)?;
        let column = |f: fn(&crate::adapter::OwnedObject) -> &str| -> Vec<String> {
            mine.iter().map(|o| f(o).to_string()).collect()
        };
        // Without a registry nothing is anyone's: every object not declared is
        // the schema's to keep.
        let owners = if registry.is_some() {
            "(SELECT o.owner FROM dbd.objects o WHERE o.namespace = x.namespace AND o.schema = $2 AND o.name = x.name)"
        } else {
            "NULL::varchar"
        };
        // An extension's members — `deptype = 'e'` — are listed as the
        // extension, once, whatever it is called; and what is part of another
        // object — `deptype = 'i'`, a range type's constructors — as that
        // object. Not a composite type: its relation is part of its type, and
        // the two are one entity.
        let member = |class: &str, oid: &str| {
            format!(
                "NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = '{class}'::regclass \
                 AND e.objid = {oid} AND e.objsubid = 0 AND (e.deptype = 'e' OR (e.deptype = 'i' \
                 AND NOT (e.classid = 'pg_class'::regclass AND e.refclassid = 'pg_type'::regclass))))"
            )
        };
        // A partition of a table that is `owner`'s — declared or claimed —
        // goes with its parent's drop.
        let own_parent = format!(
            "EXISTS (SELECT 1 FROM pg_inherits i JOIN pg_class p ON p.oid = i.inhparent \
               JOIN pg_namespace pn ON pn.oid = p.relnamespace WHERE i.inhrelid = c.oid AND ( \
               EXISTS (SELECT 1 FROM unnest($3::text[], $4::text[], $5::text[]) AS m(namespace, schema, name) \
                       WHERE m.namespace = 'relation' AND m.schema = pn.nspname AND m.name = p.relname) {claimed}))",
            claimed = if registry.is_some() {
                "OR EXISTS (SELECT 1 FROM dbd.objects o WHERE o.namespace = 'relation' AND o.schema = pn.nspname \
                 AND o.name = p.relname AND o.owner = $1)"
            } else {
                ""
            }
        );
        let sql = format!(
            "SELECT DISTINCT x.kind, x.name, {owners} AS owner \
             FROM ( \
               SELECT 'relation' AS namespace, c.relname AS name, \
                      CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' \
                                     WHEN 'S' THEN 'sequence' WHEN 'f' THEN 'foreign table' \
                                     WHEN 'c' THEN 'type' ELSE 'table' END AS kind \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = $2 AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c') AND {class_member} \
                 AND NOT (c.relispartition AND {own_parent}) \
                 AND NOT (c.relkind = 'S' AND EXISTS (SELECT 1 FROM pg_depend d \
                          WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid \
                            AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i'))) \
               UNION ALL \
               SELECT 'relation', t.typname, 'type' FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
               WHERE n.nspname = $2 AND t.typrelid = 0 AND t.typelem = 0 AND t.typtype IN ('e', 'd', 'r', 'm', 'b') \
                 AND {type_member} \
               UNION ALL \
               SELECT 'routine', p.proname, CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END \
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $2 AND {proc_member} \
               UNION ALL \
               SELECT 'extension', x.extname, 'extension' \
               FROM pg_extension x JOIN pg_namespace n ON n.oid = x.extnamespace WHERE n.nspname = $2 \
             ) x \
             WHERE NOT EXISTS (SELECT 1 FROM unnest($3::text[], $4::text[], $5::text[]) AS m(namespace, schema, name) \
                               WHERE m.namespace = x.namespace AND m.schema = $2 AND m.name = x.name) \
               AND {owners} IS DISTINCT FROM $1 \
             ORDER BY 1, 2",
            class_member = member("pg_class", "c.oid"),
            type_member = member("pg_type", "t.oid"),
            proc_member = member("pg_proc", "p.oid"),
        );
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(&sql)
            .bind(owner)
            .bind(schema)
            .bind(column(|o| &o.namespace))
            .bind(column(|o| &o.schema))
            .bind(column(|o| &o.name))
            .fetch_all(&self.pool)
            .await
            .map_err(read)?;
        Ok(rows
            .into_iter()
            .map(|(kind, name, owner)| crate::adapter::SchemaMember { kind, name, owner })
            .collect())
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

/// How long a run waits for the database's dbd write lock — for another
/// project's run to finish — before it gives up. A run holds it from its
/// first write to its last, an apply's data load included.
const WRITE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How often a run waiting for the write lock asks for it again.
const WRITE_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(100);

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

/// The ones of `objects` another owner than `owner` holds in `dbd.objects`.
async fn held_objects(
    conn: &mut sqlx::PgConnection,
    owner: &str,
    objects: &[crate::adapter::OwnedObject],
) -> Result<Vec<crate::adapter::HeldObject>> {
    let column = |f: fn(&crate::adapter::OwnedObject) -> &str| -> Vec<String> {
        objects.iter().map(|o| f(o).to_string()).collect()
    };
    let rows = sqlx::query(
        "SELECT o.namespace, o.schema, o.name, o.kind, o.owner FROM dbd.objects o \
         JOIN unnest($1::text[], $2::text[], $3::text[]) AS d(namespace, schema, name) \
           ON o.namespace = d.namespace AND o.schema = d.schema AND o.name = d.name \
         WHERE o.owner <> $4 ORDER BY o.namespace, o.schema, o.name",
    )
    .bind(column(|o| &o.namespace))
    .bind(column(|o| &o.schema))
    .bind(column(|o| &o.name))
    .bind(owner)
    .fetch_all(conn)
    .await
    .map_err(|e| registry_error(&e, "dbd.objects", "ownership read"))?;
    Ok(rows
        .iter()
        .map(|r| crate::adapter::HeldObject {
            object: crate::adapter::OwnedObject {
                namespace: r.get("namespace"),
                schema: r.get("schema"),
                name: r.get("name"),
                kind: r.get("kind"),
            },
            owner: r.get("owner"),
        })
        .collect())
}

/// The rows an entity use is looked up by: one per schema it may be in, `rank`
/// its place on the search path, `used` the use it stands for, `bare` whether
/// it was named without a schema.
#[derive(Default)]
struct Candidates {
    used: Vec<i32>,
    rank: Vec<i32>,
    namespace: Vec<String>,
    schema: Vec<String>,
    name: Vec<String>,
    via: Vec<String>,
    bare: Vec<bool>,
}

/// Bound how long `tx` waits for a lock on `table` — [`HEAL_LOCK_TIMEOUT`]
/// while `on`, and back to the transaction's own setting after: a batch's DDL
/// keeps whatever wait it had.
async fn bounded_wait(tx: &mut Transaction<'static, Postgres>, table: &str, on: bool) -> Result<()> {
    use sqlx::Executor as _;
    let sql = if on {
        format!("SET LOCAL lock_timeout = '{HEAL_LOCK_TIMEOUT}'")
    } else {
        "SET LOCAL lock_timeout TO DEFAULT".to_string()
    };
    (&mut **tx)
        .execute(sql.as_str())
        .await
        .map_err(|e| registry_error(&e, table, "registry wait"))?;
    Ok(())
}

/// A registry statement's failure: a lock held past [`HEAL_LOCK_TIMEOUT`],
/// and a write another run made after this transaction's snapshot, say so.
fn registry_error(e: &sqlx::Error, table: &str, what: &str) -> DbdError {
    DbdError::Config(match sqlstate(e).as_deref() {
        Some("55P03") => format!(
            "{what}: {table} is held by another session's open write, which did not finish \
             within {HEAL_LOCK_TIMEOUT} — let it finish, or end it, and run again"
        ),
        Some("40001") => format!("{what}: another run wrote {table} at the same time; run again"),
        _ => format!("{what} failed: {e}"),
    })
}

/// [`Bookkeeping::release_objects`]'s statement, in `tx`.
async fn release_in(
    tx: &mut Transaction<'static, Postgres>,
    owner: &str,
    declared: &[crate::adapter::OwnedObject],
) -> Result<()> {
    let column = |f: fn(&crate::adapter::OwnedObject) -> &str| -> Vec<String> {
        declared.iter().map(|o| f(o).to_string()).collect()
    };
    bounded_wait(tx, "dbd.objects", true).await?;
    sqlx::query(
        "DELETE FROM dbd.objects o WHERE o.owner = $1 \
           AND NOT EXISTS (SELECT 1 FROM unnest($2::text[], $3::text[], $4::text[]) AS d(namespace, schema, name) \
                           WHERE d.namespace = o.namespace AND d.schema = o.schema AND d.name = o.name) \
           AND NOT CASE o.namespace \
             WHEN 'routine' THEN EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                                         WHERE n.nspname = o.schema AND p.proname = o.name) \
             ELSE EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                          WHERE n.nspname = o.schema AND c.relname = o.name) \
               OR EXISTS (SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
                          WHERE n.nspname = o.schema AND t.typname = o.name) \
           END",
    )
    .bind(owner)
    .bind(column(|o| &o.namespace))
    .bind(column(|o| &o.schema))
    .bind(column(|o| &o.name))
    .execute(&mut **tx)
    .await
    .map_err(|e| registry_error(&e, "dbd.objects", "release"))?;
    bounded_wait(tx, "dbd.objects", false).await
}

/// What depends on one guarded entity, owned by another owner: `$1..$3` the
/// entity's namespace — `relation`, `routine`, or `schema` or `extension` for
/// the whole of one — schema and name; `$4` what of it goes — `whole`,
/// `column`, `constraint` or `index` — and `$5` that part's name; `$6` the
/// owner asking.
///
/// Starts where a drop starts: a table with its row type and that type's
/// array; a type and its array; a column, with the same column of every table
/// inheriting it — dropped with it, through inheritance rather than the
/// dependency graph; a constraint, with the index behind a primary key, unique
/// or exclusion constraint (a foreign key's is the referenced table's, which
/// dropping it leaves alone); an index; every overload of a routine; all of a
/// schema; an extension's members. Walks `pg_depend` dependent by dependent,
/// each as itself — a column as its column, a constraint, default, trigger or
/// policy as itself — so a part reaches what depends on the part and not on
/// its table; a view's rewrite rule stands for the whole view, which is what
/// depends on the view, and a generated column's expression leads to the
/// column it is part of. A `truncate` reaches what it empties and walks no
/// further, dropping nothing: the table, every table inheriting from or a
/// partition of one it empties, and every table whose foreign key points at
/// one it empties. Each dependent is named by its owning
/// relation, type or routine. A schema starts from all it holds: its
/// relations, its types — row and array types included, so a column of one
/// is reached — its routines, and its collations, text search objects,
/// operators, operator classes and families, and conversions — a column
/// collated by one goes with it. `$7`–`$9`: the asking owner's declared
/// entities, its own claimed or not. Rows: the other owner — none for an
/// entity nobody owns that stands on its own (a composite type does) and is
/// neither the one dropped nor declared by the owner asking — the dependent as
/// `kind schema.name`, and its schema.
const DEPENDENTS_SQL: &str = "\
WITH RECURSIVE \
rel AS (SELECT c.oid, c.reltype FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
        WHERE $1 = 'relation' AND n.nspname = $2 AND c.relname = $3), \
typ AS (SELECT t.oid, t.typarray FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
        WHERE $1 = 'relation' AND n.nspname = $2 AND t.typname = $3 AND t.typrelid = 0), \
inh(child) AS ( \
  SELECT i.inhrelid FROM pg_inherits i JOIN rel ON i.inhparent = rel.oid WHERE $4 = 'column' \
  UNION SELECT i.inhrelid FROM pg_inherits i JOIN inh ON i.inhparent = inh.child \
), \
emptied(member) AS ( \
  SELECT rel.oid FROM rel WHERE $4 = 'truncate' \
  UNION \
  SELECT x.member FROM emptied e CROSS JOIN LATERAL ( \
    SELECT i.inhrelid FROM pg_inherits i WHERE i.inhparent = e.member \
    UNION ALL \
    SELECT k.conrelid FROM pg_constraint k WHERE k.confrelid = e.member AND k.contype = 'f' \
  ) x(member) \
), \
start(classid, objid, objsubid) AS ( \
  SELECT 'pg_class'::regclass::oid, rel.oid, 0 FROM rel WHERE $4 = 'whole' \
  UNION ALL SELECT 'pg_type'::regclass::oid, rel.reltype, 0 FROM rel WHERE $4 = 'whole' AND rel.reltype <> 0 \
  UNION ALL SELECT 'pg_type'::regclass::oid, t.typarray, 0 FROM rel JOIN pg_type t ON t.oid = rel.reltype \
            WHERE $4 = 'whole' AND t.typarray <> 0 \
  UNION ALL SELECT 'pg_type'::regclass::oid, typ.oid, 0 FROM typ WHERE $4 = 'whole' \
  UNION ALL SELECT 'pg_type'::regclass::oid, typ.typarray, 0 FROM typ WHERE $4 = 'whole' AND typ.typarray <> 0 \
  UNION ALL SELECT 'pg_class'::regclass::oid, rel.oid, a.attnum FROM rel \
            JOIN pg_attribute a ON a.attrelid = rel.oid \
            WHERE $4 = 'column' AND a.attname = $5 AND NOT a.attisdropped \
  UNION ALL SELECT 'pg_class'::regclass::oid, inh.child, a.attnum FROM inh \
            JOIN pg_attribute a ON a.attrelid = inh.child \
            WHERE a.attname = $5 AND NOT a.attisdropped AND NOT a.attislocal \
  UNION ALL SELECT 'pg_constraint'::regclass::oid, k.oid, 0 FROM rel \
            JOIN pg_constraint k ON k.conrelid = rel.oid WHERE $4 = 'constraint' AND k.conname = $5 \
  UNION ALL SELECT 'pg_class'::regclass::oid, k.conindid, 0 FROM rel \
            JOIN pg_constraint k ON k.conrelid = rel.oid \
            WHERE $4 = 'constraint' AND k.conname = $5 AND k.contype IN ('p', 'u', 'x') AND k.conindid <> 0 \
  UNION ALL SELECT 'pg_class'::regclass::oid, i.oid, 0 FROM pg_class i JOIN pg_namespace n ON n.oid = i.relnamespace \
            WHERE $4 = 'index' AND $1 = 'relation' AND n.nspname = $2 AND i.relname = $5 AND i.relkind IN ('i', 'I') \
  UNION ALL SELECT 'pg_proc'::regclass::oid, p.oid, 0 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
            WHERE $1 = 'routine' AND n.nspname = $2 AND p.proname = $3 \
  UNION ALL SELECT 'pg_class'::regclass::oid, c.oid, 0 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE $1 = 'schema' AND n.nspname = $2 AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c') \
  UNION ALL SELECT 'pg_type'::regclass::oid, t.oid, 0 FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
            WHERE $1 = 'schema' AND n.nspname = $2 \
  UNION ALL SELECT 'pg_proc'::regclass::oid, p.oid, 0 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
            WHERE $1 = 'schema' AND n.nspname = $2 \
  UNION ALL SELECT x.classid, x.objid, 0 FROM pg_namespace n CROSS JOIN LATERAL ( \
              SELECT 'pg_collation'::regclass::oid, oid FROM pg_collation WHERE collnamespace = n.oid \
              UNION ALL SELECT 'pg_ts_config'::regclass::oid, oid FROM pg_ts_config WHERE cfgnamespace = n.oid \
              UNION ALL SELECT 'pg_ts_dict'::regclass::oid, oid FROM pg_ts_dict WHERE dictnamespace = n.oid \
              UNION ALL SELECT 'pg_ts_parser'::regclass::oid, oid FROM pg_ts_parser WHERE prsnamespace = n.oid \
              UNION ALL SELECT 'pg_ts_template'::regclass::oid, oid FROM pg_ts_template WHERE tmplnamespace = n.oid \
              UNION ALL SELECT 'pg_operator'::regclass::oid, oid FROM pg_operator WHERE oprnamespace = n.oid \
              UNION ALL SELECT 'pg_opclass'::regclass::oid, oid FROM pg_opclass WHERE opcnamespace = n.oid \
              UNION ALL SELECT 'pg_opfamily'::regclass::oid, oid FROM pg_opfamily WHERE opfnamespace = n.oid \
              UNION ALL SELECT 'pg_conversion'::regclass::oid, oid FROM pg_conversion WHERE connamespace = n.oid \
            ) x(classid, objid) WHERE $1 = 'schema' AND n.nspname = $2 \
  UNION ALL SELECT d.classid, d.objid, d.objsubid FROM pg_extension e \
            JOIN pg_depend d ON d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid AND d.deptype = 'e' \
            WHERE $1 = 'extension' AND e.extname = $3 \
  UNION ALL SELECT 'pg_class'::regclass::oid, emptied.member, 0 FROM emptied \
), \
walk(classid, objid, objsubid) AS ( \
  SELECT classid, objid, objsubid FROM start \
  UNION \
  SELECT x.classid, x.objid, x.objsubid FROM walk w \
  CROSS JOIN LATERAL ( \
    SELECT CASE WHEN d.classid = 'pg_rewrite'::regclass THEN 'pg_class'::regclass::oid ELSE d.classid END, \
           CASE WHEN d.classid = 'pg_rewrite'::regclass \
                THEN (SELECT r.ev_class FROM pg_rewrite r WHERE r.oid = d.objid) ELSE d.objid END, \
           CASE WHEN d.classid = 'pg_rewrite'::regclass THEN 0 ELSE d.objsubid END \
    FROM pg_depend d \
    WHERE d.refclassid = w.classid AND d.refobjid = w.objid \
      AND (w.objsubid = 0 OR d.refobjsubid = w.objsubid) AND d.deptype IN ('n', 'a') \
    UNION ALL \
    SELECT d.refclassid, d.refobjid, d.refobjsubid FROM pg_depend d \
    WHERE w.classid = 'pg_attrdef'::regclass AND d.classid = w.classid AND d.objid = w.objid AND d.deptype = 'i' \
  ) x(classid, objid, objsubid) \
  WHERE $4 <> 'truncate' \
), \
owning(classid, objid) AS ( \
  SELECT CASE WHEN w.classid IN ('pg_constraint'::regclass, 'pg_trigger'::regclass, 'pg_attrdef'::regclass, \
                                 'pg_policy'::regclass) THEN 'pg_class'::regclass::oid ELSE w.classid END, \
         CASE w.classid \
           WHEN 'pg_constraint'::regclass THEN (SELECT k.conrelid FROM pg_constraint k WHERE k.oid = w.objid) \
           WHEN 'pg_trigger'::regclass THEN (SELECT t.tgrelid FROM pg_trigger t WHERE t.oid = w.objid) \
           WHEN 'pg_attrdef'::regclass THEN (SELECT a.adrelid FROM pg_attrdef a WHERE a.oid = w.objid) \
           WHEN 'pg_policy'::regclass THEN (SELECT p.polrelid FROM pg_policy p WHERE p.oid = w.objid) \
           ELSE w.objid END \
  FROM walk w \
  UNION \
  SELECT 'pg_type'::regclass::oid, k.contypid FROM walk w JOIN pg_constraint k \
         ON w.classid = 'pg_constraint'::regclass AND k.oid = w.objid AND k.conrelid = 0 AND k.contypid <> 0 \
), \
found(namespace, schema, name, kind, classid, objid) AS ( \
  SELECT 'relation', n.nspname, c.relname, \
         CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' WHEN 'S' THEN 'sequence' \
                        WHEN 'f' THEN 'foreign table' WHEN 'c' THEN 'type' ELSE 'table' END, \
         'pg_class'::regclass::oid, c.oid \
  FROM owning w JOIN pg_class c0 ON w.classid = 'pg_class'::regclass AND c0.oid = w.objid \
  JOIN pg_class c ON c.oid = CASE WHEN c0.relkind IN ('i', 'I') \
                                  THEN (SELECT x.indrelid FROM pg_index x WHERE x.indexrelid = c0.oid) \
                                  ELSE c0.oid END \
  JOIN pg_namespace n ON n.oid = c.relnamespace \
  UNION \
  SELECT 'relation', n.nspname, COALESCE(c.relname, t.typname), \
         CASE WHEN c.oid IS NULL THEN 'type' WHEN c.relkind = 'v' THEN 'view' \
              WHEN c.relkind = 'm' THEN 'materialized view' ELSE 'table' END, \
         CASE WHEN c.oid IS NULL THEN 'pg_type'::regclass::oid ELSE 'pg_class'::regclass::oid END, \
         COALESCE(c.oid, t.oid) \
  FROM owning w JOIN pg_type t ON w.classid = 'pg_type'::regclass AND t.oid = w.objid \
  LEFT JOIN pg_class c ON c.oid = t.typrelid \
  JOIN pg_namespace n ON n.oid = COALESCE(c.relnamespace, t.typnamespace) \
  UNION \
  SELECT 'routine', n.nspname, p.proname, CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END, \
         'pg_proc'::regclass::oid, p.oid \
  FROM owning w JOIN pg_proc p ON w.classid = 'pg_proc'::regclass AND p.oid = w.objid \
  JOIN pg_namespace n ON n.oid = p.pronamespace \
) \
SELECT DISTINCT o.owner, f.kind || ' ' || f.schema || '.' || f.name, f.schema \
FROM found f LEFT JOIN dbd.objects o ON o.namespace = f.namespace AND o.schema = f.schema AND o.name = f.name \
WHERE o.owner IS DISTINCT FROM $6 \
  AND (o.owner IS NOT NULL OR ( \
    NOT (f.namespace = $1 AND f.schema = $2 AND f.name = $3) AND NOT ($1 = 'schema' AND f.schema = $2) \
    AND NOT EXISTS (SELECT 1 FROM unnest($7::text[], $8::text[], $9::text[]) AS m(namespace, schema, name) \
                    WHERE m.namespace = f.namespace AND m.schema = f.schema AND m.name = f.name) \
    AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = f.classid AND d.objid = f.objid \
                    AND d.objsubid = 0 AND d.deptype IN ('i', 'a', 'e') \
                    AND NOT (d.deptype = 'i' AND d.classid = 'pg_class'::regclass \
                             AND d.refclassid = 'pg_type'::regclass))))";
