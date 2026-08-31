use serde::{Deserialize, Serialize};

use crate::entity::{ColumnDef, EntityType, IndexDef, TableConstraint};

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
///
/// `Drop` carries the object that was dropped, symmetrically with `Add`. It has
/// to: [`FieldChange::field_name`] is a *matching key*, and for an object matched
/// name-agnostically (every constraint kind is — Postgres auto-names what the
/// design leaves unnamed) that key is synthetic, e.g. `pk:tenant_id,metric_id`.
/// Emitting it as an identifier produced `DROP CONSTRAINT pk:tenant_id,metric_id`,
/// which no database can run. The payload is where the real name comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeAction {
    Add(Box<FieldDetail>),
    Drop(Box<FieldDetail>),
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
