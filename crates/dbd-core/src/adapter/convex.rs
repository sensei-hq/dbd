//! Convex adapter — generates `convex/schema.ts` from `Entity` + `TableDef`.
//!
//! Convex is a TypeScript-first serverless database; it has no SQL execution
//! surface. `apply_entities` runs a single-pass codegen and writes the schema
//! file. SQL types are mapped to Convex `v.*` validators; entity names are
//! flattened from `schema.entity` to `schema_entity` since Convex does not
//! allow `.` in table names.
//!
//! Migration tracking and project meta are persisted in a sidecar JSON file
//! (`<output_dir>/.dbd_state.json`) since Convex has no built-in concept
//! equivalent to `_dbd_migrations` / `_dbd_meta`.
//!
//! `import_data` / `export_data` are intentionally not supported here:
//! `npx convex import` is the canonical path and runs outside of `dbd`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{DatabaseAdapter, ProjectMeta, ReferenceClass};
use crate::entity::{ColumnDef, Entity, EntityType, TableDef};
use crate::error::{DbdError, Result};

/// Sidecar state file (migrations + meta) written next to `schema.ts`.
const STATE_FILE: &str = ".dbd_state.json";
/// Codegen output file.
const SCHEMA_FILE: &str = "schema.ts";

/// Convert a Postgres-style entity name (`schema.entity`) into a Convex-safe
/// table name (`schema_entity`). Unqualified names pass through unchanged.
pub fn convex_table_name(qualified: &str) -> String {
    qualified.replace('.', "_")
}

/// Map a SQL data type string to a Convex validator expression (without
/// optional/nullable wrapping). Falls back to `v.any()` for unknown types.
pub fn sql_to_convex_validator(sql_type: &str) -> String {
    let t = sql_type.trim().to_lowercase();
    // Strip trailing array marker before matching; we re-wrap below.
    let (base, is_array) = if let Some(stripped) = t.strip_suffix("[]") {
        (stripped.trim().to_string(), true)
    } else {
        (t.clone(), false)
    };
    // Strip precision / length specifiers: `varchar(120)` -> `varchar`.
    let base_core = base.split('(').next().unwrap_or(&base).trim();

    let validator = match base_core {
        "int" | "int2" | "int4" | "int8" | "smallint" | "integer" | "bigint"
        | "serial" | "bigserial" | "smallserial" | "real" | "double precision"
        | "double" | "float" | "float4" | "float8" | "numeric" | "decimal" => "v.number()",
        "bool" | "boolean" => "v.boolean()",
        "varchar" | "char" | "character varying" | "character" | "text"
        | "citext" | "uuid" | "name" => "v.string()",
        // Timestamps stored as epoch-ms or ISO string — Convex has no native
        // date type, so we standardise on a number (epoch ms).
        "timestamp" | "timestamptz" | "timestamp with time zone"
        | "timestamp without time zone" | "date" | "time" | "timetz" => "v.number()",
        "json" | "jsonb" => "v.any()",
        "bytea" | "blob" => "v.bytes()",
        _ => "v.any()",
    }
    .to_string();

    if is_array {
        format!("v.array({validator})")
    } else {
        validator
    }
}

/// Resolve a column's data type to a Convex validator, consulting the
/// enum-name → TS-const-name map first so enum columns reference the
/// generated `v.union(v.literal(...))` instead of `v.any()`.
fn validator_for_type(sql_type: &str, enums: &HashMap<String, String>) -> String {
    let t = sql_type.trim().to_lowercase();
    let (base, is_array) = if let Some(stripped) = t.strip_suffix("[]") {
        (stripped.trim().to_string(), true)
    } else {
        (t.clone(), false)
    };
    let base_core = base.split('(').next().unwrap_or(&base).trim();

    let inner = if let Some(const_name) = enums.get(base_core) {
        const_name.clone()
    } else {
        sql_to_convex_validator(&base)
    };

    if is_array {
        format!("v.array({inner})")
    } else {
        inner
    }
}

/// Render a single column line for `defineTable({...})`. The `fk_override`
/// map is keyed by column name and supplies the inner validator (without
/// nullable wrapping) for columns that participate in a table-level FK
/// constraint. Inline FKs and enum/scalar fallbacks are resolved here.
fn render_column(
    col: &ColumnDef,
    enums: &HashMap<String, String>,
    tables: &HashMap<String, String>,
    fk_override: &HashMap<String, String>,
) -> String {
    let inner = fk_override
        .get(col.name.as_str())
        .cloned()
        .or_else(|| {
            col.inline_fk
                .as_ref()
                .and_then(|fk| fk_target_const(fk, tables))
        })
        .unwrap_or_else(|| validator_for_type(&col.data_type, enums));
    let wrapped = if col.nullable {
        format!("v.optional({inner})")
    } else {
        inner
    };
    format!("    {}: {},", json_key(&col.name), wrapped)
}

