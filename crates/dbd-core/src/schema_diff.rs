//! Full live↔design schema diff for the read-only `dbd diff` command.
//!
//! Unlike reconcile (which strips FK/CHECK/indexes/comments before diffing),
//! this normalizes those attributes so they can be compared, then reuses the
//! full diff engine. See docs/superpowers/specs/2026-07-30-dbd-diff-command-design.md.

use serde::Serialize;

use crate::diff::{self, MigrationDiff};
use crate::entity::{FkAction, ForeignKey, TableConstraint};
use crate::reconcile::{normalize_common, DEFAULT_SCHEMA};
use crate::snapshot::Snapshot;

/// The complete difference between a live database and the design.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SchemaDiff {
    /// Entity-level changes (columns, PK/unique, FK, CHECK, indexes, enum values).
    pub changes: Vec<MigrationDiff>,
    /// Risky-change advisories (from the diff engine).
    pub warnings: Vec<String>,
    /// Best-effort normalization notes (e.g. an unparseable CHECK shown as changed).
    pub advisories: Vec<String>,
}

impl SchemaDiff {
    /// Compute the diff between an introspected `live` snapshot and the `desired`
    /// snapshot built from the design. Both are normalized with
    /// [`normalize_for_diff`] first to erase parsed-vs-introspected noise.
    pub fn compute(mut live: Snapshot, mut desired: Snapshot) -> Self {
        let mut advisories = Vec::new();
        normalize_for_diff(&mut live, &mut advisories);
        normalize_for_diff(&mut desired, &mut advisories);
        let changes = diff::diff(&live, &desired);
        let warnings = diff::migration_warnings(&changes);
        SchemaDiff { changes, warnings, advisories }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Normalize a snapshot for a full diff: apply the shared representation
/// normalization (types, defaults, enum qualification, PK/unique lifting) and,
/// unlike reconcile's `canonicalize`, retain and normalize FK/CHECK/indexes/
/// comments so they compare cleanly. `advisories` collects best-effort notes.
pub fn normalize_for_diff(snap: &mut Snapshot, advisories: &mut Vec<String>) {
    normalize_common(snap);
    // Index (Task 4) + CHECK (Task 5) normalization added next.
    let _ = advisories;

    for t in &mut snap.tables {
        // Lift any inline column FK into a table constraint (introspection form).
        let inline: Vec<TableConstraint> = t.columns.iter_mut()
            .filter_map(|c| c.inline_fk.take().map(TableConstraint::ForeignKey))
            .collect();
        t.table_constraints.extend(inline);
        for con in &mut t.table_constraints {
            if let TableConstraint::ForeignKey(fk) = con {
                normalize_fk(fk);
            }
        }
    }
}

/// Normalize a foreign key so a parsed (design) and an introspected (live) form
/// of the same FK compare equal. Mirrors `reconcile::lift_pk_unique_keep_others`:
/// strip the constraint name (Postgres auto-generates one; the parser leaves it
/// `None`) so FKs match by shape, and canonicalize the referenced schema (a bare
/// ref and an explicit `public` ref are the same target). `NO ACTION` is the
/// Postgres default → collapse to `None`.
fn normalize_fk(fk: &mut ForeignKey) {
    fk.name = None;
    normalize_fk_action(&mut fk.on_delete);
    normalize_fk_action(&mut fk.on_update);
    if fk.ref_schema.as_deref() == Some(DEFAULT_SCHEMA) {
        fk.ref_schema = None;
    }
    if let Some(s) = fk.ref_schema.as_mut() {
        *s = s.to_lowercase();
    }
}

/// `NO ACTION` is the Postgres default; collapse it to `None` so an explicit
/// and an omitted default compare equal.
fn normalize_fk_action(a: &mut Option<FkAction>) {
    if *a == Some(FkAction::NoAction) {
        *a = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{ColumnDef, FkAction, ForeignKey, TableConstraint};
    use crate::snapshot::{Snapshot, TableSnapshot};

    fn col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef { name: name.into(), data_type: ty.into(), nullable: true, default_value: None,
            is_pk: false, is_unique: false, identity: None, comment: None, inline_fk: None }
    }
    fn table(cols: Vec<ColumnDef>) -> TableSnapshot {
        TableSnapshot { name: "users".into(), schema: "public".into(), columns: cols, indexes: vec![], table_constraints: vec![] }
    }
    fn snap(t: TableSnapshot) -> Snapshot {
        Snapshot { version: 0, description: String::new(), timestamp: String::new(), tables: vec![t], enums: vec![] }
    }

    fn fk(name: &str, col: &str, reft: &str, refc: &str, on_delete: Option<FkAction>) -> ForeignKey {
        ForeignKey { name: Some(name.into()), columns: vec![col.into()], ref_schema: None,
            ref_table: reft.into(), ref_columns: vec![refc.into()], on_delete, on_update: None }
    }

    /// Live = introspected: FK as a NAMED table constraint, schema-qualified,
    /// explicit NO ACTION. Desired = parsed: the SAME FK carried INLINE, UNNAMED,
    /// unqualified ref, no action — exactly what the parser emits. They must
    /// reconcile to no diff (name stripped, ref schema canonicalized, action
    /// collapsed, inline lifted).
    #[test]
    fn inline_fk_matches_table_constraint_fk() {
        let live = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::ForeignKey(ForeignKey {
                ref_schema: Some("public".into()),
                ..fk("users_org_fk", "org_id", "org", "id", Some(FkAction::NoAction))
            })],
            ..table(vec![col("org_id", "integer")])
        });
        let desired = snap(table(vec![ColumnDef {
            inline_fk: Some(ForeignKey { name: None, ref_schema: None, ..fk("_", "org_id", "org", "id", None) }),
            ..col("org_id", "integer")
        }]));
        let d = SchemaDiff::compute(live, desired);
        assert!(d.is_empty(), "unnamed inline FK must match named introspected FK, got {:?}", d.changes);
    }

    /// A genuinely changed FK target is still detected.
    #[test]
    fn changed_fk_is_detected() {
        let live = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::ForeignKey(fk("users_org_fk", "org_id", "org", "id", None))],
            ..table(vec![col("org_id", "integer")])
        });
        let desired = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::ForeignKey(fk("users_org_fk", "org_id", "team", "id", None))],
            ..table(vec![col("org_id", "integer")])
        });
        let d = SchemaDiff::compute(live, desired);
        assert!(!d.is_empty(), "changed FK target must surface");
    }

    /// An in-sync table (after normalization) yields an empty diff.
    #[test]
    fn in_sync_table_is_empty_diff() {
        let live = snap(table(vec![col("id", "integer")]));
        let desired = snap(table(vec![col("id", "int4")])); // alias normalizes to integer
        let d = SchemaDiff::compute(live, desired);
        assert!(d.is_empty(), "expected empty, got {:?}", d.changes);
    }

    /// A comment change is detected (reconcile drops comments; diff keeps them).
    #[test]
    fn comment_change_is_detected() {
        let live = snap(table(vec![ColumnDef { comment: Some("old".into()), ..col("id", "integer") }]));
        let desired = snap(table(vec![ColumnDef { comment: Some("new".into()), ..col("id", "integer") }]));
        let d = SchemaDiff::compute(live, desired);
        assert!(!d.is_empty(), "comment change must surface");
    }
}
