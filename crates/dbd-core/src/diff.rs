use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::entity::{ColumnDef, EntityType, IndexDef, TableConstraint};
use crate::snapshot::{EnumSnapshot, Snapshot, TableSnapshot};

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

/// Analyze diffs for risky changes that may need to be split across two migrations.
///
/// Returns warnings for:
/// - Column type changes (may need data correction)
/// - Possible renames (column dropped + column added with same type)
/// - Enum value drops (data may reference removed values)
pub fn migration_warnings(diffs: &[MigrationDiff]) -> Vec<String> {
    let mut warnings = Vec::new();

    for d in diffs {
        let DiffAction::Change(ref changes) = d.action else {
            if matches!(d.action, DiffAction::Drop) && d.entity_type == EntityType::Enum {
                warnings.push(format!(
                    "Enum '{}' dropped — ensure no columns reference this type before applying",
                    d.entity_name
                ));
            }
            continue;
        };

        // Detect column type changes
        for change in changes {
            if change.field_type == FieldType::Column
                && let ChangeAction::Alter { ref old, ref new } = change.action
                && let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                    (old.as_ref(), new.as_ref())
                && old_col.data_type != new_col.data_type
            {
                warnings.push(format!(
                    "{}.{}: type change {} -> {} — consider splitting across two snapshots \
                     (v(N): add new column + data correction, v(N+1): drop old column)",
                    d.entity_name, change.field_name, old_col.data_type, new_col.data_type
                ));
            }
        }

        // Detect possible renames: column dropped + column added with same type in same table
        let dropped: Vec<&FieldChange> = changes
            .iter()
            .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Drop))
            .collect();
        let added: Vec<&FieldChange> = changes
            .iter()
            .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Add(_)))
            .collect();

        for drop_col in &dropped {
            for add_col in &added {
                if let ChangeAction::Add(ref detail) = add_col.action
                    && matches!(**detail, FieldDetail::Column(_))
                {
                    warnings.push(format!(
                        "{}: column '{}' dropped and '{}' added — if this is a rename, \
                         consider splitting: v(N): add '{}' + UPDATE, v(N+1): drop '{}'",
                        d.entity_name,
                        drop_col.field_name,
                        add_col.field_name,
                        add_col.field_name,
                        drop_col.field_name,
                    ));
                }
            }
        }

        // Detect enum value drops
        for change in changes {
            if change.field_type == FieldType::EnumValue && matches!(change.action, ChangeAction::Drop)
            {
                warnings.push(format!(
                    "{}: enum value '{}' dropped — ensure no rows reference this value",
                    d.entity_name, change.field_name
                ));
            }
        }
    }

    warnings
}

/// Generate PostgreSQL migration SQL from a list of diffs.
pub fn generate_migration_sql(diffs: &[MigrationDiff]) -> String {
    let mut lines: Vec<String> = Vec::new();

    for d in diffs {
        match &d.action {
            DiffAction::Add => {
                // New entities use regular apply — no SQL emitted
            }
            DiffAction::Drop => match d.entity_type {
                EntityType::Table => {
                    lines.push(format!("DROP TABLE {} CASCADE;", d.entity_name));
                }
                EntityType::Enum => {
                    lines.push(format!(
                        "-- WARNING: manual migration required for dropped enum {}",
                        d.entity_name
                    ));
                }
                _ => {}
            },
            DiffAction::Change(changes) => {
                for change in changes {
                    generate_field_sql(&d.entity_name, &d.entity_type, change, &mut lines);
                }
            }
        }
    }

    lines.join("\n")
}

/// Generate SQL for a single field-level change.
fn generate_field_sql(
    entity_name: &str,
    _entity_type: &EntityType,
    change: &FieldChange,
    lines: &mut Vec<String>,
) {
    match (&change.field_type, &change.action) {
        // ── Column ──────────────────────────────────────
        (FieldType::Column, ChangeAction::Add(detail)) => {
            if let FieldDetail::Column(col) = detail.as_ref() {
                let mut stmt = format!(
                    "ALTER TABLE {} ADD COLUMN {} {}",
                    entity_name, col.name, col.data_type
                );
                if !col.nullable {
                    stmt.push_str(" NOT NULL");
                }
                if let Some(ref default) = col.default_value {
                    stmt.push_str(&format!(" DEFAULT {default}"));
                }
                stmt.push(';');
                lines.push(stmt);
            }
        }
        (FieldType::Column, ChangeAction::Drop) => {
            lines.push(format!(
                "ALTER TABLE {} DROP COLUMN {};",
                entity_name, change.field_name
            ));
        }
        (FieldType::Column, ChangeAction::Alter { old, new }) => {
            if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                (old.as_ref(), new.as_ref())
            {
                if old_col.data_type != new_col.data_type {
                    lines.push(format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {};",
                        entity_name, new_col.name, new_col.data_type
                    ));
                }
                if old_col.nullable != new_col.nullable {
                    if new_col.nullable {
                        lines.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL;",
                            entity_name, new_col.name
                        ));
                    } else {
                        lines.push(format!(
                            "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL;",
                            entity_name, new_col.name
                        ));
                    }
                }
                if old_col.default_value != new_col.default_value {
                    match &new_col.default_value {
                        Some(val) => {
                            lines.push(format!(
                                "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {};",
                                entity_name, new_col.name, val
                            ));
                        }
                        None => {
                            lines.push(format!(
                                "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT;",
                                entity_name, new_col.name
                            ));
                        }
                    }
                }
            }
        }

        // ── Constraint ──────────────────────────────────
        (FieldType::Constraint, ChangeAction::Add(detail)) => {
            if let FieldDetail::Constraint(con) = detail.as_ref() {
                let sql = constraint_add_sql(entity_name, con);
                lines.push(sql);
            }
        }
        (FieldType::Constraint, ChangeAction::Drop) => {
            lines.push(format!(
                "ALTER TABLE {} DROP CONSTRAINT {};",
                entity_name, change.field_name
            ));
        }

        // ── Index ───────────────────────────────────────
        (FieldType::Index, ChangeAction::Add(detail)) => {
            if let FieldDetail::Index(idx) = detail.as_ref() {
                let unique_str = if idx.unique { "UNIQUE " } else { "" };
                let idx_name = idx.name.as_deref().unwrap_or("unnamed");
                let cols: Vec<String> = idx.columns.iter().map(|c| {
                    match c.order {
                        Some(crate::entity::SortOrder::Desc) => format!("{} DESC", c.name),
                        Some(crate::entity::SortOrder::Asc) => format!("{} ASC", c.name),
                        None => c.name.clone(),
                    }
                }).collect();
                lines.push(format!(
                    "CREATE {}INDEX {} ON {} ({});",
                    unique_str,
                    idx_name,
                    entity_name,
                    cols.join(", ")
                ));
            }
        }
        (FieldType::Index, ChangeAction::Drop) => {
            lines.push(format!("DROP INDEX {};", change.field_name));
        }

        // ── EnumValue ───────────────────────────────────
        (FieldType::EnumValue, ChangeAction::Add(detail)) => {
            if let FieldDetail::EnumValue(val) = detail.as_ref() {
                lines.push(format!(
                    "ALTER TYPE {} ADD VALUE '{}';",
                    entity_name, val
                ));
            }
        }
        (FieldType::EnumValue, ChangeAction::Drop) => {
            // No SQL for enum value drop — warning only
        }

        // Catch-all for any unexpected combinations
        _ => {}
    }
}

