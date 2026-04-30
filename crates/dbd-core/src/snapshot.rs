use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::diff::{self, ComplexChange, DiffAction, MigrationDiff};
use crate::entity::{ColumnDef, Entity, EntityType, IndexDef, TableConstraint};
use crate::error::Result;

const SNAPSHOTS_DIR: &str = "snapshots";
const MIGRATIONS_DIR: &str = "migrations";
const VERSION_PAD: usize = 3;

/// Zero-pad a version number.
pub fn pad_version(n: u32) -> String {
    format!("{:0>width$}", n, width = VERSION_PAD)
}

/// SHA-256 hex digest of a string.
pub fn checksum_of(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ── Snapshot types ──────────────────────────────────────

/// A versioned schema snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub description: String,
    pub timestamp: String,
    pub tables: Vec<TableSnapshot>,
    #[serde(default)]
    pub enums: Vec<EnumSnapshot>,
}

/// Snapshot of a single enum type's structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumSnapshot {
    pub name: String,
    pub schema: String,
    pub values: Vec<String>,
}

/// Snapshot of a single table's structure.
/// Reuses ColumnDef, IndexDef, TableConstraint from entity module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub name: String,
    pub schema: String,
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub table_constraints: Vec<TableConstraint>,
}

/// Migration graph metadata (stored as graph.json in each migration folder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationGraph {
    #[serde(rename = "fromVersion")]
    pub from_version: u32,
    #[serde(rename = "toVersion")]
    pub to_version: u32,
    #[serde(default)]
    pub added: Vec<String>,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
}

/// A pending migration with its source directory and metadata.
#[derive(Debug, Clone)]
pub struct PendingMigration {
    pub from_version: u32,
    pub to_version: u32,
    pub migration_dir: PathBuf,
    pub added: Vec<String>,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
    pub checksum: String,
}

// ── Snapshot I/O ────────────────────────────────────────

/// List all snapshots in the snapshots directory, sorted by version.
pub fn list_snapshots(dir: &Path) -> Vec<SnapshotInfo> {
    let snapshots_dir = dir.join(SNAPSHOTS_DIR);
    if !snapshots_dir.exists() {
        return Vec::new();
    }

    let mut snapshots: Vec<SnapshotInfo> = std::fs::read_dir(&snapshots_dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".json") && name.len() == VERSION_PAD + 5)
        })
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let version: u32 = name.trim_end_matches(".json").parse().ok()?;
            let file = entry.path();
            let (description, timestamp) = match std::fs::read_to_string(&file) {
                Ok(content) => {
                    let snap: serde_json::Value = serde_json::from_str(&content).ok()?;
                    (
                        snap["description"].as_str().unwrap_or("").to_string(),
                        snap["timestamp"].as_str().unwrap_or("").to_string(),
                    )
                }
                Err(_) => (String::new(), String::new()),
            };
            Some(SnapshotInfo {
                version,
                file,
                description,
                timestamp,
            })
        })
        .collect();

    snapshots.sort_by_key(|s| s.version);
    snapshots
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub version: u32,
    pub file: PathBuf,
    pub description: String,
    pub timestamp: String,
}

/// Read a specific snapshot from disk.
pub fn read_snapshot(version: u32, dir: &Path) -> Result<Option<Snapshot>> {
    let file = dir
        .join(SNAPSHOTS_DIR)
        .join(format!("{}.json", pad_version(version)));
    if !file.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&file)?;
    let snapshot: Snapshot = serde_json::from_str(&content)?;
    Ok(Some(snapshot))
}

/// Get the latest snapshot, or None if none exist.
pub fn latest_snapshot(dir: &Path) -> Result<Option<Snapshot>> {
    let snapshots = list_snapshots(dir);
    match snapshots.last() {
        Some(info) => read_snapshot(info.version, dir),
        None => Ok(None),
    }
}

/// Determine the next snapshot version number.
pub fn next_version(dir: &Path) -> u32 {
    let snapshots = list_snapshots(dir);
    match snapshots.last() {
        Some(info) => info.version + 1,
        None => 1,
    }
}

/// Check if any snapshots exist.
pub fn has_snapshots(dir: &Path) -> bool {
    !list_snapshots(dir).is_empty()
}

// ── Pending migrations ──────────────────────────────────

/// List pending migrations (versions after current_db_version).
pub fn pending_migrations(current_db_version: u32, dir: &Path) -> Vec<PendingMigration> {
    let migrations_dir = dir.join(MIGRATIONS_DIR);
    if !migrations_dir.exists() {
        return Vec::new();
    }

    let mut migrations: Vec<PendingMigration> = std::fs::read_dir(&migrations_dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let to_version: u32 = name.parse().ok()?;
            if to_version <= current_db_version {
                return None;
            }
            let migration_dir = entry.path();
            let graph_file = migration_dir.join("graph.json");
            if !graph_file.exists() {
                return None;
            }
            let content = std::fs::read_to_string(&graph_file).ok()?;
            let graph: MigrationGraph = serde_json::from_str(&content).ok()?;
            let checksum = checksum_of(&content);
            Some(PendingMigration {
                from_version: graph.from_version,
                to_version,
                migration_dir,
                added: graph.added,
                altered: graph.altered,
                dropped: graph.dropped,
                checksum,
            })
        })
        .collect();

    migrations.sort_by_key(|m| m.to_version);
    migrations
}

// ── Entity → Snapshot conversion ────────────────────────

/// Convert a Table entity to a TableSnapshot, if it has a table_def.
pub fn entity_to_table_snapshot(entity: &Entity) -> Option<TableSnapshot> {
    let table_def = entity.table_def.as_ref()?;
    let schema = entity.schema.clone().unwrap_or_default();
    let (_, short_name) = crate::entity::split_qualified_name(&entity.name);
    Some(TableSnapshot {
        name: short_name,
        schema,
        columns: table_def.columns.clone(),
        indexes: table_def.indexes.clone(),
        table_constraints: table_def.constraints.clone(),
    })
}