/// Resolve a foreign key into `v.id("convex_table_name")` if its target is
/// a known table (qualified or bare name), else `None`.
fn fk_target_const(
    fk: &crate::entity::ForeignKey,
    tables: &HashMap<String, String>,
) -> Option<String> {
    let qualified = match &fk.ref_schema {
        Some(s) => format!("{}.{}", s.to_lowercase(), fk.ref_table.to_lowercase()),
        None => fk.ref_table.to_lowercase(),
    };
    tables
        .get(&qualified)
        .or_else(|| tables.get(&fk.ref_table.to_lowercase()))
        .map(|name| format!("v.id(\"{name}\")"))
}

/// Quote a column name as a JS object key when it isn't a simple identifier.
fn json_key(name: &str) -> String {
    let is_ident = !name.is_empty()
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            .unwrap_or(false)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if is_ident {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\\\""))
    }
}

/// Render any `.index(...)` calls for a table.
fn render_indexes(table: &TableDef) -> String {
    let mut out = String::new();
    for (i, idx) in table.indexes.iter().enumerate() {
        let cols: Vec<String> = idx
            .columns
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect();
        let default_name = format!("by_{}", i + 1);
        let name = idx.name.clone().unwrap_or(default_name);
        out.push_str(&format!("    .index(\"{}\", [{}])\n", name, cols.join(", ")));
    }
    out
}

/// Build a lookup from enum SQL-type names (lowercased) → TS const name.
/// Registers each enum under both its qualified form (`schema.name`) and
/// its bare name; the qualified form takes precedence on collision.
fn enum_lookup(entities: &[&Entity]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for e in entities {
        if e.entity_type != EntityType::Enum {
            continue;
        }
        let ts_name = convex_table_name(&e.name);
        let qualified = e.name.to_lowercase();
        // Insert bare name first so qualified can overwrite on collision.
        if let Some((_, bare)) = qualified.split_once('.') {
            map.entry(bare.to_string()).or_insert_with(|| ts_name.clone());
        }
        map.insert(qualified, ts_name);
    }
    map
}

/// Build a lookup from table SQL names (lowercased) → Convex TS table name.
/// Registers both qualified (`schema.name`) and bare forms.
fn table_lookup(entities: &[&Entity]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for e in entities {
        if e.entity_type != EntityType::Table {
            continue;
        }
        let ts_name = convex_table_name(&e.name);
        let qualified = e.name.to_lowercase();
        if let Some((_, bare)) = qualified.split_once('.') {
            map.entry(bare.to_string()).or_insert_with(|| ts_name.clone());
        }
        map.insert(qualified, ts_name);
    }
    map
}

/// Render an enum entity as `export const <name> = v.union(v.literal(...), ...);`.
fn render_enum_const(entity: &Entity) -> String {
    let name = convex_table_name(&entity.name);
    let literals: Vec<String> = entity
        .enum_values
        .iter()
        .map(|ev| format!("v.literal(\"{}\")", ev.name.replace('"', "\\\"")))
        .collect();
    if literals.is_empty() {
        // Empty enum: emit v.union with no args is invalid TS, so fall back
        // to v.string() so the file still type-checks.
        format!("export const {name} = v.string();\n")
    } else {
        format!("export const {name} = v.union({});\n", literals.join(", "))
    }
}

/// Generate the full content of `convex/schema.ts` from a slice of entities.
/// Enums are emitted as `export const`s above `defineSchema`; tables are
/// emitted inside `defineSchema`. Other entity types are ignored (Convex has
/// no notion of views, functions, etc.).
pub fn generate_schema_ts(entities: &[&Entity]) -> String {
    let enums = enum_lookup(entities);
    let tables = table_lookup(entities);

    let mut out = String::new();
    out.push_str("// Generated by dbd — do not edit by hand.\n");
    out.push_str("import { defineSchema, defineTable } from \"convex/server\";\n");
    out.push_str("import { v } from \"convex/values\";\n\n");

    // Emit enum consts in sorted order for deterministic output.
    let mut enum_entities: Vec<&&Entity> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .collect();
    enum_entities.sort_by(|a, b| a.name.cmp(&b.name));
    for e in &enum_entities {
        out.push_str(&render_enum_const(e));
    }
    if !enum_entities.is_empty() {
        out.push('\n');
    }

    out.push_str("export default defineSchema({\n");

    for entity in entities {
        if entity.entity_type != EntityType::Table {
            continue;
        }
        out.push_str(&render_table_def(entity, &enums, &tables));
    }

    out.push_str("});\n");
    out
}

