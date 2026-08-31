use crate::entity::EntityType;
use crate::snapshot::Snapshot;

use super::types::*;

// ── Type cast heuristic ────────────────────────────────

/// Normalize a Postgres type string: lowercase, strip precision/length info.
fn normalize_type(t: &str) -> String {
    let lower = t.to_lowercase().trim().to_string();
    // Strip parenthesized precision: VARCHAR(100) → varchar, NUMERIC(10,2) → numeric
    match lower.find('(') {
        Some(pos) => lower[..pos].trim().to_string(),
        None => lower,
    }
}

/// Categorize a normalized Postgres type into a broad category.
fn type_category(normalized: &str) -> &'static str {
    match normalized {
        "int" | "integer" | "int4" | "bigint" | "int8" | "smallint" | "int2" | "serial" | "bigserial"
        | "smallserial" => "integer",
        "numeric" | "decimal" | "real" | "float4" | "double precision" | "float8" | "money" => "numeric",
        "text" | "varchar" | "character varying" | "char" | "character" | "bpchar" | "name" => "text",
        "boolean" | "bool" => "boolean",
        "timestamp" | "timestamptz" | "timestamp with time zone" | "timestamp without time zone" => "timestamp",
        "date" => "date",
        "time" | "timetz" | "time with time zone" | "time without time zone" => "time",
        "json" | "jsonb" => "json",
        "uuid" => "uuid",
        "bytea" => "bytea",
        _ => "other",
    }
}

/// Determine if a Postgres `::` cast from one type to another is safe for
/// auto-generating data.sql. This is a heuristic — it errs on the side of
/// caution for types that would lose data.
pub fn is_castable(from: &str, to: &str) -> bool {
    let from_norm = normalize_type(from);
    let to_norm = normalize_type(to);

    // Arrays are never auto-castable
    if from.contains("[]") || to.contains("[]") {
        return false;
    }

    let from_cat = type_category(&from_norm);
    let to_cat = type_category(&to_norm);

    // JSONB/JSON → scalar is never castable
    if from_cat == "json" && to_cat != "json" {
        return false;
    }

    // Anything → TEXT/VARCHAR is castable
    if to_cat == "text" {
        return true;
    }

    // Same category = castable (INT → BIGINT, TEXT → TEXT, etc.)
    if from_cat == to_cat {
        return true;
    }

    // BOOLEAN → INTEGER is castable
    if from_cat == "boolean" && to_cat == "integer" {
        return true;
    }

    // TIMESTAMP → TEXT already handled above; no other cross-category casts
    false
}

// ── Complex change classification ──────────────────────

/// Find columns in the snapshot whose data_type matches the given enum name.
/// Checks both qualified (public.status_type) and unqualified (status_type) matches.
fn find_affected_columns(enum_name: &str, snapshot: &Snapshot) -> Vec<(String, String)> {
    let mut affected = Vec::new();
    // Extract the unqualified enum name: "public.status_type" → "status_type"
    let unqualified = match enum_name.split_once('.') {
        Some((_, name)) => name,
        None => enum_name,
    };

    for table in &snapshot.tables {
        let qualified_table = format!("{}.{}", table.schema, table.name);
        for col in &table.columns {
            let col_type_lower = col.data_type.to_lowercase();
            let enum_lower = enum_name.to_lowercase();
            let unqualified_lower = unqualified.to_lowercase();
            if col_type_lower == enum_lower || col_type_lower == unqualified_lower {
                affected.push((qualified_table.clone(), col.name.clone()));
            }
        }
    }

    affected
}

/// Separate a list of diffs into simple and complex changes.
///
/// Returns `(simple_diffs, complex_changes)`:
/// - Simple diffs can be applied with regular DDL.
/// - Complex changes need data correction scripts.
pub fn classify_changes(diffs: &[MigrationDiff], old_snapshot: &Snapshot) -> (Vec<MigrationDiff>, Vec<ComplexChange>) {
    let mut simple_diffs = Vec::new();
    let mut complex_changes = Vec::new();

    for d in diffs {
        match &d.action {
            DiffAction::Add | DiffAction::Drop => {
                simple_diffs.push(d.clone());
            }
            DiffAction::Change(changes) => {
                if d.entity_type == EntityType::Table {
                    classify_table_changes(d, changes, old_snapshot, &mut simple_diffs, &mut complex_changes);
                } else if d.entity_type == EntityType::Enum {
                    classify_enum_changes(d, changes, old_snapshot, &mut simple_diffs, &mut complex_changes);
                } else {
                    simple_diffs.push(d.clone());
                }
            }
        }
    }

    (simple_diffs, complex_changes)
}

