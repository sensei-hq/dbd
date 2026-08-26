//! Full live↔design schema diff for the read-only `dbd diff` command.
//!
//! Unlike reconcile (which strips FK/CHECK/indexes/comments before diffing),
//! this normalizes those attributes so they can be compared, then reuses the
//! full diff engine. See docs/superpowers/specs/2026-07-30-dbd-diff-command-design.md.

use serde::Serialize;

use crate::diff::{self, MigrationDiff};
use crate::entity::{FkAction, ForeignKey, IndexDef, TableConstraint};
use crate::reconcile::{DEFAULT_SCHEMA, normalize_common};
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
    /// Materialized-view drift. Matviews aren't in the tables/enums snapshots the
    /// diff engine compares, so [`Self::compute`] leaves this empty; `diff_live`
    /// populates it (see [`crate::design::Design::diff_live`]). `#[serde(default)]`
    /// is defensive: `SchemaDiff` is only ever serialized today (`dbd diff --json`).
    #[serde(default)]
    pub matview_drift: Vec<MatviewDrift>,
}

/// One drifted materialized view: its qualified `schema.name` and how it drifted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatviewDrift {
    pub name: String,
    pub kind: MatviewDriftKind,
}

/// How a materialized view differs between the design and the live database.
/// Serializes PascalCase (no `rename_all`) to match `DiffAction` in the same
/// `dbd diff --json` payload.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum MatviewDriftKind {
    /// In the design, absent from the database — a `dbd apply`/reconcile would create it.
    Missing,
    /// In both, but the deployed definition differs from the design (hash mismatch).
    Drifted,
    /// In the database but not stamped by dbd — its definition can't be verified.
    Unstamped,
    /// In the database (managed schema) but not in the design.
    Orphan,
}

impl SchemaDiff {
    /// Compute the diff between an introspected `live` snapshot and the `desired`
    /// snapshot built from the design. Both are normalized with
    /// [`normalize_for_diff`] first to erase parsed-vs-introspected noise.
    pub fn compute(mut live: Snapshot, mut desired: Snapshot) -> Self {
        let mut advisories = Vec::new();
        normalize_for_diff(&mut live, &mut advisories);
        normalize_for_diff(&mut desired, &mut advisories);
        // Both sides normalize the same table/constraint, so an unparseable CHECK
        // yields an identical advisory twice — collapse duplicates, keeping order.
        let mut seen = std::collections::HashSet::new();
        advisories.retain(|a| seen.insert(a.clone()));
        let changes = diff::diff(&live, &desired);
        let warnings = diff::migration_warnings(&changes);
        // `compute` only sees tables+enums; matview drift is filled in by `diff_live`.
        SchemaDiff {
            changes,
            warnings,
            advisories,
            matview_drift: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.matview_drift.is_empty()
    }
}

/// Normalize a snapshot for a full diff: apply the shared representation
/// normalization (types, defaults, enum qualification, PK/unique lifting) and,
/// unlike reconcile's `canonicalize`, retain and normalize FK/CHECK/indexes/
/// comments so they compare cleanly. `advisories` collects best-effort notes.
pub fn normalize_for_diff(snap: &mut Snapshot, advisories: &mut Vec<String>) {
    normalize_common(snap);

    for t in &mut snap.tables {
        // Lift any inline column FK into a table constraint (introspection form).
        let inline: Vec<TableConstraint> = t
            .columns
            .iter_mut()
            .filter_map(|c| c.inline_fk.take().map(TableConstraint::ForeignKey))
            .collect();
        t.table_constraints.extend(inline);
        for con in &mut t.table_constraints {
            if let TableConstraint::ForeignKey(fk) = con {
                normalize_fk(fk);
            }
        }

        // Drop indexes that merely back a PK/UNIQUE constraint — introspection
        // reports them, the parsed design does not. Match by covered columns.
        let constraint_cols: std::collections::HashSet<Vec<String>> = t
            .table_constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::PrimaryKey { columns, .. } | TableConstraint::Unique { columns, .. } => {
                    Some(columns.clone())
                }
                _ => None,
            })
            .collect();
        t.indexes.retain(|i| !backs_a_constraint(i, &constraint_cols));