/// Render one `<name>: defineTable({ … }).index(…),` block for a table entity.
fn render_table_def(
    entity: &Entity,
    enums: &HashMap<String, String>,
    tables: &HashMap<String, String>,
) -> String {
    let convex_name = convex_table_name(&entity.name);
    let mut out = format!("  {convex_name}: defineTable({{\n");

    if let Some(td) = &entity.table_def {
        let fk_targets = fk_columns_for_table(td, tables);
        for col in &td.columns {
            // Convex auto-supplies _id and _creationTime; skip these so user
            // columns don't collide with the platform.
            if matches!(col.name.as_str(), "_id" | "_creationTime") {
                continue;
            }
            out.push_str(&render_column(col, enums, tables, &fk_targets));
            out.push('\n');
        }
    }
    out.push_str("  })");

    if let Some(td) = &entity.table_def
        && !td.indexes.is_empty()
    {
        out.push('\n');
        out.push_str(&render_indexes(td));
        // strip trailing newline before comma
        if out.ends_with('\n') {
            out.pop();
        }
    }
    out.push_str(",\n");
    out
}

/// Map column name → `v.id("...")` for any single-column FK constraint at
/// the table level whose target is in `tables`.
fn fk_columns_for_table(
    td: &TableDef,
    tables: &HashMap<String, String>,
) -> HashMap<String, String> {
    use crate::entity::TableConstraint;
    let mut map = HashMap::new();
    for c in &td.constraints {
        if let TableConstraint::ForeignKey(fk) = c
            && fk.columns.len() == 1
            && let Some(target) = fk_target_const(fk, tables)
        {
            map.insert(fk.columns[0].clone(), target);
        }
    }
    map
}

// ── Sidecar state ─────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConvexState {
    project: String,
    env: String,
    version: u32,
    updated_at: String,
    migrations: Vec<MigrationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigrationRecord {
    version: u32,
    description: String,
    checksum: String,
    applied_at: String,
}

fn state_path(output_dir: &Path) -> PathBuf {
    output_dir.join(STATE_FILE)
}

fn schema_path(output_dir: &Path) -> PathBuf {
    output_dir.join(SCHEMA_FILE)
}

fn load_state(output_dir: &Path) -> ConvexState {
    let path = state_path(output_dir);
    if !path.exists() {
        return ConvexState::default();
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(output_dir: &Path, state: &ConvexState) -> Result<()> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::create_dir_all(output_dir)
        .map_err(|e| DbdError::Config(format!("create convex output dir: {e}")))?;
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| DbdError::Config(format!("serialize convex state: {e}")))?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::write(state_path(output_dir), json)
        .map_err(|e| DbdError::Config(format!("write convex state: {e}")))?;
    Ok(())
}

// ── Adapter ───────────────────────────────────────────────────

/// Codegen adapter — writes `convex/schema.ts` and tracks state in a sidecar.
pub struct ConvexAdapter {
    project: String,
    output_dir: PathBuf,
    /// Buffered tables awaiting batch emit. Convex's schema is holistic, so
    /// we collect all tables seen during this apply pass and write one file.
    pending: Arc<Mutex<Vec<Entity>>>,
    /// When true, run `npx convex deploy` after `apply_entities` finishes.
    auto_deploy: bool,
    /// When true, CLI shell-outs (`npx convex deploy`/`import`) are logged
    /// to stderr and skipped. Tests use this to verify wiring without
    /// requiring `npx` or a live Convex deployment.
    cli_dry_run: bool,
}

impl ConvexAdapter {
    /// Create an adapter that writes to `output_dir/schema.ts`.
    /// The directory is created on first write.
    pub fn new(output_dir: impl Into<PathBuf>, project: &str) -> Self {
        Self {
            project: project.to_string(),
            output_dir: output_dir.into(),
            pending: Arc::new(Mutex::new(Vec::new())),
            auto_deploy: false,
            cli_dry_run: false,
        }
    }

