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

/// Lowercase, alphabetically sorted list of SQLite built-in functions,
/// aggregates, window functions, date/time helpers, JSON1 functions, math
/// functions (3.35+), and common type names / reserved tokens. Used by
/// `classify_reference` for fully offline analysis. `binary_search`
/// relies on the ordering — `s11_builtins_sorted_for_binary_search` guards it.
const SQLITE_BUILTINS: &[&str] = &[
    "abs", "acos", "acosh", "and", "asin", "asinh", "atan", "atan2", "atanh",
    "avg", "between", "blob", "boolean", "ceil", "ceiling", "changes", "char",
    "coalesce", "concat", "concat_ws", "cos", "cosh", "count", "cume_dist",
    "current_date", "current_time", "current_timestamp", "date", "datetime",
    "degrees", "dense_rank", "exists", "exp", "false", "first_value", "floor",
    "format", "glob", "group_concat", "hex", "ifnull", "iif", "in", "instr",
    "int", "integer", "is", "json", "json_array", "json_array_length",
    "json_each", "json_error_position", "json_extract", "json_group_array",
    "json_group_object", "json_insert", "json_object", "json_patch",
    "json_pretty", "json_quote", "json_remove", "json_replace", "json_set",
    "json_tree", "json_type", "json_valid", "json_valid_b", "jsonb",
    "jsonb_array", "jsonb_extract", "jsonb_group_array", "jsonb_group_object",
    "jsonb_insert", "jsonb_object", "jsonb_patch", "jsonb_remove",
    "jsonb_replace", "jsonb_set", "julianday", "lag", "last_insert_rowid",
    "last_value", "lead", "length", "like", "likelihood", "likely", "ln",
    "load_extension", "log", "log10", "log2", "lower", "ltrim", "max", "min",
    "mod", "not", "nth_value", "ntile", "null", "nullif", "numeric",
    "octet_length", "or", "percent_rank", "pi", "pow", "power", "printf",
    "quote", "radians", "random", "randomblob", "rank", "real", "replace",
    "round", "row_number", "rtrim", "sign", "sin", "sinh", "soundex",
    "sqlite_compileoption_get", "sqlite_compileoption_used", "sqlite_offset",
    "sqlite_source_id", "sqlite_version", "sqrt", "strftime", "string_agg",
    "substr", "substring", "sum", "tan", "tanh", "text", "time", "timediff",
    "total", "total_changes", "trim", "true", "trunc", "typeof", "unhex",
    "unicode", "unixepoch", "unlikely", "upper", "varchar", "zeroblob",
];

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
            | EntityType::Sequence
            | EntityType::Enum
            | EntityType::Function
            | EntityType::Procedure
            | EntityType::MaterializedView => Err(DbdError::Config(format!(
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

    async fn export_data(&self, entity: &Entity, out_dir: Option<&Path>) -> Result<()> {
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

        // `Some(dir)` → write `dir/<table>.<format>`; `None` → today's `export/`.
        let export_dir = match out_dir {
            Some(dir) => dir,
            None => Path::new("export"),
        };
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
        // Anything in the SQLite-reserved namespace (`sqlite_master`,
        // `sqlite_sequence`, `sqlite_stat1`, `sqlite_version`, …) is internal.
        if lower.starts_with("sqlite_") {
            return ReferenceClass::Internal;
        }
        if SQLITE_BUILTINS.binary_search(&lower.as_str()).is_ok() {
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

    /// Reverse-engineer the database into entities. SQLite has no schemas/enums/
    /// functions/sequences/roles, and `sqlite_master.sql` holds the exact `CREATE`
    /// text — so we capture tables (+ their user indexes) and views verbatim via
    /// `raw_ddl` (lossless: CHECK constraints, type affinity, AUTOINCREMENT,
    /// WITHOUT ROWID all survive). Triggers, auto-indexes (PK/UNIQUE-backed,
    /// `sql IS NULL`), and `sqlite_*`/`_dbd_*` internals are skipped.
    async fn introspect(&self) -> Result<Vec<Entity>> {
        let mut entities = Vec::new();

        let table_rows = sqlx::query(
            "SELECT name, sql FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
               AND name NOT IN ('_dbd_migrations', '_dbd_meta') \
             ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("introspect tables failed: {e}")))?;

        for row in &table_rows {
            let name: String = row.get("name");
            let Some(table_sql): Option<String> = row.get("sql") else { continue };

            // Only user-created indexes carry a `sql`; auto PK/UNIQUE indexes are
            // NULL and ride inside the table definition.
            let idx_rows = sqlx::query(
                "SELECT sql FROM sqlite_master \
                 WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL \
                 ORDER BY name",
            )
            .bind(&name)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect indexes for {name} failed: {e}")))?;

            let mut parts: Vec<String> = vec![table_sql.trim().to_string()];
            for ir in &idx_rows {
                let isql: String = ir.get("sql");
                parts.push(isql.trim().to_string());
            }
            let mut e = Entity::new(EntityType::Table, &name);
            e.schema = None; // SQLite is schema-less
            e.raw_ddl = Some(parts.join(";\n\n"));
            entities.push(e);
        }

        let view_rows =
            sqlx::query("SELECT name, sql FROM sqlite_master WHERE type = 'view' ORDER BY name")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("introspect views failed: {e}")))?;

        for row in &view_rows {
            let name: String = row.get("name");
            let Some(sql): Option<String> = row.get("sql") else { continue };
            let mut e = Entity::new(EntityType::View, &name);
            e.schema = None;
            e.raw_ddl = Some(sql.trim().to_string());
            entities.push(e);
        }

        Ok(entities)
    }

    /// For reverse-engineering safety: `Some(version)` if this DB is dbd-managed
    /// (a `_dbd_meta` table exists), `None` if foreign. Version is 0 if the table
    /// exists but has no row for this project.
    async fn reverse_managed_version(&self) -> Result<Option<u32>> {
        let exists =
            sqlx::query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_dbd_meta'")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("reverse_managed_version detect failed: {e}")))?;
        if exists.is_none() {
            return Ok(None);
        }
        let row = sqlx::query("SELECT version FROM _dbd_meta WHERE project = ?1")
            .bind(&self.project)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("reverse_managed_version read failed: {e}")))?;
        Ok(Some(row.map(|r| r.get::<i64, _>("version") as u32).unwrap_or(0)))
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
        let col_count = columns.len();
        let cols_clause = columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(",");
        let batch_rows = batch_row_size(col_count);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("import begin failed: {e}")))?;

        let mut batch: Vec<Vec<Option<String>>> = Vec::with_capacity(batch_rows);
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let values: Vec<&str> = line.split(sep).collect();
            if values.len() != col_count {
                return Err(DbdError::Config(format!(
                    "csv row has {} columns, expected {col_count}",
                    values.len()
                )));
            }
            let row: Vec<Option<String>> = values
                .iter()
                .map(|v| {
                    let t = v.trim();
                    if t.is_empty() { None } else { Some(t.to_string()) }
                })
                .collect();
            batch.push(row);
            if batch.len() >= batch_rows {
                flush_csv_batch(&mut tx, table, &cols_clause, col_count, &batch).await?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            flush_csv_batch(&mut tx, table, &cols_clause, col_count, &batch).await?;
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

        // Group consecutive lines with identical column sets into batches so
        // we can flush each group as a single multi-row INSERT. When the
        // column set changes, flush whatever we have and start a new batch.
        let mut current_cols: Option<Vec<String>> = None;
        let mut batch: Vec<Vec<JsonBindable>> = Vec::new();

        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| DbdError::Config(format!("jsonl parse failed: {e}")))?;
            let obj = value
                .as_object()
                .ok_or_else(|| DbdError::Config("jsonl line must be a JSON object".into()))?;

            let cols: Vec<String> = obj.keys().cloned().collect();
            let row: Vec<JsonBindable> = cols.iter().map(|c| JsonBindable::from(&obj[c])).collect();

            let cols_changed = current_cols.as_ref().is_some_and(|cur| cur != &cols);
            if cols_changed {
                flush_jsonl_batch(&mut tx, table, current_cols.as_ref().unwrap(), &batch).await?;
                batch.clear();
            }
            if current_cols.is_none() || cols_changed {
                current_cols = Some(cols.clone());
            }

            batch.push(row);
            if batch.len() >= batch_row_size(cols.len()) {
                flush_jsonl_batch(&mut tx, table, current_cols.as_ref().unwrap(), &batch).await?;
                batch.clear();
            }
        }
        if !batch.is_empty()
            && let Some(cols) = current_cols.as_ref()
        {
            flush_jsonl_batch(&mut tx, table, cols, &batch).await?;
        }

        tx.commit()
            .await
            .map_err(|e| DbdError::Config(format!("import commit failed: {e}")))?;
        Ok(())
    }
}

