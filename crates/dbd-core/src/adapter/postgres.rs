#![cfg(feature = "postgres")]

use std::path::Path;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use super::{CatalogData, DatabaseAdapter, ProjectMeta, ReferenceClass};

/// Ensure `public` is included in any SET search_path statement.
/// DDL files often set search_path to specific schemas (e.g., `sensei, extensions`)
/// but omit `public`, which hides extensions installed in the public schema.
fn ensure_public_in_search_path(sql: &str) -> String {
    let re = regex::Regex::new(r"(?i)(set\s+search_path\s+to\s+)([^;]+)(;)").unwrap();
    re.replace_all(sql, |caps: &regex::Captures| {
        let prefix = &caps[1];
        let schemas = &caps[2];
        let suffix = &caps[3];
        if schemas.to_lowercase().contains("public") {
            format!("{prefix}{schemas}{suffix}")
        } else {
            format!("{prefix}{schemas}, public{suffix}")
        }
    })
    .to_string()
}
use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};
use crate::script;

/// Convert a Postgres `confdeltype`/`confupdtype` single character to `FkAction`.
/// 'a' = NO ACTION (unspecified — return None so the emitter omits the clause),
/// 'r' = RESTRICT, 'c' = CASCADE, 'n' = SET NULL, 'd' = SET DEFAULT
fn pg_conf_action(code: &str) -> Option<crate::entity::FkAction> {
    use crate::entity::FkAction;
    match code {
        "a" => None, // NO ACTION is the default; omit to avoid spurious diffs
        "r" => Some(FkAction::Restrict),
        "c" => Some(FkAction::Cascade),
        "n" => Some(FkAction::SetNull),
        "d" => Some(FkAction::SetDefault),
        _ => None,
    }
}

/// Schema that owns dbd's internal jsonb-import machinery. The
/// `import_jsonb_to_table` procedure is always created here (see
/// [`PostgresAdapter::ensure_import_procedure`]), so the `CALL` is pinned to
/// this schema — never the target table's schema, which has no such procedure.
const JSONB_IMPORT_SCHEMA: &str = "staging";

/// Fully-qualified, dbd-owned staging table that jsonl rows are loaded into
/// before `import_jsonb_to_table` moves them to the target. Schema-qualified so
/// it resolves regardless of the session `search_path`, and `_dbd_`-prefixed so
/// it never collides with a user table. Pooled connections don't share
/// `search_path`, so an unqualified `_temp` resolves nondeterministically — the
/// same failure mode fixed for `_dbd_meta` (see [`PostgresAdapter::bookkeeping_schema`]).
/// See sensei-hq/dbd#6.
const JSONB_IMPORT_TMP: &str = "staging._dbd_import_tmp";

/// `CALL` that moves staged rows into `target` via the internal import
/// procedure. Both the procedure schema and the source table are pinned and
/// fully-qualified so resolution never depends on the session `search_path`.
/// `target` must be the schema-qualified destination table.
fn jsonb_import_call_sql(target: &str) -> String {
    format!(
        "CALL {JSONB_IMPORT_SCHEMA}.import_jsonb_to_table('{JSONB_IMPORT_TMP}', '{}')",
        target.replace('\'', "''")
    )
}

/// PostgreSQL adapter using sqlx connection pool.
pub struct PostgresAdapter {
    pool: PgPool,
    project: String,
    catalog: CatalogData,
    /// SHA-256 of the connection URL, used as cache key.
    url_hash: String,
    /// Open batch transaction, if `begin_batch` has been called. While `Some`,
    /// DDL and meta writes route through it so `Design::apply` is atomic.
    /// `Pool::begin` yields a `'static` transaction (it owns a pooled
    /// connection), so it can be held across the whole apply loop.
    batch: tokio::sync::Mutex<Option<sqlx::Transaction<'static, sqlx::Postgres>>>,
}

impl PostgresAdapter {
    /// Create a new adapter from a connection URL.
    pub async fn new(url: &str, project: &str) -> Result<Self> {
        use sha2::{Digest, Sha256};

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| DbdError::Config(format!("Database connection failed: {e}")))?;

        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        let url_hash = format!("{:x}", hasher.finalize());

