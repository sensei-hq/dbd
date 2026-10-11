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
use sqlx::{Row, SqlitePool};

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
    "abs",
    "acos",
    "acosh",
    "and",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "avg",
    "between",
    "blob",
    "boolean",
    "ceil",
    "ceiling",
    "changes",
    "char",
    "coalesce",
    "concat",
    "concat_ws",
    "cos",
    "cosh",
    "count",
    "cume_dist",
    "current_date",
    "current_time",
    "current_timestamp",
    "date",
    "datetime",
    "degrees",
    "dense_rank",
    "exists",
    "exp",
    "false",
    "first_value",
    "floor",
    "format",
    "glob",
    "group_concat",
    "hex",
    "ifnull",
    "iif",
    "in",
    "instr",
    "int",
    "integer",
    "is",
    "json",
    "json_array",
    "json_array_length",
    "json_each",
    "json_error_position",
    "json_extract",
    "json_group_array",
    "json_group_object",
    "json_insert",
    "json_object",
    "json_patch",
    "json_pretty",
    "json_quote",
    "json_remove",
    "json_replace",
    "json_set",
    "json_tree",
    "json_type",
    "json_valid",
    "json_valid_b",
    "jsonb",
    "jsonb_array",
    "jsonb_extract",
    "jsonb_group_array",
    "jsonb_group_object",
    "jsonb_insert",
    "jsonb_object",
    "jsonb_patch",
    "jsonb_remove",
    "jsonb_replace",
    "jsonb_set",
    "julianday",
    "lag",
    "last_insert_rowid",
    "last_value",
    "lead",
    "length",
    "like",
    "likelihood",
    "likely",
    "ln",
    "load_extension",
    "log",
    "log10",
    "log2",
    "lower",
    "ltrim",
    "max",
    "min",
    "mod",
    "not",
    "nth_value",
    "ntile",
    "null",
    "nullif",
    "numeric",
    "octet_length",
    "or",
    "percent_rank",
    "pi",
    "pow",
    "power",
    "printf",
    "quote",
    "radians",
    "random",
    "randomblob",
    "rank",
    "real",
    "replace",
    "round",
    "row_number",
    "rtrim",
    "sign",
    "sin",
    "sinh",
    "soundex",
    "sqlite_compileoption_get",
    "sqlite_compileoption_used",
    "sqlite_offset",
    "sqlite_source_id",
    "sqlite_version",
    "sqrt",
    "strftime",
    "string_agg",
    "substr",
    "substring",
    "sum",
    "tan",
    "tanh",
    "text",
    "time",
    "timediff",
    "total",
    "total_changes",
    "trim",
    "true",
    "trunc",
    "typeof",
    "unhex",
    "unicode",
    "unixepoch",
    "unlikely",
    "upper",
    "varchar",
    "zeroblob",
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

    /// Open the database at `url` only if it already exists — `None` when its
    /// file does not (an in-memory URL names no file, so it is `None` too).
    ///
    /// [`Self::new`] creates a missing file, as a run that builds the
    /// database must. A dry run writes nothing, and asking a database that is
    /// not there for its guards created it: `reset --dry-run` left an empty
    /// database file behind. A file that is not there holds nothing to guard.
    pub async fn open_existing(url: &str, project: &str) -> Result<Option<Self>> {
        let options: SqliteConnectOptions = url
            .parse::<SqliteConnectOptions>()
            .map_err(|e| DbdError::Config(format!("Invalid sqlite URL '{url}': {e}")))?
            .create_if_missing(false);
        if !options.get_filename().is_file() {
            return Ok(None);
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| DbdError::Config(format!("SQLite connection failed: {e}")))?;
        Ok(Some(Self {
            pool,
            project: project.to_string(),
        }))
    }

    /// Strip the leading `schema.` from a Postgres-style entity name.
    /// Returns the original string when no `.` is present.
    fn bare_name(name: &str) -> &str {
        match name.split_once('.') {
            Some((_schema, rest)) => rest,
            None => name,
        }
    }

    /// Inner body for the migrations-table half of `heal_bookkeeping`. Kept as a
    /// private inherent method (not a trait method) so it can be composed with
    /// `ensure_meta_table_inner` inside `heal_bookkeeping`.
    async fn ensure_migrations_table_inner(&self) -> Result<()> {
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

    /// Inner body for the meta-table half of `heal_bookkeeping`. Kept as a
    /// private inherent method (not a trait method) so it can be composed with
    /// `ensure_migrations_table_inner` inside `heal_bookkeeping`, and called
    /// directly by `set_project_meta`.
    async fn ensure_meta_table_inner(&self) -> Result<()> {
        self.execute_script(
            "CREATE TABLE IF NOT EXISTS _dbd_meta ( \
                project    TEXT NOT NULL PRIMARY KEY, \
                env        TEXT NOT NULL DEFAULT 'dev', \
                version    INTEGER NOT NULL DEFAULT 0, \
                scope      TEXT, \
                created_at TEXT NOT NULL DEFAULT (datetime('now')), \
                updated_at TEXT NOT NULL DEFAULT (datetime('now')) \
            )",
        )
        .await?;
        // SQLite has no `ADD COLUMN IF NOT EXISTS` — add `scope` only when missing.
        let has_scope =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_table_info('_dbd_meta') WHERE name = 'scope'")
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0)
                > 0;
        if !has_scope {
            self.execute_script("ALTER TABLE _dbd_meta ADD COLUMN scope TEXT")
                .await?;
        }
        Ok(())
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

    async fn import_data(&self, entity: &Entity, null_value: &str, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }

        let file_path = entity
            .file
            .as_ref()
            .ok_or_else(|| DbdError::Config(format!("No file for import entity {}", entity.name)))?;
        let table = Self::bare_name(&entity.name);
        let format = entity.format.as_deref().unwrap_or("csv");

        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let data = std::fs::read_to_string(file_path)?;

        match format {
            "csv" | "tsv" => {
                let records = super::Delimited::new(&data, format == "tsv", file_path);
                self.import_delimited(records, table, null_value).await
            }
            "json" | "jsonl" => {
                let records = super::json_records(&data, format, file_path)?;
                self.import_json_records(&records, table).await
            }
            _ => Err(DbdError::Config(format!("Unsupported sqlite import format: {format}"))),
        }
    }

    async fn truncate_table(&self, table: &str) -> Result<()> {
        // SQLite has no `TRUNCATE`; an unfiltered `DELETE` is its equivalent,
        // and SQLite optimizes it to drop the table's pages wholesale. The
        // schema is stripped as everywhere else here — SQLite has none.
        self.execute_script(&format!(
            "DELETE FROM {}",
            crate::sql_quote::ident(Self::bare_name(table))
        ))
        .await
    }

    async fn export_data(&self, entity: &Entity, out_dir: Option<&Path>) -> Result<()> {
        let table = Self::bare_name(&entity.name);
        let format = entity.format.as_deref().unwrap_or("csv");

        // The file is named after the table, and a quoted identifier may hold path
        // separators — with an absolute one `Path::join` discards the export
        // directory outright. Checked before the query so an unfilable table costs
        // nothing to refuse.
        if !crate::path_safe::is_safe_segment(table) {
            return Err(DbdError::Config(format!(
                "cannot export {}: its name is not usable as a file name (it contains a path separator, \
                 or is '.' or '..'). Rename the table, or export it by hand",
                entity.name
            )));
        }

        // The columns an import can write, in table order — read from the
        // catalog, not from the first row: an empty table has none, and its
        // export was a blank line where the header belongs. Generated columns
        // (`hidden` 2 and 3) are left out: SQLite computes them and refuses a
        // value for one, so an export holding them could not be imported back.
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_xinfo(?1) WHERE hidden = 0 ORDER BY cid")
                .bind(table)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("reading the columns of {} failed: {e}", entity.name)))?;
        if columns.is_empty() {
            return Err(DbdError::Config(format!(
                "cannot export {}: no such table or view",
                entity.name
            )));
        }
        let select_list = columns
            .iter()
            .map(|c| crate::sql_quote::ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = sqlx::query(&format!("SELECT {select_list} FROM {}", crate::sql_quote::ident(table)))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("export query failed: {e}")))?;

        let mut output = String::new();
        match format {
            "csv" | "tsv" => {
                let sep = if format == "tsv" { '\t' } else { ',' };
                let header: Vec<String> = columns.iter().map(|c| delimited_cell(Some(c), sep)).collect();
                output.push_str(&header.join(&sep.to_string()));
                output.push('\n');
                for row in &rows {
                    let cells: Vec<String> = (0..columns.len())
                        .map(|i| delimited_cell(sqlite_cell_to_text(row, i).as_deref(), sep))
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
                return Err(DbdError::Config(format!("Unsupported sqlite export format: {format}")));
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
        let row = sqlx::query("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1")
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
            let Some(table_sql): Option<String> = row.get("sql") else {
                continue;
            };

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

        let view_rows = sqlx::query("SELECT name, sql FROM sqlite_master WHERE type = 'view' ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect views failed: {e}")))?;

        for row in &view_rows {
            let name: String = row.get("name");
            let Some(sql): Option<String> = row.get("sql") else {
                continue;
            };
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
        let exists = sqlx::query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_dbd_meta'")
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

    async fn heal_bookkeeping(&self) -> Result<()> {
        self.ensure_meta_table_inner().await?;
        self.ensure_migrations_table_inner().await
    }

    async fn get_db_version(&self) -> Result<u32> {
        // `_dbd_meta` absent ⇒ fresh DB (version 0). Probe via pragma (empty for a
        // missing table, not an error) so a genuine read failure surfaces instead
        // of being masked as 0 and misplanning apply against a live DB.
        let has_meta = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_table_info('_dbd_meta')")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("read _dbd_meta columns failed: {e}")))?
            > 0;
        if !has_meta {
            return Ok(0);
        }
        let row = sqlx::query("SELECT version FROM _dbd_meta WHERE project = ?1")
            .bind(&self.project)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("read _dbd_meta version failed: {e}")))?;
        Ok(row.map(|r| r.get::<i64, _>("version") as u32).unwrap_or(0))
    }

    async fn apply_migration(&self, version: u32, sql: &str, description: &str, checksum: &str) -> Result<()> {
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

    async fn forget_project_meta(&self) -> Result<()> {
        // No `_dbd_meta` table means nothing was ever recorded to forget; any
        // other failure is surfaced — a row left behind keeps its prod mark.
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('_dbd_meta')")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("read _dbd_meta failed: {e}")))?;
        if exists > 0 {
            sqlx::query("DELETE FROM _dbd_meta WHERE project = ?1")
                .bind(&self.project)
                .execute(&self.pool)
                .await
                .map_err(|e| DbdError::Config(format!("clear _dbd_meta failed: {e}")))?;
        }
        Ok(())
    }

    async fn ensure_import_procedure(&self) -> Result<()> {
        // No stored procedures in SQLite.
        Ok(())
    }

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        Ok(self.read_meta("project = ?1").await?.into_iter().next())
    }

    async fn other_projects_meta(&self) -> Result<Vec<ProjectMeta>> {
        self.read_meta("project <> ?1 ORDER BY project").await
    }

    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()> {
        self.ensure_meta_table_inner().await?;
        sqlx::query(
            "INSERT INTO _dbd_meta (project, env, version, scope) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project) DO UPDATE \
             SET env = excluded.env, version = excluded.version, scope = excluded.scope, updated_at = datetime('now')",
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i64)
        .bind(scope)
        .execute(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;
        Ok(())
    }
}

