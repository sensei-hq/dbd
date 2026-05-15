#![cfg(feature = "sqlite")]

//! SQLite adapter via `sqlx`.
//!
//! SQLite has no schemas, no enum types, no roles, no stored procedures,
//! and no extensions. Entities of those types are reported as unsupported.
//! `Schema` entities are silently skipped (no-op) so that projects shared
//! between Postgres and SQLite targets can still load.
//!
//! Entity names use Postgres-style `schema.name` qualification; the adapter
//! strips the leading schema component when querying SQLite (`auth.users`
//! → `users`).

use std::path::Path;

use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Column, Row, SqlitePool};

use super::{DatabaseAdapter, ProjectMeta, ReferenceClass};
use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};
use crate::script;

/// SQLite adapter using a sqlx connection pool.
pub struct SqliteAdapter {
    pool: SqlitePool,
    project: String,
}

impl SqliteAdapter {
    /// Create a new adapter from a connection URL.
    ///
    /// Accepted forms:
    /// - `sqlite://path/to/db.sqlite` (file, created if missing)
    /// - `sqlite::memory:` (in-memory, shared within the pool)
    /// - `file:/abs/path.db` (sqlx native form)
    pub async fn new(url: &str, project: &str) -> Result<Self> {
        let options: SqliteConnectOptions = url
            .parse::<SqliteConnectOptions>()
            .map_err(|e| DbdError::Config(format!("Invalid sqlite URL '{url}': {e}")))?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| DbdError::Config(format!("SQLite connection failed: {e}")))?;

        Ok(Self {
            pool,
            project: project.to_string(),
        })
    }

    /// Strip the leading `schema.` from a Postgres-style entity name.
    /// Returns the original string when no `.` is present.
    fn bare_name(name: &str) -> &str {
        match name.split_once('.') {
            Some((_schema, rest)) => rest,
            None => name,
        }
    }
}

#[async_trait]
impl DatabaseAdapter for SqliteAdapter {
    async fn connect(&mut self) -> Result<()> {
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.pool.close().await;
        Ok(())
    }

    async fn test_connection(&self) -> Result<bool> {
        Ok(sqlx::query("SELECT 1").fetch_one(&self.pool).await.is_ok())
    }