/// Convert an Enum entity to an EnumSnapshot.
pub fn entity_to_enum_snapshot(entity: &Entity) -> EnumSnapshot {
    let schema = entity.schema.clone().unwrap_or_default();
    let (_, short_name) = crate::entity::split_qualified_name(&entity.name);
    EnumSnapshot {
        name: short_name,
        schema,
        values: entity.enum_values.iter().map(|v| v.name.clone()).collect(),
    }
}

// ── Snapshot preparation ────────────────────────────────

/// The result of preparing a snapshot.
pub struct SnapshotResult {
    pub snapshot: Snapshot,
    pub diffs: Vec<MigrationDiff>,
    pub migration_files: Vec<MigrationFile>,
    pub graph: Option<MigrationGraph>,
    pub warnings: Vec<String>,
    pub is_baseline: bool,
    pub no_changes: bool,
}

/// A file to be written as part of a migration.
pub struct MigrationFile {
    pub relative_path: PathBuf,
    pub content: String,
}

/// The result of a multi-snapshot preparation that may span multiple versions.
pub struct MultiSnapshotResult {
    pub snapshots: Vec<SnapshotResult>,
    pub todos: Vec<TodoItem>,
}

/// A TODO item for the developer to address (e.g., data correction).
pub struct TodoItem {
    pub file: PathBuf,
    pub message: String,
}

/// Prepare a snapshot from entities, optionally diffing against a previous snapshot.
///
/// This is pure logic — no I/O. The caller is responsible for writing files.
pub fn prepare_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> SnapshotResult {
    // Extract table and enum snapshots from entities
    let tables: Vec<TableSnapshot> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Table && e.table_def.is_some())
        .filter_map(entity_to_table_snapshot)
        .collect();

    let enums: Vec<EnumSnapshot> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .map(entity_to_enum_snapshot)
        .collect();

    let snapshot = Snapshot {
        version: next_version,
        description: description.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        tables,
        enums,
    };

    match previous {
        None => {
            // Baseline — no previous snapshot to diff against
            SnapshotResult {
                snapshot,
                diffs: vec![],
                migration_files: vec![],
                graph: None,
                warnings: vec![],
                is_baseline: true,
                no_changes: false,
            }
        }
        Some(prev) => {
            let diffs = diff::diff(prev, &snapshot);

            if diffs.is_empty() {
                return SnapshotResult {
                    snapshot,
                    diffs: vec![],
                    migration_files: vec![],
                    graph: None,
                    warnings: vec![],
                    is_baseline: false,
                    no_changes: true,
                };
            }

            // Check for risky changes
            let warnings = diff::migration_warnings(&diffs);

            // Categorize diffs
            let mut added = Vec::new();
            let mut altered = Vec::new();
            let mut dropped = Vec::new();
            let mut migration_files = Vec::new();

            for d in &diffs {
                match &d.action {
                    DiffAction::Add => {
                        added.push(d.entity_name.clone());
                        // New entities use regular apply — no migration SQL file
                    }
                    DiffAction::Change(_) => {
                        altered.push(d.entity_name.clone());
                        let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                        if !sql.is_empty() {
                            let path = entity_migration_path(&d.entity_name);
                            migration_files.push(MigrationFile {
                                relative_path: path,
                                content: sql,
                            });
                        }
                    }
                    DiffAction::Drop => {
                        dropped.push(d.entity_name.clone());
                        let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                        if !sql.is_empty() {
                            let path = entity_migration_path(&d.entity_name);
                            migration_files.push(MigrationFile {
                                relative_path: path,
                                content: sql,
                            });
                        }
                    }
                }
            }

            let graph = MigrationGraph {
                from_version: prev.version,
                to_version: next_version,
                added,
                altered,
                dropped,
            };

            SnapshotResult {
                snapshot,
                diffs,
                migration_files,
                graph: Some(graph),
                warnings,
                is_baseline: false,
                no_changes: false,
            }
        }
    }
}

/// Build a relative path for a per-entity migration SQL file.
/// Entity name "config.users" → "config/users.sql"
fn entity_migration_path(entity_name: &str) -> PathBuf {
    let (schema, table) = crate::entity::split_qualified_name(entity_name);
    match schema {
        Some(s) => PathBuf::from(s).join(format!("{table}.sql")),
        None => PathBuf::from(format!("{table}.sql")),
    }
}

/// Build a relative path for a per-entity data migration SQL file.
/// Entity name "config.users" → "config/users.data.sql"
fn entity_data_migration_path(entity_name: &str) -> PathBuf {
    let (schema, table) = crate::entity::split_qualified_name(entity_name);
    match schema {
        Some(s) => PathBuf::from(s).join(format!("{table}.data.sql")),
        None => PathBuf::from(format!("{table}.data.sql")),
    }
}

// ── Multi-snapshot preparation ─────────────────────────