        // Settle each index's default spellings (explicit `using btree`, explicit
        // `asc`) so an authored index and an introspected one compare equal.
        for ix in &mut t.indexes {
            normalize_index(ix);
        }

        // Canonicalize CHECK expressions so equivalent spellings (extra parens,
        // casts introduced by `pg_get_constraintdef`) don't read as drift. An
        // expression we can't parse is left raw and flagged, never silently hidden.
        for con in &mut t.table_constraints {
            if let TableConstraint::Check { name, expression } = con {
                match canonicalize_check_expr(expression) {
                    Some(canon) => *expression = canon,
                    None => advisories.push(format!(
                        "CHECK {} on {}.{} couldn't be normalized — shown as changed; verify manually",
                        name.as_deref().unwrap_or("(unnamed)"),
                        t.schema,
                        t.name
                    )),
                }
                // Postgres auto-names every CHECK; the parser leaves inline CHECKs
                // unnamed. Compare by (canonicalized) expression, not name — mirrors
                // normalize_fk. constraint_key then falls back to `ck:{expr}` on both
                // sides. (advisory above is built first, so it keeps the real name.)
                *name = None;
            }
        }
    }
}

/// Whether this index exists only to back a PRIMARY KEY / UNIQUE constraint, and
/// so should be compared as a constraint rather than as an index.
///
/// A constraint's backing index is always a plain index over the constrained
/// columns: Postgres cannot build one from a `WHERE` clause or an expression. So a
/// partial or expression index is a real index even when it covers exactly the
/// constrained columns — `unique (library_id)` and
/// `unique index (library_id) where project_id is null` enforce different things.
pub(crate) fn backs_a_constraint(ix: &IndexDef, constraint_cols: &std::collections::HashSet<Vec<String>>) -> bool {
    if ix.predicate.is_some() || ix.columns.iter().any(|c| c.is_expression) {
        return false;
    }
    let cols: Vec<String> = ix.columns.iter().map(|c| c.name.clone()).collect();
    constraint_cols.contains(&cols)
}

/// Settle an index's default spellings so the authored and introspected forms of
/// the same index compare equal. Mirrors [`normalize_fk`]'s treatment of
/// `NO ACTION`: a default Postgres does not preserve must not read as drift.
///
/// - btree is the default access method, so an explicit `using btree` collapses
///   to "no `USING` clause";
/// - `ASC` is the default direction, so an explicit `asc` collapses to unset.
pub(crate) fn normalize_index(ix: &mut IndexDef) {
    use crate::entity::{IndexType, SortOrder};
    if ix.index_type == Some(IndexType::Btree) {
        ix.index_type = None;
    }
    for col in &mut ix.columns {
        if col.order == Some(SortOrder::Asc) {
            col.order = None;
        }
    }
}

