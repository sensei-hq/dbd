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

use std::collections::{HashMap, HashSet};

use crate::diff::{self, ChangeAction, DiffAction, MigrationDiff};
use crate::entity::{Entity, EntityType, TableConstraint};
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
    let mut snap = Snapshot {
        version: 0,
        description: String::new(),
        timestamp: String::new(),
        tables,
        enums,
    };
    canonicalize(&mut snap);
    snap
}

/// Canonicalize a snapshot so a **parsed** (desired) table and an **introspected**
/// (live) table of the same shape compare equal. The two representations diverge:
///
/// - Introspection decomposes inline `PRIMARY KEY`/`UNIQUE` into named table-level
///   constraints and never sets column `is_pk`/`is_unique`; the parser keeps them
///   inline. → lift inline flags into unnamed constraints, strip constraint names,
///   clear the column flags.
/// - Introspection schema-qualifies enum types not on the session `search_path`
///   (`config.status`) while DDL written with `set search_path` leaves them bare
///   (`status`). → qualify column types that name a known enum, and lowercase +
///   alias-normalize type spellings (`INT4` → `integer`, `timestamptz` →
///   `timestamp with time zone`).
///
/// Foreign keys, check constraints and indexes are **dropped from the diff
/// entirely** — their introspected/parsed forms differ too much to compare
/// reliably, so reconcile does not manage them on existing tables (create them via
/// the initial `CREATE`, or use snapshots). Column comments are cleared too.
pub fn canonicalize(snap: &mut Snapshot) {
    // short enum name (lowercased) → canonical column-type spelling. Mirrors what
    // Postgres `format_type` emits: bare for `public`, `schema.name` otherwise.
    let mut enum_types: HashMap<String, String> = HashMap::new();
    for e in &snap.enums {
        let short = e.name.to_lowercase();
        let canonical = if e.schema.eq_ignore_ascii_case(DEFAULT_SCHEMA) {
            short.clone()
        } else {
            format!("{}.{}", e.schema.to_lowercase(), short)
        };
        enum_types.insert(short, canonical);
    }

    for t in &mut snap.tables {
        // Collect PK/UNIQUE from inline column flags + existing table constraints,
        // strip names, dedup by structure. FK/CHECK are intentionally excluded.
        let mut kept: Vec<TableConstraint> = Vec::new();
        let mut seen: HashSet<(char, String)> = HashSet::new();
        let push = |kept: &mut Vec<TableConstraint>, seen: &mut HashSet<(char, String)>, c: TableConstraint| {
            let key = match &c {
                TableConstraint::PrimaryKey { columns, .. } => ('p', columns.join(",")),
                TableConstraint::Unique { columns, .. } => ('u', columns.join(",")),
                _ => return,
            };
            if seen.insert(key) {
                kept.push(c);
            }
        };
        let mut has_table_pk = false;
        for con in std::mem::take(&mut t.table_constraints) {
            match con {
                TableConstraint::PrimaryKey { columns, .. } => {
                    has_table_pk = true;
                    push(&mut kept, &mut seen, TableConstraint::PrimaryKey { name: None, columns })
                }
                TableConstraint::Unique { columns, .. } => {
                    push(&mut kept, &mut seen, TableConstraint::Unique { name: None, columns })
                }
                _ => {} // FK / CHECK excluded
            }
        }
        for c in &t.columns {
            // A column's is_pk flag restates the table's single primary key. When a
            // table-level PRIMARY KEY is already present — e.g. a composite
            // `primary key (a, b)`, which the SQL parser emits BOTH as a table
            // constraint AND as an is_pk flag on each member column — lifting the
            // per-column flags would fabricate spurious single-column PKs (pk(a),
            // pk(b)) that no live table has, producing bogus `ADD CONSTRAINT …
            // PRIMARY KEY` reconcile steps (and "multiple primary keys" errors). A
            // table has exactly one PK, so only synthesize one from a column flag
            // when no table-level PK exists (the inline single-column PK case).
            if c.is_pk && !has_table_pk {
                push(&mut kept, &mut seen, TableConstraint::PrimaryKey { name: None, columns: vec![c.name.clone()] });
            }
            if c.is_unique {
                push(&mut kept, &mut seen, TableConstraint::Unique { name: None, columns: vec![c.name.clone()] });
            }
        }
        t.table_constraints = kept;
        // Indexes are not reconciled (introspect/parse forms diverge).
        t.indexes.clear();
        // Normalize column types; clear inline flags and comments.
        for c in &mut t.columns {
            c.data_type = canonical_type(&c.data_type, &enum_types);
            c.is_pk = false;
            c.is_unique = false;
            c.inline_fk = None;
            c.comment = None;
        }
    }
}

