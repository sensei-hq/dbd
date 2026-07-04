//! Declarative reconcile: converge a live database to the project's desired
//! schema by diffing introspected state against the design and running ALTERs
//! directly — no snapshot files, no version bump.
//!
//! This is the pre-release (pre-v1) development workflow: while iterating on a
//! schema it is tedious to cut a throwaway snapshot for every column tweak.
//! `reconcile` instead computes a live→desired diff and applies it in place,
//! self-correcting whatever drift is in the dev database.
//!
//! Once a project is released ([`crate::config::set_released`]), reconcile is
//! disabled and schema changes must go through snapshots + migrations. The
//! execution and gating live in [`crate::design::Design::reconcile`]; this
//! module holds the pure planning logic so it can be unit-tested without a
//! database.

use crate::diff::{self, ChangeAction, DiffAction, MigrationDiff};
use crate::entity::{Entity, EntityType};
use crate::snapshot::{self, Snapshot};

/// A single entity that will be altered or dropped, paired with its DDL.
#[derive(Debug, Clone)]
pub struct ReconcileStatement {
    pub entity_name: String,
    pub sql: String,
}

/// A plan describing how to converge the live DB to the desired schema.
///
/// `added` names get a full `apply_entity` (CREATE — the diff engine emits no
/// SQL for additions); `altered`/`dropped` carry generated ALTER/DROP SQL.
#[derive(Debug, Clone, Default)]
pub struct ReconcilePlan {
    /// Table/enum entities present in the design but absent from the DB.
    pub added: Vec<String>,
    /// Table/enum entities whose structure changed (ALTER SQL).
    pub altered: Vec<ReconcileStatement>,
    /// Orphans: table entities in a managed schema but absent from the design.
    /// Only executed when the caller opts into pruning; otherwise reported and
    /// left untouched. Carries the `DROP TABLE … CASCADE` SQL.
    pub dropped: Vec<ReconcileStatement>,
    /// Risky-change advisories (type changes, possible renames, enum value drops,
    /// orphaned enums that are not auto-dropped).
    pub warnings: Vec<String>,
    /// Whether the plan drops a column or constraint from an existing table
    /// (data loss). Whole-table drops are separate — see [`Self::dropped`] — and
    /// gated by pruning, not by this flag.
    pub destructive: bool,
}

impl ReconcilePlan {
    /// No structural changes to make.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.altered.is_empty() && self.dropped.is_empty()
    }
}

/// The schema an unqualified entity resolves to. Introspection always reports a
/// concrete schema (`public`), so desired entities must default to the same for
/// the live→desired diff to line up by qualified name.
pub const DEFAULT_SCHEMA: &str = "public";

/// Build a diff-able snapshot (tables + enums only) from a set of entities.
///
/// Symmetric for live (introspected) and desired (project) entities. The other
/// entity types (schemas, extensions, sequences, functions, views, roles) are
/// reconciled by idempotent re-apply rather than diffing, so they are
/// intentionally excluded here.
///
/// Empty schemas are normalized to [`DEFAULT_SCHEMA`] so an unqualified project
/// table (`""`) matches its introspected counterpart (`"public"`).
pub fn snapshot_from_entities(entities: &[Entity]) -> Snapshot {
    let tables = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Table && e.table_def.is_some())
        .filter_map(snapshot::entity_to_table_snapshot)
        .map(|mut t| {
            if t.schema.is_empty() {
                t.schema = DEFAULT_SCHEMA.to_string();
            }
            t
        })
        .collect();
    let enums = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .map(snapshot::entity_to_enum_snapshot)
        .map(|mut e| {
            if e.schema.is_empty() {
                e.schema = DEFAULT_SCHEMA.to_string();
            }
            e
        })
        .collect();
    Snapshot {
        version: 0,
        description: String::new(),
        timestamp: String::new(),
        tables,
        enums,
    }
}

/// Qualified name of an entity using the same normalization as
/// [`snapshot_from_entities`] — `"{schema}.{short_name}"`, empty schema → `public`.
/// Lets execution match project entities against a plan's `added`/`altered` names.
pub fn qualified_entity_name(entity: &Entity) -> String {
    let (_, short) = crate::entity::split_qualified_name(&entity.name);
    let schema = entity.schema.clone().unwrap_or_default();
    let schema = if schema.is_empty() {
        DEFAULT_SCHEMA.to_string()
    } else {
        schema
    };
    format!("{schema}.{short}")
}