/// Canonicalize a CHECK expression so an authored spelling and the analyzed form
/// `pg_get_constraintdef` reports converge — see [`crate::sql_expr`] for what is
/// normalized (parens, case, literal casts, `IN` vs `= ANY (ARRAY[…])`).
///
/// Returns `None` if the expression can't be parsed or re-deparsed; the caller
/// records an advisory and keeps the raw text, so a real diff is never hidden.
fn canonicalize_check_expr(expr: &str) -> Option<String> {
    crate::sql_expr::canonicalize_predicate(expr)
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
    use crate::entity::{ColumnDef, FkAction, ForeignKey, IndexColumn, IndexDef, TableConstraint};
    use crate::snapshot::{Snapshot, TableSnapshot};

    fn col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: ty.into(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            identity: None,
            comment: None,
            inline_fk: None,
        }
    }

    fn idx(name: &str, cols: &[&str], unique: bool) -> IndexDef {
        IndexDef {
            name: Some(name.into()),
            columns: cols
                .iter()
                .map(|c| IndexColumn {
                    name: (*c).into(),
                    ..Default::default()
                })
                .collect(),
            unique,
            ..Default::default()
        }
    }
    fn table(cols: Vec<ColumnDef>) -> TableSnapshot {
        TableSnapshot {
            name: "users".into(),
            schema: "public".into(),
            columns: cols,
            indexes: vec![],
            table_constraints: vec![],
        }
    }
    fn snap(t: TableSnapshot) -> Snapshot {
        Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables: vec![t],
            enums: vec![],
        }
    }

    fn fk(name: &str, col: &str, reft: &str, refc: &str, on_delete: Option<FkAction>) -> ForeignKey {
        ForeignKey {
            name: Some(name.into()),
            columns: vec![col.into()],
            ref_schema: None,
            ref_table: reft.into(),
            ref_columns: vec![refc.into()],
            on_delete,
            on_update: None,
        }
    }

    fn check(name: &str, expr: &str) -> TableConstraint {
        TableConstraint::Check {
            name: Some(name.into()),
            expression: expr.into(),
        }
    }

    /// The same CHECK written with different (but equivalent) parenthesization
    /// canonicalizes to the same form → no diff.
    #[test]
    fn equivalent_check_exprs_do_not_diff() {
        let live = snap(TableSnapshot {
            table_constraints: vec![check("ck_total", "((total > 0))")],
            ..table(vec![col("total", "integer")])
        });
        let desired = snap(TableSnapshot {
            table_constraints: vec![check("ck_total", "total > 0")],
            ..table(vec![col("total", "integer")])
        });
        let d = SchemaDiff::compute(live, desired);
        assert!(
            d.is_empty(),
            "equivalent CHECK exprs must not diff, got {:?}",
            d.changes
        );
    }

    /// A genuinely different CHECK predicate is still detected.
    #[test]
    fn changed_check_expr_is_detected() {
        let live = snap(TableSnapshot {
            table_constraints: vec![check("ck_total", "total > 0")],
            ..table(vec![col("total", "integer")])
        });
        let desired = snap(TableSnapshot {
            table_constraints: vec![check("ck_total", "total >= 0")],
            ..table(vec![col("total", "integer")])
        });
        let d = SchemaDiff::compute(live, desired);
        assert!(!d.is_empty(), "changed CHECK predicate must surface");
    }

    /// An unparseable CHECK is surfaced with an advisory rather than hidden.
    #[test]
    fn unparseable_check_records_advisory() {
        let mut adv = Vec::new();
        let mut s = snap(TableSnapshot {
            table_constraints: vec![check("ck", "%%% not sql %%%")],
            ..table(vec![col("x", "integer")])
        });
        normalize_for_diff(&mut s, &mut adv);
        assert!(!adv.is_empty(), "unparseable CHECK must record an advisory");
    }

    /// Introspection names every CHECK (e.g. `items_total_check`); the parser leaves
    /// an inline CHECK unnamed. They must match by canonicalized expression, not name
    /// — otherwise every unnamed CHECK shows as phantom drift and `dbd diff
    /// --exit-code` falsely reports drift on an in-sync DB.
    #[test]
    fn unnamed_check_matches_named_introspected_check() {
        let live = snap(TableSnapshot {
            table_constraints: vec![check("items_total_check", "((total > 0))")],
            ..table(vec![col("total", "integer")])
        });
        let desired = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::Check {
                name: None,
                expression: "total > 0".into(),
            }],
            ..table(vec![col("total", "integer")])
        });
        let d = SchemaDiff::compute(live, desired);
        assert!(
            d.is_empty(),
            "unnamed CHECK must match named introspected CHECK, got {:?}",
            d.changes
        );
    }

    /// The canonicalized CHECK stored back into the snapshot is the BARE predicate,
    /// not the `SELECT 1 WHERE ...` parse wrapper (which would corrupt generated
    /// SQL and --json output).
    #[test]
    fn canonicalized_check_is_bare_predicate() {
        let mut adv = Vec::new();
        let mut s = snap(TableSnapshot {
            table_constraints: vec![check("ck", "((total > 0))")],
            ..table(vec![col("total", "integer")])
        });
        normalize_for_diff(&mut s, &mut adv);
        match &s.tables[0].table_constraints[0] {
            TableConstraint::Check { expression, .. } => {
                assert_eq!(expression, "total > 0", "must store bare predicate, got: {expression}")
            }
            other => panic!("expected a CHECK, got {other:?}"),
        }
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
            inline_fk: Some(ForeignKey {
                name: None,
                ref_schema: None,
                ..fk("_", "org_id", "org", "id", None)
            }),
            ..col("org_id", "integer")
        }]));
        let d = SchemaDiff::compute(live, desired);
        assert!(
            d.is_empty(),
            "unnamed inline FK must match named introspected FK, got {:?}",
            d.changes
        );
    }

    /// A genuinely changed FK target is still detected.
    #[test]
    fn changed_fk_is_detected() {
        let live = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::ForeignKey(fk(
                "users_org_fk",
                "org_id",
                "org",
                "id",
                None,
            ))],
            ..table(vec![col("org_id", "integer")])
        });
        let desired = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::ForeignKey(fk(
                "users_org_fk",
                "org_id",
                "team",
                "id",
                None,
            ))],
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
        let live = snap(table(vec![ColumnDef {
            comment: Some("old".into()),
            ..col("id", "integer")
        }]));
        let desired = snap(table(vec![ColumnDef {
            comment: Some("new".into()),
            ..col("id", "integer")
        }]));
        let d = SchemaDiff::compute(live, desired);
        assert!(!d.is_empty(), "comment change must surface");
    }

    /// The implicit index backing a PK (live-only, from introspection) is not a
    /// real drift and must be suppressed.
    #[test]
    fn pk_backing_index_is_suppressed() {
        let live = snap(TableSnapshot {
            table_constraints: vec![TableConstraint::PrimaryKey {
                name: Some("users_pkey".into()),
                columns: vec!["id".into()],
            }],
            indexes: vec![idx("users_pkey", &["id"], true)], // introspection reports the backing index
            ..table(vec![ColumnDef {
                nullable: false,
                is_pk: true,
                ..col("id", "integer")
            }])
        });
        let desired = snap(TableSnapshot {
            table_constraints: vec![],
            indexes: vec![],
            ..table(vec![ColumnDef {
                nullable: false,
                is_pk: true,
                ..col("id", "integer")
            }])
        });
        let d = SchemaDiff::compute(live, desired);
        assert!(
            d.is_empty(),
            "PK-backing index must not surface as a diff, got {:?}",
            d.changes
        );
    }

    /// A genuine secondary index add is still detected.
    #[test]
    fn secondary_index_add_is_detected() {
        let live = snap(table(vec![col("email", "text")]));
        let desired = snap(TableSnapshot {
            indexes: vec![idx("users_email_idx", &["email"], false)],
            ..table(vec![col("email", "text")])
        });
        let d = SchemaDiff::compute(live, desired);
        assert!(!d.is_empty(), "new index must surface");
    }

    /// `is_empty()` accounts for matview drift: a diff with no table/enum changes
    /// but a drifted matview is NOT in sync (so `--exit-code` / the "in sync"
    /// message key off it correctly); a fully empty diff is.
    #[test]
    fn is_empty_accounts_for_matview_drift() {
        let empty = SchemaDiff::default();
        assert!(empty.is_empty(), "a fully empty diff must be in sync");

        let drift_only = SchemaDiff {
            matview_drift: vec![MatviewDrift {
                name: "analytics.daily".into(),
                kind: MatviewDriftKind::Drifted,
            }],
            ..SchemaDiff::default()
        };
        assert!(!drift_only.is_empty(), "matview-drift-only diff must not be in sync");
    }
}