// ── helpers ────────────────────────────────────────────────────

impl SqliteAdapter {
    /// The `_dbd_meta` rows `filter` selects — a `WHERE` clause over
    /// `project`, bound to this project's name as `?1`.
    ///
    /// The table's shape is read via pragma (zero rows for a missing table —
    /// not an error), so the right SELECT is picked and a genuine read failure
    /// surfaces instead of reading as "no meta", which would silently disable
    /// the scope AND prod guards.
    async fn read_meta(&self, filter: &str) -> Result<Vec<ProjectMeta>> {
        let columns: Vec<String> = sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info('_dbd_meta')")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("read _dbd_meta columns failed: {e}")))?;
        if columns.is_empty() {
            return Ok(Vec::new()); // `_dbd_meta` doesn't exist yet — fresh/unmanaged DB
        }
        let has_scope = columns.iter().any(|c| c == "scope");
        let scope = if has_scope { "scope, " } else { "" };
        let rows = sqlx::query(&format!(
            "SELECT project, env, version, {scope}updated_at AS applied_at FROM _dbd_meta WHERE {filter}"
        ))
        .bind(&self.project)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("read _dbd_meta failed: {e}")))?;
        Ok(rows
            .iter()
            .map(|row| ProjectMeta {
                project: row.get("project"),
                env: row.get("env"),
                version: row.get::<i64, _>("version") as u32,
                scope: if has_scope { row.try_get("scope").ok() } else { None },
                applied_at: row.try_get("applied_at").ok(),
            })
            .collect())
    }

    /// Load a CSV/TSV file's rows into `table`, each value into the column its
    /// header names.
    ///
    /// Read by [`super::Delimited`], as Postgres reads the same file: each line
    /// was split on the delimiter and every cell trimmed, so a quoted comma was
    /// "too many columns" and a quote stayed in the value — a CSV Postgres
    /// exported could not be imported here. An unquoted field matching
    /// `null_value` is NULL; a quoted one is a value, as in Postgres's COPY,
    /// which writes `''` as `""` and NULL as an empty field.
    async fn import_delimited(&self, mut records: super::Delimited<'_>, table: &str, null_value: &str) -> Result<()> {
        let Some(header) = records.next_record()? else {
            return Ok(()); // An empty file: no columns, no rows.
        };
        let col_count = header.len();
        let cols_clause = header
            .iter()
            .map(|f| crate::sql_quote::ident(&f.value))
            .collect::<Vec<_>>()
            .join(",");
        let batch_rows = batch_row_size(col_count);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("import begin failed: {e}")))?;

        let mut batch: Vec<Vec<Option<String>>> = Vec::with_capacity(batch_rows);
        while let Some(record) = records.next_record()? {
            if record.len() != col_count {
                return Err(DbdError::Config(format!(
                    "{}: the row on line {} has {} field(s), but the header has {col_count}",
                    records.file().display(),
                    records.line(),
                    record.len()
                )));
            }
            let row: Vec<Option<String>> = record
                .into_iter()
                .map(|f| (f.quoted || f.value != null_value).then_some(f.value))
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

    async fn import_json_records(&self, records: &[String], table: &str) -> Result<()> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbdError::Config(format!("import begin failed: {e}")))?;

        // Group consecutive records with identical column sets into batches so
        // we can flush each group as a single multi-row INSERT. When the
        // column set changes, flush whatever we have and start a new batch.
        let mut current_cols: Option<Vec<String>> = None;
        let mut batch: Vec<Vec<JsonBindable>> = Vec::new();

        for record in records {
            let value: serde_json::Value =
                serde_json::from_str(record).map_err(|e| DbdError::Config(format!("json parse failed: {e}")))?;
            let obj = value
                .as_object()
                .ok_or_else(|| DbdError::Config("a json record must be a JSON object".into()))?;

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
    let sql = format!(
        "INSERT INTO {} ({cols_clause}) VALUES {placeholders}",
        crate::sql_quote::ident(table)
    );
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
    let cols_clause = cols.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(",");
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

/// A cell's text, `None` for NULL — kept apart from `''`, which a delimited
/// export has to write differently.
fn sqlite_cell_to_text(row: &sqlx::sqlite::SqliteRow, idx: usize) -> Option<String> {
    if let Ok(v) = row.try_get::<Option<String>, _>(idx) {
        return v;
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(idx) {
        return v.map(|n| n.to_string());
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(idx) {
        return v.map(|n| n.to_string());
    }
    None
}

/// One CSV/TSV field as Postgres's `COPY … CSV` writes it, so the RFC 4180
/// reader on the other side — this adapter's or Postgres's — reads back what
/// was there: NULL is an empty field, `''` is `""`, and a value holding the
/// delimiter, a quote or a line break is quoted with its quotes doubled.
/// Joined bare, a comma in a value split the row, and NULL and `''` both came
/// out empty.
fn delimited_cell(value: Option<&str>, sep: char) -> String {
    match value {
        None => String::new(),
        Some("") => "\"\"".to_string(),
        Some(v) if v.contains([sep, '"', '\n', '\r']) => format!("\"{}\"", v.replace('"', "\"\"")),
        Some(v) => v.to_string(),
    }
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
    async fn s_sqlite_has_no_grant_model() {
        // SQLite has no GRANT; the apply path must skip grant emission for it
        // rather than feed it Postgres-only DDL.
        assert!(!mem().await.supports_schema_grants());
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
        a.execute_script("CREATE TABLE users (id INTEGER)").await.unwrap();
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
        a.heal_bookkeeping().await.unwrap();
        a.apply_migration(1, "CREATE TABLE m (id INTEGER)", "init", "abc")
            .await
            .unwrap();

        // Idempotent re-apply doesn't error.
        a.apply_migration(1, "", "init", "abc").await.unwrap();
    }

    #[tokio::test]
    async fn s6_meta_set_get() {
        let a = mem().await;
        a.heal_bookkeeping().await.unwrap();
        assert_eq!(a.get_db_version().await.unwrap(), 0);
        a.set_project_meta("dev", 3, None).await.unwrap();
        assert_eq!(a.get_db_version().await.unwrap(), 3);
        let m = a.get_project_meta().await.unwrap().unwrap();
        assert_eq!(m.env, "dev");
        assert_eq!(m.version, 3);
    }

    #[tokio::test]
    async fn s8_meta_scope_backfills_on_legacy_table() {
        let a = mem().await; // project = "test"
        // Legacy `_dbd_meta` WITHOUT the `scope` column (pre-scope-guard schema).
        a.execute_script(
            "CREATE TABLE _dbd_meta ( \
                project TEXT NOT NULL PRIMARY KEY, \
                env TEXT NOT NULL DEFAULT 'dev', \
                version INTEGER NOT NULL DEFAULT 0, \
                created_at TEXT NOT NULL DEFAULT (datetime('now')), \
                updated_at TEXT NOT NULL DEFAULT (datetime('now')) )",
        )
        .await
        .unwrap();
        a.execute_script("INSERT INTO _dbd_meta (project, env, version) VALUES ('test', 'prod', 2)")
            .await
            .unwrap();

        // Resilient read: prod guard still sees env/version; scope = None.
        let m = a.get_project_meta().await.unwrap().expect("legacy meta reads");
        assert_eq!(m.env, "prod");
        assert_eq!(m.version, 2);
        assert_eq!(m.scope, None);

        // A write backfills the column and pins the scope.
        a.set_project_meta("prod", 3, Some("public")).await.unwrap();
        let m2 = a.get_project_meta().await.unwrap().unwrap();
        assert_eq!(m2.scope.as_deref(), Some("public"));
    }

    #[tokio::test]
    async fn s9_scope_guard_fires_against_real_pin() {
        use crate::design::Design;
        let a = mem().await; // project = "test"
        a.heal_bookkeeping().await.unwrap();
        // Pin the DB to scope "public" through the real adapter round-trip.
        a.set_project_meta("dev", 1, Some("public")).await.unwrap();
        let meta = a.get_project_meta().await.unwrap();
        assert_eq!(meta.as_ref().unwrap().scope.as_deref(), Some("public"));
        let meta = crate::design::DatabaseMarks::of_own("test", meta);

        // Same scope → allowed.
        assert!(Design::check_scope_guard(&meta, "public", false).is_ok());
        // Different scope → blocked, message names both scopes.
        let err = Design::check_scope_guard(&meta, "internal", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("public") && msg.contains("internal"), "msg was: {msg}");
        // Bypass → allowed.
        assert!(Design::check_scope_guard(&meta, "internal", true).is_ok());
    }

    /// `_dbd_meta` is keyed on `project.name` as `dbd.meta` is, and a renamed
    /// design read a production SQLite database as one it had never seen:
    /// `reset` with no `--force` dropped its tables. The database holds the
    /// design's tables and only another name's record of them, so that record
    /// is the guard's mark.
    #[tokio::test]
    async fn s11_a_renamed_project_is_held_to_the_record_its_old_name_left() {
        use crate::design::{Design, Progress};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let url = format!("sqlite://{}", dir.join("app.db").display());
        std::fs::write(dir.join("app.db"), "").unwrap();
        std::fs::create_dir_all(dir.join("ddl/table")).unwrap();
        std::fs::write(
            dir.join("ddl/table/items.ddl"),
            "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY);\n",
        )
        .unwrap();
        let design = |name: &str| {
            std::fs::write(
                dir.join("design.yaml"),
                format!("project:\n  name: {name}\nsource:\n  dialect: sqlite\nschemas: []\n"),
            )
            .unwrap();
            Design::from_config_with_dir(&dir.join("design.yaml"), "prod", Some(dir)).unwrap()
        };

        let alpha = design("alpha");
        let built = SqliteAdapter::new(&url, "alpha").await.unwrap();
        alpha
            .apply(&built, None, false, None, Progress::none())
            .await
            .expect("alpha's prod apply");

        let renamed = design("alpha2");
        let adapter = SqliteAdapter::new(&url, "alpha2").await.unwrap();
        let err = renamed
            .reset(&adapter, "sqlite", false, false, false, None)
            .await
            .expect_err("the renamed project's unforced reset of a prod database is refused")
            .to_string();
        assert!(err.contains("'alpha'") && err.contains("project.name"), "{err}");
        assert_eq!(built.list_entities().await.unwrap(), vec!["items".to_string()]);
    }

    /// A fresh DB with no `_dbd_meta` reads as `None` (not an error) — the pragma
    /// probe returns no columns, so the resilient read short-circuits cleanly
    /// rather than erroring on the absent table.
    #[tokio::test]
    async fn s10_get_meta_on_fresh_db_is_none() {
        let a = mem().await; // no `_dbd_meta` created
        assert!(a.get_project_meta().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn s7_internal_tables_excluded_from_list() {
        let a = mem().await;
        a.heal_bookkeeping().await.unwrap();
        a.execute_script("CREATE TABLE keep_me (id INTEGER)").await.unwrap();
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
        a.import_data(&entity, "", false).await.unwrap();

        let row = sqlx::query("SELECT count(*) AS c FROM rows")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let count: i64 = row.get("c");
        assert_eq!(count, 2);
    }

    /// `null_value == ""` (the default) means an empty cell loads as SQL NULL —
    /// byte-for-byte the pre-`null_value` behavior. A non-empty sentinel (e.g.
    /// `\N`) maps only cells matching that exact sentinel to NULL; a plain empty
    /// cell then loads as a literal empty string, not NULL.
    #[tokio::test]
    async fn s18_import_csv_null_value_sentinel() {
        use tempfile::NamedTempFile;

        // Default behavior: empty cell → NULL.
        let a = mem().await;
        a.execute_script("CREATE TABLE defaults (id INTEGER, note TEXT)")
            .await
            .unwrap();
        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(tmp, "id,note").unwrap();
        writeln!(tmp, "1,").unwrap();
        let mut entity = Entity::new(EntityType::Import, "default.defaults");
        entity.file = Some(tmp.path().to_path_buf());
        entity.format = Some("csv".to_string());
        a.import_data(&entity, "", false).await.unwrap();
        let row = sqlx::query("SELECT note FROM defaults WHERE id = 1")
            .fetch_one(&a.pool)
            .await
            .unwrap();
        let note: Option<String> = row.get("note");
        assert_eq!(note, None, "empty cell should be NULL under the default sentinel");

        // Custom sentinel: a cell equal to the sentinel is NULL; a plain empty
        // cell is now a literal empty string (NOT null).
        let b = mem().await;
        b.execute_script("CREATE TABLE sentinels (id INTEGER, note TEXT)")
            .await
            .unwrap();
        let mut tmp2 = NamedTempFile::new().unwrap();
        writeln!(tmp2, "id,note").unwrap();
        writeln!(tmp2, "1,\\N").unwrap();
        writeln!(tmp2, "2,").unwrap();
        let mut entity2 = Entity::new(EntityType::Import, "default.sentinels");
        entity2.file = Some(tmp2.path().to_path_buf());
        entity2.format = Some("csv".to_string());
        b.import_data(&entity2, "\\N", false).await.unwrap();

        let row1 = sqlx::query("SELECT note FROM sentinels WHERE id = 1")
            .fetch_one(&b.pool)
            .await
            .unwrap();
        let note1: Option<String> = row1.get("note");
        assert_eq!(note1, None, "sentinel cell should be NULL");

        let row2 = sqlx::query("SELECT note FROM sentinels WHERE id = 2")
            .fetch_one(&b.pool)
            .await
            .unwrap();
        let note2: Option<String> = row2.get("note");
        assert_eq!(
            note2,
            Some(String::new()),
            "empty cell should be a literal empty string when a non-empty sentinel is configured"
        );
    }

    /// `import.options.truncate` defaults to true, so a default import empties
    /// each staging table before loading it. That step was issued as Postgres's
    /// `TRUNCATE "schema"."table"`, which SQLite has no statement for — every
    /// default import on SQLite failed before a single row loaded.
    #[tokio::test]
    async fn the_default_import_empties_a_staging_table_before_loading_it() {
        use crate::design::{Design, Progress};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("design.yaml"),
            "project:\n  name: t\nsource:\n  dialect: sqlite\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("import/staging")).unwrap();
        std::fs::write(dir.path().join("import/staging/items.csv"), "id,name\n1,fresh\n").unwrap();
        let design = Design::from_config_with_dir(&dir.path().join("design.yaml"), "dev", Some(dir.path())).unwrap();
        assert!(
            design.config().import.table_truncate("staging.items"),
            "precondition: truncate is the default"
        );

        let a = mem().await;
        a.execute_script("CREATE TABLE items (id INTEGER, name TEXT); INSERT INTO items VALUES (99, 'stale');")
            .await
            .unwrap();

        design
            .import_data(&a, None, false, None, Progress::none())
            .await
            .expect("the default import must run on SQLite");

        let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM items ORDER BY id")
            .fetch_all(&a.pool)
            .await
            .unwrap();
        assert_eq!(
            rows,
            vec![(1, "fresh".to_string())],
            "the stale row must be gone and only the file's row loaded"
        );
    }

    /// A SQLite project keeps a table's data in `import/<table>.csv` and its
    /// dev-only rows in `import/dev/<table>.csv`. The dev file was read as a
    /// table `dev.<table>` that every env loads, and SQLite, having no
    /// schemas, loaded it into `<table>` — in prod.
    #[tokio::test]
    async fn a_prod_import_leaves_a_dev_folders_rows_out() {
        use crate::design::{Design, Progress};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("design.yaml"),
            "project:\n  name: t\nsource:\n  dialect: sqlite\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("import/dev")).unwrap();
        std::fs::write(dir.path().join("import/items.csv"), "id,name\n1,shared\n").unwrap();
        std::fs::write(dir.path().join("import/dev/items.csv"), "id,name\n2,dev only\n").unwrap();

        let loaded = |env: &'static str| {
            let dir = dir.path().to_path_buf();
            async move {
                let design = Design::from_config_with_dir(&dir.join("design.yaml"), env, Some(&dir)).unwrap();
                let a = mem().await;
                a.execute_script("CREATE TABLE items (id INTEGER, name TEXT)")
                    .await
                    .unwrap();
                design
                    .import_data(&a, None, false, None, Progress::none())
                    .await
                    .unwrap();
                let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM items ORDER BY id")
                    .fetch_all(&a.pool)
                    .await
                    .unwrap();
                (rows, design.import_warnings(None))
            }
        };

        let (prod, warnings) = loaded("prod").await;
        assert_eq!(prod, vec![(1, "shared".to_string())], "prod loads the shared rows only");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("import/dev/") && w.contains("'prod'")),
            "and says it left the dev file out: {warnings:?}"
        );

        let (dev, _) = loaded("dev").await;
        assert_eq!(
            dev,
            vec![(1, "shared".to_string()), (2, "dev only".to_string())],
            "dev loads both"
        );
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

    /// The header came from the first row, so an empty table wrote an empty
    /// line — a file no import can read as the table's columns. Postgres's
    /// COPY writes the header either way.
    #[tokio::test]
    async fn an_empty_table_exports_its_header() {
        let a = mem().await;
        a.execute_script("CREATE TABLE empty_t (id INTEGER, label TEXT)")
            .await
            .unwrap();
        for (format, header) in [("csv", "id,label\n"), ("tsv", "id\tlabel\n")] {
            let mut entity = Entity::new(EntityType::Table, "empty_t");
            entity.format = Some(format.to_string());
            let out = tempfile::tempdir().unwrap();
            a.export_data(&entity, Some(out.path())).await.unwrap();
            let body = std::fs::read_to_string(out.path().join(format!("empty_t.{format}"))).unwrap();
            assert_eq!(body, header, "{format}");
        }
    }

    /// A generated column is computed, and SQLite refuses a value for one
    /// ("cannot INSERT into generated column"), so an export that wrote it
    /// could not be imported back. The export writes what an import can, and
    /// the import recomputes the rest.
    #[tokio::test]
    async fn an_export_leaves_out_generated_columns_and_imports_back() {
        const DDL: &str = "CREATE TABLE g (id INTEGER PRIMARY KEY, a INTEGER, \
                           b INTEGER GENERATED ALWAYS AS (a * 2) STORED, \
                           c INTEGER GENERATED ALWAYS AS (a * 3) VIRTUAL)";
        let a = mem().await;
        a.execute_script(DDL).await.unwrap();
        a.execute_script("INSERT INTO g (id, a) VALUES (1, 10), (2, 20)")
            .await
            .unwrap();

        for format in ["csv", "tsv", "jsonl"] {
            let mut entity = Entity::new(EntityType::Table, "g");
            entity.format = Some(format.to_string());
            let out = tempfile::tempdir().unwrap();
            a.export_data(&entity, Some(out.path())).await.unwrap();
            let file = out.path().join(format!("g.{format}"));
            let body = std::fs::read_to_string(&file).unwrap();
            let first = body.lines().next().unwrap_or_default();
            match format {
                "csv" => assert_eq!(first, "id,a", "only the columns an import can write"),
                "tsv" => assert_eq!(first, "id\ta", "only the columns an import can write"),
                _ => assert_eq!(
                    serde_json::from_str::<serde_json::Value>(first).unwrap(),
                    serde_json::json!({"id": 1, "a": 10}),
                    "jsonl: only the columns an import can write"
                ),
            }

            let b = mem().await;
            b.execute_script(DDL).await.unwrap();
            entity.file = Some(file);
            b.import_data(&entity, "", false)
                .await
                .unwrap_or_else(|e| panic!("{format}: the export imports back: {e}"));
            let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as("SELECT id, a, b, c FROM g ORDER BY id")
                .fetch_all(&b.pool)
                .await
                .unwrap();
            assert_eq!(rows, vec![(1, 10, 20, 30), (2, 20, 40, 60)], "{format}");
        }
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

    // ── CSV/TSV is read as CSV ───────────────────────────────────────────────

    /// Load `body` as a `format` data file into `t (id INTEGER, v TEXT)`, and
    /// read every row back in id order.
    async fn load_delimited(format: &str, body: &str, null_value: &str) -> Result<Vec<(i64, Option<String>)>> {
        let a = mem().await;
        a.execute_script("CREATE TABLE t (id INTEGER, v TEXT)").await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("t.{format}"));
        std::fs::write(&path, body).unwrap();
        let mut entity = Entity::new(EntityType::Import, "staging.t");
        entity.file = Some(path);
        entity.format = Some(format.to_string());
        a.import_data(&entity, null_value, false).await?;
        Ok(
            sqlx::query_as::<_, (i64, Option<String>)>("SELECT id, v FROM t ORDER BY id")
                .fetch_all(&a.pool)
                .await
                .unwrap(),
        )
    }

    fn rows(expected: &[(i64, Option<&str>)]) -> Vec<(i64, Option<String>)> {
        expected.iter().map(|(id, v)| (*id, v.map(str::to_string))).collect()
    }

    /// A SQLite export imports back to the same values. Cells were joined with
    /// the delimiter unquoted, so a comma, quote or line break in a value broke
    /// the row, and NULL and `''` both came out empty. Written as Postgres
    /// writes CSV — quoted where needed, NULL empty, `''` as `""` — the RFC 4180
    /// reader on the other side gets back what was there.
    #[tokio::test]
    async fn a_csv_export_imports_back_to_the_same_values() {
        let expected = rows(&[
            (1, Some("Japan, Nippon")),
            (2, Some("say \"hi\"")),
            (3, Some("two\nlines")),
            (4, Some("  padded  ")),
            (5, Some("")),
            (6, None),
        ]);
        let a = mem().await;
        a.execute_script("CREATE TABLE t (id INTEGER, v TEXT)").await.unwrap();
        for (id, v) in &expected {
            sqlx::query("INSERT INTO t (id, v) VALUES (?, ?)")
                .bind(id)
                .bind(v)
                .execute(&a.pool)
                .await
                .unwrap();
        }
        let mut entity = Entity::new(EntityType::Table, "default.t");
        entity.format = Some("csv".to_string());
        let out = tempfile::tempdir().unwrap();
        a.export_data(&entity, Some(out.path())).await.unwrap();
        let body = std::fs::read_to_string(out.path().join("t.csv")).unwrap();

        let got = load_delimited("csv", &body, "").await.expect("the export must import");
        assert_eq!(got, expected, "{body}");
    }

    /// RFC 4180: a quoted field may hold the delimiter, a doubled quote and a
    /// line break, and nothing is trimmed. Each line was split on commas and
    /// every cell trimmed — a quoted comma was "too many columns", quotes were
    /// kept in the value, and padding was lost — so a CSV Postgres exported
    /// could not be imported here.
    #[tokio::test]
    async fn a_csv_is_read_as_rfc_4180() {
        let got = load_delimited(
            "csv",
            "id,v\n1,\"Japan, Nippon\"\n2,\"say \"\"hi\"\"\"\n3,\"two\nlines\"\n4,  padded  \n",
            "",
        )
        .await
        .expect("import failed");
        assert_eq!(
            got,
            rows(&[
                (1, Some("Japan, Nippon")),
                (2, Some("say \"hi\"")),
                (3, Some("two\nlines")),
                (4, Some("  padded  ")),
            ])
        );
    }

    /// RFC 4180 ends lines with CRLF; a `\r` is not part of the last value.
    #[tokio::test]
    async fn a_crlf_csv_loads_without_its_carriage_returns() {
        let got = load_delimited("csv", "id,v\r\n1,a\r\n2,\"b\"\r\n", "")
            .await
            .expect("import failed");
        assert_eq!(got, rows(&[(1, Some("a")), (2, Some("b"))]));
    }

    /// Quoting is how CSV tells an empty string from NULL: Postgres's COPY
    /// writes NULL as an empty unquoted field and `''` as `""`, and reads them
    /// back that way. An unquoted field matching `null_value` is NULL; a quoted
    /// one is always a value — under the default sentinel and a configured one.
    #[tokio::test]
    async fn a_quoted_field_is_a_value_never_null() {
        let got = load_delimited("csv", "id,v\n1,\n2,\"\"\n", "")
            .await
            .expect("import failed");
        assert_eq!(got, rows(&[(1, None), (2, Some(""))]));

        let got = load_delimited("csv", "id,v\n1,\\N\n2,\"\\N\"\n3,\n", "\\N")
            .await
            .expect("import failed");
        assert_eq!(got, rows(&[(1, None), (2, Some("\\N")), (3, Some(""))]));
    }

    /// A TSV is the same format with a tab between fields, so a quoted field
    /// may hold a tab.
    #[tokio::test]
    async fn a_tsv_field_may_quote_a_tab() {
        let got = load_delimited("tsv", "id\tv\n1\t\"a\tb\"\n2\t c \n", "")
            .await
            .expect("import failed");
        assert_eq!(got, rows(&[(1, Some("a\tb")), (2, Some(" c "))]));
    }

    /// A quote left open runs to the end of the file: loading it would put
    /// every row after it into one value. Refused, saying where it opened, and
    /// nothing is loaded.
    #[tokio::test]
    async fn an_unterminated_quote_is_refused() {
        let err = load_delimited("csv", "id,v\n1,a\n2,\"open\n3,c\n", "")
            .await
            .expect_err("an unterminated quote must be refused")
            .to_string();
        assert!(err.contains("quote") && err.contains("line 3"), "{err}");
    }

    /// A row with more or fewer fields than the header is refused, saying which.
    #[tokio::test]
    async fn a_row_with_the_wrong_field_count_is_refused() {
        let err = load_delimited("csv", "id,v\n1,a\n2,b,c\n", "")
            .await
            .expect_err("a ragged row must be refused")
            .to_string();
        assert!(
            err.contains("line 3") && err.contains('3') && err.contains('2'),
            "{err}"
        );
    }

    #[tokio::test]
    async fn s15_import_csv_multi_batch_path() {
        use tempfile::NamedTempFile;
        let a = mem().await;
        a.execute_script("CREATE TABLE big (id INTEGER, v TEXT)").await.unwrap();

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
        a.import_data(&entity, "", false).await.unwrap();

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
        a.import_data(&entity, "", false).await.unwrap();

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

    /// Load `body` as a `json`-format file into a fresh `people` table and
    /// return its rows.
    async fn import_json(body: &str) -> Result<Vec<(i64, String)>> {
        let a = mem().await;
        a.execute_script("CREATE TABLE people (id INTEGER, name TEXT)")
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("people.json");
        std::fs::write(&path, body).unwrap();
        let mut entity = Entity::new(EntityType::Import, "default.people");
        entity.file = Some(path);
        entity.format = Some("json".to_string());
        a.import_data(&entity, "", false).await?;
        Ok(sqlx::query_as("SELECT id, name FROM people ORDER BY id")
            .fetch_all(&a.pool)
            .await
            .unwrap())
    }

    /// A `.json` file is what most tools mean by one — Convex's importer
    /// included: a JSON array of records. The scanner has always picked `.json`
    /// files out of `import/`, and SQLite refused every one of them.
    #[tokio::test]
    async fn a_json_array_file_imports_one_row_per_record() {
        let rows = import_json("[\n  {\"id\": 1, \"name\": \"alpha\"},\n  {\"id\": 2, \"name\": \"beta\"}\n]\n")
            .await
            .expect("a JSON array must import");
        assert_eq!(rows, vec![(1, "alpha".to_string()), (2, "beta".to_string())]);
    }

    /// Postgres has always read a `.json` file as JSON lines; that shape must
    /// keep working, and work the same here.
    #[tokio::test]
    async fn a_json_file_of_json_lines_still_imports() {
        let rows = import_json("{\"id\": 1, \"name\": \"alpha\"}\n{\"id\": 2, \"name\": \"beta\"}\n")
            .await
            .expect("JSON lines in a .json file must import");
        assert_eq!(rows, vec![(1, "alpha".to_string()), (2, "beta".to_string())]);
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
            "json_extract",
            "row_number",
            "lag",
            "lead",
            "pi",
            "pow",
            "ceil",
            "unixepoch",
            "concat_ws",
            "string_agg",
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
        assert!(
            !raw.contains("author_name_idx"),
            "another table's index must not leak: {raw}"
        );
        let author = by("author").expect("author table captured");
        let araw = author.raw_ddl.as_deref().unwrap();
        assert!(araw.contains("author_name_idx"), "author keeps its own index");
        assert!(
            !araw.contains("book_"),
            "book indexes must not leak into author: {araw}"
        );

        // View captured; trigger NOT captured; internals excluded.
        let v = by("published").expect("view captured");
        assert_eq!(v.entity_type, EntityType::View);
        assert!(v.raw_ddl.as_deref().unwrap().to_uppercase().contains("CREATE VIEW"));
        assert!(by("trg").is_none(), "triggers are not captured in v1");
        assert!(
            ents.iter()
                .all(|e| e.name != "_dbd_meta" && !e.name.starts_with("sqlite_"))
        );
    }

    #[tokio::test]
    async fn reverse_managed_version_detects_meta() {
        let a = mem().await;
        // Fresh DB with no _dbd_meta → foreign.
        assert_eq!(a.reverse_managed_version().await.unwrap(), None);
        // After dbd's meta table + a row for this project → managed at that version.
        a.heal_bookkeeping().await.unwrap();
        a.set_project_meta("prod", 3, None).await.unwrap();
        assert_eq!(a.reverse_managed_version().await.unwrap(), Some(3));
    }
}

#[cfg(test)]
mod export_containment_tests {
    use super::*;
    use crate::entity::EntityType;

    async fn mem() -> SqliteAdapter {
        SqliteAdapter::new("sqlite::memory:", "test").await.unwrap()
    }

    /// A table name is a quoted identifier, so SQLite accepts one holding path
    /// separators and `..`. Joined into the export directory unchecked, it writes
    /// outside it — so the export must refuse rather than escape.
    /// A table name is a quoted identifier, so SQLite accepts one holding path
    /// separators. `bare_name` splits at the first `.`, so everything after it is
    /// used verbatim as the file stem — and `Path::join` with an absolute path
    /// *discards the base entirely*, so the export lands wherever the name says.
    ///
    /// The table really exists and the export directory really is elsewhere: this
    /// has to reach the write, or it would pass on a query error and pin nothing.
    #[tokio::test]
    async fn export_refuses_a_table_name_that_escapes_the_export_directory() {
        let adapter = mem().await;
        let escape_dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();

        let escaping = escape_dir.path().join("pwned");
        let table = escaping.display().to_string();
        adapter
            .execute_script(&format!(r#"CREATE TABLE "{table}" (id integer);"#))
            .await
            .expect("sqlite accepts a quoted identifier containing separators");

        let mut e = Entity::new(EntityType::Table, &format!("x.{table}"));
        e.format = Some("csv".to_string());
        let err = adapter
            .export_data(&e, Some(out.path()))
            .await
            .expect_err("an escaping table name must be refused");

        assert!(
            err.to_string().contains("pwned"),
            "the refusal must name the offending object; got: {err}"
        );
        assert!(
            !escape_dir.path().join("pwned.csv").exists(),
            "nothing may be written outside the export directory"
        );
    }

    /// The containment check must not cost ordinary exports: a plain name still
    /// lands where it always did.
    #[tokio::test]
    async fn export_still_writes_an_ordinary_table_name() {
        let adapter = mem().await;
        adapter
            .execute_script("CREATE TABLE items (id integer, name text); INSERT INTO items VALUES (1, 'a');")
            .await
            .unwrap();
        let out = tempfile::tempdir().unwrap();

        let mut e = Entity::new(EntityType::Table, "items");
        e.format = Some("csv".to_string());
        adapter.export_data(&e, Some(out.path())).await.expect("export failed");

        let written = std::fs::read_to_string(out.path().join("items.csv")).expect("items.csv");
        assert!(written.contains("id,name"), "got: {written}");
    }
}
