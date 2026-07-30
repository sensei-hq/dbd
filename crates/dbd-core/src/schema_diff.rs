//! Full live↔design schema diff for the read-only `dbd diff` command.
//!
//! Unlike reconcile (which strips FK/CHECK/indexes/comments before diffing),
//! this normalizes those attributes so they can be compared, then reuses the
//! full diff engine. See docs/superpowers/specs/2026-07-30-dbd-diff-command-design.md.

use serde::Serialize;

use crate::diff::{self, MigrationDiff};
use crate::reconcile::normalize_common;
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
    // FK / CHECK / index / comment normalization added in Tasks 3–5.
    let _ = advisories;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::ColumnDef;
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