    /// Parse a `convex:` URL and pick the output directory.
    /// Supported forms:
    /// - `convex:` → `./convex`
    /// - `convex://./somedir` → `./somedir`
    /// - `convex:./out` → `./out`
    ///
    /// Query parameters:
    /// - `?deploy=true` — run `npx convex deploy` after `apply_entities`.
    pub fn from_url(url: &str, project: &str) -> Result<Self> {
        let rest = url
            .strip_prefix("convex://")
            .or_else(|| url.strip_prefix("convex:"))
            .ok_or_else(|| DbdError::Config(format!("Invalid convex URL: {url}")))?;
        let (path_part, query_part) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        // The directory is supplied by the project's own design.yaml — same
        // trust boundary as any other config string. File operations against
        // this path are individually annotated below.
        let dir = if path_part.is_empty() {
            PathBuf::from("convex")
        } else {
            // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            PathBuf::from(path_part)
        };
        let mut adapter = Self::new(dir, project);
        if let Some(q) = query_part {
            for pair in q.split('&') {
                if let Some((k, v)) = pair.split_once('=')
                    && k == "deploy"
                    && matches!(v, "true" | "1" | "yes")
                {
                    adapter.auto_deploy = true;
                }
            }
        }
        Ok(adapter)
    }

    /// Toggle auto-deploy on/off (overrides URL-derived setting).
    pub fn with_auto_deploy(mut self, enabled: bool) -> Self {
        self.auto_deploy = enabled;
        self
    }

    /// Whether auto-deploy is currently enabled.
    pub fn auto_deploy(&self) -> bool {
        self.auto_deploy
    }

    /// When set, all `npx convex …` invocations are logged and skipped.
    /// Useful in tests and `--dry-run`-style flows that should not touch
    /// a live deployment.
    pub fn with_cli_dry_run(mut self, enabled: bool) -> Self {
        self.cli_dry_run = enabled;
        self
    }

    /// Convenience for tests / external callers.
    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    /// The directory `npx convex` commands run from — one level above
    /// `convex/schema.ts` so the CLI finds its own `convex/` folder.
    fn cli_cwd(&self) -> PathBuf {
        self.output_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Build the argv (after `npx convex`) for a deploy invocation.
    fn deploy_args() -> Vec<&'static str> {
        vec!["convex", "deploy"]
    }

    /// Build the argv (after `npx convex`) for a per-table import.
    /// `--replace -y` means: overwrite existing rows and skip the
    /// interactive confirmation prompt.
    fn import_args(table: &str, file: &str) -> Vec<String> {
        vec![
            "convex".into(),
            "import".into(),
            "--table".into(),
            table.into(),
            "--replace".into(),
            "-y".into(),
            file.into(),
        ]
    }

