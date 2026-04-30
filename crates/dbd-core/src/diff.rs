use serde::{Deserialize, Serialize};

use crate::entity::{ColumnDef, EntityType, IndexDef, TableConstraint};
use crate::snapshot::Snapshot;

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
pub fn diff(_old: &Snapshot, _new: &Snapshot) -> Vec<MigrationDiff> {
    // TODO: implement in Task 4
    Vec::new()
}

#[cfg(test)]
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