/// Classify field changes within a table diff.
fn classify_table_changes(
    diff: &MigrationDiff,
    changes: &[FieldChange],
    old_snapshot: &Snapshot,
    simple_diffs: &mut Vec<MigrationDiff>,
    complex_changes: &mut Vec<ComplexChange>,
) {
    // First pass: column type changes (Alter where data_type changed).
    let type_change_indices = detect_type_changes(diff, changes, complex_changes);

    // Collect drops/adds (excluding type changes) for rename detection.
    let mut column_drops: Vec<(usize, &FieldChange)> = Vec::new();
    let mut column_adds: Vec<(usize, &FieldChange)> = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        if type_change_indices.contains(&i) {
            continue;
        }
        if change.field_type == FieldType::Column {
            match &change.action {
                ChangeAction::Drop(_) => column_drops.push((i, change)),
                ChangeAction::Add(_) => column_adds.push((i, change)),
                _ => {}
            }
        }
    }

    // Detect a column rename: exactly 1 drop + 1 add with the same data_type.
    let rename_indices = detect_column_rename(diff, &column_drops, &column_adds, old_snapshot, complex_changes);

    // Everything not consumed above is a simple change.
    let remaining_changes: Vec<FieldChange> = changes
        .iter()
        .enumerate()
        .filter(|(i, _)| !type_change_indices.contains(i) && !rename_indices.contains(i))
        .map(|(_, change)| change.clone())
        .collect();

    if !remaining_changes.is_empty() {
        simple_diffs.push(MigrationDiff {
            entity_name: diff.entity_name.clone(),
            entity_type: diff.entity_type,
            action: DiffAction::Change(remaining_changes),
        });
    }
}

/// Record each column type change as a `ComplexChange::ColumnTypeChange`,
/// returning the indices of the changes it consumed.
fn detect_type_changes(
    diff: &MigrationDiff,
    changes: &[FieldChange],
    complex_changes: &mut Vec<ComplexChange>,
) -> std::collections::HashSet<usize> {
    let mut indices = std::collections::HashSet::new();
    for (i, change) in changes.iter().enumerate() {
        if change.field_type == FieldType::Column
            && let ChangeAction::Alter { ref old, ref new } = change.action
            && let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) = (old.as_ref(), new.as_ref())
            && old_col.data_type != new_col.data_type
        {
            complex_changes.push(ComplexChange::ColumnTypeChange {
                table_name: diff.entity_name.clone(),
                column_name: change.field_name.clone(),
                old_type: old_col.data_type.clone(),
                new_type: new_col.data_type.clone(),
                old_col: Box::new(old_col.clone()),
                new_col: Box::new(new_col.clone()),
            });
            indices.insert(i);
        }
    }
    indices
}

/// Detect a 1-drop + 1-add column rename (same type). On a match, pushes a
/// `ComplexChange::ColumnRename` and returns the consumed change indices.
fn detect_column_rename(
    diff: &MigrationDiff,
    column_drops: &[(usize, &FieldChange)],
    column_adds: &[(usize, &FieldChange)],
    old_snapshot: &Snapshot,
    complex_changes: &mut Vec<ComplexChange>,
) -> std::collections::HashSet<usize> {
    let mut indices = std::collections::HashSet::new();
    if column_drops.len() != 1 || column_adds.len() != 1 {
        return indices;
    }
    let (drop_idx, drop_change) = column_drops[0];
    let (add_idx, add_change) = column_adds[0];

    let old_col_type = find_column_type_in_snapshot(&diff.entity_name, &drop_change.field_name, old_snapshot);
    let added_col = added_column_def(add_change);
    let new_col_type = added_col.map(|cd| cd.data_type.clone());

    if let (Some(old_type), Some(new_type)) = (old_col_type, new_col_type)
        && old_type == new_type
        && let Some(col_def) = added_col
    {
        complex_changes.push(ComplexChange::ColumnRename {
            table_name: diff.entity_name.clone(),
            old_name: drop_change.field_name.clone(),
            new_name: add_change.field_name.clone(),
            col_def: Box::new(col_def.clone()),
        });
        indices.insert(drop_idx);
        indices.insert(add_idx);
    }
    indices
}

/// The `ColumnDef` carried by an `Add` change, when it is a column add.
fn added_column_def(change: &FieldChange) -> Option<&crate::entity::ColumnDef> {
    if let ChangeAction::Add(ref detail) = change.action
        && let FieldDetail::Column(ref col_def) = **detail
    {
        Some(col_def)
    } else {
        None
    }
}

/// Find a column's data_type in the old snapshot by table name and column name.
fn find_column_type_in_snapshot(table_name: &str, column_name: &str, snapshot: &Snapshot) -> Option<String> {
    for table in &snapshot.tables {
        let qualified = format!("{}.{}", table.schema, table.name);
        if qualified == table_name {
            for col in &table.columns {
                if col.name == column_name {
                    return Some(col.data_type.clone());
                }
            }
        }
    }
    None
}