/// Compute a reconcile plan from a live→desired snapshot diff.
pub fn plan_reconcile(live: &Snapshot, desired: &Snapshot) -> ReconcilePlan {
    let diffs = diff::diff(live, desired);
    let warnings = diff::migration_warnings(&diffs);
    let destructive = diffs.iter().any(has_column_drop);

    let mut plan = ReconcilePlan {
        warnings,
        destructive,
        ..Default::default()
    };

    for d in &diffs {
        match &d.action {
            DiffAction::Add => plan.added.push(d.entity_name.clone()),
            DiffAction::Change(_) => {
                let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                if !sql.trim().is_empty() {
                    plan.altered.push(ReconcileStatement {
                        entity_name: d.entity_name.clone(),
                        sql,
                    });
                }
            }
            DiffAction::Drop => {
                let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                // Only actionable drops (real `DROP` DDL) become prune targets.
                // Enum drops emit a warning comment instead — already captured in
                // `warnings` — so they are never auto-dropped.
                if sql.to_uppercase().contains("DROP ") {
                    plan.dropped.push(ReconcileStatement {
                        entity_name: d.entity_name.clone(),
                        sql,
                    });
                }
            }
        }
    }

    plan
}

/// Whether a diff drops a column or constraint from an existing table — the
/// data-loss case gated by `allow_destructive`. Whole-object drops are handled
/// separately via pruning.
fn has_column_drop(d: &MigrationDiff) -> bool {
    matches!(&d.action, DiffAction::Change(changes)
        if changes.iter().any(|c| matches!(c.action, ChangeAction::Drop)))
}

/// Summary of an executed reconcile, passed to the `on_complete` callback.
#[derive(Debug, Clone, Default)]
pub struct ReconcileComplete {
    /// Entities created via full apply (added tables/enums).
    pub created: u32,
    /// Entities altered via generated ALTER SQL.
    pub altered: u32,
    /// Entities dropped.
    pub dropped: u32,
    /// Idempotent objects re-applied (schemas, extensions, sequences,
    /// functions, views, roles).
    pub reapplied: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColumnDef;
    use crate::snapshot::TableSnapshot;

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

    fn table(schema: &str, name: &str, columns: Vec<ColumnDef>) -> TableSnapshot {
        TableSnapshot {
            name: name.to_string(),
            schema: schema.to_string(),
            columns,
            indexes: vec![],
            table_constraints: vec![],
        }
    }

    fn snap(tables: Vec<TableSnapshot>) -> Snapshot {
        Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables,
            enums: vec![],
        }
    }

    #[test]
    fn empty_plan_when_live_matches_desired() {
        let live = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let desired = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let plan = plan_reconcile(&live, &desired);
        assert!(plan.is_empty());
        assert!(!plan.destructive);
    }

    #[test]
    fn added_table_is_planned_for_create() {
        let live = snap(vec![]);
        let desired = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.added, vec!["public.users".to_string()]);
        assert!(plan.altered.is_empty());
        assert!(plan.dropped.is_empty());
        assert!(!plan.destructive, "a pure addition is not destructive");
    }

    #[test]
    fn added_column_produces_alter_sql() {
        let live = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let desired = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )]);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.altered.len(), 1);
        assert_eq!(plan.altered[0].entity_name, "public.users");
        assert!(
            plan.altered[0].sql.contains("ADD COLUMN"),
            "expected ADD COLUMN, got: {}",
            plan.altered[0].sql
        );
        assert!(!plan.destructive, "adding a column is not destructive");
    }

    #[test]
    fn dropped_column_is_destructive() {
        let live = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )]);
        let desired = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.altered.len(), 1);
        assert!(plan.altered[0].sql.contains("DROP COLUMN"));
        assert!(plan.destructive, "dropping a column is destructive");
    }

    #[test]
    fn dropped_table_becomes_a_prune_target() {
        let live = snap(vec![table("public", "users", vec![col("id", "int")])]);
        let desired = snap(vec![]);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.dropped.len(), 1);
        assert_eq!(plan.dropped[0].entity_name, "public.users");
        assert!(plan.dropped[0].sql.contains("DROP TABLE"));
        // A whole-table drop is a prune target, not a `destructive` (column-drop) change.
        assert!(
            !plan.destructive,
            "whole-table drop is gated by prune, not allow_destructive"
        );
    }
}