/// Convert an FkAction to its SQL keyword.
fn fk_action_to_sql(action: &crate::entity::FkAction) -> &'static str {
    use crate::entity::FkAction;
    match action {
        FkAction::Cascade => "CASCADE",
        FkAction::Restrict => "RESTRICT",
        FkAction::SetNull => "SET NULL",
        FkAction::SetDefault => "SET DEFAULT",
        FkAction::NoAction => "NO ACTION",
    }
}

/// Generate ADD CONSTRAINT SQL for a table constraint.
fn constraint_add_sql(entity_name: &str, con: &TableConstraint) -> String {
    match con {
        TableConstraint::PrimaryKey { name, columns } => {
            let con_name = name.as_deref().unwrap_or("unnamed");
            format!(
                "ALTER TABLE {} ADD CONSTRAINT {} PRIMARY KEY ({});",
                entity_name,
                con_name,
                columns.join(", ")
            )
        }
        TableConstraint::Unique { name, columns } => {
            let con_name = name.as_deref().unwrap_or("unnamed");
            format!(
                "ALTER TABLE {} ADD CONSTRAINT {} UNIQUE ({});",
                entity_name,
                con_name,
                columns.join(", ")
            )
        }
        TableConstraint::ForeignKey(fk) => {
            let con_name = fk.name.as_deref().unwrap_or("unnamed");
            let ref_schema = fk
                .ref_schema
                .as_deref()
                .map(|s| format!("{}.", s))
                .unwrap_or_default();
            let mut sql = format!(
                "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}{}({})",
                entity_name,
                con_name,
                fk.columns.join(", "),
                ref_schema,
                fk.ref_table,
                fk.ref_columns.join(", ")
            );
            if let Some(ref action) = fk.on_delete {
                sql.push_str(&format!(" ON DELETE {}", fk_action_to_sql(action)));
            }
            if let Some(ref action) = fk.on_update {
                sql.push_str(&format!(" ON UPDATE {}", fk_action_to_sql(action)));
            }
            sql.push(';');
            sql
        }
        TableConstraint::Check { name, expression } => {
            let con_name = name.as_deref().unwrap_or("unnamed");
            format!(
                "ALTER TABLE {} ADD CONSTRAINT {} CHECK ({});",
                entity_name, con_name, expression
            )
        }
    }
}

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
        "int" | "integer" | "int4" | "bigint" | "int8" | "smallint" | "int2" | "serial"
        | "bigserial" | "smallserial" => "integer",
        "numeric" | "decimal" | "real" | "float4" | "double precision" | "float8" | "money" => {
            "numeric"
        }
        "text" | "varchar" | "character varying" | "char" | "character" | "bpchar" | "name" => {
            "text"
        }
        "boolean" | "bool" => "boolean",
        "timestamp" | "timestamptz" | "timestamp with time zone"
        | "timestamp without time zone" => "timestamp",
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

/// A complex schema change that requires special handling beyond simple DDL.
#[derive(Debug, Clone)]
pub enum ComplexChange {
    /// A column's data type was changed.
    ColumnTypeChange {
        table_name: String,
        column_name: String,
        old_type: String,
        new_type: String,
        old_col: Box<ColumnDef>,
        new_col: Box<ColumnDef>,
    },
    /// A column was likely renamed (drop + add with same type).
    ColumnRename {
        table_name: String,
        old_name: String,
        new_name: String,
        col_def: Box<ColumnDef>,
    },
    /// Enum values were removed (requires data correction).
    EnumValueRemoval {
        enum_name: String,
        removed_values: Vec<String>,
        remaining_values: Vec<String>,
        affected_columns: Vec<(String, String)>,
    },
}

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
        let table_qualified = format!("{}.{}", table.schema, table.name);
        for col in &table.columns {
            let col_type_lower = col.data_type.to_lowercase();
            if col_type_lower == enum_name.to_lowercase()
                || col_type_lower == unqualified.to_lowercase()
            {
                affected.push((table_qualified.clone(), col.name.clone()));
            }
        }
    }

    affected
}