/// Prepare a multi-snapshot result from entities, potentially splitting complex
/// changes across multiple migration stages.
///
/// This is pure logic — no I/O. The caller is responsible for writing files.
pub fn prepare_multi_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> MultiSnapshotResult {
    // Build the final desired snapshot from entities
    let final_tables: Vec<TableSnapshot> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Table && e.table_def.is_some())
        .filter_map(entity_to_table_snapshot)
        .collect();

    let final_enums: Vec<EnumSnapshot> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .map(entity_to_enum_snapshot)
        .collect();

    let final_snapshot = Snapshot {
        version: next_version,
        description: description.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        tables: final_tables,
        enums: final_enums,
    };

    // No previous → baseline
    let Some(prev) = previous else {
        return MultiSnapshotResult {
            snapshots: vec![SnapshotResult {
                snapshot: final_snapshot,
                diffs: vec![],
                migration_files: vec![],
                graph: None,
                warnings: vec![],
                is_baseline: true,
                no_changes: false,
            }],
            todos: vec![],
        };
    };

    // Diff previous vs final
    let diffs = diff::diff(prev, &final_snapshot);

    if diffs.is_empty() {
        return MultiSnapshotResult {
            snapshots: vec![SnapshotResult {
                snapshot: final_snapshot,
                diffs: vec![],
                migration_files: vec![],
                graph: None,
                warnings: vec![],
                is_baseline: false,
                no_changes: true,
            }],
            todos: vec![],
        };
    }

    // Classify into simple and complex
    let (simple_diffs, complex_changes) = diff::classify_changes(&diffs, prev);

    // No complex → delegate to existing prepare_snapshot (single stage)
    if complex_changes.is_empty() {
        let result = prepare_snapshot(entities, Some(prev), next_version, description);
        return MultiSnapshotResult {
            snapshots: vec![result],
            todos: vec![],
        };
    }

    // Determine number of stages
    let has_enum_removal = complex_changes
        .iter()
        .any(|c| matches!(c, ComplexChange::EnumValueRemoval { .. }));
    let num_stages = if has_enum_removal { 3 } else { 2 };

    let mut snapshots = Vec::new();
    let mut todos = Vec::new();

    // ── Stage 1 ───────────────────────────────────────────
    let mut stage1_snap = prev.clone();
    stage1_snap.version = next_version;
    stage1_snap.description = format!("{description} (stage 1/{num_stages})");
    stage1_snap.timestamp = chrono::Utc::now().to_rfc3339();

    let mut stage1_files = Vec::new();
    let mut stage1_added = Vec::new();
    let mut stage1_altered = Vec::new();
    let mut stage1_dropped = Vec::new();

    // Apply simple diffs to stage1 snapshot
    for d in &simple_diffs {
        match &d.action {
            DiffAction::Add => {
                stage1_added.push(d.entity_name.clone());
                // Add new tables/enums to snapshot
                if d.entity_type == EntityType::Table {
                    if let Some(t) = final_snapshot
                        .tables
                        .iter()
                        .find(|t| format!("{}.{}", t.schema, t.name) == d.entity_name)
                    {
                        stage1_snap.tables.push(t.clone());
                    }
                } else if d.entity_type == EntityType::Enum
                    && let Some(e) = final_snapshot
                        .enums
                        .iter()
                        .find(|e| format!("{}.{}", e.schema, e.name) == d.entity_name)
                {
                    stage1_snap.enums.push(e.clone());
                }
            }
            DiffAction::Drop => {
                stage1_dropped.push(d.entity_name.clone());
                let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                if !sql.is_empty() {
                    stage1_files.push(MigrationFile {
                        relative_path: entity_migration_path(&d.entity_name),
                        content: sql,
                    });
                }
                // Remove from snapshot
                if d.entity_type == EntityType::Table {
                    stage1_snap
                        .tables
                        .retain(|t| format!("{}.{}", t.schema, t.name) != d.entity_name);
                } else if d.entity_type == EntityType::Enum {
                    stage1_snap
                        .enums
                        .retain(|e| format!("{}.{}", e.schema, e.name) != d.entity_name);
                }
            }
            DiffAction::Change(_) => {
                stage1_altered.push(d.entity_name.clone());
                let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                if !sql.is_empty() {
                    stage1_files.push(MigrationFile {
                        relative_path: entity_migration_path(&d.entity_name),
                        content: sql,
                    });
                }
                // Update snapshot table with simple changes from final
                if d.entity_type == EntityType::Table
                    && let Some(final_t) = final_snapshot
                        .tables
                        .iter()
                        .find(|t| format!("{}.{}", t.schema, t.name) == d.entity_name)
                    && let Some(snap_t) = stage1_snap
                        .tables
                        .iter_mut()
                        .find(|t| format!("{}.{}", t.schema, t.name) == d.entity_name)
                {
                    // Apply simple column/constraint/index changes from final
                    *snap_t = final_t.clone();
                } else if d.entity_type == EntityType::Enum
                    && let Some(final_e) = final_snapshot
                        .enums
                        .iter()
                        .find(|e| format!("{}.{}", e.schema, e.name) == d.entity_name)
                    && let Some(snap_e) = stage1_snap
                        .enums
                        .iter_mut()
                        .find(|e| format!("{}.{}", e.schema, e.name) == d.entity_name)
                {
                    *snap_e = final_e.clone();
                }
            }
        }
    }

    // Apply complex step 1
    for change in &complex_changes {
        match change {
            ComplexChange::ColumnRename {
                table_name,
                new_name,
                col_def,
                ..
            }
            | ComplexChange::ColumnTypeChange {
                table_name,
                new_col: col_def,
                column_name: new_name,
                ..
            } => {
                // Add new column to table snapshot
                if let Some(snap_t) = stage1_snap
                    .tables
                    .iter_mut()
                    .find(|t| format!("{}.{}", t.schema, t.name) == *table_name)
                {
                    // For ColumnTypeChange, new_name is actually column_name and
                    // col_def is new_col; we need to add the new column definition.
                    // For ColumnRename, col_def has the new name already.
                    let mut new_col_def = col_def.as_ref().clone();
                    if let ComplexChange::ColumnTypeChange { .. } = change {
                        // The new column gets a temp name: column_name_new
                        new_col_def.name = format!("{new_name}_new");
                    }
                    snap_t.columns.push(new_col_def);
                }

                if !stage1_altered.contains(table_name) {
                    stage1_altered.push(table_name.clone());
                }

                // Generate ADD COLUMN SQL
                let add_col_sql = match change {
                    ComplexChange::ColumnRename { col_def, new_name, .. } => {
                        format!(
                            "ALTER TABLE {} ADD COLUMN {} {};",
                            table_name, new_name, col_def.data_type
                        )
                    }
                    ComplexChange::ColumnTypeChange { new_col, column_name, .. } => {
                        format!(
                            "ALTER TABLE {} ADD COLUMN {}_new {};",
                            table_name, column_name, new_col.data_type
                        )
                    }
                    _ => unreachable!(),
                };

                // Append or create migration file for this table
                let path = entity_migration_path(table_name);
                if let Some(existing) = stage1_files
                    .iter_mut()
                    .find(|f| f.relative_path == path)
                {
                    existing.content.push('\n');
                    existing.content.push_str(&add_col_sql);
                } else {
                    stage1_files.push(MigrationFile {
                        relative_path: path,
                        content: add_col_sql,
                    });
                }

                // Generate data.sql
                let data_sql = diff::generate_data_sql(change);
                let data_path = entity_data_migration_path(table_name);
                stage1_files.push(MigrationFile {
                    relative_path: data_path.clone(),
                    content: data_sql,
                });

                todos.push(TodoItem {
                    file: data_path,
                    message: format!(
                        "Review data migration SQL for {}",
                        table_name
                    ),
                });
            }
            ComplexChange::EnumValueRemoval {
                enum_name,
                affected_columns,
                ..
            } => {
                // No schema change in stage 1 for enum removal
                // Generate data.sql for affected tables
                let data_sql = diff::generate_data_sql(change);
                // Use the first affected table for the data path, or the enum name
                let data_entity = if let Some((table, _)) = affected_columns.first() {
                    table.as_str()
                } else {
                    enum_name.as_str()
                };
                let data_path = entity_data_migration_path(data_entity);
                stage1_files.push(MigrationFile {
                    relative_path: data_path.clone(),
                    content: data_sql,
                });

                todos.push(TodoItem {
                    file: data_path,
                    message: format!(
                        "Review data migration SQL for enum value removal in {}",
                        enum_name
                    ),
                });
            }
        }
    }

    let stage1_graph = MigrationGraph {
        from_version: prev.version,
        to_version: next_version,
        added: stage1_added,
        altered: stage1_altered,
        dropped: stage1_dropped,
    };

    let stage1_warnings = diff::migration_warnings(&diffs);

    snapshots.push(SnapshotResult {
        snapshot: stage1_snap.clone(),
        diffs: diffs.clone(),
        migration_files: stage1_files,
        graph: Some(stage1_graph),
        warnings: stage1_warnings,
        is_baseline: false,
        no_changes: false,
    });

    // ── Stage 2 ───────────────────────────────────────────
    let mut stage2_snap = stage1_snap;
    stage2_snap.version = next_version + 1;
    stage2_snap.description = format!("{description} (stage 2/{num_stages})");
    stage2_snap.timestamp = chrono::Utc::now().to_rfc3339();

    let mut stage2_files: Vec<MigrationFile> = Vec::new();
    let mut stage2_altered: Vec<String> = Vec::new();
    let mut stage2_dropped = Vec::new();

    for change in &complex_changes {
        match change {
            ComplexChange::ColumnRename {
                table_name,
                old_name,
                ..
            } => {
                // Drop old column from snapshot
                if let Some(snap_t) = stage2_snap
                    .tables
                    .iter_mut()
                    .find(|t| format!("{}.{}", t.schema, t.name) == *table_name)
                {
                    snap_t.columns.retain(|c| c.name != *old_name);
                }

                let sql = format!(
                    "ALTER TABLE {} DROP COLUMN {};",
                    table_name, old_name
                );
                let path = entity_migration_path(table_name);
                if let Some(existing) = stage2_files
                    .iter_mut()
                    .find(|f| f.relative_path == path)
                {
                    existing.content.push('\n');
                    existing.content.push_str(&sql);
                } else {
                    stage2_files.push(MigrationFile {
                        relative_path: path,
                        content: sql,
                    });
                }

                if !stage2_altered.contains(table_name) {
                    stage2_altered.push(table_name.clone());
                }
            }
            ComplexChange::ColumnTypeChange {
                table_name,
                column_name,
                old_col,
                new_col,
                ..
            } => {
                // Drop old column and rename new column in snapshot
                if let Some(snap_t) = stage2_snap
                    .tables
                    .iter_mut()
                    .find(|t| format!("{}.{}", t.schema, t.name) == *table_name)
                {
                    snap_t.columns.retain(|c| c.name != *column_name);
                    // Rename temp column to original name
                    let temp_name = format!("{column_name}_new");
                    if let Some(col) = snap_t.columns.iter_mut().find(|c| c.name == temp_name) {
                        col.name = new_col.name.clone();
                    }
                }

                let sql = format!(
                    "ALTER TABLE {} DROP COLUMN {};\nALTER TABLE {} RENAME COLUMN {}_new TO {};",
                    table_name, old_col.name, table_name, column_name, column_name
                );
                let path = entity_migration_path(table_name);
                if let Some(existing) = stage2_files
                    .iter_mut()
                    .find(|f| f.relative_path == path)
                {
                    existing.content.push('\n');
                    existing.content.push_str(&sql);
                } else {
                    stage2_files.push(MigrationFile {
                        relative_path: path,
                        content: sql,
                    });
                }

                if !stage2_altered.contains(table_name) {
                    stage2_altered.push(table_name.clone());
                }
            }
            ComplexChange::EnumValueRemoval {
                enum_name,
                affected_columns,
                ..
            } => {
                // Change affected columns' data_type to TEXT
                for (table_name, col_name) in affected_columns {
                    if let Some(snap_t) = stage2_snap
                        .tables
                        .iter_mut()
                        .find(|t| format!("{}.{}", t.schema, t.name) == *table_name)
                        && let Some(col) = snap_t.columns.iter_mut().find(|c| c.name == *col_name)
                    {
                        col.data_type = "TEXT".to_string();
                    }

                    let sql = format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE TEXT;",
                        table_name, col_name
                    );
                    let path = entity_migration_path(table_name);
                    if let Some(existing) = stage2_files
                        .iter_mut()
                        .find(|f| f.relative_path == path)
                    {
                        existing.content.push('\n');
                        existing.content.push_str(&sql);
                    } else {
                        stage2_files.push(MigrationFile {
                            relative_path: path,
                            content: sql,
                        });
                    }

                    if !stage2_altered.contains(table_name) {
                        stage2_altered.push(table_name.clone());
                    }
                }

                // Remove enum from snapshot
                stage2_snap
                    .enums
                    .retain(|e| format!("{}.{}", e.schema, e.name) != *enum_name);

                // Generate DROP TYPE SQL
                let drop_sql = format!("DROP TYPE {};", enum_name);
                let path = entity_migration_path(enum_name);
                if let Some(existing) = stage2_files
                    .iter_mut()
                    .find(|f| f.relative_path == path)
                {
                    existing.content.push('\n');
                    existing.content.push_str(&drop_sql);
                } else {
                    stage2_files.push(MigrationFile {
                        relative_path: path,
                        content: drop_sql,
                    });
                }

                if !stage2_dropped.contains(enum_name) {
                    stage2_dropped.push(enum_name.clone());
                }
            }
        }
    }

    let stage2_graph = MigrationGraph {
        from_version: next_version,
        to_version: next_version + 1,
        added: vec![],
        altered: stage2_altered,
        dropped: stage2_dropped,
    };

    snapshots.push(SnapshotResult {
        snapshot: stage2_snap.clone(),
        diffs: vec![],
        migration_files: stage2_files,
        graph: Some(stage2_graph),
        warnings: vec![],
        is_baseline: false,
        no_changes: false,
    });

    // ── Stage 3 (only for enum removal) ───────────────────
    if has_enum_removal {
        let mut stage3_snap = final_snapshot;
        stage3_snap.version = next_version + 2;
        stage3_snap.description = format!("{description} (stage 3/{num_stages})");
        stage3_snap.timestamp = chrono::Utc::now().to_rfc3339();

        let mut stage3_files = Vec::new();
        let mut stage3_added = Vec::new();
        let mut stage3_altered = Vec::new();

        for change in &complex_changes {
            if let ComplexChange::EnumValueRemoval {
                enum_name,
                remaining_values,
                affected_columns,
                ..
            } = change
            {
                // Generate CREATE TYPE SQL with remaining values
                let values_sql: Vec<String> =
                    remaining_values.iter().map(|v| format!("'{v}'")).collect();
                let create_sql = format!(
                    "CREATE TYPE {} AS ENUM ({});",
                    enum_name,
                    values_sql.join(", ")
                );
                let path = entity_migration_path(enum_name);
                stage3_files.push(MigrationFile {
                    relative_path: path,
                    content: create_sql,
                });

                stage3_added.push(enum_name.clone());

                // Generate ALTER COLUMN TYPE enum_name USING for affected columns
                for (table_name, col_name) in affected_columns {
                    let alter_sql = format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::{};",
                        table_name, col_name, enum_name, col_name, enum_name
                    );
                    let tbl_path = entity_migration_path(table_name);
                    if let Some(existing) = stage3_files
                        .iter_mut()
                        .find(|f| f.relative_path == tbl_path)
                    {
                        existing.content.push('\n');
                        existing.content.push_str(&alter_sql);
                    } else {
                        stage3_files.push(MigrationFile {
                            relative_path: tbl_path,
                            content: alter_sql,
                        });
                    }

                    if !stage3_altered.contains(table_name) {
                        stage3_altered.push(table_name.clone());
                    }
                }
            }
        }

        let stage3_graph = MigrationGraph {
            from_version: next_version + 1,
            to_version: next_version + 2,
            added: stage3_added,
            altered: stage3_altered,
            dropped: vec![],
        };

        snapshots.push(SnapshotResult {
            snapshot: stage3_snap,
            diffs: vec![],
            migration_files: stage3_files,
            graph: Some(stage3_graph),
            warnings: vec![],
            is_baseline: false,
            no_changes: false,
        });
    }

    MultiSnapshotResult { snapshots, todos }
}