/// Normalize a column type for cross-representation comparison: lowercase, drop a
/// redundant `public.` prefix, map common Postgres aliases to the `format_type`
/// spelling, and schema-qualify a bare enum name using `enum_types`.
fn canonical_type(raw: &str, enum_types: &HashMap<String, String>) -> String {
    let mut s = raw.trim().to_lowercase();
    if let Some(rest) = s.strip_prefix("public.") {
        s = rest.to_string();
    }
    // Split "base(args)" so parameterized aliases (varchar(255)) normalize too.
    let (base, args) = match s.split_once('(') {
        Some((b, rest)) => (b.trim().to_string(), Some(format!("({rest}"))),
        None => (s.clone(), None),
    };
    let base = match base.as_str() {
        "int" | "int4" => "integer".to_string(),
        "int8" => "bigint".to_string(),
        "int2" => "smallint".to_string(),
        "bool" => "boolean".to_string(),
        "float8" => "double precision".to_string(),
        "float4" => "real".to_string(),
        "timestamptz" => "timestamp with time zone".to_string(),
        "timetz" => "time with time zone".to_string(),
        "varchar" => "character varying".to_string(),
        "char" | "bpchar" => "character".to_string(),
        "decimal" => "numeric".to_string(),
        // Qualify a bare enum reference to match introspection.
        other if !other.contains('.') => {
            enum_types.get(other).cloned().unwrap_or_else(|| other.to_string())
        }
        other => other.to_string(),
    };
    match args {
        Some(a) => format!("{base}{a}"),
        None => base,
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
    use crate::snapshot::{EnumSnapshot, TableSnapshot};

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

    fn pk_col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef { is_pk: true, nullable: false, ..col(name, ty) }
    }
    fn not_null(name: &str, ty: &str) -> ColumnDef {
        ColumnDef { nullable: false, ..col(name, ty) }
    }

    /// The core cross-representation guarantee: a table as the parser sees it
    /// (inline PK, bare enum type, uppercase spelling) and as introspection sees
    /// it (named PK constraint, schema-qualified enum, lowercase) reconcile to
    /// *no changes* after canonicalization.
    #[test]
    fn canonicalize_reconciles_parsed_vs_introspected() {
        let enum_def = EnumSnapshot {
            name: "assistant_family".to_string(),
            schema: "config".to_string(),
            values: vec!["gpt".to_string(), "claude".to_string()],
        };

        // Parsed (desired): inline PK, uppercase `UUID`, bare enum type, int alias.
        let mut desired = Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables: vec![TableSnapshot {
                name: "agents".to_string(),
                schema: "config".to_string(),
                columns: vec![
                    pk_col("id", "UUID"),
                    not_null("family", "assistant_family"),
                    not_null("rank", "int4"),
                ],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![enum_def.clone()],
        };

        // Introspected (live): PK as a named table constraint, qualified enum type,
        // canonical lowercase spellings, plus a PK-backing index (ignored).
        let mut live = Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables: vec![TableSnapshot {
                name: "agents".to_string(),
                schema: "config".to_string(),
                columns: vec![
                    not_null("id", "uuid"),
                    not_null("family", "config.assistant_family"),
                    not_null("rank", "integer"),
                ],
                indexes: vec![],
                table_constraints: vec![TableConstraint::PrimaryKey {
                    name: Some("agents_pkey".to_string()),
                    columns: vec!["id".to_string()],
                }],
            }],
            enums: vec![enum_def],
        };

        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert!(
            plan.is_empty() && !plan.destructive,
            "parsed and introspected forms of the same table must reconcile to no changes; got {plan:?}"
        );
    }

    /// Regression: a COMPOSITE table-level `primary key (a, b)` must reconcile to
    /// no changes. The SQL parser emits it both as a table constraint AND as an
    /// is_pk flag on each member column; canonicalize must not lift those flags
    /// into spurious single-column PKs (pk(a), pk(b)) that the live DB lacks —
    /// which produced bogus `ADD CONSTRAINT … PRIMARY KEY` steps and Postgres
    /// "multiple primary keys" apply failures.
    #[test]
    fn canonicalize_composite_pk_no_spurious_single_column_pks() {
        // Parsed (desired): composite PK as a table constraint, and — as the SQL
        // parser does — is_pk flagged on each member column.
        let mut desired = Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables: vec![TableSnapshot {
                name: "transcript_cursor".to_string(),
                schema: "activity".to_string(),
                columns: vec![
                    pk_col("source", "text"),
                    pk_col("file_path", "text"),
                    col("session_id", "text"),
                ],
                indexes: vec![],
                table_constraints: vec![TableConstraint::PrimaryKey {
                    name: None,
                    columns: vec!["source".to_string(), "file_path".to_string()],
                }],
            }],
            enums: vec![],
        };

        // Introspected (live): one named composite PK; columns carry no is_pk flag.
        let mut live = Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables: vec![TableSnapshot {
                name: "transcript_cursor".to_string(),
                schema: "activity".to_string(),
                columns: vec![
                    not_null("source", "text"),
                    not_null("file_path", "text"),
                    col("session_id", "text"),
                ],
                indexes: vec![],
                table_constraints: vec![TableConstraint::PrimaryKey {
                    name: Some("transcript_cursor_pkey".to_string()),
                    columns: vec!["source".to_string(), "file_path".to_string()],
                }],
            }],
            enums: vec![],
        };

        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert!(
            plan.is_empty() && !plan.destructive,
            "composite PK must reconcile to no changes (no spurious single-column PK adds); got {plan:?}"
        );
    }
}