/// Classify migration diffs into simple and complex changes.
///
/// Returns `(simple_diffs, complex_changes)`:
/// - Simple diffs can be applied with regular DDL.
/// - Complex changes need data correction scripts.
pub fn classify_changes(
    diffs: &[MigrationDiff],
    old_snapshot: &Snapshot,
) -> (Vec<MigrationDiff>, Vec<ComplexChange>) {
    let mut simple_diffs = Vec::new();
    let mut complex_changes = Vec::new();

    for d in diffs {
        match &d.action {
            DiffAction::Add | DiffAction::Drop => {
                simple_diffs.push(d.clone());
            }
            DiffAction::Change(changes) => {
                if d.entity_type == EntityType::Table {
                    classify_table_changes(
                        d,
                        changes,
                        old_snapshot,
                        &mut simple_diffs,
                        &mut complex_changes,
                    );
                } else if d.entity_type == EntityType::Enum {
                    classify_enum_changes(
                        d,
                        changes,
                        old_snapshot,
                        &mut simple_diffs,
                        &mut complex_changes,
                    );
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
    let mut remaining_changes: Vec<FieldChange> = Vec::new();

    // First pass: detect column type changes (Alter where data_type changed)
    let mut type_change_indices = std::collections::HashSet::new();
    for (i, change) in changes.iter().enumerate() {
        if change.field_type == FieldType::Column
            && let ChangeAction::Alter { ref old, ref new } = change.action
            && let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                (old.as_ref(), new.as_ref())
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
            type_change_indices.insert(i);
        }
    }

    // Collect drops and adds for rename detection
    let mut column_drops: Vec<(usize, &FieldChange)> = Vec::new();
    let mut column_adds: Vec<(usize, &FieldChange)> = Vec::new();

    for (i, change) in changes.iter().enumerate() {
        if type_change_indices.contains(&i) {
            continue;
        }
        if change.field_type == FieldType::Column {
            match &change.action {
                ChangeAction::Drop => column_drops.push((i, change)),
                ChangeAction::Add(_) => column_adds.push((i, change)),
                _ => {}
            }
        }
    }

    // Detect column renames: exactly 1 drop + 1 add with same data_type
    let mut rename_indices = std::collections::HashSet::new();
    if column_drops.len() == 1 && column_adds.len() == 1 {
        let (drop_idx, drop_change) = column_drops[0];
        let (add_idx, add_change) = column_adds[0];

        // Get the old column's type from old_snapshot
        let old_col_type = find_column_type_in_snapshot(
            &diff.entity_name,
            &drop_change.field_name,
            old_snapshot,
        );

        // Get the new column's type from the Add detail
        let new_col_type = if let ChangeAction::Add(ref detail) = add_change.action {
            if let FieldDetail::Column(ref col_def) = **detail {
                Some(col_def.data_type.clone())
            } else {
                None
            }
        } else {
            None
        };

        if let (Some(old_type), Some(new_type)) = (old_col_type, new_col_type)
            && old_type == new_type
        {
            let col_def = if let ChangeAction::Add(ref detail) = add_change.action {
                if let FieldDetail::Column(ref cd) = **detail {
                    cd.clone()
                } else {
                    unreachable!()
                }
            } else {
                unreachable!()
            };

            complex_changes.push(ComplexChange::ColumnRename {
                table_name: diff.entity_name.clone(),
                old_name: drop_change.field_name.clone(),
                new_name: add_change.field_name.clone(),
                col_def: Box::new(col_def),
            });
            rename_indices.insert(drop_idx);
            rename_indices.insert(add_idx);
        }
    }

    // Collect remaining simple changes
    for (i, change) in changes.iter().enumerate() {
        if !type_change_indices.contains(&i) && !rename_indices.contains(&i) {
            remaining_changes.push(change.clone());
        }
    }

    if !remaining_changes.is_empty() {
        simple_diffs.push(MigrationDiff {
            entity_name: diff.entity_name.clone(),
            entity_type: diff.entity_type,
            action: DiffAction::Change(remaining_changes),
        });
    }
}

/// Find a column's data_type in the old snapshot by table name and column name.
fn find_column_type_in_snapshot(
    table_name: &str,
    column_name: &str,
    snapshot: &Snapshot,
) -> Option<String> {
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
        .filter(|c| c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Drop))
        .collect();
    let enum_adds: Vec<&FieldChange> = changes
        .iter()
        .filter(|c| {
            c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Add(_))
        })
        .collect();

    // 1:1 swap (rename) → stays simple (PG17+ ALTER TYPE RENAME VALUE)
    if enum_drops.len() == 1 && enum_adds.len() == 1 {
        simple_diffs.push(diff.clone());
        return;
    }

    // Enum value removal: drops without matching adds
    let added_names: std::collections::HashSet<&str> =
        enum_adds.iter().map(|c| c.field_name.as_str()).collect();
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
            old_e
                .values
                .iter()
                .filter(|v| !removed.contains(v))
                .cloned()
                .collect()
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
            .filter(|c| {
                !(c.field_type == FieldType::EnumValue
                    && matches!(c.action, ChangeAction::Drop))
            })
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

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    use crate::entity::{ColumnDef, FkAction, ForeignKey, IndexColumn, IndexDef, IndexType, SortOrder, TableConstraint};
    use crate::snapshot::{EnumSnapshot, Snapshot, TableSnapshot};

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

    /// Build a Snapshot from a list of TableSnapshots and EnumSnapshots.
    fn snap(tables: Vec<TableSnapshot>, enums: Vec<EnumSnapshot>) -> Snapshot {
        Snapshot {
            version: 1,
            description: "test".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables,
            enums,
        }
    }

    // ── D1: identical snapshots → no diffs ──────────────────

    #[test]
    fn d1_identical_snapshots_produce_no_diff() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical snapshots should produce no diffs");
    }

    // ── D2: new table added ─────────────────────────────────

    #[test]
    fn d2_new_table_detected_as_add() {
        let a = snap(vec![], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.users");
        assert!(matches!(diffs[0].action, DiffAction::Add));
    }

    // ── D3: table dropped ───────────────────────────────────

    #[test]
    fn d3_removed_table_detected_as_drop() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.users");
        assert!(matches!(diffs[0].action, DiffAction::Drop));
    }

    // ── D4: column added to existing table ──────────────────

    #[test]
    fn d4_added_column_detected() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )], vec![]);
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
        )], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
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
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "bigint")])], vec![]);
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
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
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
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_type, FieldType::Index);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D9: constraint dropped ──────────────────────────────

    #[test]
    fn d9_dropped_constraint_detected() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_id".to_string()),
            columns: vec!["id".to_string()],
        });
        let t_new = table("public", "users", vec![col("id", "int")]);
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "uq_id");
            assert_eq!(changes[0].field_type, FieldType::Constraint);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D10: index dropped ──────────────────────────────────

    #[test]
    fn d10_dropped_index_detected() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });
        let t_new = table("public", "users", vec![col("id", "int")]);
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "idx_id");
            assert_eq!(changes[0].field_type, FieldType::Index);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D11: constraint changed (same name, different definition → Drop + Add) ──

    #[test]
    fn d11_changed_constraint_detected_as_drop_add() {
        let mut t_old = table("public", "users", vec![col("id", "int"), col("email", "text")]);
        t_old.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });
        let mut t_new = table("public", "users", vec![col("id", "int"), col("email", "text")]);
        // Same name, but now covers both columns
        t_new.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string(), "id".to_string()],
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            // Changed constraint = Drop old + Add new
            assert_eq!(changes.len(), 2);
            let drop_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Drop))
                .count();
            let add_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Add(_)))
                .count();
            assert_eq!(drop_count, 1);
            assert_eq!(add_count, 1);
            assert!(changes.iter().all(|c| c.field_name == "uq_email"));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D12: index changed (same name, different type → Drop + Add) ──

    #[test]
    fn d12_changed_index_detected_as_drop_add() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: Some(IndexType::Hash),
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 2);
            let drop_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Drop))
                .count();
            let add_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Add(_)))
                .count();
            assert_eq!(drop_count, 1);
            assert_eq!(add_count, 1);
            assert!(changes.iter().all(|c| c.field_name == "idx_id"));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D13: column nullable changed ────────────────────────

    #[test]
    fn d13_column_nullable_change_detected_as_alter() {
        let a = snap(
            vec![table("public", "users", vec![col_not_null("id", "int")])],
            vec![],
        );
        let b = snap(
            vec![table("public", "users", vec![col("id", "int")])],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "id");
            assert_eq!(changes[0].field_type, FieldType::Column);
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
            if let ChangeAction::Alter { ref old, ref new } = changes[0].action {
                if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                    (old.as_ref(), new.as_ref())
                {
                    assert!(!old_col.nullable);
                    assert!(new_col.nullable);
                } else {
                    panic!("expected Column details");
                }
            }
        } else {
            panic!("expected Change action");
        }
    }

    // ── D14: column default changed ─────────────────────────

    #[test]
    fn d14_column_default_change_detected_as_alter() {
        let a = snap(
            vec![table("public", "users", vec![col("status", "text")])],
            vec![],
        );
        let b = snap(
            vec![table(
                "public",
                "users",
                vec![col_with_default("status", "text", "'active'")],
            )],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "status");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
            if let ChangeAction::Alter { ref old, ref new } = changes[0].action {
                if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                    (old.as_ref(), new.as_ref())
                {
                    assert!(old_col.default_value.is_none());
                    assert_eq!(new_col.default_value.as_deref(), Some("'active'"));
                } else {
                    panic!("expected Column details");
                }
            }
        } else {
            panic!("expected Change action");
        }
    }

    // ── D15: multiple changes on same table ─────────────────

    #[test]
    fn d15_multiple_changes_on_same_table() {
        let mut t_old = table("public", "users", vec![col("id", "int"), col("email", "text")]);
        t_old.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });

        let mut t_new = table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text"), col("name", "text")],
        );
        // constraint dropped (not added back), index added
        t_new.indexes.push(IndexDef {
            name: Some("idx_name".to_string()),
            columns: vec![IndexColumn {
                name: "name".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });

        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            // add col "name" + drop constraint "uq_email" + add index "idx_name"
            assert_eq!(changes.len(), 3);
            let col_add = changes
                .iter()
                .find(|c| c.field_type == FieldType::Column && c.field_name == "name")
                .expect("should have column add");
            assert!(matches!(col_add.action, ChangeAction::Add(_)));
            let con_drop = changes
                .iter()
                .find(|c| c.field_type == FieldType::Constraint && c.field_name == "uq_email")
                .expect("should have constraint drop");
            assert!(matches!(con_drop.action, ChangeAction::Drop));
            let idx_add = changes
                .iter()
                .find(|c| c.field_type == FieldType::Index && c.field_name == "idx_name")
                .expect("should have index add");
            assert!(matches!(idx_add.action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D16: multiple tables changed simultaneously ─────────

    #[test]
    fn d16_multiple_tables_changed() {
        let a = snap(
            vec![
                table("public", "users", vec![col("id", "int")]),
                table("public", "orders", vec![col("id", "int")]),
            ],
            vec![],
        );
        let b = snap(
            vec![
                table(
                    "public",
                    "users",
                    vec![col("id", "int"), col("email", "text")],
                ),
                table(
                    "public",
                    "orders",
                    vec![col("id", "int"), col("total", "numeric")],
                ),
            ],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 2);
        let names: Vec<&str> = diffs.iter().map(|d| d.entity_name.as_str()).collect();
        assert!(names.contains(&"public.users"));
        assert!(names.contains(&"public.orders"));
        for d in &diffs {
            assert!(matches!(d.action, DiffAction::Change(_)));
        }
    }

    // ── D17: mixed add/alter/drop across entities ───────────

    #[test]
    fn d17_mixed_add_alter_drop_across_entities() {
        let a = snap(
            vec![
                table("public", "users", vec![col("id", "int")]),
                table("public", "legacy", vec![col("id", "int")]),
            ],
            vec![],
        );
        let b = snap(
            vec![
                // users modified (column added)
                table(
                    "public",
                    "users",
                    vec![col("id", "int"), col("name", "text")],
                ),
                // legacy dropped (absent in new)
                // orders added (new table)
                table("public", "orders", vec![col("id", "int")]),
            ],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 3);

        let added = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Add))
            .expect("should have an Add");
        assert_eq!(added.entity_name, "public.orders");

        let dropped = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Drop))
            .expect("should have a Drop");
        assert_eq!(dropped.entity_name, "public.legacy");

        let changed = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Change(_)))
            .expect("should have a Change");
        assert_eq!(changed.entity_name, "public.users");
    }

    // ── D18: unnamed constraint matching ────────────────────

    #[test]
    fn d18_unnamed_pk_with_same_columns_no_diff() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.table_constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["id".to_string()],
        });
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.table_constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["id".to_string()],
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert!(diffs.is_empty(), "identical unnamed PKs should produce no diff");
    }

    // ── D19: enum value added ───────────────────────────────

    #[test]
    fn d19_enum_value_added() {
        let old_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec!["active".to_string(), "inactive".to_string()],
        };
        let new_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec![
                "active".to_string(),
                "inactive".to_string(),
                "pending".to_string(),
            ],
        };
        let diffs = diff(
            &snap(vec![], vec![old_enum]),
            &snap(vec![], vec![new_enum]),
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.status");
        assert_eq!(diffs[0].entity_type, EntityType::Enum);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "pending");
            assert_eq!(changes[0].field_type, FieldType::EnumValue);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D20: enum value dropped ─────────────────────────────

    #[test]
    fn d20_enum_value_dropped() {
        let old_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec!["active".to_string(), "inactive".to_string()],
        };
        let new_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec!["active".to_string()],
        };
        let diffs = diff(
            &snap(vec![], vec![old_enum]),
            &snap(vec![], vec![new_enum]),
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.status");
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "inactive");
            assert_eq!(changes[0].field_type, FieldType::EnumValue);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D21: new enum added / enum dropped ──────────────────

    #[test]
    fn d21_enum_added_and_dropped() {
        let old_enum = EnumSnapshot {
            name: "old_type".to_string(),
            schema: "public".to_string(),
            values: vec!["a".to_string()],
        };
        let new_enum = EnumSnapshot {
            name: "new_type".to_string(),
            schema: "public".to_string(),
            values: vec!["x".to_string()],
        };
        let diffs = diff(
            &snap(vec![], vec![old_enum]),
            &snap(vec![], vec![new_enum]),
        );
        assert_eq!(diffs.len(), 2);

        let added = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Add))
            .expect("should have an Add");
        assert_eq!(added.entity_name, "public.new_type");
        assert_eq!(added.entity_type, EntityType::Enum);

        let dropped = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Drop))
            .expect("should have a Drop");
        assert_eq!(dropped.entity_name, "public.old_type");
        assert_eq!(dropped.entity_type, EntityType::Enum);
    }

    // ════════════════════════════════════════════════════════
    // SQL Generation Tests (S1-S14)
    // ════════════════════════════════════════════════════════

    // ── S1: Add action produces no SQL ──────────────────────

    #[test]
    fn s1_add_action_produces_no_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Add,
        }];
        let sql = generate_migration_sql(&diffs);
        assert!(sql.is_empty(), "Add action should produce no SQL");
    }

    // ── S2: Drop table produces DROP TABLE CASCADE ──────────

    #[test]
    fn s2_drop_table_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Drop,
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "DROP TABLE public.users CASCADE;");
    }

    // ── S3: Drop enum produces warning comment ──────────────

    #[test]
    fn s3_drop_enum_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Drop,
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "-- WARNING: manual migration required for dropped enum public.status"
        );
    }

    // ── S4: Column add SQL ──────────────────────────────────

    #[test]
    fn s4_column_add_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "text")))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "ALTER TABLE public.users ADD COLUMN email text;");
    }

    // ── S5: Column add with NOT NULL and DEFAULT ────────────

    #[test]
    fn s5_column_add_not_null_with_default_sql() {
        let c = ColumnDef {
            nullable: false,
            default_value: Some("'active'".to_string()),
            ..col("status", "text")
        };
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column(c))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD COLUMN status text NOT NULL DEFAULT 'active';"
        );
    }

    // ── S6: Column drop SQL ─────────────────────────────────

    #[test]
    fn s6_column_drop_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "ALTER TABLE public.users DROP COLUMN email;");
    }

    // ── S7: Column alter type SQL ───────────────────────────

    #[test]
    fn s7_column_alter_type_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "id".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("id", "int"))),
                    new: Box::new(FieldDetail::Column(col("id", "bigint"))),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ALTER COLUMN id TYPE bigint;"
        );
    }

    // ── S8: Column alter nullable SQL ───────────────────────

    #[test]
    fn s8_column_alter_nullable_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "name".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col_not_null("name", "text"))),
                    new: Box::new(FieldDetail::Column(col("name", "text"))),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ALTER COLUMN name DROP NOT NULL;"
        );
    }

    // ── S9: Column alter default SQL ────────────────────────

    #[test]
    fn s9_column_alter_default_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("status", "text"))),
                    new: Box::new(FieldDetail::Column(col_with_default(
                        "status", "text", "'active'",
                    ))),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ALTER COLUMN status SET DEFAULT 'active';"
        );
    }

    // ── S10: Constraint add SQL (PK, Unique, FK, Check) ─────

    #[test]
    fn s10_constraint_add_sql() {
        // PK
        let pk_diff = MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "pk_users".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::PrimaryKey {
                        name: Some("pk_users".to_string()),
                        columns: vec!["id".to_string()],
                    },
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[pk_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD CONSTRAINT pk_users PRIMARY KEY (id);"
        );

        // Unique
        let uq_diff = MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "uq_email".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::Unique {
                        name: Some("uq_email".to_string()),
                        columns: vec!["email".to_string()],
                    },
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[uq_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD CONSTRAINT uq_email UNIQUE (email);"
        );

        // FK
        let fk_diff = MigrationDiff {
            entity_name: "public.orders".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "fk_user".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::ForeignKey(ForeignKey {
                        name: Some("fk_user".to_string()),
                        columns: vec!["user_id".to_string()],
                        ref_schema: Some("public".to_string()),
                        ref_table: "users".to_string(),
                        ref_columns: vec!["id".to_string()],
                        on_delete: None,
                        on_update: None,
                    }),
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[fk_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES public.users(id);"
        );

        // Check
        let ck_diff = MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "ck_age".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::Check {
                        name: Some("ck_age".to_string()),
                        expression: "age > 0".to_string(),
                    },
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[ck_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD CONSTRAINT ck_age CHECK (age > 0);"
        );
    }

    // ── S11: Constraint drop SQL ────────────────────────────

    #[test]
    fn s11_constraint_drop_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "uq_email".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "ALTER TABLE public.users DROP CONSTRAINT uq_email;");
    }

    // ── S12: Index add SQL ──────────────────────────────────

    #[test]
    fn s12_index_add_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                    name: Some("idx_email".to_string()),
                    columns: vec![IndexColumn {
                        name: "email".to_string(),
                        order: None,
                    }],
                    unique: false,
                    index_type: None,
                }))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "CREATE INDEX idx_email ON public.users (email);");

        // Unique index
        let diffs_unique = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email_unique".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                    name: Some("idx_email_unique".to_string()),
                    columns: vec![IndexColumn {
                        name: "email".to_string(),
                        order: None,
                    }],
                    unique: true,
                    index_type: None,
                }))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs_unique);
        assert_eq!(
            sql,
            "CREATE UNIQUE INDEX idx_email_unique ON public.users (email);"
        );
    }

    // ── S13: Index drop SQL ─────────────────────────────────

    #[test]
    fn s13_index_drop_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "DROP INDEX idx_email;");
    }

    // ── S14: Enum value add / drop SQL ──────────────────────

    #[test]
    fn s14_enum_value_add_and_drop_sql() {
        // Add value
        let diffs_add = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "pending".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Add(Box::new(FieldDetail::EnumValue(
                    "pending".to_string(),
                ))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs_add);
        assert_eq!(sql, "ALTER TYPE public.status ADD VALUE 'pending';");

        // Drop value — should produce no SQL
        let diffs_drop = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "inactive".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs_drop);
        assert!(sql.is_empty(), "enum value drop should produce no SQL");
    }

    // ════════════════════════════════════════════════════════
    // Scenario Tests: Column property change edge cases
    // ════════════════════════════════════════════════════════

    // M1.1: inline FK change detected
    #[test]
    fn d_column_inline_fk_changed() {
        let old_col = ColumnDef {
            inline_fk: Some(ForeignKey {
                name: Some("fk_a".to_string()),
                columns: vec!["user_id".to_string()],
                ref_schema: None,
                ref_table: "table_a".to_string(),
                ref_columns: vec!["id".to_string()],
                on_delete: None,
                on_update: None,
            }),
            ..col("user_id", "int")
        };
        let new_col = ColumnDef {
            inline_fk: Some(ForeignKey {
                name: Some("fk_a".to_string()),
                columns: vec!["user_id".to_string()],
                ref_schema: None,
                ref_table: "table_b".to_string(),
                ref_columns: vec!["id".to_string()],
                on_delete: None,
                on_update: None,
            }),
            ..col("user_id", "int")
        };
        let a = snap(vec![table("public", "orders", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "orders", vec![new_col])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "user_id");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // M1.2: is_pk changed
    #[test]
    fn d_column_is_pk_changed() {
        let old_col = ColumnDef {
            is_pk: false,
            ..col("id", "int")
        };
        let new_col = ColumnDef {
            is_pk: true,
            ..col("id", "int")
        };
        let a = snap(vec![table("public", "users", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "users", vec![new_col])], vec![]);
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

    // M1.3: is_identity changed
    #[test]
    fn d_column_is_identity_changed() {
        let old_col = ColumnDef {
            is_identity: false,
            ..col("id", "int")
        };
        let new_col = ColumnDef {
            is_identity: true,
            ..col("id", "int")
        };
        let a = snap(vec![table("public", "users", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "users", vec![new_col])], vec![]);
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

    // M1.4: comment changed
    #[test]
    fn d_column_comment_changed() {
        let old_col = ColumnDef {
            comment: None,
            ..col("email", "text")
        };
        let new_col = ColumnDef {
            comment: Some("user email".to_string()),
            ..col("email", "text")
        };
        let a = snap(vec![table("public", "users", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "users", vec![new_col])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "email");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // M1.7: empty table (zero columns)
    #[test]
    fn d_empty_table_no_diff() {
        let a = snap(vec![table("public", "empty", vec![])], vec![]);
        let b = snap(vec![table("public", "empty", vec![])], vec![]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical empty tables should produce no diff");
    }

    // M1.8: enum with zero values
    #[test]
    fn d_enum_zero_values_no_diff() {
        let e1 = EnumSnapshot {
            name: "empty_enum".to_string(),
            schema: "public".to_string(),
            values: vec![],
        };
        let e2 = EnumSnapshot {
            name: "empty_enum".to_string(),
            schema: "public".to_string(),
            values: vec![],
        };
        let a = snap(vec![], vec![e1]);
        let b = snap(vec![], vec![e2]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical empty enums should produce no diff");
    }

    // ════════════════════════════════════════════════════════
    // Scenario Tests: SQL generation edge cases
    // ════════════════════════════════════════════════════════

    // M2.1: Column alter with type + nullable + default all changed
    #[test]
    fn s_column_alter_multiple_changes_at_once() {
        let old_col = ColumnDef {
            nullable: false,
            default_value: Some("'x'".to_string()),
            ..col("status", "VARCHAR(50)")
        };
        let new_col = ColumnDef {
            nullable: true,
            default_value: None,
            ..col("status", "TEXT")
        };
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(old_col)),
                    new: Box::new(FieldDetail::Column(new_col)),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert!(sql.contains("ALTER TABLE public.users ALTER COLUMN status TYPE TEXT;"));
        assert!(sql.contains("ALTER TABLE public.users ALTER COLUMN status DROP NOT NULL;"));
        assert!(sql.contains("ALTER TABLE public.users ALTER COLUMN status DROP DEFAULT;"));
        // Should have exactly 3 ALTER statements
        let line_count = sql.lines().count();
        assert_eq!(line_count, 3, "expected 3 ALTER statements, got {}", line_count);
    }

    // M2.2: FK with on_delete and on_update
    #[test]
    fn s_fk_constraint_with_on_delete_on_update() {
        let fk_diff = MigrationDiff {
            entity_name: "public.orders".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "fk_user".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::ForeignKey(ForeignKey {
                        name: Some("fk_user".to_string()),
                        columns: vec!["user_id".to_string()],
                        ref_schema: Some("public".to_string()),
                        ref_table: "users".to_string(),
                        ref_columns: vec!["id".to_string()],
                        on_delete: Some(FkAction::Cascade),
                        on_update: Some(FkAction::Restrict),
                    }),
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[fk_diff]);
        assert!(
            sql.contains("ON DELETE CASCADE"),
            "SQL should include ON DELETE CASCADE, got: {sql}"
        );
        assert!(
            sql.contains("ON UPDATE RESTRICT"),
            "SQL should include ON UPDATE RESTRICT, got: {sql}"
        );
        assert_eq!(
            sql,
            "ALTER TABLE public.orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE ON UPDATE RESTRICT;"
        );
    }

    // M2.6: Index with ASC/DESC
    #[test]
    fn s_index_with_column_ordering() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email_name".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                    name: Some("idx_email_name".to_string()),
                    columns: vec![
                        IndexColumn {
                            name: "email".to_string(),
                            order: Some(SortOrder::Desc),
                        },
                        IndexColumn {
                            name: "name".to_string(),
                            order: Some(SortOrder::Asc),
                        },
                    ],
                    unique: false,
                    index_type: None,
                }))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "CREATE INDEX idx_email_name ON public.users (email DESC, name ASC);"
        );
    }

    // M2.8: Multiple enum values added
    #[test]
    fn s_enum_multiple_values_added() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "pending".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("pending".to_string()))),
                },
                FieldChange {
                    field_name: "archived".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("archived".to_string()))),
                },
                FieldChange {
                    field_name: "deleted".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("deleted".to_string()))),
                },
            ]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert!(sql.contains("ALTER TYPE public.status ADD VALUE 'pending';"));
        assert!(sql.contains("ALTER TYPE public.status ADD VALUE 'archived';"));
        assert!(sql.contains("ALTER TYPE public.status ADD VALUE 'deleted';"));
        let line_count = sql.lines().count();
        assert_eq!(line_count, 3, "expected 3 ALTER TYPE statements, got {}", line_count);
    }

    // ════════════════════════════════════════════════════════
    // Migration Warnings Tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn warn_column_type_change() {
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("email", "VARCHAR(100)"))),
                    new: Box::new(FieldDetail::Column(col("email", "TEXT"))),
                },
            }]),
        }];
        let warnings = migration_warnings(&diffs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("type change"));
        assert!(warnings[0].contains("VARCHAR(100)"));
        assert!(warnings[0].contains("TEXT"));
        assert!(warnings[0].contains("splitting"));
    }

    #[test]
    fn warn_possible_rename_drop_plus_add() {
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "display_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("display_name", "TEXT")))),
                },
            ]),
        }];
        let warnings = migration_warnings(&diffs);
        assert!(warnings.iter().any(|w| w.contains("'name' dropped") && w.contains("'display_name' added")));
    }

    #[test]
    fn warn_enum_value_dropped() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];
        let warnings = migration_warnings(&diffs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("enum value 'deleted' dropped"));
    }

    #[test]
    fn warn_enum_type_dropped() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Drop,
        }];
        let warnings = migration_warnings(&diffs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Enum 'public.status' dropped"));
    }

    #[test]
    fn no_warnings_for_simple_column_add() {
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "TEXT")))),
            }]),
        }];
        let warnings = migration_warnings(&diffs);
        assert!(warnings.is_empty(), "simple column add should not produce warnings");
    }

    // ════════════════════════════════════════════════════════
    // Task 1: is_castable() tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn ca1_integer_to_text_castable() {
        assert!(is_castable("INTEGER", "TEXT"));
        assert!(is_castable("BIGINT", "TEXT"));
        assert!(is_castable("SMALLINT", "TEXT"));
    }

    #[test]
    fn ca2_varchar_to_text_castable() {
        assert!(is_castable("VARCHAR(100)", "TEXT"));
    }

    #[test]
    fn ca3_text_to_varchar_castable() {
        assert!(is_castable("TEXT", "VARCHAR(50)"));
    }

    #[test]
    fn ca4_jsonb_to_integer_not_castable() {
        assert!(!is_castable("JSONB", "INTEGER"));
        assert!(!is_castable("JSON", "INTEGER"));
    }

    #[test]
    fn ca5_array_to_scalar_not_castable() {
        assert!(!is_castable("TEXT[]", "TEXT"));
    }

    #[test]
    fn ca_numeric_to_text_castable() {
        assert!(is_castable("NUMERIC", "TEXT"));
        assert!(is_castable("DECIMAL", "TEXT"));
    }

    #[test]
    fn ca_boolean_castable() {
        assert!(is_castable("BOOLEAN", "TEXT"));
        assert!(is_castable("BOOLEAN", "INTEGER"));
    }

    #[test]
    fn ca_timestamp_to_text_castable() {
        assert!(is_castable("TIMESTAMP", "TEXT"));
        assert!(is_castable("TIMESTAMPTZ", "TEXT"));
    }

    #[test]
    fn ca_same_category_castable() {
        assert!(is_castable("INTEGER", "BIGINT"));
        assert!(is_castable("TEXT", "TEXT"));
    }

    // ════════════════════════════════════════════════════════
    // Task 2: classify_changes() tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn c1_simple_changes_only() {
        // Use different types for drop (INTEGER) and add (TEXT) so it's not a rename
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("age", "INTEGER")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "email".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "TEXT")))),
                },
                FieldChange {
                    field_name: "age".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "idx_email".to_string(),
                    field_type: FieldType::Index,
                    action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                        name: Some("idx_email".to_string()),
                        columns: vec![IndexColumn { name: "email".to_string(), order: None }],
                        unique: false,
                        index_type: None,
                    }))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "no complex changes expected");
        assert_eq!(simple.len(), 1);
        if let DiffAction::Change(ref changes) = simple[0].action {
            assert_eq!(changes.len(), 3);
        } else {
            panic!("expected Change action");
        }
    }

    #[test]
    fn c2_column_type_change_detected() {
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("email", "VARCHAR(100)")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("email", "VARCHAR(100)"))),
                    new: Box::new(FieldDetail::Column(col("email", "TEXT"))),
                },
            }]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::ColumnTypeChange { ref old_type, ref new_type, .. } = complex[0] {
            assert_eq!(old_type, "VARCHAR(100)");
            assert_eq!(new_type, "TEXT");
        } else {
            panic!("expected ColumnTypeChange");
        }
        // No remaining simple changes for this table
        assert!(simple.is_empty() || simple.iter().all(|d| {
            if let DiffAction::Change(ref c) = d.action { !c.is_empty() } else { true }
        }));
    }

    #[test]
    fn c3_column_rename_detected() {
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("name", "TEXT")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "display_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("display_name", "TEXT")))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::ColumnRename { ref old_name, ref new_name, .. } = complex[0] {
            assert_eq!(old_name, "name");
            assert_eq!(new_name, "display_name");
        } else {
            panic!("expected ColumnRename");
        }
        assert!(simple.is_empty());
    }

    #[test]
    fn c4_different_types_not_rename() {
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("name", "TEXT")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "age".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("age", "INTEGER")))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "different types should not be detected as rename");
        assert_eq!(simple.len(), 1);
        if let DiffAction::Change(ref changes) = simple[0].action {
            assert_eq!(changes.len(), 2);
        } else {
            panic!("expected Change action");
        }
    }

    #[test]
    fn c5_multiple_drops_adds_not_rename() {
        let old_snap = snap(
            vec![table("config", "users", vec![
                col("id", "int"),
                col("first_name", "TEXT"),
                col("last_name", "TEXT"),
            ])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "first_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "last_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "given_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("given_name", "TEXT")))),
                },
                FieldChange {
                    field_name: "family_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("family_name", "TEXT")))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "multiple drops+adds should not be detected as rename");
        assert_eq!(simple.len(), 1);
        if let DiffAction::Change(ref changes) = simple[0].action {
            assert_eq!(changes.len(), 4);
        } else {
            panic!("expected Change action");
        }
    }

    #[test]
    fn c6_enum_value_removal_detected() {
        let old_snap = snap(
            vec![],
            vec![EnumSnapshot {
                name: "status_type".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()],
            }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::EnumValueRemoval {
            ref removed_values,
            ref remaining_values,
            ..
        } = complex[0]
        {
            assert_eq!(removed_values, &vec!["deleted".to_string()]);
            assert!(remaining_values.contains(&"active".to_string()));
            assert!(remaining_values.contains(&"inactive".to_string()));
            assert!(!remaining_values.contains(&"deleted".to_string()));
        } else {
            panic!("expected EnumValueRemoval");
        }
        assert!(simple.is_empty());
    }

    #[test]
    fn c7_enum_removal_identifies_affected_columns() {
        let old_snap = snap(
            vec![table("public", "users", vec![
                col("id", "int"),
                col("status", "status_type"),
            ])],
            vec![EnumSnapshot {
                name: "status_type".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()],
            }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];

        let (_, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::EnumValueRemoval {
            ref affected_columns,
            ..
        } = complex[0]
        {
            assert!(!affected_columns.is_empty());
            assert!(affected_columns.contains(&("public.users".to_string(), "status".to_string())));
        } else {
            panic!("expected EnumValueRemoval");
        }
    }

    #[test]
    fn c_enum_value_rename_is_simple() {
        let old_snap = snap(
            vec![],
            vec![EnumSnapshot {
                name: "status_type".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()],
            }],
        );
        // 1:1 swap: drop "deleted", add "archived"
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "deleted".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "archived".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("archived".to_string()))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "1:1 enum value swap should be treated as simple");
        assert_eq!(simple.len(), 1);
    }
}
