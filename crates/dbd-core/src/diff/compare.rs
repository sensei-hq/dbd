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

/// Diff two keyed entity lists into Add/Drop/Change migration diffs.
///
/// Shared by [`diff_tables`] and [`diff_enums`]: entries only in `new` are
/// Adds, entries only in `old` are Drops, and entries present in both with a
/// non-empty `field_changes` result are Changes.
fn diff_by_qualified_name<T>(
    old: &[T],
    new: &[T],
    entity_type: EntityType,
    qualified_name: impl Fn(&T) -> String,
    field_changes: impl Fn(&T, &T) -> Vec<FieldChange>,
) -> Vec<MigrationDiff> {
    let old_map: HashMap<String, &T> = old.iter().map(|t| (qualified_name(t), t)).collect();
    let new_map: HashMap<String, &T> = new.iter().map(|t| (qualified_name(t), t)).collect();

    let mut diffs = Vec::new();

    // In new but not in old → Add
    for name in new_map.keys() {
        if !old_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.clone(),
                entity_type,
                action: DiffAction::Add,
            });
        }
    }

    // In old but not in new → Drop
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.clone(),
                entity_type,
                action: DiffAction::Drop,
            });
        }
    }

    // In both → check for field-level changes
    for (name, &old_t) in &old_map {
        if let Some(&new_t) = new_map.get(name) {
            let changes = field_changes(old_t, new_t);
            if !changes.is_empty() {
                diffs.push(MigrationDiff {
                    entity_name: name.clone(),
                    entity_type,
                    action: DiffAction::Change(changes),
                });
            }
        }
    }

    diffs
}

/// Diff tables between two snapshots by qualified name.
fn diff_tables(old: &[TableSnapshot], new: &[TableSnapshot]) -> Vec<MigrationDiff> {
    diff_by_qualified_name(old, new, EntityType::Table, qualified_name, diff_table_fields)
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
    for (name, col) in &old_map {
        if !new_map.contains_key(name) {
            changes.push(FieldChange {
                field_name: name.to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Drop(Box::new(FieldDetail::Column((*col).clone()))),
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

/// Stable key for a constraint — a **matching key only**, never a SQL identifier.
///
/// PRIMARY KEY and UNIQUE are keyed by their columns and never by their name, so
/// a live constraint Postgres auto-named (`metrics_pkey`) matches the design's
/// unnamed declaration over the same columns. That is the whole point: reconcile
/// must see them as one constraint, not as a drop plus an add. FK and CHECK reach
/// here already name-stripped (`reconcile::canonicalize`,
/// `schema_diff::normalize_for_diff`), so every constraint kind matches by shape.
///
/// The corollary is that this key is synthetic whenever the object is unnamed, so
/// callers must take the real name from the constraint itself — see
/// [`ChangeAction::Drop`].
fn constraint_key(c: &TableConstraint) -> String {
    match c {
        TableConstraint::PrimaryKey { columns, .. } => format!("pk:{}", columns.join(",")),
        TableConstraint::Unique { columns, .. } => format!("uq:{}", columns.join(",")),
        TableConstraint::ForeignKey(fk) => fk
            .name
            .clone()
            .unwrap_or_else(|| format!("fk:{}", fk.columns.join(","))),
        TableConstraint::Check { name, expression } => name.clone().unwrap_or_else(|| format!("ck:{expression}")),
    }
}

/// Whether two constraints that share a [`constraint_key`] actually differ.
///
/// For PRIMARY KEY and UNIQUE the key *is* the full column list, so a shared key
/// leaves only the name free to differ. Comparing those by `PartialEq` (which
/// includes the name) made every in-sync PK read as changed, churning a
/// destructive drop+add on every run — because the live side is auto-named and the
/// design side usually is not.
///
/// "Usually" is why this is not simply `false`: the DDL parser *does* capture an
/// explicit `constraint <name> primary key (…)`, so when BOTH sides name the
/// constraint, a difference is a deliberate rename and must still be expressed.
/// An unnamed side means "any name will do" and never reads as drift.
fn constraint_differs(old: &TableConstraint, new: &TableConstraint) -> bool {
    let renamed = |old_name: &Option<String>, new_name: &Option<String>| match (old_name, new_name) {
        (Some(o), Some(n)) => o != n,
        _ => false,
    };
    match (old, new) {
        (TableConstraint::PrimaryKey { name: o, .. }, TableConstraint::PrimaryKey { name: n, .. })
        | (TableConstraint::Unique { name: o, .. }, TableConstraint::Unique { name: n, .. }) => renamed(o, n),
        _ => old != new,
    }
}

/// Diff constraints by stable key: Add / Drop only (changed = Drop + Add).
///
/// Every drop is emitted before every add. Replacing a constraint in place is not
/// something Postgres can do, and the two halves are not interchangeable: adding a
/// second PRIMARY KEY before dropping the first fails with "multiple primary keys
/// for table are not allowed". Keys are also walked in sorted order so the SQL for
/// a given diff is byte-identical run to run rather than following `HashMap`'s
/// randomized iteration.
fn diff_constraints(old: &[TableConstraint], new: &[TableConstraint]) -> Vec<FieldChange> {
    let old_map: HashMap<String, &TableConstraint> = old.iter().map(|c| (constraint_key(c), c)).collect();
    let new_map: HashMap<String, &TableConstraint> = new.iter().map(|c| (constraint_key(c), c)).collect();

    let mut changes = Vec::new();
    let mut old_keys: Vec<&String> = old_map.keys().collect();
    old_keys.sort();
    let mut new_keys: Vec<&String> = new_map.keys().collect();
    new_keys.sort();

    // Dropped constraints first (including the drop half of a changed one).
    for key in &old_keys {
        let old_con = old_map[*key];
        let replaced = match new_map.get(*key) {
            None => true,
            Some(new_con) => constraint_differs(old_con, new_con),
        };
        if replaced {
            changes.push(FieldChange {
                field_name: (*key).clone(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop(Box::new(FieldDetail::Constraint(old_con.clone()))),
            });
        }
    }

    // Then adds: constraints new to this table, plus the add half of a changed one.
    for key in &new_keys {
        let new_con = new_map[*key];
        let added = match old_map.get(*key) {
            None => true,
            Some(old_con) => constraint_differs(old_con, new_con),
        };
        if added {
            changes.push(FieldChange {
                field_name: (*key).clone(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(new_con.clone()))),
            });
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
                    action: ChangeAction::Drop(Box::new(FieldDetail::Index((*old_idx).clone()))),
                });
            }
            Some(new_idx) => {
                if old_idx != new_idx {
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Index,
                        action: ChangeAction::Drop(Box::new(FieldDetail::Index((*old_idx).clone()))),
                    });
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Index,
                        action: ChangeAction::Add(Box::new(FieldDetail::Index((*new_idx).clone()))),
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
    diff_by_qualified_name(old, new, EntityType::Enum, enum_qualified_name, diff_enum_values)
}

/// Diff enum values between two versions of the same enum.
fn diff_enum_values(old: &EnumSnapshot, new: &EnumSnapshot) -> Vec<FieldChange> {
    let old_set: std::collections::HashSet<&str> = old.values.iter().map(|v| v.as_str()).collect();
    let new_set: std::collections::HashSet<&str> = new.values.iter().map(|v| v.as_str()).collect();

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
                action: ChangeAction::Drop(Box::new(FieldDetail::EnumValue(val.clone()))),
            });
        }
    }

    changes
}