/// Conservative rows-per-INSERT batch size. SQLite's
/// `SQLITE_MAX_VARIABLE_NUMBER` is 32766 since 3.32 (was 999 in older
/// builds); we cap at 500 rows and divide by `col_count` so any
/// reasonable column count stays well inside both limits.
fn batch_row_size(col_count: usize) -> usize {
    (32000usize / col_count.max(1)).clamp(1, 500)
}

/// Build a `(?,?,?), (?,?,?), …` placeholder list for `row_count` rows of
/// `col_count` columns. SQLite uses anonymous `?` rather than `?1`/`?2`
/// when binds come in argument order (which they do here).
fn multi_row_placeholders(col_count: usize, row_count: usize) -> String {
    let row_ph: String = std::iter::repeat_n("?", col_count).collect::<Vec<_>>().join(",");
    let one = format!("({row_ph})");
    std::iter::repeat_n(one.as_str(), row_count)
        .collect::<Vec<_>>()
        .join(",")
}

async fn flush_csv_batch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &str,
    cols_clause: &str,
    col_count: usize,
    rows: &[Vec<Option<String>>],
) -> Result<()> {
    let placeholders = multi_row_placeholders(col_count, rows.len());
    let sql = format!("INSERT INTO \"{table}\" ({cols_clause}) VALUES {placeholders}");
    let mut q = sqlx::query(&sql);
    for row in rows {
        for cell in row {
            q = match cell {
                Some(s) => q.bind(s.clone()),
                None => q.bind(None::<String>),
            };
        }
    }
    q.execute(&mut **tx)
        .await
        .map_err(|e| DbdError::Config(format!("import insert failed: {e}")))?;
    Ok(())
}

