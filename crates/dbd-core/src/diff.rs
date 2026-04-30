use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::entity::{ColumnDef, EntityType, IndexDef, TableConstraint};
use crate::snapshot::{Snapshot, TableSnapshot};

/// A single entity-level diff between two snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationDiff {
    pub entity_name: String,
    pub entity_type: EntityType,
    pub action: DiffAction,
}

/// What happened to an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiffAction {
    Add,
    Drop,
    Change(Vec<FieldChange>),
}

/// A single field-level change within an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChange {
    pub field_name: String,
    pub field_type: FieldType,
    pub action: ChangeAction,
}

/// The kind of sub-entity that changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Column,
    Constraint,
    Index,
    EnumValue,
}

/// What happened to a specific field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeAction {
    Add(Box<FieldDetail>),
    Drop,
    Alter {
        old: Box<FieldDetail>,
        new: Box<FieldDetail>,
    },
}

/// The concrete detail of a field value, for Add / Alter payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldDetail {
    Column(ColumnDef),
    Constraint(TableConstraint),
    Index(IndexDef),
    EnumValue(String),
}

/// Compare two snapshots and return a list of migration diffs.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Vec<MigrationDiff> {
    let mut diffs = diff_tables(&old.tables, &new.tables);
    diffs.extend(diff_enums());
    diffs
}

/// Qualified name for a table: "schema.name".
fn qualified_name(t: &TableSnapshot) -> String {
    format!("{}.{}", t.schema, t.name)
}

/// Diff tables between two snapshots by qualified name.
fn diff_tables(old: &[TableSnapshot], new: &[TableSnapshot]) -> Vec<MigrationDiff> {
    let old_map: HashMap<String, &TableSnapshot> =
        old.iter().map(|t| (qualified_name(t), t)).collect();
    let new_map: HashMap<String, &TableSnapshot> =
        new.iter().map(|t| (qualified_name(t), t)).collect();

    let mut diffs = Vec::new();

    // Tables in new but not in old → Add
    for name in new_map.keys() {
        if !old_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.clone(),
                entity_type: EntityType::Table,
                action: DiffAction::Add,
            });
        }
    }

    // Tables in old but not in new → Drop
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.clone(),
                entity_type: EntityType::Table,
                action: DiffAction::Drop,
            });
        }
    }

    // Tables in both → check for field-level changes
    for (name, old_t) in &old_map {
        if let Some(new_t) = new_map.get(name) {
            let changes = diff_table_fields(old_t, new_t);
            if !changes.is_empty() {
                diffs.push(MigrationDiff {
                    entity_name: name.clone(),
                    entity_type: EntityType::Table,
                    action: DiffAction::Change(changes),
                });
            }
        }
    }

    diffs
}

/// Diff all fields within a single table pair.
fn diff_table_fields(old: &TableSnapshot, new: &TableSnapshot) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    changes.extend(diff_columns(&old.columns, &new.columns));
    changes.extend(diff_constraints(&old.table_constraints, &new.table_constraints));
    changes.extend(diff_indexes(&old.indexes, &new.indexes));
    changes
}

/// Diff columns by name: Add / Drop / Alter (via PartialEq).
fn diff_columns(old: &[ColumnDef], new: &[ColumnDef]) -> Vec<FieldChange> {
    let old_map: HashMap<&str, &ColumnDef> = old.iter().map(|c| (c.name.as_str(), c)).collect();
    let new_map: HashMap<&str, &ColumnDef> = new.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut changes = Vec::new();

    // Added columns
    for (name, col) in &new_map {
        if !old_map.contains_key(name) {
            changes.push(FieldChange {
                field_name: name.to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column((*col).clone()))),
            });
        }
    }

    // Dropped columns
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            changes.push(FieldChange {
                field_name: name.to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Drop,
            });
        }
    }

    // Altered columns (present in both, but different by PartialEq)
    for (name, old_col) in &old_map {
        if let Some(new_col) = new_map.get(name)
            && old_col != new_col
        {
            changes.push(FieldChange {
                field_name: name.to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column((*old_col).clone())),
                    new: Box::new(FieldDetail::Column((*new_col).clone())),
                },
            });
        }
    }

    changes
}

/// Stable key for a constraint: use name if present, else type+columns.
fn constraint_key(c: &TableConstraint) -> String {
    match c {
        TableConstraint::PrimaryKey { name, columns } => name
            .clone()
            .unwrap_or_else(|| format!("pk:{}", columns.join(","))),
        TableConstraint::Unique { name, columns } => name
            .clone()
            .unwrap_or_else(|| format!("uq:{}", columns.join(","))),
        TableConstraint::ForeignKey(fk) => fk
            .name
            .clone()
            .unwrap_or_else(|| format!("fk:{}", fk.columns.join(","))),
        TableConstraint::Check { name, expression } => name
            .clone()
            .unwrap_or_else(|| format!("ck:{expression}")),
    }
}