    /// Spawn `npx <args>` from `cli_cwd()` and return the combined output.
    /// When `cli_dry_run` is set, the command is logged and skipped —
    /// used by tests to verify wiring without requiring `npx` on the path.
    async fn run_npx(&self, args: &[String]) -> Result<()> {
        let cwd = self.cli_cwd();
        if self.cli_dry_run {
            eprintln!("[dbd convex dry-run] cwd={} npx {}", cwd.display(), args.join(" "));
            return Ok(());
        }
        // nosemgrep: rust.lang.security.tempfile.tempfile, rust.actix.path-traversal.tainted-path.tainted-path
        let output = tokio::process::Command::new("npx")
            .args(args)
            .current_dir(&cwd)
            .output()
            .await
            .map_err(|e| {
                DbdError::Config(format!(
                    "failed to spawn npx {}: {e} (is Node/npm installed?)",
                    args.join(" ")
                ))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DbdError::Config(format!(
                "npx {} failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        Ok(())
    }

    /// Flush pending tables to `schema.ts`. Idempotent — sorting + full
    /// regeneration keeps the output deterministic.
    fn write_schema(&self) -> Result<()> {
        let mut pending = self.pending.lock().unwrap();
        pending.sort_by(|a, b| a.name.cmp(&b.name));
        let refs: Vec<&Entity> = pending.iter().collect();
        let content = generate_schema_ts(&refs);

        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        std::fs::create_dir_all(&self.output_dir)
            .map_err(|e| DbdError::Config(format!("create convex output dir: {e}")))?;
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        std::fs::write(schema_path(&self.output_dir), content)
            .map_err(|e| DbdError::Config(format!("write convex schema.ts: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl DatabaseAdapter for ConvexAdapter {
    async fn connect(&mut self) -> Result<()> {
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        Ok(())
    }

    async fn test_connection(&self) -> Result<bool> {
        Ok(true)
    }

    async fn execute_script(&self, _sql: &str) -> Result<()> {
        Err(DbdError::Config(
            "Convex adapter does not execute SQL scripts".into(),
        ))
    }

    async fn apply_entity(&self, entity: &Entity) -> Result<()> {
        match entity.entity_type {
            // Buffer tables and enums and (re)emit schema.ts on every call
            // so that single-entity applies still produce a valid file.
            EntityType::Table | EntityType::Enum => {
                {
                    let mut pending = self.pending.lock().unwrap();
                    if let Some(slot) = pending.iter_mut().find(|e| e.name == entity.name) {
                        *slot = entity.clone();
                    } else {
                        pending.push(entity.clone());
                    }
                }
                self.write_schema()
            }
            // Convex has no concept of these — silently ignore so that
            // cross-target projects can still apply.
            EntityType::Schema
            | EntityType::View
            | EntityType::External
            | EntityType::Import
            | EntityType::Export => Ok(()),
            EntityType::Extension
            | EntityType::Role
            | EntityType::Sequence
            | EntityType::MaterializedView
            | EntityType::Function
            | EntityType::Procedure => Err(DbdError::Config(format!(
                "Convex adapter does not support {:?} entities ({})",
                entity.entity_type, entity.name
            ))),
        }
    }

    async fn apply_entities(&self, entities: &[Entity]) -> Result<()> {
        let had_entities = {
            let mut pending = self.pending.lock().unwrap();
            pending.clear();
            for e in entities {
                if matches!(e.entity_type, EntityType::Table | EntityType::Enum) {
                    pending.push(e.clone());
                }
            }
            !pending.is_empty()
        };
        self.write_schema()?;
        if self.auto_deploy && had_entities {
            let args: Vec<String> = Self::deploy_args().iter().map(|s| s.to_string()).collect();
            self.run_npx(&args).await?;
        }
        Ok(())
    }

    fn prefers_batch_apply(&self) -> bool {
        true
    }

    async fn import_data(&self, entity: &Entity, dry_run: bool) -> Result<()> {
        let file_path = entity
            .file
            .as_ref()
            .ok_or_else(|| DbdError::Config(format!("No file for import entity {}", entity.name)))?;
        let file_str = file_path.to_str().ok_or_else(|| {
            DbdError::Config(format!("Non-UTF8 import path for {}", entity.name))
        })?;
        // Convex CLI accepts JSONL or CSV; we pass through whatever the user
        // provided and let the CLI surface the error if it's something else.
        let table = convex_table_name(&entity.name);
        let args = Self::import_args(&table, file_str);
        if dry_run {
            eprintln!(
                "[dbd convex dry-run] would run: npx {} (from {})",
                args.join(" "),
                self.cli_cwd().display()
            );
            return Ok(());
        }
        self.run_npx(&args).await
    }

    async fn export_data(&self, _entity: &Entity, _out_dir: Option<&Path>) -> Result<()> {
        // Convex CLI exports the entire deployment as a zip, not per table.
        // Point the user at the CLI rather than implementing a partial story.
        Err(DbdError::Config(
            "Convex CLI does not support per-table export — run `npx convex export --path <dir>` to dump the whole deployment".into(),
        ))
    }

    async fn load_catalog(&mut self) -> Result<()> {
        Ok(())
    }

    fn classify_reference(&self, _name: &str, _installed: &[String]) -> ReferenceClass {
        // Convex has no SQL catalog — everything is user-defined by default.
        ReferenceClass::UserDefined
    }

    async fn resolve_entity(&self, name: &str) -> Result<Option<String>> {
        // Resolve against tables currently emitted in schema.ts (buffered).
        let pending = self.pending.lock().unwrap();
        let target = convex_table_name(name);
        Ok(pending
            .iter()
            .find(|e| convex_table_name(&e.name) == target)
            .map(|_| name.to_string()))
    }

    async fn list_entities(&self) -> Result<Vec<String>> {
        let pending = self.pending.lock().unwrap();
        Ok(pending.iter().map(|e| convex_table_name(&e.name)).collect())
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        // State file is created lazily on first record.
        Ok(())
    }

    async fn get_db_version(&self) -> Result<u32> {
        Ok(load_state(&self.output_dir).version)
    }

    async fn apply_migration(
        &self,
        version: u32,
        _sql: &str,
        description: &str,
        checksum: &str,
    ) -> Result<()> {
        let mut state = load_state(&self.output_dir);
        if state.migrations.iter().any(|m| m.version == version) {
            return Ok(());
        }
        state.migrations.push(MigrationRecord {
            version,
            description: description.to_string(),
            checksum: checksum.to_string(),
            applied_at: chrono::Utc::now().to_rfc3339(),
        });
        save_state(&self.output_dir, &state)
    }

    async fn clear_project_migrations(&self) -> Result<()> {
        let mut state = load_state(&self.output_dir);
        state.migrations.clear();
        save_state(&self.output_dir, &state)
    }

    async fn ensure_import_procedure(&self) -> Result<()> {
        Ok(())
    }

    async fn ensure_meta_table(&self) -> Result<()> {
        Ok(())
    }

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        let state = load_state(&self.output_dir);
        if state.project.is_empty() {
            return Ok(None);
        }
        Ok(Some(ProjectMeta {
            project: state.project,
            env: state.env,
            version: state.version,
            applied_at: Some(state.updated_at),
        }))
    }

    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()> {
        let mut state = load_state(&self.output_dir);
        state.project = self.project.clone();
        state.env = env.to_string();
        state.version = version;
        state.updated_at = chrono::Utc::now().to_rfc3339();
        save_state(&self.output_dir, &state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{
        ColumnDef, EnumValue, ForeignKey, IndexColumn, IndexDef, TableComments, TableConstraint,
        TableDef,
    };
    use tempfile::tempdir;

    fn col(name: &str, ty: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: ty.to_string(),
            nullable,
            default_value: None,
            is_pk: false,
            is_unique: false,
            identity: None,
            comment: None,
            inline_fk: None,
        }
    }

    fn table(name: &str, columns: Vec<ColumnDef>, indexes: Vec<IndexDef>) -> Entity {
        let mut e = Entity::new(EntityType::Table, name);
        e.table_def = Some(TableDef {
            columns,
            constraints: vec![],
            indexes,
            comments: TableComments::default(),
        });
        e
    }

    #[test]
    fn cv1_table_name_flattens_schema() {
        assert_eq!(convex_table_name("config.users"), "config_users");
        assert_eq!(convex_table_name("plain"), "plain");
    }

    #[test]
    fn cv2_sql_type_mapping_covers_common_types() {
        assert_eq!(sql_to_convex_validator("int4"), "v.number()");
        assert_eq!(sql_to_convex_validator("bigint"), "v.number()");
        assert_eq!(sql_to_convex_validator("varchar(120)"), "v.string()");
        assert_eq!(sql_to_convex_validator("text"), "v.string()");
        assert_eq!(sql_to_convex_validator("boolean"), "v.boolean()");
        assert_eq!(sql_to_convex_validator("timestamptz"), "v.number()");
        assert_eq!(sql_to_convex_validator("jsonb"), "v.any()");
        assert_eq!(sql_to_convex_validator("uuid"), "v.string()");
        assert_eq!(sql_to_convex_validator("bytea"), "v.bytes()");
        assert_eq!(sql_to_convex_validator("text[]"), "v.array(v.string())");
        assert_eq!(sql_to_convex_validator("unknown_xyz"), "v.any()");
    }

    #[test]
    fn cv3_schema_ts_contains_define_calls() {
        let t = table(
            "config.users",
            vec![col("id", "int8", false), col("email", "text", true)],
            vec![],
        );
        let out = generate_schema_ts(&[&t]);
        assert!(out.contains("import { defineSchema, defineTable } from \"convex/server\";"));
        assert!(out.contains("config_users: defineTable({"));
        assert!(out.contains("id: v.number(),"));
        assert!(out.contains("email: v.optional(v.string()),"));
        assert!(out.contains("export default defineSchema({"));
    }

    #[test]
    fn cv4_indexes_render_via_dot_index() {
        let t = table(
            "auth.sessions",
            vec![col("user_id", "uuid", false)],
            vec![IndexDef {
                name: Some("by_user".into()),
                columns: vec![IndexColumn {
                    name: "user_id".into(),
                    order: None,
                }],
                unique: false,
                index_type: None,
            }],
        );
        let out = generate_schema_ts(&[&t]);
        assert!(out.contains(".index(\"by_user\", [\"user_id\"])"));
    }

    #[test]
    fn cv5_internal_columns_are_skipped() {
        let t = table(
            "config.thing",
            vec![
                col("_id", "uuid", false),
                col("_creationTime", "timestamptz", false),
                col("name", "text", false),
            ],
            vec![],
        );
        let out = generate_schema_ts(&[&t]);
        assert!(!out.contains("_id: v."));
        assert!(!out.contains("_creationTime"));
        assert!(out.contains("name: v.string()"));
    }

    #[tokio::test]
    async fn cv6_apply_entities_writes_schema_ts() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        let t1 = table("config.users", vec![col("email", "text", false)], vec![]);
        let t2 = table("auth.sessions", vec![col("token", "text", false)], vec![]);
        adapter.apply_entities(&[t1, t2]).await.unwrap();

        let schema = std::fs::read_to_string(tmp.path().join("schema.ts")).unwrap();
        assert!(schema.contains("config_users:"));
        assert!(schema.contains("auth_sessions:"));
        // Sorted output: auth_ comes before config_
        let auth_pos = schema.find("auth_sessions:").unwrap();
        let cfg_pos = schema.find("config_users:").unwrap();
        assert!(auth_pos < cfg_pos);
    }

    #[tokio::test]
    async fn cv7_unsupported_entity_types_error() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        for t in [
            EntityType::Extension,
            EntityType::Role,
            EntityType::Function,
            EntityType::Procedure,
        ] {
            let e = Entity::new(t, "x");
            assert!(adapter.apply_entity(&e).await.is_err(), "{t:?}");
        }
    }

    #[tokio::test]
    async fn cv19_convex_rejects_materialized_view() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        let e = Entity::new(EntityType::MaterializedView, "app.mv");
        let err = adapter.apply_entity(&e).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("materialized"));
    }

    #[tokio::test]
    async fn cv8_meta_and_migrations_roundtrip_via_sidecar() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        assert_eq!(adapter.get_db_version().await.unwrap(), 0);
        adapter.set_project_meta("dev", 4).await.unwrap();
        assert_eq!(adapter.get_db_version().await.unwrap(), 4);
        let m = adapter.get_project_meta().await.unwrap().unwrap();
        assert_eq!(m.env, "dev");
        assert_eq!(m.version, 4);

        adapter.apply_migration(1, "", "init", "sum1").await.unwrap();
        adapter.apply_migration(2, "", "next", "sum2").await.unwrap();
        // Idempotent re-apply.
        adapter.apply_migration(1, "", "init", "sum1").await.unwrap();

        // Sidecar exists and contains both records.
        let sidecar = std::fs::read_to_string(tmp.path().join(".dbd_state.json")).unwrap();
        assert!(sidecar.contains("\"version\": 1"));
        assert!(sidecar.contains("\"version\": 2"));

        adapter.clear_project_migrations().await.unwrap();
        let sidecar = std::fs::read_to_string(tmp.path().join(".dbd_state.json")).unwrap();
        assert!(sidecar.contains("\"migrations\": []"));
    }

    #[tokio::test]
    async fn cv9_url_picks_default_output_dir() {
        let a = ConvexAdapter::from_url("convex:", "p").unwrap();
        assert_eq!(a.output_dir(), Path::new("convex"));
        assert!(!a.auto_deploy());
        let a = ConvexAdapter::from_url("convex://./custom", "p").unwrap();
        assert_eq!(a.output_dir(), Path::new("./custom"));
        let a = ConvexAdapter::from_url("convex:./out", "p").unwrap();
        assert_eq!(a.output_dir(), Path::new("./out"));
        assert!(ConvexAdapter::from_url("postgres://x", "p").is_err());

        let a = ConvexAdapter::from_url("convex:./out?deploy=true", "p").unwrap();
        assert_eq!(a.output_dir(), Path::new("./out"));
        assert!(a.auto_deploy(), "deploy=true should enable auto-deploy");
        let a = ConvexAdapter::from_url("convex:?deploy=false", "p").unwrap();
        assert!(!a.auto_deploy(), "deploy=false should leave it off");
    }

    fn enum_entity(name: &str, values: &[&str]) -> Entity {
        let mut e = Entity::new(EntityType::Enum, name);
        e.enum_values = values
            .iter()
            .map(|v| EnumValue {
                name: v.to_string(),
                note: None,
            })
            .collect();
        e
    }

    #[test]
    fn cv11_enum_emits_top_level_const() {
        let e = enum_entity("config.user_status", &["active", "inactive"]);
        let out = generate_schema_ts(&[&e]);
        assert!(
            out.contains(
                "export const config_user_status = v.union(v.literal(\"active\"), v.literal(\"inactive\"));"
            ),
            "{out}"
        );
        // Const appears before defineSchema.
        let const_pos = out.find("export const config_user_status").unwrap();
        let schema_pos = out.find("export default defineSchema").unwrap();
        assert!(const_pos < schema_pos);
    }

    #[test]
    fn cv12_column_references_enum_const() {
        let en = enum_entity("config.user_status", &["active", "inactive"]);
        let mut id = col("id", "int8", false);
        id.is_pk = true;
        let t = table(
            "config.users",
            vec![
                id,
                col("status", "user_status", false),
                col("prev_status", "config.user_status", true),
                col("history", "user_status[]", false),
            ],
            vec![],
        );
        let out = generate_schema_ts(&[&en, &t]);
        assert!(
            out.contains("status: config_user_status,"),
            "non-nullable enum column should reference const: {out}"
        );
        assert!(
            out.contains("prev_status: v.optional(config_user_status),"),
            "nullable qualified enum column: {out}"
        );
        assert!(
            out.contains("history: v.array(config_user_status),"),
            "enum array column: {out}"
        );
    }

    #[test]
    fn cv13_fk_columns_emit_v_id() {
        let parent = table("config.users", vec![col("id", "int8", false)], vec![]);
        let mut fk_col = col("user_id", "int8", false);
        fk_col.inline_fk = Some(ForeignKey {
            name: None,
            columns: vec!["user_id".into()],
            ref_schema: Some("config".into()),
            ref_table: "users".into(),
            ref_columns: vec!["id".into()],
            on_delete: None,
            on_update: None,
        });
        let child_inline = table(
            "auth.sessions_inline",
            vec![col("id", "int8", false), fk_col],
            vec![],
        );
        let mut child_constraint = table(
            "auth.sessions_constraint",
            vec![col("id", "int8", false), col("user_id", "int8", true)],
            vec![],
        );
        if let Some(td) = child_constraint.table_def.as_mut() {
            td.constraints.push(TableConstraint::ForeignKey(ForeignKey {
                name: Some("sessions_user_fk".into()),
                columns: vec!["user_id".into()],
                ref_schema: Some("config".into()),
                ref_table: "users".into(),
                ref_columns: vec!["id".into()],
                on_delete: None,
                on_update: None,
            }));
        }
        let out = generate_schema_ts(&[&parent, &child_inline, &child_constraint]);
        assert!(
            out.contains("user_id: v.id(\"config_users\"),"),
            "inline FK should map to v.id: {out}"
        );
        assert!(
            out.contains("user_id: v.optional(v.id(\"config_users\")),"),
            "nullable table-level FK should map to v.optional(v.id(...)): {out}"
        );
    }

    #[tokio::test]
    async fn cv14_apply_entity_accepts_enum() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        let en = enum_entity("config.user_status", &["a", "b"]);
        adapter.apply_entity(&en).await.unwrap();
        let schema = std::fs::read_to_string(tmp.path().join("schema.ts")).unwrap();
        assert!(schema.contains("export const config_user_status = v.union("));
    }

    #[tokio::test]
    async fn cv10_export_unsupported_with_helpful_message() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        let mut e = Entity::new(EntityType::Table, "config.users");
        e.file = Some(tmp.path().join("users.jsonl"));
        let err = adapter.export_data(&e, None).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("npx convex export"),
            "error should point to the CLI: {msg}"
        );
        // Import without a file path fails fast (no shell-out attempted).
        let no_file = Entity::new(EntityType::Import, "config.users");
        assert!(adapter.import_data(&no_file, false).await.is_err());
    }