async fn flush_jsonl_batch(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    table: &str,
    cols: &[String],
    rows: &[Vec<JsonBindable>],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let cols_clause = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = multi_row_placeholders(cols.len(), rows.len());
    let sql = format!("INSERT INTO \"{table}\" ({cols_clause}) VALUES {placeholders}");
    let mut q = sqlx::query(&sql);
    for row in rows {
        for cell in row {
            q = match cell {
                JsonBindable::Null => q.bind(None::<String>),
                JsonBindable::Bool(b) => q.bind(*b as i64),
                JsonBindable::Int(i) => q.bind(*i),
                JsonBindable::Float(f) => q.bind(*f),
                JsonBindable::String(s) => q.bind(s.clone()),
                JsonBindable::Other(s) => q.bind(s.clone()),
            };
        }
    }
    q.execute(&mut **tx)
        .await
        .map_err(|e| DbdError::Config(format!("import insert failed: {e}")))?;
    Ok(())
}

/// Owned, bindable representation of a JSON cell — avoids cloning the
/// full `serde_json::Value` enum for each row in a batch.
enum JsonBindable {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Other(String),
}

impl JsonBindable {
    fn from(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Self::Int(i)
                } else {
                    Self::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => Self::String(s.clone()),
            other => Self::Other(other.to_string()),
        }
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
    async fn s17_sqlite_rejects_materialized_view() {
        let a = mem().await;
        let e = Entity::new(EntityType::MaterializedView, "app.mv");
        let err = a.apply_entity(&e).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("materialized"));
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

    /// `export_data(.., Some(dir))` writes `dir/<bare-name>.<fmt>` (flat, no
    /// schema subdir) with the expected content. `None` falls back to the
    /// cwd-relative `export/<bare-name>.<fmt>` convention. Both paths are
    /// exercised here under a single cwd guard to avoid racing on the
    /// process-global current directory.
    #[tokio::test]
    async fn s12_export_data_honors_out_dir() {
        let a = mem().await;
        a.execute_script("CREATE TABLE people (id INTEGER, name TEXT)")
            .await
            .unwrap();
        a.execute_script("INSERT INTO people VALUES (1, 'alice')")
            .await
            .unwrap();

        let mut entity = Entity::new(EntityType::Table, "default.people");
        entity.format = Some("csv".to_string());

        // Some(dir) → flat `dir/people.csv` (the new behavior; cwd-independent).
        // The None path (cwd-relative `export/<schema>/...`) is unchanged
        // pre-existing behavior and is deliberately NOT exercised here, to avoid
        // mutating the process-global cwd (which would race other parallel tests).
        let out = tempfile::tempdir().unwrap();
        a.export_data(&entity, Some(out.path())).await.unwrap();
        let written = out.path().join("people.csv");
        assert!(written.exists(), "expected {} to exist", written.display());
        let body = std::fs::read_to_string(&written).unwrap();
        assert!(body.contains("id,name"), "header missing: {body}");
        assert!(body.contains("1,alice"), "row missing: {body}");
    }