/// Classify field changes within an enum diff.
fn classify_enum_changes(
    diff: &MigrationDiff,
    changes: &[FieldChange],
    old_snapshot: &Snapshot,
    simple_diffs: &mut Vec<MigrationDiff>,
    complex_changes: &mut Vec<ComplexChange>,
) {
    let enum_drops: Vec<&FieldChange> = changes
        .iter()
        .filter(|c| c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Drop(_)))
        .collect();
    let enum_adds: Vec<&FieldChange> = changes
        .iter()
        .filter(|c| c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Add(_)))
        .collect();

    // 1:1 swap (rename) → stays simple (PG17+ ALTER TYPE RENAME VALUE)
    if enum_drops.len() == 1 && enum_adds.len() == 1 {
        simple_diffs.push(diff.clone());
        return;
    }

    // Enum value removal: drops without matching adds
    let added_names: std::collections::HashSet<&str> = enum_adds.iter().map(|c| c.field_name.as_str()).collect();
    let removed: Vec<String> = enum_drops
        .iter()
        .filter(|c| !added_names.contains(c.field_name.as_str()))
        .map(|c| c.field_name.clone())
        .collect();

    if !removed.is_empty() {
        // Find remaining values from old_snapshot
        let old_enum = old_snapshot
            .enums
            .iter()
            .find(|e| format!("{}.{}", e.schema, e.name) == diff.entity_name);
        let remaining_values = if let Some(old_e) = old_enum {
            old_e.values.iter().filter(|v| !removed.contains(v)).cloned().collect()
        } else {
            Vec::new()
        };

        let affected_columns = find_affected_columns(&diff.entity_name, old_snapshot);

        complex_changes.push(ComplexChange::EnumValueRemoval {
            enum_name: diff.entity_name.clone(),
            removed_values: removed,
            remaining_values,
            affected_columns,
        });

        // Keep any non-removal changes as simple
        let simple_changes: Vec<FieldChange> = changes
            .iter()
            .filter(|c| !(c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Drop(_))))
            .cloned()
            .collect();

        if !simple_changes.is_empty() {
            simple_diffs.push(MigrationDiff {
                entity_name: diff.entity_name.clone(),
                entity_type: diff.entity_type,
                action: DiffAction::Change(simple_changes),
            });
        }
    } else {
        // No removals — everything is simple
        simple_diffs.push(diff.clone());
    }
}

// ── Data SQL generation ────────────────────────────────

/// Generate a data correction SQL script for a complex change.
pub fn generate_data_sql(change: &ComplexChange) -> String {
    match change {
        ComplexChange::ColumnRename {
            table_name,
            old_name,
            new_name,
            ..
        } => {
            format!("UPDATE {table_name} SET {new_name} = {old_name};\n")
        }
        ComplexChange::ColumnTypeChange {
            table_name,
            old_type,
            new_type,
            old_col,
            new_col,
            ..
        } => {
            if is_castable(old_type, new_type) {
                let mut sql = String::new();
                // Truncation warning for TEXT → VARCHAR
                let to_norm = normalize_type(new_type);
                if (to_norm == "varchar" || to_norm == "character varying" || to_norm == "char")
                    && type_category(&normalize_type(old_type)) == "text"
                {
                    sql.push_str(&format!(
                        "-- WARNING: Casting {} to {} may truncate data. Review row lengths before applying.\n",
                        old_type, new_type
                    ));
                }
                sql.push_str(&format!(
                    "UPDATE {table_name} SET {} = {}::{};\n",
                    new_col.name, old_col.name, new_type
                ));
                sql
            } else {
                format!(
                    "-- TODO: Data correction required for {table_name}.{}.\n\
                     -- Column type changed from {old_type} to {new_type}.\n\
                     -- No safe automatic cast is available. Manual data migration needed.\n",
                    new_col.name
                )
            }
        }
        ComplexChange::EnumValueRemoval {
            enum_name,
            removed_values,
            remaining_values,
            affected_columns,
        } => {
            let mut sql = String::new();
            sql.push_str(&format!(
                "-- TODO: Map removed enum values to remaining values for {enum_name}.\n"
            ));
            sql.push_str(&format!("-- Removed: {}\n", removed_values.join(", ")));
            sql.push_str(&format!("-- Remaining: {}\n", remaining_values.join(", ")));

            for (table, col) in affected_columns {
                for removed_val in removed_values {
                    sql.push_str(&format!(
                        "UPDATE {table} SET {col} = '???' WHERE {col} = '{removed_val}';\n"
                    ));
                }
            }

            sql
        }
    }
}