    async fn execute_script(&self, sql: &str) -> Result<()> {
        sqlx::raw_sql(sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("SQL execution failed: {e}")))?;
        Ok(())
    }

    async fn apply_entity(&self, entity: &Entity) -> Result<()> {
        match entity.entity_type {
            // SQLite has no schemas — silently treat as no-op so cross-target
            // projects can still declare schemas for documentation.
            EntityType::Schema => Ok(()),
            // External entities never apply.
            EntityType::External => Ok(()),
            EntityType::Extension
            | EntityType::Role
            | EntityType::Enum
            | EntityType::Function
            | EntityType::Procedure => Err(DbdError::Config(format!(
                "SQLite adapter does not support {:?} entities ({})",
                entity.entity_type, entity.name
            ))),
            _ => {
                let sql = match script::ddl_from_entity(entity) {
                    Some(s) => s,
                    None => return Ok(()),
                };
                self.execute_script(&sql).await.map_err(|e| {
                    DbdError::Config(format!(
                        "Failed to apply {:?} {}: {}",
                        entity.entity_type, entity.name, e
                    ))
                })
            }
        }
    }

    async fn import_data(&self, entity: &Entity, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }

        let file_path = entity.file.as_ref().ok_or_else(|| {
            DbdError::Config(format!("No file for import entity {}", entity.name))
        })?;
        let table = Self::bare_name(&entity.name);
        let format = entity.format.as_deref().unwrap_or("csv");

        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let data = std::fs::read_to_string(file_path)?;

        match format {
            "csv" | "tsv" => self.import_delimited(&data, table, format == "tsv").await,
            "jsonl" => self.import_jsonl(&data, table).await,
            _ => Err(DbdError::Config(format!(
                "Unsupported sqlite import format: {format}"
            ))),
        }
    }

    async fn export_data(&self, entity: &Entity) -> Result<()> {
        let table = Self::bare_name(&entity.name);
        let format = entity.format.as_deref().unwrap_or("csv");

        let rows = sqlx::query(&format!("SELECT * FROM \"{table}\""))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("export query failed: {e}")))?;

        let columns: Vec<String> = rows
            .first()
            .map(|row| row.columns().iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();

        let mut output = String::new();
        match format {
            "csv" | "tsv" => {
                let sep = if format == "tsv" { '\t' } else { ',' };
                output.push_str(&columns.join(&sep.to_string()));
                output.push('\n');
                for row in &rows {
                    let cells: Vec<String> = (0..columns.len())
                        .map(|i| sqlite_cell_to_text(row, i))
                        .collect();
                    output.push_str(&cells.join(&sep.to_string()));
                    output.push('\n');
                }
            }
            "jsonl" => {
                for row in &rows {
                    let mut obj = serde_json::Map::new();
                    for (i, col) in columns.iter().enumerate() {
                        obj.insert(col.clone(), sqlite_cell_to_json(row, i));
                    }
                    output.push_str(&serde_json::Value::Object(obj).to_string());
                    output.push('\n');
                }
            }
            _ => {
                return Err(DbdError::Config(format!(
                    "Unsupported sqlite export format: {format}"
                )));
            }
        }

        let export_dir = Path::new("export");
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        std::fs::create_dir_all(export_dir)?;
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        std::fs::write(export_dir.join(format!("{table}.{format}")), output.as_bytes())?;
        Ok(())
    }

    async fn load_catalog(&mut self) -> Result<()> {
        // SQLite has no extension catalog to load.
        Ok(())
    }

    fn classify_reference(&self, name: &str, _installed: &[String]) -> ReferenceClass {
        let lower = name.to_lowercase();
        // Common SQLite built-in functions / types.
        const BUILTINS: &[&str] = &[
            "abs", "changes", "char", "coalesce", "count", "current_date",
            "current_time", "current_timestamp", "date", "datetime", "exists",
            "format", "glob", "group_concat", "hex", "ifnull", "iif", "instr",
            "json", "json_array", "json_each", "json_extract", "json_group_array",
            "json_group_object", "json_object", "json_tree", "json_valid",
            "julianday", "last_insert_rowid", "length", "like", "likely", "lower",
            "ltrim", "max", "min", "nullif", "printf", "quote", "random",
            "randomblob", "replace", "round", "rtrim", "sign", "soundex",
            "sqlite_version", "strftime", "substr", "substring", "sum", "time",
            "total", "total_changes", "trim", "typeof", "unhex", "unicode",
            "unlikely", "upper", "zeroblob",
            // types
            "integer", "int", "real", "text", "blob", "numeric", "boolean",
            "true", "false", "null", "and", "or", "not", "in", "between", "is",
        ];
        if BUILTINS.contains(&lower.as_str()) {
            return ReferenceClass::Internal;
        }
        ReferenceClass::UserDefined
    }

    async fn resolve_entity(&self, name: &str) -> Result<Option<String>> {
        let bare = Self::bare_name(name);
        let row = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
        )
        .bind(bare)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("resolve_entity query failed: {e}")))?;
        Ok(row.map(|_| name.to_string()))
    }

    async fn list_entities(&self) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT name FROM sqlite_master \
             WHERE type IN ('table', 'view') \
               AND name NOT LIKE 'sqlite_%' \
               AND name NOT IN ('_dbd_migrations', '_dbd_meta')",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("list_entities failed: {e}")))?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("name")).collect())
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        self.execute_script(
            "CREATE TABLE IF NOT EXISTS _dbd_migrations ( \
                project     TEXT NOT NULL, \
                version     INTEGER NOT NULL, \
                applied_at  TEXT NOT NULL DEFAULT (datetime('now')), \
                description TEXT, \
                checksum    TEXT, \
                PRIMARY KEY (project, version) \
            )",
        )
        .await
    }

    async fn get_db_version(&self) -> Result<u32> {
        let result = sqlx::query("SELECT version FROM _dbd_meta WHERE project = ?1")
            .bind(&self.project)
            .fetch_optional(&self.pool)
            .await;

        match result {
            Ok(Some(row)) => {
                let version: i64 = row.get("version");
                Ok(version as u32)
            }
            Ok(None) | Err(_) => Ok(0),
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
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project, version) DO NOTHING",
        )
        .bind(&self.project)
        .bind(version as i64)
        .bind(description)
        .bind(checksum)
        .execute(&self.pool)
        .await
        .map_err(|e| DbdError::Migration(format!("Record migration failed: {e}")))?;
        Ok(())
    }

    async fn clear_project_migrations(&self) -> Result<()> {
        sqlx::query("DELETE FROM _dbd_migrations WHERE project = ?1")
            .bind(&self.project)
            .execute(&self.pool)
            .await
            .ok();
        Ok(())
    }

    async fn ensure_import_procedure(&self) -> Result<()> {
        // No stored procedures in SQLite.
        Ok(())
    }

    async fn ensure_meta_table(&self) -> Result<()> {
        self.execute_script(
            "CREATE TABLE IF NOT EXISTS _dbd_meta ( \
                project    TEXT NOT NULL PRIMARY KEY, \
                env        TEXT NOT NULL DEFAULT 'dev', \
                version    INTEGER NOT NULL DEFAULT 0, \
                created_at TEXT NOT NULL DEFAULT (datetime('now')), \
                updated_at TEXT NOT NULL DEFAULT (datetime('now')) \
            )",
        )
        .await
    }

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        let result = sqlx::query(
            "SELECT project, env, version, updated_at AS applied_at FROM _dbd_meta WHERE project = ?1",
        )
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await;

        match result {
            Ok(Some(row)) => Ok(Some(ProjectMeta {
                project: row.get("project"),
                env: row.get("env"),
                version: row.get::<i64, _>("version") as u32,
                applied_at: row.try_get("applied_at").ok(),
            })),
            Ok(None) | Err(_) => Ok(None),
        }
    }

    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()> {
        self.ensure_meta_table().await?;
        sqlx::query(
            "INSERT INTO _dbd_meta (project, env, version) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT (project) DO UPDATE \
             SET env = excluded.env, version = excluded.version, updated_at = datetime('now')",
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;
        Ok(())
    }
}

