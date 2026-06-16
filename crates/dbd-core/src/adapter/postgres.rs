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

/// PostgreSQL adapter using sqlx connection pool.
pub struct PostgresAdapter {
    pool: PgPool,
    project: String,
    catalog: CatalogData,
    /// SHA-256 of the connection URL, used as cache key.
    url_hash: String,
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
        })
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
        use crate::entity::{
            ColumnDef, ForeignKey, IndexColumn, IndexDef, TableComments, TableConstraint,
            TableDef,
        };

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

            // ── Columns ──────────────────────────────────────────────────────
            let cols_sql =
                "SELECT c.column_name, \
                        c.is_nullable, \
                        c.column_default, \
                        c.is_identity, \
                        format_type(a.atttypid, a.atttypmod) AS col_type, \
                        col_description(a.attrelid, a.attnum) AS col_comment \
                 FROM information_schema.columns c \
                 JOIN pg_attribute a \
                   ON a.attrelid = (SELECT oid FROM pg_class \
                                    WHERE relname = $2 \
                                    AND relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) \
                   AND a.attname = c.column_name \
                   AND a.attnum > 0 \
                   AND NOT a.attisdropped \
                 WHERE c.table_schema = $1 AND c.table_name = $2 \
                 ORDER BY c.ordinal_position";

            let col_rows = sqlx::query(cols_sql)
                .bind(&schema)
                .bind(&table)
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
                    let is_nullable: String = row.get("is_nullable");
                    let default_value: Option<String> = row.get("column_default");
                    let is_identity_str: String = row.get("is_identity");
                    let comment: Option<String> = row.get("col_comment");
                    ColumnDef {
                        name,
                        data_type: col_type,
                        nullable: is_nullable == "YES",
                        default_value,
                        is_pk: false,        // filled from constraints
                        is_unique: false,    // filled from constraints
                        is_identity: is_identity_str == "YES",
                        comment,
                        inline_fk: None,
                    }
                })
                .collect();

            // ── Constraints ───────────────────────────────────────────────────
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
                .bind(&schema)
                .bind(&table)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| {
                    DbdError::Config(format!(
                        "introspect_tables constraints {schema}.{table}: {e}"
                    ))
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
                        let ref_table: String =
                            con.try_get("ref_table").unwrap_or_default();
                        let confdeltype: Option<String> =
                            con.try_get("confdeltype").ok();
                        let confupdtype: Option<String> =
                            con.try_get("confupdtype").ok();

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

            // ── Indexes ───────────────────────────────────────────────────────
            // Exclude indexes that back a PK or UNIQUE constraint.
            // Also skip expression indexes (indexprs IS NOT NULL), partial indexes
            // (indpred IS NOT NULL), and any index whose indkey contains a 0
            // (expression column) — IndexDef cannot represent these, and emitting
            // an empty column list would produce invalid DDL.
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
                .bind(&schema)
                .bind(&table)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| {
                    DbdError::Config(format!(
                        "introspect_tables indexes {schema}.{table}: {e}"
                    ))
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

            // ── Table comment ─────────────────────────────────────────────────
            let tc_sql = "SELECT obj_description((SELECT oid FROM pg_class \
                           WHERE relname = $2 \
                           AND relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1)) \
                          , 'pg_class') AS tbl_comment";
            let tc_row = sqlx::query(tc_sql)
                .bind(&schema)
                .bind(&table)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| {
                    DbdError::Config(format!(
                        "introspect_tables table_comment {schema}.{table}: {e}"
                    ))
                })?;
            let table_comment: Option<String> = tc_row.try_get("tbl_comment").ok().flatten();

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

        sqlx::raw_sql(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("SQL execution failed: {e}")))?;
        Ok(())
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
                // JSONL: create temp table, load lines, call import procedure
                let schema = entity.schema.as_deref().unwrap_or("staging");
                self.execute_script("CREATE TABLE IF NOT EXISTS _temp (data jsonb)").await?;
                self.execute_script("TRUNCATE _temp").await?;

                // Insert each line as a JSONB row
                for line in data.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let insert = format!(
                        "INSERT INTO _temp (data) VALUES ('{}'::jsonb)",
                        line.replace('\'', "''")
                    );
                    self.execute_script(&insert).await?;
                }

                // Call the import procedure to move data from _temp to the target
                let proc_call = format!(
                    "CALL {schema}.import_jsonb_to_table('_temp', '{}')",
                    entity.name.replace('\'', "''")
                );
                self.execute_script(&proc_call).await?;
                self.execute_script("DROP TABLE IF EXISTS _temp").await?;
            }
            _ => {
                return Err(DbdError::Config(format!(
                    "Unsupported import format: {format}"
                )));
            }
        }

        Ok(())
    }

    async fn export_data(&self, entity: &Entity) -> Result<()> {
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

        // Write to export/<schema>/<name>.<format>
        let parts: Vec<&str> = entity.name.split('.').collect();
        let (schema, name) = if parts.len() > 1 {
            (parts[0], parts[1])
        } else {
            ("public", parts[0])
        };

        let export_dir = Path::new("export").join(schema);
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
        out.extend(self.introspect_enums().await?);
        out.extend(self.introspect_tables().await?);
        out.extend(self.introspect_views().await?);
        Ok(out)
    }

    async fn reverse_managed_version(&self, env: &str) -> Result<Option<u32>> {
        // 1. Find the schema that holds `_dbd_meta` via the catalog (not an
        //    unqualified SELECT) — it commonly lives off the search_path
        //    (e.g. `staging._dbd_meta`). No row → foreign DB.
        let schema_row = sqlx::query(
            "SELECT n.nspname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = '_dbd_meta' AND c.relkind = 'r' LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("reverse_managed_version schema lookup failed: {e}")))?;

        let Some(schema_row) = schema_row else {
            return Ok(None);
        };
        let schema: String = schema_row.get("nspname");

        // 2. Read the applied version for (project, env) from that schema's
        //    `_dbd_meta`. `schema` comes from the catalog (not user input) but is
        //    still quoted defensively. Row → Some(version); no row → Some(0)
        //    (the table exists, so the DB is managed, just no matching record).
        let quoted = schema.replace('"', "\"\"");
        let query = format!(
            "SELECT version FROM \"{quoted}\"._dbd_meta WHERE project = $1 AND env = $2"
        );
        let version_row = sqlx::query(&query)
            .bind(&self.project)
            .bind(env)
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
        self.execute_script(
            "CREATE TABLE IF NOT EXISTS _dbd_migrations ( \
                project     varchar NOT NULL, \
                version     integer NOT NULL, \
                applied_at  timestamptz NOT NULL DEFAULT now(), \
                description text, \
                checksum    text, \
                PRIMARY KEY (project, version) \
            )"
        ).await
    }

    async fn get_db_version(&self) -> Result<u32> {
        // Read from _dbd_meta (authoritative version source)
        let result = sqlx::query(
            "SELECT version FROM _dbd_meta WHERE project = $1"
        )
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
        sqlx::query(
            "INSERT INTO _dbd_migrations (project, version, description, checksum) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project, version) DO NOTHING"
        )
        .bind(&self.project)
        .bind(version as i32)
        .bind(description)
        .bind(checksum)
        .execute(&self.pool)
        .await
        .map_err(|e| DbdError::Migration(format!("Record migration failed: {e}")))?;

        Ok(())
    }

    async fn clear_project_migrations(&self) -> Result<()> {
        sqlx::query("DELETE FROM _dbd_migrations WHERE project = $1")
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
        self.execute_script(
            "CREATE TABLE IF NOT EXISTS _dbd_meta ( \
                project     varchar NOT NULL PRIMARY KEY, \
                env         varchar NOT NULL DEFAULT 'dev', \
                version     integer NOT NULL DEFAULT 0, \
                created_at  timestamptz NOT NULL DEFAULT now(), \
                updated_at  timestamptz NOT NULL DEFAULT now() \
            )"
        ).await
    }

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        let result = sqlx::query(
            "SELECT project, env, version, updated_at::text as applied_at FROM _dbd_meta WHERE project = $1"
        )
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
        self.ensure_meta_table().await?;
        sqlx::query(
            "INSERT INTO _dbd_meta (project, env, version) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (project) DO UPDATE \
             SET env = EXCLUDED.env, version = EXCLUDED.version, updated_at = now()"
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;

        Ok(())
    }
}