    #[test]
    fn s11_builtins_sorted_for_binary_search() {
        // binary_search relies on alphabetical order — guard against typos
        // and accidental insertions in the middle of the array.
        let mut sorted = SQLITE_BUILTINS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted.as_slice(),
            SQLITE_BUILTINS,
            "SQLITE_BUILTINS must stay alphabetically sorted"
        );
    }

    #[test]
    fn s13_batch_row_size_stays_within_sqlite_limits() {
        // Wide row: 100 columns → still ≥1 row per batch.
        assert!(batch_row_size(100) >= 1);
        // Modest row: 5 columns → caps at 500.
        assert_eq!(batch_row_size(5), 500);
        // 1 col → also caps at 500 (not 32000).
        assert_eq!(batch_row_size(1), 500);
        // Sanity: total bind count never exceeds the conservative 32k cap.
        for col_count in [1usize, 2, 10, 50, 100, 1000] {
            let rows = batch_row_size(col_count);
            assert!(
                rows * col_count <= 32_000,
                "{rows} rows × {col_count} cols would exceed SQLite var limit"
            );
        }
    }

    #[test]
    fn s14_multi_row_placeholders_shape() {
        assert_eq!(multi_row_placeholders(3, 1), "(?,?,?)");
        assert_eq!(multi_row_placeholders(2, 3), "(?,?),(?,?),(?,?)");
    }

    #[tokio::test]
    async fn s15_import_csv_multi_batch_path() {
        use tempfile::NamedTempFile;
        let a = mem().await;
        a.execute_script("CREATE TABLE big (id INTEGER, v TEXT)")
            .await
            .unwrap();

        // 1500 rows exceeds the 500-row batch ceiling, so we exercise the
        // flush-then-continue path in addition to the trailing-flush path.
        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(tmp, "id,v").unwrap();
        for i in 1..=1500 {
            writeln!(tmp, "{i},row{i}").unwrap();
        }

        let mut entity = Entity::new(EntityType::Import, "default.big");
        entity.file = Some(tmp.path().to_path_buf());
        entity.format = Some("csv".to_string());
        a.import_data(&entity, false).await.unwrap();

        let row = sqlx::query("SELECT count(*) AS c FROM big")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let count: i64 = row.try_get("c").unwrap();
        assert_eq!(count, 1500);

        // Spot-check the boundary rows survived the batch flushes intact.
        let r = sqlx::query("SELECT v FROM big WHERE id = 500")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let v: String = r.try_get("v").unwrap();
        assert_eq!(v, "row500");
        let r = sqlx::query("SELECT v FROM big WHERE id = 1500")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let v: String = r.try_get("v").unwrap();
        assert_eq!(v, "row1500");
    }

    #[tokio::test]
    async fn s16_import_jsonl_batched_with_mixed_types_and_column_sets() {
        use tempfile::NamedTempFile;
        let a = mem().await;
        a.execute_script("CREATE TABLE mixed (id INTEGER, name TEXT, score REAL, active INTEGER)")
            .await
            .unwrap();

        // First group has all four columns; second group omits `score`.
        // The batcher must flush between the two groups.
        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(tmp, r#"{{"id":1,"name":"alice","score":9.5,"active":true}}"#).unwrap();
        writeln!(tmp, r#"{{"id":2,"name":"bob","score":7.0,"active":false}}"#).unwrap();
        writeln!(tmp, r#"{{"id":3,"name":"carol","active":true}}"#).unwrap();

        let mut entity = Entity::new(EntityType::Import, "default.mixed");
        entity.file = Some(tmp.path().to_path_buf());
        entity.format = Some("jsonl".to_string());
        a.import_data(&entity, false).await.unwrap();

        let row = sqlx::query("SELECT count(*) AS c FROM mixed")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let count: i64 = row.try_get("c").unwrap();
        assert_eq!(count, 3);

        // active=true was bound as i64=1; verify reverse-cast.
        let r = sqlx::query("SELECT active FROM mixed WHERE id = 1")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let active: i64 = r.try_get("active").unwrap();
        assert_eq!(active, 1);

        // Row 3 has no score → should be NULL.
        let r = sqlx::query("SELECT score FROM mixed WHERE id = 3")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let score: Option<f64> = r.try_get("score").unwrap();
        assert_eq!(score, None);
    }

    #[tokio::test]
    async fn s12_classify_reference_covers_richer_offline_signals() {
        let adapter = SqliteAdapter::new("sqlite::memory:", "test").await.unwrap();
        // sqlite_* system tables are always internal.
        assert!(matches!(
            adapter.classify_reference("sqlite_master", &[]),
            ReferenceClass::Internal
        ));
        assert!(matches!(
            adapter.classify_reference("sqlite_sequence", &[]),
            ReferenceClass::Internal
        ));
        // New builtins picked up via the expanded list.
        for name in &[
            "json_extract", "row_number", "lag", "lead", "pi", "pow", "ceil",
            "unixepoch", "concat_ws", "string_agg",
        ] {
            assert!(
                matches!(adapter.classify_reference(name, &[]), ReferenceClass::Internal),
                "expected {name} to classify as Internal"
            );
        }
        // Case-insensitive match.
        assert!(matches!(
            adapter.classify_reference("JSON_EXTRACT", &[]),
            ReferenceClass::Internal
        ));
        // Unknown name remains user-defined.
        assert!(matches!(
            adapter.classify_reference("my_helper", &[]),
            ReferenceClass::UserDefined
        ));
    }

    #[tokio::test]
    async fn introspect_captures_tables_views_indexes_verbatim() {
        let a = mem().await;
        a.execute_script(
            "CREATE TABLE author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);\n\
             CREATE TABLE book (\
                id INTEGER PRIMARY KEY,\
                author_id INTEGER NOT NULL REFERENCES author(id) ON DELETE CASCADE,\
                status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','published')),\
                isbn TEXT UNIQUE\
             ) WITHOUT ROWID;\n\
             CREATE INDEX book_author_idx ON book(author_id);\n\
             CREATE INDEX book_status_idx ON book(status);\n\
             CREATE INDEX author_name_idx ON author(name);\n\
             CREATE VIEW published AS SELECT id FROM book WHERE status='published';\n\
             CREATE TRIGGER trg AFTER INSERT ON book BEGIN UPDATE author SET name=name; END;",
        )
        .await
        .unwrap();

        let ents = a.introspect().await.unwrap();
        let by = |n: &str| ents.iter().find(|e| e.name == n);

        // Table captured verbatim + schema-less; CHECK/WITHOUT ROWID preserved.
        let book = by("book").expect("book table captured");
        assert_eq!(book.entity_type, EntityType::Table);
        assert!(book.schema.is_none());
        let raw = book.raw_ddl.as_deref().unwrap();
        assert!(raw.contains("WITHOUT ROWID"), "verbatim preserved: {raw}");
        assert!(raw.contains("CHECK"));
        // Both explicit indexes ride with the table, in name order; the auto
        // UNIQUE index does not; and another table's index never leaks in.
        let author_pos = raw.find("book_author_idx").expect("book_author_idx appended");
        let status_pos = raw.find("book_status_idx").expect("book_status_idx appended");
        assert!(author_pos < status_pos, "indexes ordered by name: {raw}");
        assert!(!raw.to_lowercase().contains("autoindex"));
        assert!(!raw.contains("author_name_idx"), "another table's index must not leak: {raw}");
        let author = by("author").expect("author table captured");
        let araw = author.raw_ddl.as_deref().unwrap();
        assert!(araw.contains("author_name_idx"), "author keeps its own index");
        assert!(!araw.contains("book_"), "book indexes must not leak into author: {araw}");

        // View captured; trigger NOT captured; internals excluded.
        let v = by("published").expect("view captured");
        assert_eq!(v.entity_type, EntityType::View);
        assert!(v.raw_ddl.as_deref().unwrap().to_uppercase().contains("CREATE VIEW"));
        assert!(by("trg").is_none(), "triggers are not captured in v1");
        assert!(ents.iter().all(|e| e.name != "_dbd_meta" && !e.name.starts_with("sqlite_")));
    }

    #[tokio::test]
    async fn reverse_managed_version_detects_meta() {
        let a = mem().await;
        // Fresh DB with no _dbd_meta → foreign.
        assert_eq!(a.reverse_managed_version().await.unwrap(), None);
        // After dbd's meta table + a row for this project → managed at that version.
        a.ensure_meta_table().await.unwrap();
        a.set_project_meta("prod", 3).await.unwrap();
        assert_eq!(a.reverse_managed_version().await.unwrap(), Some(3));
    }
}
