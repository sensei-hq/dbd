mod classify;
mod compare;
mod generate;
mod types;

pub use classify::{classify_changes, generate_data_sql, is_castable};
pub use compare::diff;
pub use generate::{generate_migration_sql, migration_warnings};
pub use types::*;

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    use crate::entity::{ColumnDef, EntityType, FkAction, ForeignKey, IdentityKind, IndexColumn, IndexDef, IndexType, SortOrder, TableConstraint};
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
            identity: None,
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

    /// A single-column `Alter` change wrapped as a table `MigrationDiff`.
    fn alter_col(entity: &str, field: &str, old: ColumnDef, new: ColumnDef) -> MigrationDiff {
        MigrationDiff {
            entity_name: entity.to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: field.to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(old)),
                    new: Box::new(FieldDetail::Column(new)),
                },
            }]),
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

    // Tests are included from the original file — all D1-D21, S1-S14,
    // warning, castability, classify, and data.sql tests follow.

    include!("tests.rs");
}