// ── helpers ────────────────────────────────────────────────────

impl SqliteAdapter {
    async fn import_delimited(&self, data: &str, table: &str, tab: bool) -> Result<()> {
        let sep = if tab { '\t' } else { ',' };
        let mut lines = data.lines();
        let header_line = match lines.next() {
            Some(h) if !h.trim().is_empty() => h,
            _ => return Ok(()),
        };
        let columns: Vec<&str> = header_line.split(sep).map(|s| s.trim()).collect();
        if columns.is_empty() {
            return Ok(());
        }

        let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
        let quoted_cols: Vec<String> = columns.iter().map(|c| format!("\"{c}\"")).collect();
        let insert_sql = format!(
            "INSERT INTO \"{table}\" ({}) VALUES ({})",
            quoted_cols.join(","),
            placeholders.join(",")
        );

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("import begin failed: {e}")))?;

        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let values: Vec<&str> = line.split(sep).collect();
            if values.len() != columns.len() {
                return Err(DbdError::Config(format!(
                    "csv row has {} columns, expected {}",
                    values.len(),
                    columns.len()
                )));
            }
            let mut q = sqlx::query(&insert_sql);
            for v in &values {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    q = q.bind(None::<String>);
                } else {
                    q = q.bind(trimmed.to_string());
                }
            }
            q.execute(&mut *tx)
                .await
                .map_err(|e| DbdError::Config(format!("import insert failed: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| DbdError::Config(format!("import commit failed: {e}")))?;
        Ok(())
    }

    async fn import_jsonl(&self, data: &str, table: &str) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("import begin failed: {e}")))?;

        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                DbdError::Config(format!("jsonl parse failed: {e}"))
            })?;
            let obj = value.as_object().ok_or_else(|| {
                DbdError::Config("jsonl line must be a JSON object".into())
            })?;

            let columns: Vec<&String> = obj.keys().collect();
            let placeholders: Vec<String> =
                (1..=columns.len()).map(|i| format!("?{i}")).collect();
            let quoted_cols: Vec<String> =
                columns.iter().map(|c| format!("\"{c}\"")).collect();
            let insert_sql = format!(
                "INSERT INTO \"{table}\" ({}) VALUES ({})",
                quoted_cols.join(","),
                placeholders.join(",")
            );

            let mut q = sqlx::query(&insert_sql);
            for c in &columns {
                let val = &obj[*c];
                match val {
                    serde_json::Value::Null => q = q.bind(None::<String>),
                    serde_json::Value::Bool(b) => q = q.bind(*b as i64),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            q = q.bind(i);
                        } else {
                            q = q.bind(n.as_f64().unwrap_or(0.0));
                        }
                    }
                    serde_json::Value::String(s) => q = q.bind(s.clone()),
                    other => q = q.bind(other.to_string()),
                }
            }
            q.execute(&mut *tx)
                .await
                .map_err(|e| DbdError::Config(format!("import insert failed: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| DbdError::Config(format!("import commit failed: {e}")))?;
        Ok(())
    }
}

fn sqlite_cell_to_text(row: &sqlx::sqlite::SqliteRow, idx: usize) -> String {
    if let Ok(v) = row.try_get::<Option<String>, _>(idx) {
        return v.unwrap_or_default();
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(idx) {
        return v.map(|n| n.to_string()).unwrap_or_default();
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(idx) {
        return v.map(|n| n.to_string()).unwrap_or_default();
    }
    String::new()
}

fn sqlite_cell_to_json(row: &sqlx::sqlite::SqliteRow, idx: usize) -> serde_json::Value {
    if let Ok(Some(s)) = row.try_get::<Option<String>, _>(idx) {
        return serde_json::Value::String(s);
    }
    if let Ok(Some(n)) = row.try_get::<Option<i64>, _>(idx) {
        return serde_json::Value::Number(n.into());
    }
    if let Ok(Some(n)) = row.try_get::<Option<f64>, _>(idx)
        && let Some(jn) = serde_json::Number::from_f64(n)
    {
        return serde_json::Value::Number(jn);
    }
    serde_json::Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    async fn mem() -> SqliteAdapter {
        SqliteAdapter::new("sqlite::memory:", "test").await.unwrap()
    }

    #[tokio::test]
    async fn s1_apply_table_then_list_entities() {
        let a = mem().await;
        a.execute_script("CREATE TABLE foo (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .unwrap();
        let list = a.list_entities().await.unwrap();
        assert_eq!(list, vec!["foo".to_string()]);
    }

    #[tokio::test]
    async fn s2_resolve_entity_qualified_name() {
        let a = mem().await;
        a.execute_script("CREATE TABLE users (id INTEGER)")
            .await
            .unwrap();
        // Qualified "default.users" should still resolve via bare name.
        let r = a.resolve_entity("default.users").await.unwrap();
        assert_eq!(r.as_deref(), Some("default.users"));
        assert!(a.resolve_entity("default.missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn s3_schema_entity_is_noop() {
        let a = mem().await;
        let schema = Entity::new(EntityType::Schema, "auth");
        a.apply_entity(&schema).await.unwrap();
    }

    #[tokio::test]
    async fn s4_unsupported_entity_types_error() {
        let a = mem().await;
        for t in [
            EntityType::Extension,
            EntityType::Role,
            EntityType::Enum,
            EntityType::Function,
            EntityType::Procedure,
        ] {
            let e = Entity::new(t, "x");
            assert!(a.apply_entity(&e).await.is_err(), "{t:?} should error");
        }
    }

    #[tokio::test]
    async fn s5_migrations_table_roundtrip() {
        let a = mem().await;
        a.ensure_migrations_table().await.unwrap();
        a.apply_migration(1, "CREATE TABLE m (id INTEGER)", "init", "abc")
            .await
            .unwrap();

        // Idempotent re-apply doesn't error.
        a.apply_migration(1, "", "init", "abc").await.unwrap();
    }

    #[tokio::test]
    async fn s6_meta_set_get() {
        let a = mem().await;
        a.ensure_meta_table().await.unwrap();
        assert_eq!(a.get_db_version().await.unwrap(), 0);
        a.set_project_meta("dev", 3).await.unwrap();
        assert_eq!(a.get_db_version().await.unwrap(), 3);
        let m = a.get_project_meta().await.unwrap().unwrap();
        assert_eq!(m.env, "dev");
        assert_eq!(m.version, 3);
    }

    #[tokio::test]
    async fn s7_internal_tables_excluded_from_list() {
        let a = mem().await;
        a.ensure_meta_table().await.unwrap();
        a.ensure_migrations_table().await.unwrap();
        a.execute_script("CREATE TABLE keep_me (id INTEGER)")
            .await
            .unwrap();
        let list = a.list_entities().await.unwrap();
        assert_eq!(list, vec!["keep_me".to_string()]);
    }

    #[tokio::test]
    async fn s8_classify_reference_builtins() {
        let a = mem().await;
        assert_eq!(a.classify_reference("count", &[]), ReferenceClass::Internal);
        assert_eq!(a.classify_reference("json_extract", &[]), ReferenceClass::Internal);
        assert_eq!(a.classify_reference("my_func", &[]), ReferenceClass::UserDefined);
    }

    #[tokio::test]
    async fn s9_import_csv_inserts_rows() {
        use tempfile::NamedTempFile;
        let a = mem().await;
        a.execute_script("CREATE TABLE rows (id INTEGER, name TEXT)")
            .await
            .unwrap();

        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(tmp, "id,name").unwrap();
        writeln!(tmp, "1,alice").unwrap();
        writeln!(tmp, "2,bob").unwrap();
        let path = tmp.path().to_path_buf();

        let mut entity = Entity::new(EntityType::Import, "default.rows");
        entity.file = Some(path);
        entity.format = Some("csv".to_string());
        a.import_data(&entity, false).await.unwrap();

        let row = sqlx::query("SELECT count(*) AS c FROM rows")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let count: i64 = row.get("c");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn s10_bare_name_strips_schema() {
        assert_eq!(SqliteAdapter::bare_name("auth.users"), "users");
        assert_eq!(SqliteAdapter::bare_name("plain"), "plain");
    }
}