        Ok(Self {
            pool,
            project: project.to_string(),
            catalog: CatalogData::default(),
            url_hash,
            batch: tokio::sync::Mutex::new(None),
        })
    }

    /// Execute raw SQL against the open batch transaction if one exists, else
    /// against the pool. Kept as an inherent async fn (not a trait method) so
    /// the `Executor` bound resolves without the HRTB error that `#[async_trait]`
    /// desugaring produces for `raw_sql` against `&mut PgConnection`.
    async fn exec_raw(&self, sql: &str) -> Result<()> {
        use sqlx::Executor as _;
        let mut guard = self.batch.lock().await;
        if let Some(mut tx) = guard.take() {
            // Inside a batch transaction, execute via `Executor::execute(&str)`.
            // A bare `&str` carries no bind arguments, so sqlx uses the simple
            // query protocol — multi-statement DDL runs as one unit, just like
            // `raw_sql` on the pool below. Unlike `raw_sql`, this future is
            // `Send` against `&mut PgConnection`, which `#[async_trait]` requires.
            // We own the transaction locally while executing, then put it back.
            let result = (&mut *tx).execute(sql).await;
            *guard = Some(tx);
            result.map_err(|e| DbdError::Config(format!("SQL execution failed: {e}")))?;
        } else {
            sqlx::raw_sql(sql)
                .execute(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("SQL execution failed: {e}")))?;
        }
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

    /// Static pattern matching for reference classification (offline fallback).
    fn matches_static_pattern(name: &str) -> bool {
        let lower = name.to_lowercase();
        let patterns = [
            "pg_", "information_schema.", "array_", "json_", "jsonb_",
            "regexp_", "current_", "gen_random_", "to_", "date_", "time_",
            "string_to_", "lo_", "xml_",
        ];
        patterns.iter().any(|p| lower.starts_with(p))
    }

    /// Apply an enum DDL idempotently.
    ///
    /// PostgreSQL's CREATE TYPE fails if the type already exists.
    /// Wrap in a DO block that checks pg_type first.
    async fn apply_enum(&self, entity: &Entity, sql: &str) -> Result<()> {
        let parts: Vec<&str> = entity.name.split('.').collect();
        let (schema, type_name) = if parts.len() > 1 {
            (parts[0], parts[1])
        } else {
            ("public", parts[0])
        };

        // Check if the type already exists
        let exists = sqlx::query(
            "SELECT 1 FROM pg_type t JOIN pg_namespace n ON t.typnamespace = n.oid \
             WHERE n.nspname = $1 AND t.typname = $2 AND t.typtype = 'e'"
        )
        .bind(schema)
        .bind(type_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Enum check failed: {e}")))?;

        if exists.is_some() {
            // Type already exists — skip (idempotent)
            return Ok(());
        }

        // Extract the SET search_path and CREATE TYPE from the DDL
        self.execute_script(sql).await
    }

    fn cache_path(&self) -> std::path::PathBuf {
        let base = dirs::cache_dir().unwrap_or_else(|| std::path::PathBuf::from(".cache"));
        base.join("dbd")
            .join("catalog")
            .join(format!("{}.json", self.url_hash))
    }

    fn load_catalog_cache(&self) -> Option<CatalogData> {
        let path = self.cache_path();
        let content = std::fs::read_to_string(&path).ok()?;

        #[derive(serde::Deserialize)]
        struct CacheFile {
            created_at: String,
            ttl_hours: u32,
            data: CatalogData,
        }

        let cache: CacheFile = serde_json::from_str(&content).ok()?;

        // Check TTL (allow override via env var)
        let ttl_hours = std::env::var("DBD_CATALOG_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(cache.ttl_hours);

        let created = chrono::DateTime::parse_from_rfc3339(&cache.created_at).ok()?;
        let age = chrono::Utc::now().signed_duration_since(created);
        if age.num_hours() >= ttl_hours as i64 {
            return None; // Stale
        }

        Some(cache.data)
    }

    fn save_catalog_cache(&self) {
        let path = self.cache_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let cache = serde_json::json!({
            "created_at": chrono::Utc::now().to_rfc3339(),
            "ttl_hours": 24,
            "data": &self.catalog,
        });

        std::fs::write(
            &path,
            serde_json::to_string_pretty(&cache).unwrap_or_default(),
        )
        .ok();
    }

    // ── Schema filter shared by all introspect_* helpers ──────────────────────

    /// Returns the SQL WHERE clause fragment that filters out system schemas.
    fn schema_filter_column(col: &str) -> String {
        // Keep in sync with `reverse::ALWAYS_EXCLUDED` / `reverse::is_internal`.
        format!(
            "{col} NOT IN ('pg_catalog', 'information_schema') \
             AND {col} NOT LIKE 'pg_toast%' \
             AND {col} NOT LIKE 'pg_temp%'"
        )
    }

    // ── Private introspection helpers ──────────────────────────────────────────

    async fn introspect_schemas(&self) -> crate::error::Result<Vec<Entity>> {
        let filter = Self::schema_filter_column("nspname");
        let sql = format!(
            "SELECT nspname FROM pg_namespace WHERE {filter} ORDER BY nspname"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_schemas failed: {e}")))?;

        let entities = rows
            .iter()
            .map(|row| {
                let nspname: String = row.get("nspname");
                Entity::new(EntityType::Schema, &nspname)
            })
            .collect();
        Ok(entities)
    }

    async fn introspect_extensions(&self) -> crate::error::Result<Vec<Entity>> {
        let ns_filter = Self::schema_filter_column("n.nspname");
        let sql = format!(
            "SELECT e.extname, n.nspname \
             FROM pg_extension e \
             JOIN pg_namespace n ON e.extnamespace = n.oid \
             WHERE {ns_filter} \
             ORDER BY n.nspname, e.extname"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_extensions failed: {e}")))?;

        let entities = rows
            .iter()
            .map(|row| {
                let extname: String = row.get("extname");
                let nspname: String = row.get("nspname");
                let mut e = Entity::new(EntityType::Extension, &extname);
                e.schema = Some(nspname);
                e
            })
            .collect();
        Ok(entities)
    }

    async fn introspect_enums(&self) -> crate::error::Result<Vec<Entity>> {
        use crate::entity::EnumValue;

        let ns_filter = Self::schema_filter_column("n.nspname");
        // Fetch enums and their ordered values in one query
        let sql = format!(
            "SELECT n.nspname, t.typname, e.enumlabel \
             FROM pg_type t \
             JOIN pg_namespace n ON t.typnamespace = n.oid \
             JOIN pg_enum e ON e.enumtypid = t.oid \
             WHERE t.typtype = 'e' AND {ns_filter} \
             ORDER BY n.nspname, t.typname, e.enumsortorder"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_enums failed: {e}")))?;

        // Group by (schema, typname)
        let mut map: indexmap::IndexMap<(String, String), Vec<EnumValue>> =
            indexmap::IndexMap::new();
        for row in &rows {
            let schema: String = row.get("nspname");
            let typname: String = row.get("typname");
            let label: String = row.get("enumlabel");
            map.entry((schema, typname))
                .or_default()
                .push(EnumValue { name: label, note: None });
        }

        let entities = map
            .into_iter()
            .map(|((schema, typname), values)| {
                let mut e = Entity::new(EntityType::Enum, &format!("{schema}.{typname}"));
                e.enum_values = values;
                e
            })
            .collect();
        Ok(entities)
    }

    async fn introspect_tables(&self) -> crate::error::Result<Vec<Entity>> {
        use crate::entity::{TableComments, TableDef};

        let ns_filter = Self::schema_filter_column("c.table_schema");

        // 1. Fetch all base tables in user schemas. Skip dbd's own bookkeeping
        //    tables (`_dbd_meta`, `_dbd_migrations`) — they are created/managed by
        //    `dbd apply`, not authored DDL, so they must never be reverse-engineered
        //    into the project (mirrors the sqlite adapter's exclusion).
        let tables_sql = format!(
            "SELECT c.table_schema, c.table_name \
             FROM information_schema.tables c \
             WHERE c.table_type = 'BASE TABLE' AND {ns_filter} \
             AND c.table_name NOT IN ('_dbd_meta', '_dbd_migrations') \
             ORDER BY c.table_schema, c.table_name"
        );
        let table_rows = sqlx::query(&tables_sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_tables list failed: {e}")))?;

        let mut entities: Vec<Entity> = Vec::new();

        for trow in &table_rows {
            let schema: String = trow.get("table_schema");
            let table: String = trow.get("table_name");

            let columns = self.introspect_columns(&schema, &table).await?;
            let (constraints, constraint_index_oids) =
                self.introspect_constraints(&schema, &table).await?;
            let indexes = self
                .introspect_indexes(&schema, &table, &constraint_index_oids)
                .await?;

            let table_comment = self.introspect_table_comment(&schema, &table).await?;

            // Assemble TableComments: table-level + per-column (already in ColumnDef.comment)
            let mut col_comments = std::collections::HashMap::new();
            for c in &columns {
                if let Some(cm) = &c.comment {
                    col_comments.insert(c.name.clone(), cm.clone());
                }
            }
            let comments = TableComments {
                table: table_comment,
                columns: col_comments,
            };

            let table_def = TableDef {
                columns,
                constraints,
                indexes,
                comments,
            };

            let mut entity = Entity::new(EntityType::Table, &format!("{schema}.{table}"));
            entity.table_def = Some(table_def);
            entities.push(entity);
        }

        Ok(entities)
    }

    /// Introspect a table's columns.
    ///
    /// Reads, per column:
    ///   - `attidentity` ('a'=ALWAYS, 'd'=BY DEFAULT, ''=none) → identity.
    ///   - `col_type` (format_type with typmod) → the declared type.
    ///   - `underlying_type` (atttypid::regtype) → base int type for serial mapping.
    ///   - `owns_default_seq`: whether the column's DEFAULT nextval(...) targets a
    ///     sequence OWNED BY this column (pg_depend deptype 'a'). Only those become
    ///     `serial`; a nextval default on a standalone sequence is kept verbatim.
    async fn introspect_columns(
        &self,
        schema: &str,
        table: &str,
    ) -> crate::error::Result<Vec<crate::entity::ColumnDef>> {
        use crate::entity::ColumnDef;

        let cols_sql =
            "SELECT a.attname AS column_name, \
                    a.attnotnull, \
                    a.attidentity::text AS attidentity, \
                    pg_get_expr(ad.adbin, ad.adrelid) AS column_default, \
                    format_type(a.atttypid, a.atttypmod) AS col_type, \
                    a.atttypid::regtype::text AS underlying_type, \
                    col_description(a.attrelid, a.attnum) AS col_comment, \
                    EXISTS ( \
                      SELECT 1 FROM pg_depend d \
                      WHERE d.refclassid = 'pg_class'::regclass \
                        AND d.refobjid = a.attrelid \
                        AND d.refobjsubid = a.attnum \
                        AND d.classid = 'pg_class'::regclass \
                        AND d.deptype = 'a' \
                        AND (SELECT relkind FROM pg_class WHERE oid = d.objid) = 'S' \
                    ) AS owns_default_seq \
             FROM pg_attribute a \
             JOIN pg_class cls ON cls.oid = a.attrelid \
             JOIN pg_namespace ns ON ns.oid = cls.relnamespace \
             LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
             WHERE ns.nspname = $1 AND cls.relname = $2 \
               AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum";

        let col_rows = sqlx::query(cols_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                DbdError::Config(format!("introspect_tables columns {schema}.{table}: {e}"))
            })?;

        let columns: Vec<ColumnDef> = col_rows
            .iter()
            .map(|row| {
                let name: String = row.get("column_name");
                let col_type: String = row.get("col_type");
                let attnotnull: bool = row.get("attnotnull");
                let default_value: Option<String> = row.get("column_default");
                let attidentity: String = row.get("attidentity");
                let underlying_type: String = row.get("underlying_type");
                let owns_default_seq: bool = row.get("owns_default_seq");
                let comment: Option<String> = row.get("col_comment");

                // Identity ('a' = ALWAYS, 'd' = BY DEFAULT, '' = none). Identity
                // columns carry no user default.
                let identity = match attidentity.as_str() {
                    "a" => Some(crate::entity::IdentityKind::Always),
                    "d" => Some(crate::entity::IdentityKind::ByDefault),
                    _ => None,
                };

                // Serial detection: a `nextval('…'::regclass)` default whose sequence is
                // OWNED BY this column (deptype 'a') is the `serial`/`bigserial`/
                // `smallserial` sugar — map to the serial type per the underlying int
                // width and CLEAR the default (Postgres re-creates the owned sequence).
                // A nextval default on a STANDALONE sequence (not owned) is left intact;
                // that sequence is emitted separately as EntityType::Sequence.
                let is_serial = identity.is_none()
                    && owns_default_seq
                    && default_value
                        .as_deref()
                        .is_some_and(|d| d.contains("nextval("));

                let (data_type, default_value) = if is_serial {
                    let serial = match underlying_type.as_str() {
                        "smallint" | "int2" => "smallserial",
                        "bigint" | "int8" => "bigserial",
                        // integer / int4 (and any other int alias) → serial
                        _ => "serial",
                    };
                    (serial.to_string(), None)
                } else if identity.is_some() {
                    // Identity columns: keep the declared type, drop any default.
                    (col_type, None)
                } else {
                    (col_type, default_value)
                };

                ColumnDef {
                    name,
                    data_type,
                    nullable: !attnotnull,
                    default_value,
                    is_pk: false,     // filled from constraints
                    is_unique: false, // filled from constraints
                    identity,
                    comment,
                    inline_fk: None,
                }
            })
            .collect();

        Ok(columns)
    }

    /// Introspect a table's PK/UNIQUE/FK/CHECK constraints. Also returns the set
    /// of index OIDs that back a PK/UNIQUE constraint, so `introspect_indexes`
    /// can skip re-emitting them as standalone indexes.
    async fn introspect_constraints(
        &self,
        schema: &str,
        table: &str,
    ) -> crate::error::Result<(
        Vec<crate::entity::TableConstraint>,
        std::collections::HashSet<i64>,
    )> {
        use crate::entity::{ForeignKey, TableConstraint};

        let cons_sql =
            "SELECT c.conname, c.contype::text, c.confdeltype::text, c.confupdtype::text, \
                    c.conindid::int8 AS conindid, \
                    pg_get_constraintdef(c.oid, true) AS condef, \
                    (SELECT array_agg(a.attname ORDER BY pos.ord) \
                     FROM unnest(c.conkey) WITH ORDINALITY AS pos(attnum, ord) \
                     JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = pos.attnum \
                    ) AS col_names, \
                    (SELECT array_agg(a.attname ORDER BY pos.ord) \
                     FROM unnest(c.confkey) WITH ORDINALITY AS pos(attnum, ord) \
                     JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = pos.attnum \
                    ) AS ref_col_names, \
                    ref_ns.nspname AS ref_schema, \
                    ref_cls.relname AS ref_table \
             FROM pg_constraint c \
             JOIN pg_class cls ON cls.oid = c.conrelid \
             JOIN pg_namespace ns ON ns.oid = cls.relnamespace \
             LEFT JOIN pg_class ref_cls ON ref_cls.oid = c.confrelid \
             LEFT JOIN pg_namespace ref_ns ON ref_ns.oid = ref_cls.relnamespace \
             WHERE ns.nspname = $1 AND cls.relname = $2 \
               AND c.contype IN ('p', 'u', 'f', 'c') \
             ORDER BY c.contype, c.conname";

        let con_rows = sqlx::query(cons_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                DbdError::Config(format!("introspect_tables constraints {schema}.{table}: {e}"))
            })?;

        let mut constraints: Vec<TableConstraint> = Vec::new();
        let mut constraint_index_oids: std::collections::HashSet<i64> =
            std::collections::HashSet::new();

        for con in &con_rows {
            let conname: String = con.get("conname");
            let contype: String = con.get("contype");
            let col_names: Vec<String> = con
                .try_get::<Vec<String>, _>("col_names")
                .unwrap_or_default();
            let conindid: Option<i64> = con.try_get("conindid").ok();

            match contype.as_str() {
                "p" => {
                    if let Some(oid) = conindid
                        && oid != 0 {
                            constraint_index_oids.insert(oid);
                        }
                    constraints.push(TableConstraint::PrimaryKey {
                        name: Some(conname),
                        columns: col_names,
                    });
                }
                "u" => {
                    if let Some(oid) = conindid
                        && oid != 0 {
                            constraint_index_oids.insert(oid);
                        }
                    constraints.push(TableConstraint::Unique {
                        name: Some(conname),
                        columns: col_names,
                    });
                }
                "f" => {
                    let ref_col_names: Vec<String> = con
                        .try_get::<Vec<String>, _>("ref_col_names")
                        .unwrap_or_default();
                    let ref_schema: Option<String> = con.try_get("ref_schema").ok();
                    let ref_table: String = con.try_get("ref_table").unwrap_or_default();
                    let confdeltype: Option<String> = con.try_get("confdeltype").ok();
                    let confupdtype: Option<String> = con.try_get("confupdtype").ok();

                    let on_delete = confdeltype.as_deref().and_then(pg_conf_action);
                    let on_update = confupdtype.as_deref().and_then(pg_conf_action);

                    constraints.push(TableConstraint::ForeignKey(ForeignKey {
                        name: Some(conname),
                        columns: col_names,
                        ref_schema,
                        ref_table,
                        ref_columns: ref_col_names,
                        on_delete,
                        on_update,
                    }));
                }
                "c" => {
                    let condef: String = con.get("condef");
                    // pg_get_constraintdef returns "CHECK (expr)"
                    let expression = condef
                        .strip_prefix("CHECK (")
                        .and_then(|s| s.strip_suffix(')'))
                        .unwrap_or(&condef)
                        .to_string();
                    constraints.push(TableConstraint::Check {
                        name: Some(conname),
                        expression,
                    });
                }
                _ => {}
            }
        }

        Ok((constraints, constraint_index_oids))
    }

    /// Introspect a table's standalone indexes.
    ///
    /// Excludes indexes that back a PK or UNIQUE constraint (`constraint_index_oids`
    /// / `indisprimary`). Also skips expression indexes (indexprs IS NOT NULL),
    /// partial indexes (indpred IS NOT NULL), and any index whose indkey contains
    /// a 0 (expression column) — IndexDef cannot represent these, and emitting an
    /// empty column list would produce invalid DDL.
    async fn introspect_indexes(
        &self,
        schema: &str,
        table: &str,
        constraint_index_oids: &std::collections::HashSet<i64>,
    ) -> crate::error::Result<Vec<crate::entity::IndexDef>> {
        use crate::entity::{IndexColumn, IndexDef};

        let idx_sql =
            "SELECT i.relname AS index_name, \
                    ix.indexrelid::int8 AS index_oid, \
                    ix.indisunique, \
                    ix.indisprimary, \
                    am.amname AS index_type, \
                    (SELECT array_agg(a.attname ORDER BY pos.ord) \
                     FROM unnest(ix.indkey) WITH ORDINALITY AS pos(attnum, ord) \
                     JOIN pg_attribute a ON a.attrelid = ix.indrelid AND a.attnum = pos.attnum \
                     WHERE pos.attnum > 0 \
                    ) AS col_names \
             FROM pg_index ix \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_namespace ns ON ns.oid = t.relnamespace \
             JOIN pg_am am ON am.oid = i.relam \
             WHERE ns.nspname = $1 AND t.relname = $2 \
               AND ix.indexprs IS NULL \
               AND ix.indpred IS NULL \
               AND NOT (0 = ANY(ix.indkey::int2[])) \
             ORDER BY i.relname";

        let idx_rows = sqlx::query(idx_sql)
            .bind(schema)
            .bind(table)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| {
                DbdError::Config(format!("introspect_tables indexes {schema}.{table}: {e}"))
            })?;

        let mut indexes: Vec<IndexDef> = Vec::new();
        for ix in &idx_rows {
            let index_oid: i64 = ix.get("index_oid");
            let indisprimary: bool = ix.get("indisprimary");
            // Skip if it backs a PK/UNIQUE constraint
            if indisprimary || constraint_index_oids.contains(&index_oid) {
                continue;
            }

            let index_name: String = ix.get("index_name");
            let indisunique: bool = ix.get("indisunique");
            let index_type_str: String = ix.get("index_type");
            let col_names: Vec<String> = ix
                .try_get::<Vec<String>, _>("col_names")
                .unwrap_or_default();

            use crate::entity::IndexType;
            let index_type = match index_type_str.as_str() {
                "hash" => Some(IndexType::Hash),
                "gin" => Some(IndexType::Gin),
                "gist" => Some(IndexType::Gist),
                "brin" => Some(IndexType::Brin),
                "spgist" => Some(IndexType::SpGist),
                _ => None, // btree is the default — represent as None (no USING clause)
            };

            indexes.push(IndexDef {
                name: Some(index_name),
                columns: col_names
                    .into_iter()
                    .map(|c| IndexColumn { name: c, order: None })
                    .collect(),
                unique: indisunique,
                index_type,
            });
        }

        Ok(indexes)
    }

    /// Introspect a table's `COMMENT ON TABLE` text, if any.
    async fn introspect_table_comment(
        &self,
        schema: &str,
        table: &str,
    ) -> crate::error::Result<Option<String>> {
        let tc_sql = "SELECT obj_description((SELECT oid FROM pg_class \
                       WHERE relname = $2 \
                       AND relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) \
                      , 'pg_class') AS tbl_comment";
        let tc_row = sqlx::query(tc_sql)
            .bind(schema)
            .bind(table)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| {
                DbdError::Config(format!("introspect_tables table_comment {schema}.{table}: {e}"))
            })?;
        Ok(tc_row.try_get("tbl_comment").ok().flatten())
    }

    async fn introspect_views(&self) -> crate::error::Result<Vec<Entity>> {
        let ns_filter = Self::schema_filter_column("schemaname");
        let sql = format!(
            "SELECT schemaname, viewname, definition \
             FROM pg_views \
             WHERE {ns_filter} \
             ORDER BY schemaname, viewname"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_views failed: {e}")))?;

        let entities = rows
            .iter()
            .map(|row| {
                let schema: String = row.get("schemaname");
                let viewname: String = row.get("viewname");
                let definition: String = row.get("definition");
                let mut e = Entity::new(EntityType::View, &format!("{schema}.{viewname}"));
                e.writes = vec![definition];
                e
            })
            .collect();
        Ok(entities)
    }

    /// Reverse-engineer materialized views from `pg_matviews`.
    ///
    /// Mirrors [`Self::introspect_views`]: the SELECT body is captured verbatim in
    /// `writes[0]` (the parser/emitter contract — `emit_matview` re-adds the
    /// `WITH DATA` clause). Additionally, a matview's indexes live in `pg_index`
    /// exactly like a table's, so they are read with the SAME index reader the
    /// table path uses ([`Self::introspect_indexes`]) and attached via a
    /// columns-empty `TableDef`. Matviews cannot own PK/UNIQUE *constraints* (only
    /// standalone indexes), so there are no constraint-backed index OIDs to skip —
    /// an empty set is passed. `emit_matview` re-creates the indexes from there.
    async fn introspect_matviews(&self) -> crate::error::Result<Vec<Entity>> {
        use crate::entity::TableDef;

        let ns_filter = Self::schema_filter_column("schemaname");
        let sql = format!(
            "SELECT schemaname, matviewname, definition \
             FROM pg_matviews \
             WHERE {ns_filter} \
             ORDER BY schemaname, matviewname"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_matviews failed: {e}")))?;

        let mut entities: Vec<Entity> = Vec::new();
        for row in &rows {
            let schema: String = row.get("schemaname");
            let matviewname: String = row.get("matviewname");
            let definition: String = row.get("definition");

            let indexes = self
                .introspect_indexes(&schema, &matviewname, &std::collections::HashSet::new())
                .await?;

            let mut e =
                Entity::new(EntityType::MaterializedView, &format!("{schema}.{matviewname}"));
            e.writes = vec![definition];
            if !indexes.is_empty() {
                e.table_def = Some(TableDef {
                    columns: Vec::new(),
                    constraints: Vec::new(),
                    indexes,
                    comments: Default::default(),
                });
            }
            entities.push(e);
        }
        Ok(entities)
    }

    /// Reverse-engineer **standalone** sequences as `EntityType::Sequence`.
    ///
    /// A sequence is `pg_class.relkind = 'S'`. Column-owned sequences (those backing
    /// a `serial`/`bigserial` default or an `IDENTITY` column) are EXCLUDED — they
    /// have a `pg_depend` row `(classid=pg_class, objid=seq, refobjsubid>0,
    /// deptype IN ('a','i'))` and are reproduced via their owning column (see the
    /// serial/identity handling in `introspect_tables`). Only non-owned sequences
    /// are emitted here.
    ///
    /// Attributes are read from `pg_sequences` (PG ≥ 10) and the element type from
    /// `pg_sequence.seqtypid::regtype`. The fully-rendered `CREATE SEQUENCE …;` is
    /// stashed in `writes[0]` for `emit_sequence` (deterministic; no
    /// `pg_get_sequencedef` exists, so this is reconstruct-from-catalog).
    async fn introspect_sequences(&self) -> crate::error::Result<Vec<Entity>> {
        let ns_filter = Self::schema_filter_column("n.nspname");
        let sql = format!(
            "SELECT s.schemaname, s.sequencename, \
                    s.start_value, s.min_value, s.max_value, \
                    s.increment_by, s.cycle, s.cache_size, \
                    format_type(pgs.seqtypid, NULL) AS seq_type \
             FROM pg_sequences s \
             JOIN pg_class c ON c.relname = s.sequencename \
             JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = s.schemaname \
             JOIN pg_sequence pgs ON pgs.seqrelid = c.oid \
             WHERE c.relkind = 'S' \
               AND {ns_filter} \
               AND NOT EXISTS ( \
                   SELECT 1 FROM pg_depend d \
                   WHERE d.classid = 'pg_class'::regclass \
                     AND d.objid = c.oid \
                     AND d.refobjsubid > 0 \
                     AND d.deptype IN ('a', 'i') \
               ) \
             ORDER BY s.schemaname, s.sequencename"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_sequences failed: {e}")))?;

        let mut entities: Vec<Entity> = Vec::new();
        for row in &rows {
            let schema: String = row.get("schemaname");
            let name: String = row.get("sequencename");
            let seq_type: String = row.get("seq_type");
            let start_value: i64 = row.get("start_value");
            let min_value: i64 = row.get("min_value");
            let max_value: i64 = row.get("max_value");
            let increment_by: i64 = row.get("increment_by");
            let cycle: bool = row.get("cycle");
            let cache_size: i64 = row.get("cache_size");

            // Deterministic, fully-explicit CREATE SEQUENCE for fidelity. Quoting
            // matches the rest of the emitter (q()).
            let mut ddl = format!(
                "CREATE SEQUENCE \"{schema}\".\"{name}\" AS {seq_type} \
                 INCREMENT BY {increment_by} MINVALUE {min_value} MAXVALUE {max_value} \
                 START WITH {start_value} CACHE {cache_size}"
            );
            if cycle {
                ddl.push_str(" CYCLE");
            }
            ddl.push(';');

            let mut e = Entity::new(EntityType::Sequence, &format!("{schema}.{name}"));
            e.schema = Some(schema);
            e.writes = vec![ddl];
            entities.push(e);
        }
        Ok(entities)
    }

    /// Reverse-engineer functions & procedures via `pg_get_functiondef`.
    ///
    /// Captures the canonical `CREATE OR REPLACE FUNCTION|PROCEDURE …` text
    /// verbatim (lossless). Aggregates/window functions (`prokind a/w`) are
    /// excluded by the filter; extension-owned routines are excluded via
    /// `pg_depend deptype = 'e'`.
    ///
    /// **Overload grouping:** rows are ordered by `(nspname, proname, oid)`;
    /// consecutive rows sharing the same `(schema, name, kind)` collapse into a
    /// single `Entity` whose `writes` holds each overload's definition in oid
    /// order. This avoids `ddl/<kind>/<schema>/<name>.ddl` path collisions.
    async fn introspect_functions(&self) -> crate::error::Result<Vec<Entity>> {
        let ns_filter = Self::schema_filter_column("n.nspname");
        let sql = format!(
            "SELECT n.nspname AS schema, p.proname AS name, p.prokind::text AS kind, \
                    pg_get_functiondef(p.oid) AS definition \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE p.prokind IN ('f', 'p') \
               AND {ns_filter} \
               AND NOT EXISTS ( \
                   SELECT 1 FROM pg_depend d \
                   WHERE d.objid = p.oid AND d.deptype = 'e' \
               ) \
             ORDER BY n.nspname, p.proname, p.prokind, p.oid"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_functions failed: {e}")))?;

        let mut entities: Vec<Entity> = Vec::new();
        // Track the key of the entity currently being accumulated so consecutive
        // overload rows append to its `writes` instead of starting a new entity.
        let mut current_key: Option<(String, String, String)> = None;

        for row in &rows {
            let schema: String = row.get("schema");
            let name: String = row.get("name");
            let kind: String = row.get("kind");
            let definition: String = row.get("definition");

            let key = (schema.clone(), name.clone(), kind.clone());
            if current_key.as_ref() == Some(&key) {
                // Same routine (overload) — append its body to the current entity.
                if let Some(last) = entities.last_mut() {
                    last.writes.push(definition);
                }
                continue;
            }

            let entity_type = if kind == "p" {
                EntityType::Procedure
            } else {
                EntityType::Function
            };
            let mut e = Entity::new(entity_type, &format!("{schema}.{name}"));
            e.schema = Some(schema);
            e.writes = vec![definition];
            entities.push(e);
            current_key = Some(key);
        }

        Ok(entities)
    }

    /// SQL keywords and types that appear as false-positive references.
    fn is_sql_noise(name: &str) -> bool {
        let lower = name.to_lowercase();
        let noise = [
            "varchar", "int", "integer", "bigint", "smallint", "numeric", "decimal",
            "boolean", "text", "date", "timestamp", "timestamptz", "uuid", "jsonb",
            "json", "bytea", "float", "double", "real", "serial", "bigserial",
            "btree", "hash", "gin", "gist", "brin",
            "now", "coalesce", "nullif", "greatest", "least", "extract",
            "count", "sum", "avg", "min", "max", "string_agg",
            "row_number", "rank", "dense_rank", "lead", "lag",
            "upper", "lower", "trim", "length", "replace", "substring",
            "cast", "exists", "between", "like", "in", "not", "and", "or",
            "true", "false", "null", "default", "current_user", "localtime",
            "localtimestamp", "random", "floor", "ceil", "abs", "round",
            "enum", "record", "void", "trigger", "event_trigger",
        ];
        noise.contains(&lower.as_str())
    }
}

#[async_trait]
impl DatabaseAdapter for PostgresAdapter {
    async fn connect(&mut self) -> Result<()> {
        // Pool is already connected at construction
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.pool.close().await;
        Ok(())
    }

    async fn test_connection(&self) -> Result<bool> {
        match sqlx::query("SELECT 1").fetch_one(&self.pool).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    async fn execute_script(&self, sql: &str) -> Result<()> {
        // Ensure `public` is always in the search_path — extensions may be
        // installed there and DDL files often SET search_path without including it.
        let sql = ensure_public_in_search_path(sql);
        // Route through the open batch transaction if one exists (see `exec_raw`).
        self.exec_raw(&sql).await
    }

    async fn apply_entity(&self, entity: &Entity) -> Result<()> {
        let sql = match script::ddl_from_entity(entity) {
            Some(s) => s,
            None => return Ok(()), // External entities, etc.
        };

        // Enums need idempotent wrapping — CREATE TYPE fails on duplicate
        if entity.entity_type == EntityType::Enum {
            return self.apply_enum(entity, &sql).await;
        }

        self.execute_script(&sql).await.map_err(|e| {
            DbdError::Config(format!(
                "Failed to apply {:?} {}: {}",
                entity.entity_type, entity.name, e
            ))
        })
    }

    async fn import_data(&self, entity: &Entity, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }

        // Import via COPY FROM STDIN
        let file_path = match &entity.file {
            Some(f) => f,
            None => return Err(DbdError::Config(format!("No file for import entity {}", entity.name))),
        };

        let format = entity.format.as_deref().unwrap_or("csv");
        let qualified = entity.name.replace('.', "\".\"");
        let data = std::fs::read_to_string(file_path)?;

        match format {
            "csv" | "tsv" => {
                let delimiter = if format == "tsv" { ", DELIMITER E'\\t'" } else { "" };
                let copy_sql = format!(
                    "COPY \"{qualified}\" FROM STDIN WITH (FORMAT csv, HEADER true{delimiter})"
                );
                let mut conn = self.pool.acquire().await
                    .map_err(|e| DbdError::Config(format!("Connection acquire failed: {e}")))?;
                let mut copy = conn.copy_in_raw(&copy_sql).await
                    .map_err(|e| DbdError::Config(format!("COPY failed: {e}")))?;
                copy.send(data.as_bytes()).await
                    .map_err(|e| DbdError::Config(format!("COPY send failed: {e}")))?;
                copy.finish().await
                    .map_err(|e| DbdError::Config(format!("COPY finish failed: {e}")))?;
            }
            "json" | "jsonl" => {
                // JSONL: stage rows in dbd's schema-qualified temp table, then hand
                // them to the internal import procedure. The temp table, the procedure,
                // and the CALL are all fully-qualified against `staging` — pooled
                // connections don't share `search_path`, so unqualified names (a bare
                // `_temp`, or a CALL qualified with the target's own schema) resolve
                // nondeterministically and fail. See sensei-hq/dbd#6.
                self.execute_script(&format!("CREATE TABLE IF NOT EXISTS {JSONB_IMPORT_TMP} (data jsonb)")).await?;
                self.execute_script(&format!("TRUNCATE {JSONB_IMPORT_TMP}")).await?;

                // Insert each line as a JSONB row
                for line in data.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let insert = format!(
                        "INSERT INTO {JSONB_IMPORT_TMP} (data) VALUES ('{}'::jsonb)",
                        line.replace('\'', "''")
                    );
                    self.execute_script(&insert).await?;
                }

                // Move data from the temp table to the target. `entity.name` is the
                // schema-qualified destination; the procedure always lives in `staging`.
                self.execute_script(&jsonb_import_call_sql(&entity.name)).await?;
                self.execute_script(&format!("DROP TABLE IF EXISTS {JSONB_IMPORT_TMP}")).await?;
            }
            _ => {
                return Err(DbdError::Config(format!(
                    "Unsupported import format: {format}"
                )));
            }
        }

        Ok(())
    }

    async fn export_data(&self, entity: &Entity, out_dir: Option<&Path>) -> Result<()> {
        let qualified = entity.name.replace('.', "\".\"");
        let format = entity.format.as_deref().unwrap_or("csv");

        let copy_sql = match format {
            "tsv" => format!(
                "COPY (SELECT * FROM \"{qualified}\") TO STDOUT WITH (FORMAT csv, HEADER true, DELIMITER E'\\t')"
            ),
            "jsonl" => format!(
                "COPY (SELECT row_to_json(t) FROM \"{qualified}\" t) TO STDOUT"
            ),
            _ => format!(
                "COPY (SELECT * FROM \"{qualified}\") TO STDOUT WITH (FORMAT csv, HEADER true)"
            ),
        };

        let mut conn = self.pool.acquire().await
            .map_err(|e| DbdError::Config(format!("Connection acquire failed: {e}")))?;

        let mut data = Vec::new();
        let mut copy = conn
            .copy_out_raw(&copy_sql)
            .await
            .map_err(|e| DbdError::Config(format!("COPY OUT failed: {e}")))?;

        use futures_lite::StreamExt;
        while let Some(chunk) = copy.next().await {
            let chunk = chunk.map_err(|e| DbdError::Config(format!("COPY OUT read failed: {e}")))?;
            data.extend_from_slice(&chunk);
        }

        // Resolve the bare table name (strip any `schema.` prefix).
        let parts: Vec<&str> = entity.name.split('.').collect();
        let (schema, name) = if parts.len() > 1 {
            (parts[0], parts[1])
        } else {
            ("public", parts[0])
        };

        // `Some(dir)` → write `dir/<name>.<format>` (flat).
        // `None`      → folder convention `export/<schema>/<name>.<format>`.
        let export_dir = match out_dir {
            Some(dir) => dir.to_path_buf(),
            None => Path::new("export").join(schema),
        };
        std::fs::create_dir_all(&export_dir)?;
        std::fs::write(export_dir.join(format!("{name}.{format}")), &data)?;

        Ok(())
    }

    async fn load_catalog(&mut self) -> Result<()> {
        // Try loading from cache first
        if let Some(cached) = self.load_catalog_cache() {
            self.catalog = cached;
            return Ok(());
        }

        // Functions with namespace
        let rows = sqlx::query(
            "SELECT n.nspname, p.proname FROM pg_proc p \
             JOIN pg_namespace n ON p.pronamespace = n.oid",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Catalog query failed: {e}")))?;

        for row in &rows {
            let schema: String = row.get("nspname");
            let name: String = row.get("proname");
            self.catalog.functions.insert(format!("{schema}.{name}"));
        }

        // Types with namespace
        let rows = sqlx::query(
            "SELECT n.nspname, t.typname FROM pg_type t \
             JOIN pg_namespace n ON t.typnamespace = n.oid",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Type catalog query failed: {e}")))?;

        for row in &rows {
            let schema: String = row.get("nspname");
            let name: String = row.get("typname");
            self.catalog.types.insert(format!("{schema}.{name}"));
        }

        // Extension objects (functions + types with extension name)
        let rows = sqlx::query(
            "SELECT p.proname AS obj_name, e.extname, n.nspname FROM pg_proc p \
             JOIN pg_depend d ON d.objid = p.oid AND d.deptype = 'e' \
             JOIN pg_extension e ON e.oid = d.refobjid \
             JOIN pg_namespace n ON p.pronamespace = n.oid",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        for row in &rows {
            let name: String = row.get("obj_name");
            let ext: String = row.get("extname");
            self.catalog.extension_objects.insert(name, ext);
        }

        // Extension schemas
        let rows = sqlx::query(
            "SELECT n.nspname FROM pg_extension e \
             JOIN pg_namespace n ON e.extnamespace = n.oid",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        for row in &rows {
            let schema: String = row.get("nspname");
            self.catalog.extension_schemas.insert(schema);
        }

        // Save cache
        self.save_catalog_cache();

        Ok(())
    }

    fn classify_reference(&self, name: &str, _installed: &[String]) -> ReferenceClass {
        let lower = name.to_lowercase();

        // SQL noise (keywords, types)
        if Self::is_sql_noise(&lower) {
            return ReferenceClass::Internal;
        }

        // Extension objects (bare name lookup)
        if let Some(ext) = self.catalog.extension_objects.get(&lower) {
            return ReferenceClass::Extension(ext.clone());
        }

        // Qualified name check
        if lower.contains('.')
            && (self.catalog.functions.contains(&lower) || self.catalog.types.contains(&lower))
        {
            // Check if it's in an extension schema
            let schema = lower.split('.').next().unwrap_or("");
            if self.catalog.extension_schemas.contains(schema) {
                return ReferenceClass::Extension("unknown".to_string());
            }
            return ReferenceClass::Internal;
        }

        // Bare name: check pg_catalog namespace
        let pg_qualified = format!("pg_catalog.{lower}");
        if self.catalog.functions.contains(&pg_qualified)
            || self.catalog.types.contains(&pg_qualified)
        {
            return ReferenceClass::Internal;
        }

        // Check if name is in any extension schema
        for ext_schema in &self.catalog.extension_schemas {
            if self
                .catalog
                .functions
                .contains(&format!("{ext_schema}.{lower}"))
                || self
                    .catalog
                    .types
                    .contains(&format!("{ext_schema}.{lower}"))
            {
                return ReferenceClass::Extension("unknown".to_string());
            }
        }

        // Static pattern fallback (for offline/no-catalog mode)
        if Self::matches_static_pattern(&lower) {
            return ReferenceClass::Internal;
        }

        ReferenceClass::UserDefined
    }

    async fn resolve_entity(&self, name: &str) -> Result<Option<String>> {
        let parts: Vec<&str> = name.split('.').collect();
        let (schema, entity_name) = if parts.len() > 1 {
            (parts[0], parts[1])
        } else {
            ("public", parts[0])
        };

        // Check tables/views
        let result = sqlx::query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2"
        )
        .bind(schema)
        .bind(entity_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Entity resolve query failed: {e}")))?;

        if result.is_some() {
            return Ok(Some(name.to_string()));
        }

        // Check enum types
        let result = sqlx::query(
            "SELECT typname FROM pg_type t \
             JOIN pg_namespace n ON t.typnamespace = n.oid \
             WHERE n.nspname = $1 AND t.typname = $2 AND t.typtype = 'e'"
        )
        .bind(schema)
        .bind(entity_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Enum resolve query failed: {e}")))?;

        Ok(result.map(|_| name.to_string()))
    }

    async fn list_entities(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = Vec::new();

        // Tables + views (information_schema covers BASE TABLE, VIEW, FOREIGN, etc.)
        let rows = sqlx::query(
            "SELECT table_schema, table_name FROM information_schema.tables \
             WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
               AND table_schema NOT LIKE 'pg_toast%' \
               AND table_schema NOT LIKE 'pg_temp%'"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("list_entities tables query failed: {e}")))?;

        for row in &rows {
            let schema: String = row.get("table_schema");
            let name: String = row.get("table_name");
            names.push(format!("{schema}.{name}"));
        }

        // Enum types
        let rows = sqlx::query(
            "SELECT n.nspname, t.typname FROM pg_type t \
             JOIN pg_namespace n ON t.typnamespace = n.oid \
             WHERE t.typtype = 'e' \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
               AND n.nspname NOT LIKE 'pg_toast%'"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("list_entities enums query failed: {e}")))?;

        for row in &rows {
            let schema: String = row.get("nspname");
            let name: String = row.get("typname");
            names.push(format!("{schema}.{name}"));
        }

        Ok(names)
    }

    async fn introspect(&self) -> Result<Vec<Entity>> {
        let mut out: Vec<Entity> = Vec::new();
        out.extend(self.introspect_schemas().await?);
        out.extend(self.introspect_extensions().await?);
        out.extend(self.introspect_sequences().await?);
        out.extend(self.introspect_enums().await?);
        out.extend(self.introspect_tables().await?);
        out.extend(self.introspect_views().await?);
        out.extend(self.introspect_matviews().await?);
        out.extend(self.introspect_functions().await?);
        Ok(out)
    }

    async fn introspect_roles(&self) -> Result<Vec<Entity>> {
        use std::collections::HashSet;

        // 1. All roles + superuser flag. Filter out platform/managed roles via
        //    `role_is_managed`; the survivors are the project's own roles.
        let role_rows = sqlx::query("SELECT r.rolname, r.rolsuper FROM pg_roles r ORDER BY r.rolname")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_roles roles query failed: {e}")))?;

        let mut kept_names: Vec<String> = Vec::new();
        for row in &role_rows {
            let rolname: String = row.get("rolname");
            let rolsuper: bool = row.get("rolsuper");
            if !crate::reverse::role_is_managed(&rolname, rolsuper) {
                kept_names.push(rolname);
            }
        }
        let kept: HashSet<String> = kept_names.iter().cloned().collect();

        // 2. Memberships as (member rolname, granted rolname) pairs. Filter to
        //    only kept↔kept edges so every emitted `GRANT … TO …` target is also
        //    emitted (self-contained); drop memberships into denied/platform roles.
        let mem_rows = sqlx::query(
            "SELECT m.rolname AS member, g.rolname AS granted \
             FROM pg_auth_members am \
             JOIN pg_roles m ON m.oid = am.member \
             JOIN pg_roles g ON g.oid = am.roleid",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("introspect_roles memberships query failed: {e}")))?;

        let pairs: Vec<(String, String)> = mem_rows
            .iter()
            .map(|row| {
                let member: String = row.get("member");
                let granted: String = row.get("granted");
                (member, granted)
            })
            .collect();
        let mut refers_by_member = crate::reverse::keep_memberships(&pairs, &kept);

        // 3. Build one Role entity per kept role (sorted via the ORDER BY above),
        //    attaching its sorted, self-contained memberships.
        let entities = kept_names
            .into_iter()
            .map(|name| {
                let mut e = Entity::new(EntityType::Role, &name);
                if let Some(refers) = refers_by_member.remove(&name) {
                    e.refers = refers;
                }
                e
            })
            .collect();
        Ok(entities)
    }

    async fn reverse_managed_version(&self) -> Result<Option<u32>> {
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
            .map_err(|e| DbdError::Config(format!("reverse_managed_version version read failed: {e}")))?;

        match version_row {
            Some(row) => {
                let version: i32 = row.get("version");
                Ok(Some(version as u32))
            }
            None => Ok(Some(0)),
        }
    }

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

    async fn get_db_version(&self) -> Result<u32> {
        // Read from `_dbd_meta` (authoritative version source). Resolve its
        // schema via the catalog rather than an unqualified SELECT so a stray
        // copy left by a scoped apply (e.g. `dojo._dbd_meta`) is read correctly
        // instead of silently missed as version 0.
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
            Ok(None) => Ok(0),
            Err(_) => Ok(0), // Table doesn't exist yet
        }
    }

    async fn apply_migration(
        &self,
        version: u32,
        sql: &str,
        description: &str,
        checksum: &str,
    ) -> Result<()> {
        if !sql.is_empty() {
            self.execute_script(sql).await?;
        }
        // `_dbd_migrations` is pinned to `public` (see `ensure_migrations_table`),
        // so the qualified insert lands in the right table regardless of the
        // ambient search_path or which pooled connection runs it.
        let q = sqlx::query(
            "INSERT INTO public._dbd_migrations (project, version, description, checksum) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project, version) DO NOTHING"
        )
        .bind(&self.project)
        .bind(version as i32)
        .bind(description)
        .bind(checksum);
        let mut guard = self.batch.lock().await;
        match guard.as_mut() {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Migration(format!("Record migration failed: {e}")))?;

        Ok(())
    }

    async fn clear_project_migrations(&self) -> Result<()> {
        sqlx::query("DELETE FROM public._dbd_migrations WHERE project = $1")
            .bind(&self.project)
            .execute(&self.pool)
            .await
            .ok(); // Ignore if table doesn't exist
        Ok(())
    }

    async fn ensure_import_procedure(&self) -> Result<()> {
        // Embedded at compile time — always up to date with this version of dbd.
        const DDL: &str = include_str!("../internal/import_jsonb_to_table.ddl");
        self.execute_script("CREATE SCHEMA IF NOT EXISTS staging").await?;
        // Use sqlx::query() instead of execute_script() — sqlx::raw_sql() splits on all
        // semicolons including those inside $$-quoted PL/pgSQL bodies, which mangles
        // CREATE PROCEDURE. sqlx::query() sends the statement as a single unit.
        sqlx::query(DDL)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("ensure staging.import_jsonb_to_table failed: {e}")))?;
        Ok(())
    }

    async fn ensure_meta_table(&self) -> Result<()> {
        self.ensure_public_bookkeeping(
            "_dbd_meta",
            "CREATE TABLE IF NOT EXISTS public._dbd_meta ( \
                project     varchar NOT NULL PRIMARY KEY, \
                env         varchar NOT NULL DEFAULT 'dev', \
                version     integer NOT NULL DEFAULT 0, \
                created_at  timestamptz NOT NULL DEFAULT now(), \
                updated_at  timestamptz NOT NULL DEFAULT now() \
            )",
        )
        .await
    }

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        // Resolve `_dbd_meta`'s schema via the catalog (a scoped apply may have
        // left it outside `public`) rather than reading an unqualified name.
        let Some(schema) = self.bookkeeping_schema("_dbd_meta").await? else {
            return Ok(None); // no `_dbd_meta` anywhere yet
        };
        let quoted = schema.replace('"', "\"\"");
        let result = sqlx::query(&format!(
            "SELECT project, env, version, updated_at::text as applied_at FROM \"{quoted}\"._dbd_meta WHERE project = $1"
        ))
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await;

        match result {
            Ok(Some(row)) => Ok(Some(ProjectMeta {
                project: row.get("project"),
                env: row.get("env"),
                version: row.get::<i32, _>("version") as u32,
                applied_at: row.try_get("applied_at").ok(),
            })),
            Ok(None) => Ok(None),
            Err(_) => Ok(None), // Table doesn't exist yet
        }
    }

    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()> {
        // `ensure_meta_table` pins `_dbd_meta` to `public` (relocating a stray
        // copy a scoped apply may have left in another schema). The insert is
        // schema-qualified, so it resolves deterministically regardless of the
        // ambient search_path or which pooled connection runs it — no `RESET`
        // dance required, which never worked across the non-batch pool anyway.
        self.ensure_meta_table().await?;
        let q = sqlx::query(
            "INSERT INTO public._dbd_meta (project, env, version) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (project) DO UPDATE \
             SET env = EXCLUDED.env, version = EXCLUDED.version, updated_at = now()"
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i32);
        let mut guard = self.batch.lock().await;
        match guard.as_mut() {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;

        Ok(())
    }

    // ── Batch transaction (atomic apply) ───────────────

    fn supports_transactional_apply(&self) -> bool {
        true
    }

    async fn begin_batch(&self) -> Result<()> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("begin batch transaction failed: {e}")))?;
        *self.batch.lock().await = Some(tx);
        Ok(())
    }

    async fn commit_batch(&self) -> Result<()> {
        if let Some(tx) = self.batch.lock().await.take() {
            tx.commit()
                .await
                .map_err(|e| DbdError::Config(format!("commit batch transaction failed: {e}")))?;
        }
        Ok(())
    }

    async fn rollback_batch(&self) -> Result<()> {
        if let Some(tx) = self.batch.lock().await.take() {
            // Best-effort — the caller is already returning the original error.
            let _ = tx.rollback().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression tests for sensei-hq/dbd#6: the dbd-managed jsonb-import
    // machinery must be schema-qualified so it resolves independently of the
    // session `search_path` (which pooled connections don't share).

    #[test]
    fn jsonb_call_pins_procedure_to_staging_regardless_of_target_schema() {
        // Targeted imports resolve the target to its own schema (e.g. `activity`).
        // The procedure only ever exists in `staging`, so the CALL must be pinned
        // there — not to `activity`, which has no such procedure (Defect A).
        let sql = jsonb_import_call_sql("activity.assistant_events");
        assert!(
            sql.contains("CALL staging.import_jsonb_to_table("),
            "CALL must target the staging procedure, got: {sql}"
        );
        assert!(
            !sql.contains("activity.import_jsonb_to_table"),
            "CALL must not use the target table's schema, got: {sql}"
        );
    }

    #[test]
    fn jsonb_call_passes_schema_qualified_source_table() {
        // The source (staging temp) table must be fully-qualified so the
        // procedure's `from <source>` resolves regardless of search_path (Defect B).
        let sql = jsonb_import_call_sql("staging.assistant_events");
        assert!(
            sql.contains(&format!("'{JSONB_IMPORT_TMP}'")),
            "source must be the qualified temp table, got: {sql}"
        );
        assert!(
            !sql.contains("'_temp'"),
            "source must not be a bare unqualified _temp, got: {sql}"
        );
    }

    #[test]
    fn jsonb_temp_table_is_schema_qualified_and_dbd_namespaced() {
        assert!(
            JSONB_IMPORT_TMP.starts_with(&format!("{JSONB_IMPORT_SCHEMA}.")),
            "temp table must be qualified with the import schema: {JSONB_IMPORT_TMP}"
        );
        assert!(
            JSONB_IMPORT_TMP.contains("_dbd_"),
            "temp table must be dbd-namespaced to avoid clobbering user tables: {JSONB_IMPORT_TMP}"
        );
    }

    #[test]
    fn jsonb_call_escapes_single_quotes_in_target() {
        let sql = jsonb_import_call_sql("weird.o'brien");
        assert!(sql.contains("weird.o''brien"), "single quotes must be escaped: {sql}");
    }
}