    #[test]
    fn cv15_deploy_and_import_args_are_well_formed() {
        assert_eq!(ConvexAdapter::deploy_args(), vec!["convex", "deploy"]);
        let args = ConvexAdapter::import_args("config_users", "import/config/users.jsonl");
        assert_eq!(
            args,
            vec![
                "convex".to_string(),
                "import".to_string(),
                "--table".into(),
                "config_users".into(),
                "--replace".into(),
                "-y".into(),
                "import/config/users.jsonl".into(),
            ]
        );
    }

    #[tokio::test]
    async fn cv16_import_data_dry_run_skips_shell_out() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path(), "test");
        let mut e = Entity::new(EntityType::Import, "config.users");
        let data_path = tmp.path().join("users.jsonl");
        std::fs::write(&data_path, "{}\n").unwrap();
        e.file = Some(data_path);
        // dry_run = true should succeed without spawning npx.
        adapter.import_data(&e, true).await.unwrap();
    }

    #[tokio::test]
    async fn cv17_auto_deploy_runs_npx_in_dry_run_mode() {
        let tmp = tempdir().unwrap();
        let project_root = tmp.path();
        let adapter = ConvexAdapter::new(project_root.join("convex"), "test")
            .with_auto_deploy(true)
            .with_cli_dry_run(true);
        assert!(adapter.auto_deploy());
        let t = table("config.users", vec![col("email", "text", false)], vec![]);
        // dry-run skips the actual spawn, so the call succeeds without npx.
        adapter.apply_entities(&[t]).await.unwrap();
        // schema.ts was still written.
        assert!(project_root.join("convex/schema.ts").exists());
    }

    #[tokio::test]
    async fn cv18_auto_deploy_skipped_when_no_entities() {
        let tmp = tempdir().unwrap();
        let adapter = ConvexAdapter::new(tmp.path().join("convex"), "test")
            .with_auto_deploy(true)
            .with_cli_dry_run(true);
        // Empty input — deploy should not fire (no entities to apply).
        adapter.apply_entities(&[]).await.unwrap();
    }
}