/// Diff constraints by stable key: Add / Drop only (changed = Drop + Add).
fn diff_constraints(old: &[TableConstraint], new: &[TableConstraint]) -> Vec<FieldChange> {
    let old_map: HashMap<String, &TableConstraint> =
        old.iter().map(|c| (constraint_key(c), c)).collect();
    let new_map: HashMap<String, &TableConstraint> =
        new.iter().map(|c| (constraint_key(c), c)).collect();

    let mut changes = Vec::new();

    // Added constraints
    for (key, con) in &new_map {
        if !old_map.contains_key(key) {
            changes.push(FieldChange {
                field_name: key.clone(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint((*con).clone()))),
            });
        }
    }

    // Dropped constraints (including changed ones: drop old)
    for (key, old_con) in &old_map {
        match new_map.get(key) {
            None => {
                changes.push(FieldChange {
                    field_name: key.clone(),
                    field_type: FieldType::Constraint,
                    action: ChangeAction::Drop,
                });
            }
            Some(new_con) => {
                if old_con != new_con {
                    // Changed constraint = drop + add
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Constraint,
                        action: ChangeAction::Drop,
                    });
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Constraint,
                        action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                            (*new_con).clone(),
                        ))),
                    });
                }
            }
        }
    }

    changes
}

/// Stable key for an index: use name if present, else columns.
fn index_key(idx: &IndexDef) -> String {
    idx.name.clone().unwrap_or_else(|| {
        let cols: Vec<&str> = idx.columns.iter().map(|c| c.name.as_str()).collect();
        format!("idx:{}", cols.join(","))
    })
}

/// Diff indexes by stable key: Add / Drop only (changed = Drop + Add).
fn diff_indexes(old: &[IndexDef], new: &[IndexDef]) -> Vec<FieldChange> {
    let old_map: HashMap<String, &IndexDef> = old.iter().map(|i| (index_key(i), i)).collect();
    let new_map: HashMap<String, &IndexDef> = new.iter().map(|i| (index_key(i), i)).collect();

    let mut changes = Vec::new();

    // Added indexes
    for (key, idx) in &new_map {
        if !old_map.contains_key(key) {
            changes.push(FieldChange {
                field_name: key.clone(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index((*idx).clone()))),
            });
        }
    }

    // Dropped indexes (including changed ones: drop old)
    for (key, old_idx) in &old_map {
        match new_map.get(key) {
            None => {
                changes.push(FieldChange {
                    field_name: key.clone(),
                    field_type: FieldType::Index,
                    action: ChangeAction::Drop,
                });
            }
            Some(new_idx) => {
                if old_idx != new_idx {
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Index,
                        action: ChangeAction::Drop,
                    });
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Index,
                        action: ChangeAction::Add(Box::new(FieldDetail::Index(
                            (*new_idx).clone(),
                        ))),
                    });
                }
            }
        }
    }

    changes
}

/// Stub: enum diffs not yet implemented.
fn diff_enums() -> Vec<MigrationDiff> {
    Vec::new()
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    use crate::entity::{ColumnDef, IndexColumn, IndexDef, TableConstraint};
    use crate::snapshot::{Snapshot, TableSnapshot};

    // ── Helpers ─────────────────────────────────────────────

    /// Simple nullable column with no default.
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

    /// Non-nullable column.
    fn col_not_null(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            nullable: false,
            ..col(name, data_type)
        }
    }

    /// Column with a default value.
    fn col_with_default(name: &str, data_type: &str, default: &str) -> ColumnDef {
        ColumnDef {
            default_value: Some(default.to_string()),
            ..col(name, data_type)
        }
    }

    /// Build a TableSnapshot with given columns (no indexes/constraints).
    fn table(schema: &str, name: &str, columns: Vec<ColumnDef>) -> TableSnapshot {
        TableSnapshot {
            name: name.to_string(),
            schema: schema.to_string(),
            columns,
            indexes: vec![],
            table_constraints: vec![],
        }
    }

    /// Build a Snapshot from a list of TableSnapshots.
    fn snap(tables: Vec<TableSnapshot>) -> Snapshot {
        Snapshot {
            version: 1,
            description: "test".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables,
        }
    }

    // ── D1: identical snapshots → no diffs ──────────────────

    #[test]
    fn d1_identical_snapshots_produce_no_diff() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical snapshots should produce no diffs");
    }

    // ── D2: new table added ─────────────────────────────────

    #[test]
    fn d2_new_table_detected_as_add() {
        let a = snap(vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.users");
        assert!(matches!(diffs[0].action, DiffAction::Add));
    }

    // ── D3: table dropped ───────────────────────────────────

    #[test]
    fn d3_removed_table_detected_as_drop() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let b = snap(vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.users");
        assert!(matches!(diffs[0].action, DiffAction::Drop));
    }

    // ── D4: column added to existing table ──────────────────

    #[test]
    fn d4_added_column_detected() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let b = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "email");
            assert_eq!(changes[0].field_type, FieldType::Column);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D5: column dropped from existing table ──────────────

    #[test]
    fn d5_dropped_column_detected() {
        let a = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "email");
            assert_eq!(changes[0].field_type, FieldType::Column);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D6: column altered (type changed) ───────────────────

    #[test]
    fn d6_altered_column_detected() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let b = snap(vec![table("public", "users", vec![col("id", "bigint")])]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "id");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D7: constraint added ────────────────────────────────

    #[test]
    fn d7_added_constraint_detected() {
        let t_old = table("public", "users", vec![col("id", "int")]);
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_id".to_string()),
            columns: vec!["id".to_string()],
        });
        let diffs = diff(&snap(vec![t_old]), &snap(vec![t_new]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_type, FieldType::Constraint);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D8: index added ─────────────────────────────────────

    #[test]
    fn d8_added_index_detected() {
        let t_old = table("public", "users", vec![col("id", "int")]);
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });
        let diffs = diff(&snap(vec![t_old]), &snap(vec![t_new]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_type, FieldType::Index);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }
}