// ── Snapshot I/O: create_snapshot ───────────────────────

/// Create a snapshot from entities, writing all files to disk.
///
/// This is the thin I/O wrapper around `prepare_multi_snapshot`.
pub fn create_snapshot(
    entities: &[Entity],
    project_dir: &Path,
    config_path: &Path,
    description: &str,
) -> Result<MultiSnapshotResult> {
    let previous = latest_snapshot(project_dir)?;
    let base_version = next_version(project_dir);
    let result = prepare_multi_snapshot(entities, previous.as_ref(), base_version, description);

    // Check for no-changes
    if result.snapshots.len() == 1 && result.snapshots[0].no_changes {
        return Ok(result);
    }

    let snapshots_dir = project_dir.join(SNAPSHOTS_DIR);
    std::fs::create_dir_all(&snapshots_dir)?;

    let mut final_version = base_version;
    for snap_result in &result.snapshots {
        if snap_result.no_changes {
            continue;
        }
        let version = snap_result.snapshot.version;
        final_version = version;

        // Write snapshot JSON
        let snap_file = snapshots_dir.join(format!("{}.json", pad_version(version)));
        std::fs::write(&snap_file, serde_json::to_string_pretty(&snap_result.snapshot)?)?;

        // Write migration files
        if let Some(ref graph) = snap_result.graph {
            let migration_dir = project_dir.join(MIGRATIONS_DIR).join(pad_version(version));
            std::fs::create_dir_all(&migration_dir)?;
            std::fs::write(
                migration_dir.join("graph.json"),
                serde_json::to_string_pretty(graph)?,
            )?;
            for file in &snap_result.migration_files {
                let full_path = migration_dir.join(&file.relative_path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&full_path, &file.content)?;
            }
        }
    }

    crate::config::update_version(config_path, final_version)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_snapshot_dir(tmp: &TempDir) {
        let dir = tmp.path().join("snapshots");
        fs::create_dir_all(&dir).unwrap();

        let snap1 = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![],
            enums: vec![],
        };
        fs::write(
            dir.join("001.json"),
            serde_json::to_string_pretty(&snap1).unwrap(),
        )
        .unwrap();

        let snap2 = Snapshot {
            version: 2,
            description: "add notes column".to_string(),
            timestamp: "2026-02-01T00:00:00Z".to_string(),
            tables: vec![],
            enums: vec![],
        };
        fs::write(
            dir.join("002.json"),
            serde_json::to_string_pretty(&snap2).unwrap(),
        )
        .unwrap();
    }

    fn create_migration_dir(tmp: &TempDir) {
        let dir = tmp.path().join("migrations/002");
        fs::create_dir_all(&dir).unwrap();

        let graph = MigrationGraph {
            from_version: 1,
            to_version: 2,
            added: vec![],
            altered: vec!["config.lookup_values".to_string()],
            dropped: vec![],
        };
        fs::write(
            dir.join("graph.json"),
            serde_json::to_string_pretty(&graph).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pad_version_formats() {
        assert_eq!(pad_version(1), "001");
        assert_eq!(pad_version(42), "042");
        assert_eq!(pad_version(100), "100");
    }

    #[test]
    fn checksum_is_deterministic() {
        let a = checksum_of("hello");
        let b = checksum_of("hello");
        assert_eq!(a, b);
        assert_ne!(a, checksum_of("world"));
    }

    #[test]
    fn list_snapshots_finds_all() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        let snapshots = list_snapshots(tmp.path());

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].version, 1);
        assert_eq!(snapshots[0].description, "initial");
        assert_eq!(snapshots[1].version, 2);
        assert_eq!(snapshots[1].description, "add notes column");
    }

    #[test]
    fn list_snapshots_empty_when_no_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(list_snapshots(tmp.path()).is_empty());
    }

    #[test]
    fn read_snapshot_by_version() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        let snap = read_snapshot(1, tmp.path()).unwrap().unwrap();
        assert_eq!(snap.version, 1);
        assert_eq!(snap.description, "initial");
    }

    #[test]
    fn read_snapshot_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        assert!(read_snapshot(99, tmp.path()).unwrap().is_none());
    }

    #[test]
    fn latest_snapshot_returns_highest_version() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        let snap = latest_snapshot(tmp.path()).unwrap().unwrap();
        assert_eq!(snap.version, 2);
    }

    #[test]
    fn next_version_increments() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        assert_eq!(next_version(tmp.path()), 3);
    }

    #[test]
    fn next_version_starts_at_1() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(next_version(tmp.path()), 1);
    }

    #[test]
    fn has_snapshots_detects_presence() {
        let tmp = TempDir::new().unwrap();
        assert!(!has_snapshots(tmp.path()));
        create_snapshot_dir(&tmp);
        assert!(has_snapshots(tmp.path()));
    }

    #[test]
    fn pending_migrations_filters_by_version() {
        let tmp = TempDir::new().unwrap();
        create_migration_dir(&tmp);

        // DB at version 0 → migration 002 is pending
        let pending = pending_migrations(0, tmp.path());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].to_version, 2);
        assert_eq!(pending[0].altered, vec!["config.lookup_values"]);
        assert!(!pending[0].checksum.is_empty());

        // DB at version 2 → nothing pending
        let pending = pending_migrations(2, tmp.path());
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_migrations_empty_when_no_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(pending_migrations(0, tmp.path()).is_empty());
    }

    // ── Helpers for entity-based tests ─────────────────────

    use crate::entity::{Entity, EntityType, EnumValue, TableDef, TableComments};

    fn col(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            is_identity: false,
            comment: None,
            inline_fk: None,
        }
    }

    fn make_table_entity(name: &str, columns: Vec<ColumnDef>) -> Entity {
        let mut entity = Entity::new(EntityType::Table, name);
        entity.table_def = Some(TableDef {
            columns,
            constraints: vec![],
            indexes: vec![],
            comments: TableComments::default(),
        });
        entity
    }

    fn make_enum_entity(name: &str, values: Vec<&str>) -> Entity {
        let mut entity = Entity::new(EntityType::Enum, name);
        entity.enum_values = values
            .into_iter()
            .map(|v| EnumValue {
                name: v.to_string(),
                note: None,
            })
            .collect();
        entity
    }

    // ── SC1: First snapshot baseline ────────────────────────

    #[test]
    fn sc1_first_snapshot_is_baseline() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int"), col("name", "text")]),
            make_table_entity("config.orders", vec![col("id", "int")]),
            make_enum_entity("config.status", vec!["active", "inactive"]),
        ];

        let result = prepare_snapshot(&entities, None, 1, "initial");
        assert!(result.is_baseline);
        assert!(!result.no_changes);
        assert!(result.diffs.is_empty());
        assert!(result.migration_files.is_empty());
        assert!(result.graph.is_none());
        assert_eq!(result.snapshot.version, 1);
        assert_eq!(result.snapshot.tables.len(), 2);
        assert_eq!(result.snapshot.enums.len(), 1);
    }

    // ── SC2: Second snapshot with changes ───────────────────

    #[test]
    fn sc2_second_snapshot_with_changes() {
        // Previous snapshot has one table with one column
        let prev = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "int")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![],
        };

        // New entities: same table with an added column
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int"), col("email", "text")]),
        ];

        let result = prepare_snapshot(&entities, Some(&prev), 2, "add email");
        assert!(!result.is_baseline);
        assert!(!result.no_changes);
        assert!(!result.diffs.is_empty());
        assert!(!result.migration_files.is_empty());
        let graph = result.graph.as_ref().unwrap();
        assert_eq!(graph.from_version, 1);
        assert_eq!(graph.to_version, 2);
        assert!(graph.altered.contains(&"config.users".to_string()));
    }

    // ── SC3: No changes ─────────────────────────────────────

    #[test]
    fn sc3_no_changes_detected() {
        let prev = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "int")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![],
        };

        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int")]),
        ];

        let result = prepare_snapshot(&entities, Some(&prev), 2, "no changes");
        assert!(result.no_changes);
        assert!(!result.is_baseline);
        assert!(result.diffs.is_empty());
        assert!(result.graph.is_none());
    }

    // ── SC4: New table added ────────────────────────────────

    #[test]
    fn sc4_new_table_added() {
        let prev = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "int")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![],
        };

        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int")]),
            make_table_entity("config.orders", vec![col("id", "int")]),
        ];

        let result = prepare_snapshot(&entities, Some(&prev), 2, "add orders");
        let graph = result.graph.as_ref().unwrap();
        assert!(graph.added.contains(&"config.orders".to_string()));
        // Added tables don't generate migration SQL files
        assert!(
            result.migration_files.iter().all(|f| {
                !f.relative_path.to_string_lossy().contains("orders")
            }),
            "added tables should not produce migration SQL files"
        );
    }

    // ── SC5: Dropped table ──────────────────────────────────

    #[test]
    fn sc5_dropped_table() {
        let prev = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![
                TableSnapshot {
                    name: "users".to_string(),
                    schema: "config".to_string(),
                    columns: vec![col("id", "int")],
                    indexes: vec![],
                    table_constraints: vec![],
                },
                TableSnapshot {
                    name: "legacy".to_string(),
                    schema: "config".to_string(),
                    columns: vec![col("id", "int")],
                    indexes: vec![],
                    table_constraints: vec![],
                },
            ],
            enums: vec![],
        };

        // Only users remains
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int")]),
        ];

        let result = prepare_snapshot(&entities, Some(&prev), 2, "drop legacy");
        let graph = result.graph.as_ref().unwrap();
        assert!(graph.dropped.contains(&"config.legacy".to_string()));
        // Should have a migration file with DROP TABLE
        let drop_file = result.migration_files.iter().find(|f| {
            f.relative_path.to_string_lossy().contains("legacy")
        });
        assert!(drop_file.is_some(), "dropped table should have migration SQL");
        assert!(drop_file.unwrap().content.contains("DROP TABLE"));
    }

    // ── SC7: Entity to TableSnapshot conversion ─────────────

    #[test]
    fn sc7_entity_to_table_snapshot_includes_all_fields() {
        let mut entity = make_table_entity("config.users", vec![col("id", "int")]);
        entity.table_def.as_mut().unwrap().constraints.push(
            crate::entity::TableConstraint::PrimaryKey {
                name: Some("pk_users".to_string()),
                columns: vec!["id".to_string()],
            },
        );
        entity.table_def.as_mut().unwrap().indexes.push(
            crate::entity::IndexDef {
                name: Some("idx_id".to_string()),
                columns: vec![crate::entity::IndexColumn {
                    name: "id".to_string(),
                    order: None,
                }],
                unique: false,
                index_type: None,
            },
        );

        let snap = entity_to_table_snapshot(&entity).unwrap();
        assert_eq!(snap.name, "users");
        assert_eq!(snap.schema, "config");
        assert_eq!(snap.columns.len(), 1);
        assert_eq!(snap.columns[0].name, "id");
        assert_eq!(snap.table_constraints.len(), 1);
        assert_eq!(snap.indexes.len(), 1);
    }

    // ── SC8: Entity to EnumSnapshot conversion ──────────────

    #[test]
    fn sc8_entity_to_enum_snapshot() {
        let entity = make_enum_entity("config.status", vec!["active", "inactive", "pending"]);
        let snap = entity_to_enum_snapshot(&entity);
        assert_eq!(snap.name, "status");
        assert_eq!(snap.schema, "config");
        assert_eq!(snap.values, vec!["active", "inactive", "pending"]);
    }

    // ── SC9: Snapshot serialization round-trip ───────────────

    #[test]
    fn sc9_snapshot_serialization_round_trip() {
        let snapshot = Snapshot {
            version: 1,
            description: "test".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "int")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![EnumSnapshot {
                name: "status".to_string(),
                schema: "config".to_string(),
                values: vec!["active".to_string()],
            }],
        };

        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let deserialized: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, 1);
        assert_eq!(deserialized.tables.len(), 1);
        assert_eq!(deserialized.tables[0].name, "users");
        assert_eq!(deserialized.enums.len(), 1);
        assert_eq!(deserialized.enums[0].name, "status");
    }

    // ── SC10: create_snapshot I/O integration ────────────

    #[test]
    fn sc10_create_snapshot_writes_files() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("design.yaml");
        fs::write(&config_path, "project:\n  name: test\ntarget: {}\n").unwrap();

        let entities = vec![
            make_table_entity("config.users", vec![col("id", "BIGINT")]),
        ];

        // First snapshot — baseline
        let result = create_snapshot(&entities, tmp.path(), &config_path, "initial").unwrap();
        assert!(result.snapshots[0].is_baseline);
        assert!(tmp.path().join("snapshots/001.json").exists());

        // Verify design.yaml updated
        let config_content = fs::read_to_string(&config_path).unwrap();
        assert!(config_content.contains("1"));

        // Second snapshot with changes
        let entities_v2 = vec![
            make_table_entity("config.users", vec![col("id", "BIGINT"), col("email", "TEXT")]),
        ];
        let result2 = create_snapshot(&entities_v2, tmp.path(), &config_path, "add email").unwrap();
        assert!(!result2.snapshots[0].no_changes);
        assert!(tmp.path().join("snapshots/002.json").exists());
        assert!(tmp.path().join("migrations/002/graph.json").exists());
    }

    #[test]
    fn migration_graph_without_added_field_deserializes_with_empty_vec() {
        let json = r#"{
            "fromVersion": 1,
            "toVersion": 2,
            "altered": ["config.users"],
            "dropped": []
        }"#;
        let graph: MigrationGraph = serde_json::from_str(json).unwrap();
        assert!(graph.added.is_empty());
        assert_eq!(graph.altered, vec!["config.users"]);
        assert_eq!(graph.from_version, 1);
        assert_eq!(graph.to_version, 2);
    }

    // ════════════════════════════════════════════════════════
    // Scenario Tests: Snapshot edge cases
    // ════════════════════════════════════════════════════════

    // M3.2: External entity excluded from snapshot
    #[test]
    fn sc_external_entity_excluded_from_snapshot() {
        let external = Entity::new(EntityType::External, "pg_catalog.pg_type");
        let table_entity = make_table_entity("config.users", vec![col("id", "int")]);
        let entities = vec![external, table_entity];

        let result = prepare_snapshot(&entities, None, 1, "test");
        assert_eq!(result.snapshot.tables.len(), 1);
        assert_eq!(result.snapshot.tables[0].name, "users");
    }

    // M3.3: View entity excluded from snapshot
    #[test]
    fn sc_view_entity_excluded_from_snapshot() {
        let mut view = Entity::new(EntityType::View, "config.active_users");
        view.table_def = None;
        let table_entity = make_table_entity("config.users", vec![col("id", "int")]);
        let entities = vec![view, table_entity];

        let result = prepare_snapshot(&entities, None, 1, "test");
        // Only Table entities with table_def are included
        assert_eq!(result.snapshot.tables.len(), 1);
        assert_eq!(result.snapshot.tables[0].name, "users");
    }

    // M3.4: Table without table_def excluded
    #[test]
    fn sc_table_without_table_def_excluded() {
        let mut table_no_def = Entity::new(EntityType::Table, "config.broken");
        table_no_def.table_def = None;
        let table_with_def = make_table_entity("config.users", vec![col("id", "int")]);
        let entities = vec![table_no_def, table_with_def];

        let result = prepare_snapshot(&entities, None, 1, "test");
        assert_eq!(result.snapshot.tables.len(), 1);
        assert_eq!(result.snapshot.tables[0].name, "users");
    }

    // M3.5: Empty enum snapshot
    #[test]
    fn sc_enum_with_zero_values() {
        let empty_enum = make_enum_entity("config.empty_type", vec![]);
        let entities = vec![empty_enum];

        let result = prepare_snapshot(&entities, None, 1, "test");
        assert_eq!(result.snapshot.enums.len(), 1);
        assert_eq!(result.snapshot.enums[0].name, "empty_type");
        assert!(result.snapshot.enums[0].values.is_empty());
    }

    // M3.6: Tables + enums both changed
    #[test]
    fn sc_table_and_enum_both_changed() {
        let prev = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "int")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![EnumSnapshot {
                name: "status".to_string(),
                schema: "config".to_string(),
                values: vec!["active".to_string(), "inactive".to_string()],
            }],
        };

        // New: table gets extra column, enum gets extra value
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int"), col("email", "text")]),
            make_enum_entity("config.status", vec!["active", "inactive", "pending"]),
        ];

        let result = prepare_snapshot(&entities, Some(&prev), 2, "add email and pending");
        assert!(!result.no_changes);
        assert!(!result.diffs.is_empty());
        // Both table and enum should appear in diffs
        let table_diff = result.diffs.iter().find(|d| d.entity_type == EntityType::Table);
        let enum_diff = result.diffs.iter().find(|d| d.entity_type == EntityType::Enum);
        assert!(table_diff.is_some(), "table diff should be present");
        assert!(enum_diff.is_some(), "enum diff should be present");
    }

    // M3.7: Only enum changed, no tables
    #[test]
    fn sc_only_enum_changed_no_tables() {
        let prev = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "int")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![EnumSnapshot {
                name: "status".to_string(),
                schema: "config".to_string(),
                values: vec!["active".to_string()],
            }],
        };

        // Same table, different enum values
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "int")]),
            make_enum_entity("config.status", vec!["active", "inactive"]),
        ];

        let result = prepare_snapshot(&entities, Some(&prev), 2, "add inactive");
        assert!(!result.no_changes);
        assert!(!result.diffs.is_empty());
        // All diffs should be enum-related
        assert!(result.diffs.iter().all(|d| d.entity_type == EntityType::Enum));
    }

    // ════════════════════════════════════════════════════════
    // Multi-Snapshot Tests (B1-B3, S1-S2, baseline, no-changes)
    // ════════════════════════════════════════════════════════

    #[test]
    fn b1_simple_only_one_snapshot() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("email", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "add email");
        assert_eq!(result.snapshots.len(), 1, "simple change should produce 1 snapshot");
        assert!(result.todos.is_empty());
    }

    #[test]
    fn b2_column_rename_two_snapshots() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT"), col("name", "TEXT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("display_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename");
        assert_eq!(result.snapshots.len(), 2, "column rename should produce 2 snapshots");
        // Stage 1 should have data.sql file
        assert!(result.snapshots[0].migration_files.iter().any(|f|
            f.relative_path.to_string_lossy().contains("data.sql")
        ), "stage 1 should have data.sql for copy");
    }

    #[test]
    fn b3_enum_removal_three_snapshots() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot {
                name: "events".to_string(), schema: "config".to_string(),
                columns: vec![col("id", "INT"), ColumnDef { data_type: "public.status_type".to_string(), ..col("status", "public.status_type") }],
                indexes: vec![], table_constraints: vec![],
            }],
            // Use short name to match entity_to_enum_snapshot output
            enums: vec![EnumSnapshot { name: "status_type".to_string(), schema: "public".to_string(), values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()] }],
        };
        let entities = vec![
            make_table_entity("config.events", vec![col("id", "INT"), ColumnDef { data_type: "public.status_type".to_string(), ..col("status", "public.status_type") }]),
            make_enum_entity("public.status_type", vec!["active", "inactive"]),
        ];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "remove deleted");
        assert_eq!(result.snapshots.len(), 3, "enum removal should produce 3 snapshots");
        assert!(!result.todos.is_empty(), "should have TODO items for enum mapping");
    }

    #[test]
    fn s1_stage1_adds_new_column_for_rename() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT"), col("name", "TEXT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("display_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename");
        assert!(result.snapshots.len() >= 2);
        // Stage 1 should have 3 columns: id, name, display_name
        let stage1_table = result.snapshots[0].snapshot.tables.iter()
            .find(|t| t.name == "users").expect("should have users table");
        let col_names: Vec<&str> = stage1_table.columns.iter().map(|c| c.name.as_str()).collect();
        assert!(col_names.contains(&"id"), "should have id");
        assert!(col_names.contains(&"name"), "should still have old name");
        assert!(col_names.contains(&"display_name"), "should have new display_name");
    }

    #[test]
    fn s2_stage2_drops_old_column_for_rename() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT"), col("name", "TEXT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("display_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename");
        assert!(result.snapshots.len() >= 2);
        // Stage 2 should have 2 columns: id, display_name (no "name")
        let stage2_table = result.snapshots[1].snapshot.tables.iter()
            .find(|t| t.name == "users").expect("should have users table");
        let col_names: Vec<&str> = stage2_table.columns.iter().map(|c| c.name.as_str()).collect();
        assert!(col_names.contains(&"id"));
        assert!(col_names.contains(&"display_name"));
        assert!(!col_names.contains(&"name"), "old name should be dropped in stage 2");
    }

    #[test]
    fn b_baseline_returns_one_snapshot() {
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT")])];
        let result = prepare_multi_snapshot(&entities, None, 1, "initial");
        assert_eq!(result.snapshots.len(), 1);
        assert!(result.snapshots[0].is_baseline);
    }

    #[test]
    fn b_no_changes_returns_one_snapshot() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "no change");
        assert_eq!(result.snapshots.len(), 1);
        assert!(result.snapshots[0].no_changes);
    }
}
