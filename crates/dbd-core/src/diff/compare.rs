use std::collections::HashMap;

use crate::entity::{ColumnDef, EntityType, IndexDef, TableConstraint};
use crate::snapshot::{EnumSnapshot, Snapshot, TableSnapshot};

use super::types::*;

/// Compare two snapshots and return a list of migration diffs.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Vec<MigrationDiff> {
    let mut diffs = diff_tables(&old.tables, &new.tables);
    diffs.extend(diff_enums(&old.enums, &new.enums));
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

/// Qualified name for an enum: "schema.name".
fn enum_qualified_name(e: &EnumSnapshot) -> String {
    format!("{}.{}", e.schema, e.name)
}

/// Diff enums between two snapshots by qualified name.
fn diff_enums(old: &[EnumSnapshot], new: &[EnumSnapshot]) -> Vec<MigrationDiff> {
    let old_map: HashMap<String, &EnumSnapshot> =
        old.iter().map(|e| (enum_qualified_name(e), e)).collect();
    let new_map: HashMap<String, &EnumSnapshot> =
        new.iter().map(|e| (enum_qualified_name(e), e)).collect();

    let mut diffs = Vec::new();

    // Enums in new but not in old → Add
    for name in new_map.keys() {
        if !old_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.clone(),
                entity_type: EntityType::Enum,
                action: DiffAction::Add,
            });
        }
    }

    // Enums in old but not in new → Drop
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.clone(),
                entity_type: EntityType::Enum,
                action: DiffAction::Drop,
            });
        }
    }

    // Enums in both → check for value-level changes
    for (name, old_e) in &old_map {
        if let Some(new_e) = new_map.get(name) {
            let changes = diff_enum_values(old_e, new_e);
            if !changes.is_empty() {
                diffs.push(MigrationDiff {
                    entity_name: name.clone(),
                    entity_type: EntityType::Enum,
                    action: DiffAction::Change(changes),
                });
            }
        }
    }

    diffs
}

/// Diff enum values between two versions of the same enum.
fn diff_enum_values(old: &EnumSnapshot, new: &EnumSnapshot) -> Vec<FieldChange> {
    let old_set: std::collections::HashSet<&str> =
        old.values.iter().map(|v| v.as_str()).collect();
    let new_set: std::collections::HashSet<&str> =
        new.values.iter().map(|v| v.as_str()).collect();

    let mut changes = Vec::new();

    // Added values
    for val in &new.values {
        if !old_set.contains(val.as_str()) {
            changes.push(FieldChange {
                field_name: val.clone(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Add(Box::new(FieldDetail::EnumValue(val.clone()))),
            });
        }
    }

    // Dropped values
    for val in &old.values {
        if !new_set.contains(val.as_str()) {
            changes.push(FieldChange {
                field_name: val.clone(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            });
        }
    }

    changes
}
