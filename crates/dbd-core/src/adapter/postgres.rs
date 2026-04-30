#![cfg(feature = "postgres")]

use std::collections::HashSet;
use std::path::Path;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

use super::{DatabaseAdapter, ProjectMeta, ReferenceClass};

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

/// PostgreSQL adapter using sqlx connection pool.
pub struct PostgresAdapter {
    pool: PgPool,
    project: String,
    builtin_functions: HashSet<String>,
    builtin_types: HashSet<String>,
    extension_objects: std::collections::HashMap<String, String>,
}

impl PostgresAdapter {
    /// Create a new adapter from a connection URL.
    pub async fn new(url: &str, project: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| DbdError::Config(format!("Database connection failed: {e}")))?;

        Ok(Self {
            pool,
            project: project.to_string(),
            builtin_functions: HashSet::new(),
            builtin_types: HashSet::new(),
            extension_objects: std::collections::HashMap::new(),
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
        let copy_sql = format!(
            "COPY (SELECT * FROM \"{}\") TO STDOUT WITH (FORMAT csv, HEADER true)",
            qualified
        );

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
        let format = entity.format.as_deref().unwrap_or("csv");
        std::fs::write(export_dir.join(format!("{name}.{format}")), &data)?;

        Ok(())
    }

    async fn load_catalog(&mut self) -> Result<()> {
        // Built-in functions
        let rows = sqlx::query(
            "SELECT proname FROM pg_proc p \
             JOIN pg_namespace n ON p.pronamespace = n.oid \
             WHERE n.nspname = 'pg_catalog'"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Catalog query failed: {e}")))?;

        for row in &rows {
            let name: String = row.get("proname");
            self.builtin_functions.insert(name);
        }

        // Built-in types
        let rows = sqlx::query(
            "SELECT typname FROM pg_type \
             WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'pg_catalog')"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Type catalog query failed: {e}")))?;

        for row in &rows {
            let name: String = row.get("typname");
            self.builtin_types.insert(name);
        }

        // Extension functions
        let rows = sqlx::query(
            "SELECT p.proname AS func_name, e.extname AS extension \
             FROM pg_proc p \
             JOIN pg_depend d ON d.objid = p.oid AND d.deptype = 'e' \
             JOIN pg_extension e ON e.oid = d.refobjid"
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        for row in &rows {
            let func: String = row.get("func_name");
            let ext: String = row.get("extension");
            self.extension_objects.insert(func, ext);
        }

        Ok(())
    }

    fn classify_reference(&self, name: &str, _installed: &[String]) -> ReferenceClass {
        let lower = name.to_lowercase();

        // SQL noise (keywords, types)
        if Self::is_sql_noise(&lower) {
            return ReferenceClass::Internal;
        }

        // Catalog lookup (if loaded)
        if self.builtin_functions.contains(&lower) || self.builtin_types.contains(&lower) {
            return ReferenceClass::Internal;
        }
        if let Some(ext) = self.extension_objects.get(&lower) {
            return ReferenceClass::Extension(ext.clone());
        }

        // Static pattern fallback
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
