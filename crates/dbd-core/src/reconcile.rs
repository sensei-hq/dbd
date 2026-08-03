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
use crate::entity::{Entity, EntityType, FkAction, ForeignKey, TableConstraint};
use crate::snapshot::{self, Snapshot, TableSnapshot};

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

/// Build a diff-able snapshot (tables + enums only) from a set of entities,
/// WITHOUT canonicalizing — so FK/CHECK constraints, indexes, inline FKs and
/// column comments are retained. This is the raw form the read-only `dbd diff`
/// needs: it applies its own [`crate::schema_diff::normalize_for_diff`] which
/// *normalizes* those attributes instead of stripping them.
///
/// Symmetric for live (introspected) and desired (project) entities. The other
/// entity types (schemas, extensions, sequences, functions, views, roles) are
/// reconciled by idempotent re-apply rather than diffing, so they are
/// intentionally excluded here.
///
/// Empty schemas are normalized to [`DEFAULT_SCHEMA`] so an unqualified project
/// table (`""`) matches its introspected counterpart (`"public"`).
pub fn raw_snapshot_from_entities(entities: &[Entity]) -> Snapshot {
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

/// Build a diff-able snapshot (tables + enums only) from a set of entities,
/// then [`canonicalize`] it for reconcile's apply-path diff — which strips
/// FK/CHECK/indexes/comments (see `canonicalize`). For the read-only diff that
/// keeps those attributes, use [`raw_snapshot_from_entities`].
pub fn snapshot_from_entities(entities: &[Entity]) -> Snapshot {
    let mut snap = raw_snapshot_from_entities(entities);
    canonicalize(&mut snap);
    snap
}

/// Representation normalization shared by reconcile's `canonicalize` and (in a
/// future task) the read-only `dbd diff`'s `schema_diff::normalize_for_diff`.
/// Makes a **parsed** (desired) and an **introspected** (live) form of the
/// *same* table compare equal for the attributes both paths care about:
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
/// Foreign keys, check constraints, indexes and column comments are left
/// **untouched** — callers that don't want them (reconcile's `canonicalize`)
/// strip them afterward; callers that do want them (the diff path) keep them.
pub(crate) fn normalize_common(snap: &mut Snapshot) {
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
        lift_pk_unique_keep_others(t);
        normalize_column_types(t, &enum_types);
    }
}

/// Canonicalize a snapshot so a **parsed** (desired) table and an **introspected**
/// (live) table of the same shape compare equal for the column/PK/unique diff.
///
/// Reconcile does not diff check constraints or indexes on existing tables here —
/// their introspected/parsed forms differ too much to compare reliably (create
/// them via the initial `CREATE`, or use snapshots) — so after the shared
/// [`normalize_common`] pass this drops them from the diff entirely, along with
/// column comments and inline FK. **Foreign keys are handled separately** by
/// [`plan_fk_convergence`] from the raw (un-canonicalized) snapshots, so they are
/// stripped here too — keeping this column/PK/unique diff free of FK noise.
pub fn canonicalize(snap: &mut Snapshot) {
    normalize_common(snap);
    for t in &mut snap.tables {
        t.indexes.clear();
        t.table_constraints
            .retain(|c| matches!(c, TableConstraint::PrimaryKey { .. } | TableConstraint::Unique { .. }));
        for c in &mut t.columns {
            c.inline_fk = None;
            c.comment = None;
        }
    }
}

/// Collapse a table's PK/UNIQUE (from inline column flags + table constraints)
/// into name-stripped, structurally-deduped table constraints, leaving any
/// FK/CHECK constraints already present untouched (reconcile's `canonicalize`
/// strips those afterward; the diff path keeps them).
fn lift_pk_unique_keep_others(t: &mut snapshot::TableSnapshot) {
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
    let mut others: Vec<TableConstraint> = Vec::new();
    for con in std::mem::take(&mut t.table_constraints) {
        match con {
            TableConstraint::PrimaryKey { columns, .. } => {
                has_table_pk = true;
                push(&mut kept, &mut seen, TableConstraint::PrimaryKey { name: None, columns })
            }
            TableConstraint::Unique { columns, .. } => {
                push(&mut kept, &mut seen, TableConstraint::Unique { name: None, columns })
            }
            other => others.push(other), // FK / CHECK preserved
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
    kept.extend(others);
    t.table_constraints = kept;
}

/// Normalize each column's type + default to the introspection-comparable form
/// and clear the inline PK/unique flags (now lifted into constraints). Leaves
/// `inline_fk` and `comment` intact — callers that don't want them strip them.
fn normalize_column_types(t: &mut snapshot::TableSnapshot, enum_types: &HashMap<String, String>) {
    for c in &mut t.columns {
        c.data_type = canonical_type(&c.data_type, enum_types);
        c.default_value = c.default_value.as_deref().map(canonical_default);
        c.is_pk = false;
        c.is_unique = false;
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

/// Normalize a column default so a **parsed** literal and Postgres's
/// **introspected** round-trip of the same default compare equal.
///
/// Introspection reads defaults back through `pg_get_expr`, which re-emits them
/// in a canonical form that annotates the whole expression with an explicit cast:
/// a source `'{}'` comes back as `'{}'::text[]`, `'active'` as
/// `'active'::config.status`, `''` as `''::text`. Compared textually against the
/// source these look changed, so reconcile emits a redundant `SET DEFAULT` on
/// every run against an already-current DB (issue #5).
///
/// Strip a single trailing top-level `::type` cast plus surrounding whitespace so
/// both sides converge on the bare literal. Casts *inside* the expression are left
/// intact — `nextval('seq'::regclass)` and other function calls are untouched —
/// and a genuine type change is still caught by the column's `data_type` diff, so
/// erasing the cast here can't hide a real change.
fn canonical_default(raw: &str) -> String {
    strip_trailing_cast(raw.trim()).trim().to_string()
}

/// Remove a trailing top-level `::type` cast from a default expression. Tracks
/// single-quoted string literals (with `''` escapes) and parenthesis depth so a
/// `::` inside a string or a function call's arguments is never mistaken for the
/// outer cast. Returns the input unchanged when there is no such cast or the tail
/// after it isn't a plausible type name (guards against clipping an operator
/// expression like `'a'::text || 'b'`).
fn strip_trailing_cast(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut in_str = false;
    let mut depth: i32 = 0;
    let mut last_cast: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                // `''` inside a string is an escaped quote, not a terminator.
                if in_str && bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_str = !in_str;
            }
            b'(' if !in_str => depth += 1,
            b')' if !in_str => depth -= 1,
            b':' if !in_str && depth == 0 && bytes.get(i + 1) == Some(&b':') => {
                last_cast = Some(i);
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    match last_cast {
        Some(pos) if is_plausible_type(&s[pos + 2..]) => s[..pos].trim_end(),
        _ => s,
    }
}

/// Whether `tail` (the text after a top-level `::`) looks like a bare type name —
/// only the characters Postgres uses in type spellings: letters, digits, and
/// `_ . [ ] ( ) ,` plus spaces (`timestamp with time zone`) and `"` (quoted
/// identifiers). Anything else means the `::` was part of a larger expression, so
/// we must not strip it.
fn is_plausible_type(tail: &str) -> bool {
    let tail = tail.trim();
    !tail.is_empty()
        && tail
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " _.[](),\"".contains(c))
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

// ── Foreign-key convergence (issue #8) ──────────────────────

/// A foreign key's comparable shape — everything that defines it EXCEPT its
/// constraint name. Used to match a live FK (auto-named by Postgres) against the
/// design's inline/unnamed FK. Mirrors `schema_diff::normalize_fk`: `NO ACTION`
/// collapses to the default (`None`), and a `public` ref schema is treated the
/// same as an unqualified one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FkShape {
    columns: Vec<String>,
    ref_schema: Option<String>,
    ref_table: String,
    ref_columns: Vec<String>,
    on_delete: Option<FkAction>,
    on_update: Option<FkAction>,
}

/// The name-agnostic shape of an FK for cross-representation matching.
fn fk_shape(fk: &ForeignKey) -> FkShape {
    let norm_action = |a: Option<FkAction>| match a {
        Some(FkAction::NoAction) => None,
        other => other,
    };
    let ref_schema = match fk.ref_schema.as_deref() {
        None | Some(DEFAULT_SCHEMA) => None,
        Some(s) => Some(s.to_lowercase()),
    };
    FkShape {
        columns: fk.columns.clone(),
        ref_schema,
        ref_table: fk.ref_table.clone(),
        ref_columns: fk.ref_columns.clone(),
        on_delete: norm_action(fk.on_delete),
        on_update: norm_action(fk.on_update),
    }
}

/// All foreign keys on a table snapshot: inline column FKs + table-level FK
/// constraints (raw snapshots keep both; canonicalized ones have neither).
fn table_fks(t: &TableSnapshot) -> Vec<ForeignKey> {
    let mut out: Vec<ForeignKey> = t.columns.iter().filter_map(|c| c.inline_fk.clone()).collect();
    for con in &t.table_constraints {
        if let TableConstraint::ForeignKey(fk) = con {
            out.push(fk.clone());
        }
    }
    out
}

/// Converge foreign keys into an existing reconcile `plan` (issue #8).
///
/// Reconcile's [`canonicalize`] strips FKs from the snapshots the main diff
/// sees, so FK drift is handled here from the RAW (un-canonicalized) `live` and
/// `desired` snapshots. For every table present in BOTH sides:
/// - an FK the design declares but the live DB lacks is **added**
///   (`ADD FOREIGN KEY …`, non-destructive), and
/// - an FK the live DB has but the design dropped is **removed**
///   (`DROP CONSTRAINT <live-name>`, which sets `plan.destructive` so it is
///   gated behind `--allow-destructive`).
///
/// FKs match by [`FkShape`] (name-agnostic), so a live auto-named FK and the
/// design's inline unnamed FK of the same shape reconcile to no change — the
/// same matching the read-only `dbd diff` uses. An FK whose shape changed
/// (e.g. a different `ON DELETE`) is a drop of the old plus an add of the new.
/// Newly-added and pruned tables are skipped: their FKs ride along with the
/// `CREATE`/`DROP TABLE`.
pub fn plan_fk_convergence(plan: &mut ReconcilePlan, live: &Snapshot, desired: &Snapshot) {
    use crate::diff::{FieldChange, FieldDetail, FieldType};

    let live_by_name: HashMap<String, &TableSnapshot> = live
        .tables
        .iter()
        .map(|t| (format!("{}.{}", t.schema, t.name), t))
        .collect();

    for dt in &desired.tables {
        let qname = format!("{}.{}", dt.schema, dt.name);
        // Only tables present in both sides — new/pruned tables carry their FKs
        // via the full CREATE/DROP.
        let Some(lt) = live_by_name.get(&qname) else {
            continue;
        };

        let desired_fks = table_fks(dt);
        let live_fks = table_fks(lt);
        let desired_shapes: HashSet<FkShape> = desired_fks.iter().map(fk_shape).collect();
        let live_shapes: HashSet<FkShape> = live_fks.iter().map(fk_shape).collect();

        let mut changes: Vec<FieldChange> = Vec::new();

        // Drops first (destructive): a live FK the design no longer declares.
        // Use the live constraint's real name — that's what `DROP CONSTRAINT`
        // needs, and it's why FKs can't simply be name-stripped before diffing.
        for fk in live_fks.iter().filter(|fk| !desired_shapes.contains(&fk_shape(fk))) {
            let name = fk
                .name
                .clone()
                .unwrap_or_else(|| format!("fk:{}", fk.columns.join(",")));
            changes.push(FieldChange {
                field_name: name,
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop,
            });
            plan.destructive = true;
        }

        // Adds: a design FK missing from the live DB (unnamed → Postgres auto-names).
        for fk in desired_fks.iter().filter(|fk| !live_shapes.contains(&fk_shape(fk))) {
            changes.push(FieldChange {
                field_name: format!("fk:{}", fk.columns.join(",")),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::ForeignKey(fk.clone()),
                ))),
            });
        }

        if changes.is_empty() {
            continue;
        }

        let diff = MigrationDiff {
            entity_name: qname.clone(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(changes),
        };
        let sql = diff::generate_migration_sql(std::slice::from_ref(&diff));
        if sql.trim().is_empty() {
            continue;
        }
        merge_altered_sql(plan, &qname, sql);
    }
}

/// Append `sql` to the `altered` statement for `entity_name`, appending after any
/// existing ALTER SQL (so an FK add runs after the `ADD COLUMN` that created its
/// column), or pushing a new statement when the table had no other changes.
fn merge_altered_sql(plan: &mut ReconcilePlan, entity_name: &str, sql: String) {
    if let Some(stmt) = plan.altered.iter_mut().find(|s| s.entity_name == entity_name) {
        stmt.sql.push('\n');
        stmt.sql.push_str(&sql);
    } else {
        plan.altered.push(ReconcileStatement {
            entity_name: entity_name.to_string(),
            sql,
        });
    }
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

// ── Materialized-view convergence (Task 13) ──────────────────
//
// Postgres has no `CREATE OR REPLACE MATERIALIZED VIEW`, so converging a changed
// definition would mean DROP … CASCADE + recreate — which repopulates the matview
// and drops its dependents. dbd deliberately does NOT do that automatically: a
// dev-loop `reconcile` must never silently lose data or dependent objects.
// Instead reconcile CREATEs an absent matview, and for one that already exists it
// only *detects* drift and WARNS — leaving the live object untouched so the user
// drops it deliberately, after which `apply`/reconcile recreates it (snapshots
// exclude matviews, so migrations can't recreate one).
//
// Drift is detected by stamping a deterministic hash of the DESIGN onto the live
// object as a `dbd:hash=…` comment sentinel (matview comments are otherwise
// unused) at CREATE time, then comparing the stored hash to a freshly computed
// one on later runs. The hash is over the design, not Postgres's deparsed
// `pg_matviews.definition`, so it is exact and deparser-independent.

/// What a reconcile should do with one design materialized view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatviewAction {
    /// Absent live → create it and stamp the hash sentinel.
    Create,
    /// Live hash matches the design → nothing to do.
    Skip,
    /// Exists but the design differs (or it carries no dbd sentinel) → warn and
    /// leave it untouched. dbd never auto-drops a materialized view.
    Warn,
}

/// Decide the action for a design matview from its wanted hash and the live
/// sentinel state:
/// - `None` — the matview does not exist live → **Create**.
/// - `Some(Some(h))` — it exists with stored hash `h`; **Skip** iff `h == want`,
///   else **Warn** (definition drifted).
/// - `Some(None)` — it exists but carries no `dbd:hash` sentinel (created outside
///   dbd, or before this feature) → **Warn** (cannot verify its definition).
pub(crate) fn decide_matview_action(
    want_hash: &str,
    live: Option<Option<String>>,
) -> MatviewAction {
    match live {
        None => MatviewAction::Create,
        Some(Some(h)) if h == want_hash => MatviewAction::Skip,
        Some(_) => MatviewAction::Warn,
    }
}

/// Deterministic content hash of a design materialized view — the drift
/// sentinel. Covers exactly what would need a recreate: the normalized SELECT
/// body and the name-agnostic index key set. Uses SHA-256 (the codebase's
/// established persisted-hash algorithm — see [`crate::snapshot::checksum_of`]),
/// NOT `DefaultHasher`, so the stamped value is stable across toolchain versions
/// and never triggers spurious drift warnings after a Rust upgrade. Truncated to
/// a 16-hex-char prefix — ample to distinguish definitions for a drift signal.
pub(crate) fn matview_hash(entity: &Entity) -> String {
    use sha2::{Digest, Sha256};
    // Serialize the sorted index key set unambiguously so distinct sets can't
    // collide via concatenation (delimiters that can't occur in identifiers).
    let indexes = matview_index_keys(entity)
        .into_iter()
        .map(|(unique, cols)| format!("{unique}:{}", cols.join(",")))
        .collect::<Vec<_>>()
        .join(";");
    let mut hasher = Sha256::new();
    hasher.update(normalize_matview_body(entity).as_bytes());
    hasher.update(b"\x00indexes\x00");
    hasher.update(indexes.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// `COMMENT ON MATERIALIZED VIEW "s"."n" IS 'dbd:hash=<hash>';` for the sentinel.
/// `qualified` is `schema.name` (unqualified → `public`); single quotes in the
/// payload are doubled so the literal stays well-formed.
pub(crate) fn matview_hash_comment_sql(qualified: &str, hash: &str) -> String {
    let (schema, name) = qualified.split_once('.').unwrap_or((DEFAULT_SCHEMA, qualified));
    let payload = format!("dbd:hash={hash}").replace('\'', "''");
    format!("COMMENT ON MATERIALIZED VIEW \"{schema}\".\"{name}\" IS '{payload}';")
}

/// Extract the hash from a `dbd:hash=<hex>` comment, or `None` when the comment
/// is absent or carries no such sentinel. Inverse of [`matview_hash_comment_sql`]'s
/// payload; tolerant of trailing text after the hash token.
pub(crate) fn parse_dbd_hash(comment: Option<&str>) -> Option<String> {
    let rest = comment?.split("dbd:hash=").nth(1)?;
    let hash: String = rest.chars().take_while(char::is_ascii_alphanumeric).collect();
    (!hash.is_empty()).then_some(hash)
}

/// `CREATE MATERIALIZED VIEW … WITH DATA;` (+ any index statements, via
/// [`crate::emit::emit_entity`]) followed by the hash-sentinel comment.
pub(crate) fn matview_create_sql(entity: &Entity, hash: &str) -> String {
    let create = crate::emit::emit_entity(entity).unwrap_or_default();
    let comment = matview_hash_comment_sql(&qualified_matview_name(entity), hash);
    format!("{create}\n{comment}")
}

/// `schema.name` for an entity, defaulting the schema to `public`.
fn qualified_matview_name(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or(DEFAULT_SCHEMA);
    let name = entity.name.rsplit('.').next().unwrap_or(&entity.name);
    format!("{schema}.{name}")
}

/// Normalize a matview's `SELECT` body for a stable hash input: first `writes`
/// entry, trimmed, a single trailing `;` removed, internal whitespace collapsed
/// to single spaces, lowercased.
fn normalize_matview_body(e: &Entity) -> String {
    let raw = e.writes.first().map(String::as_str).unwrap_or_default();
    raw.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A matview's index set as name-agnostic `(unique, [lowercased columns])` keys.
/// `table_def = None` (introspected, index-less) and `Some { indexes: [] }`
/// (parsed, index-less) both yield the empty set, so that Task 8 asymmetry does
/// not perturb the hash.
fn matview_index_keys(e: &Entity) -> std::collections::BTreeSet<(bool, Vec<String>)> {
    e.table_def
        .as_ref()
        .map(|d| d.indexes.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|ix| {
            (
                ix.unique,
                ix.columns.iter().map(|c| c.name.to_lowercase()).collect(),
            )
        })
        .collect()
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

    /// Issue #8: the read-only diff builder retains foreign keys; the reconcile
    /// (canonicalize) builder strips them. This split is why `dbd diff` can now
    /// see FK drift while reconcile's apply-path diff still ignores it.
    #[test]
    fn raw_builder_keeps_fk_while_canonicalize_builder_strips_it() {
        use crate::entity::{ColumnDef, ForeignKey, TableComments, TableDef};

        let fk = ForeignKey {
            columns: vec!["org_id".to_string()],
            ref_table: "org".to_string(),
            ref_columns: vec!["id".to_string()],
            ..Default::default()
        };
        let mut entity = Entity::new(EntityType::Table, "public.users");
        entity.table_def = Some(TableDef {
            columns: vec![ColumnDef { inline_fk: Some(fk), ..col("org_id", "uuid") }],
            constraints: vec![],
            indexes: vec![],
            comments: TableComments::default(),
        });
        let entities = vec![entity];

        let raw = raw_snapshot_from_entities(&entities);
        assert!(raw.tables[0].columns[0].inline_fk.is_some(), "raw builder must retain the inline FK");

        let canon = snapshot_from_entities(&entities);
        assert!(canon.tables[0].columns[0].inline_fk.is_none(), "canonicalize builder must strip the inline FK");
    }

    // ── Foreign-key convergence (issue #8) ──────────────────

    fn ref_fk(name: Option<&str>, col: &str, ref_schema: Option<&str>, ref_table: &str, on_delete: Option<FkAction>) -> ForeignKey {
        ForeignKey {
            name: name.map(str::to_string),
            columns: vec![col.to_string()],
            ref_schema: ref_schema.map(str::to_string),
            ref_table: ref_table.to_string(),
            ref_columns: vec!["id".to_string()],
            on_delete,
            on_update: None,
        }
    }

    /// The design declares an inline FK the live DB lacks → non-destructive
    /// `ADD FOREIGN KEY` (Postgres auto-names it). This is the issue's core case:
    /// an FK dropped out from under the design, or added alongside a new column.
    #[test]
    fn fk_convergence_adds_missing_fk() {
        let desired = snap(vec![TableSnapshot {
            columns: vec![ColumnDef {
                inline_fk: Some(ref_fk(None, "parent_id", Some("app"), "parents", None)),
                ..col("parent_id", "uuid")
            }],
            ..table("app", "children", vec![])
        }]);
        let live = snap(vec![table("app", "children", vec![col("parent_id", "uuid")])]);

        let mut plan = ReconcilePlan::default();
        plan_fk_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "expected one altered table; got {plan:?}");
        assert_eq!(plan.altered[0].entity_name, "app.children");
        assert!(
            plan.altered[0].sql.contains("ADD FOREIGN KEY (parent_id) REFERENCES app.parents(id)"),
            "expected unnamed ADD FOREIGN KEY; got: {}",
            plan.altered[0].sql
        );
        assert!(!plan.destructive, "adding an FK is not destructive");
    }

    /// The live DB has an FK the design dropped → `DROP CONSTRAINT <live-name>`,
    /// gated as destructive.
    #[test]
    fn fk_convergence_drops_extra_fk_is_destructive() {
        let mut live_t = table("app", "children", vec![col("parent_id", "uuid")]);
        live_t.table_constraints.push(TableConstraint::ForeignKey(ref_fk(
            Some("children_parent_id_fkey"), "parent_id", Some("app"), "parents", None,
        )));
        let live = snap(vec![live_t]);
        let desired = snap(vec![table("app", "children", vec![col("parent_id", "uuid")])]);

        let mut plan = ReconcilePlan::default();
        plan_fk_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        assert!(
            plan.altered[0].sql.contains("DROP CONSTRAINT children_parent_id_fkey"),
            "expected DROP CONSTRAINT with the live name; got: {}",
            plan.altered[0].sql
        );
        assert!(plan.destructive, "dropping an FK is destructive");
    }

    /// A live auto-named FK and the design's inline unnamed FK of the same shape
    /// reconcile to no change — no phantom drop/add churn on an in-sync DB.
    #[test]
    fn fk_convergence_in_sync_is_no_change() {
        let mut live_t = table("app", "children", vec![col("parent_id", "uuid")]);
        live_t.table_constraints.push(TableConstraint::ForeignKey(ref_fk(
            Some("children_parent_id_fkey"), "parent_id", Some("app"), "parents", Some(FkAction::NoAction),
        )));
        let live = snap(vec![live_t]);
        let desired = snap(vec![TableSnapshot {
            columns: vec![ColumnDef {
                inline_fk: Some(ref_fk(None, "parent_id", Some("app"), "parents", None)),
                ..col("parent_id", "uuid")
            }],
            ..table("app", "children", vec![])
        }]);

        let mut plan = ReconcilePlan::default();
        plan_fk_convergence(&mut plan, &live, &desired);

        assert!(plan.is_empty() && !plan.destructive, "in-sync FK must reconcile to no change; got {plan:?}");
    }

    /// A changed FK action (design adds `ON DELETE CASCADE`) drops the old and
    /// adds the new — Postgres can't alter FK actions in place.
    #[test]
    fn fk_convergence_changed_action_replaces() {
        let mut live_t = table("app", "children", vec![col("parent_id", "uuid")]);
        live_t.table_constraints.push(TableConstraint::ForeignKey(ref_fk(
            Some("children_parent_id_fkey"), "parent_id", Some("app"), "parents", None,
        )));
        let live = snap(vec![live_t]);
        let desired = snap(vec![TableSnapshot {
            columns: vec![ColumnDef {
                inline_fk: Some(ref_fk(None, "parent_id", Some("app"), "parents", Some(FkAction::Cascade))),
                ..col("parent_id", "uuid")
            }],
            ..table("app", "children", vec![])
        }]);

        let mut plan = ReconcilePlan::default();
        plan_fk_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("DROP CONSTRAINT children_parent_id_fkey"), "must drop old FK; got: {sql}");
        assert!(sql.contains("ADD FOREIGN KEY (parent_id) REFERENCES app.parents(id) ON DELETE CASCADE"),
            "must add the new FK with the action; got: {sql}");
        assert!(plan.destructive, "replacing an FK drops the old one → destructive");
    }

    /// FK add SQL is appended AFTER any existing ALTER (e.g. the `ADD COLUMN`
    /// that created the FK's column), so the column exists before the FK is added.
    #[test]
    fn fk_convergence_appends_after_existing_alter() {
        let desired = snap(vec![TableSnapshot {
            columns: vec![ColumnDef {
                inline_fk: Some(ref_fk(None, "parent_id", Some("app"), "parents", None)),
                ..col("parent_id", "uuid")
            }],
            ..table("app", "children", vec![])
        }]);
        let live = snap(vec![table("app", "children", vec![col("parent_id", "uuid")])]);

        let mut plan = ReconcilePlan::default();
        plan.altered.push(ReconcileStatement {
            entity_name: "app.children".to_string(),
            sql: "ALTER TABLE app.children ADD COLUMN parent_id uuid;".to_string(),
        });
        plan_fk_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "FK merges into the existing statement");
        let sql = &plan.altered[0].sql;
        let col_pos = sql.find("ADD COLUMN parent_id").expect("ADD COLUMN present");
        let fk_pos = sql.find("ADD FOREIGN KEY").expect("ADD FOREIGN KEY present");
        assert!(col_pos < fk_pos, "ADD COLUMN must precede ADD FOREIGN KEY; got: {sql}");
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

    fn col_default(name: &str, ty: &str, default: &str) -> ColumnDef {
        ColumnDef {
            default_value: Some(default.to_string()),
            nullable: false,
            ..col(name, ty)
        }
    }

    /// Issue #5: a source default (`'{}'`) and Postgres's introspected round-trip
    /// of the same default (`'{}'::text[]`) must canonicalize equal, so an
    /// already-current DB reconciles to an empty plan instead of re-emitting a
    /// no-op `SET DEFAULT` every run.
    #[test]
    fn canonicalize_matches_default_across_introspected_cast() {
        // Parsed (desired): bare literal from the `.ddl` source.
        let mut desired = snap(vec![table(
            "public",
            "org",
            vec![col_default("org_slugs", "text[]", "'{}'")],
        )]);
        // Introspected (live): pg_get_expr appends the type cast.
        let mut live = snap(vec![table(
            "public",
            "org",
            vec![col_default("org_slugs", "text[]", "'{}'::text[]")],
        )]);

        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert!(
            plan.is_empty() && !plan.destructive,
            "unchanged array default must reconcile to no changes; got {plan:?}"
        );
    }

    /// A genuinely changed default still produces a `SET DEFAULT` — normalization
    /// strips only the redundant cast, never a real change.
    #[test]
    fn canonicalize_still_detects_real_default_change() {
        let mut desired = snap(vec![table(
            "public",
            "counter",
            vec![col_default("n", "integer", "1")],
        )]);
        let mut live = snap(vec![table(
            "public",
            "counter",
            vec![col_default("n", "integer", "0")],
        )]);

        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.altered.len(), 1, "changed default must be planned; got {plan:?}");
        assert!(plan.altered[0].sql.contains("SET DEFAULT 1"));
    }

    #[test]
    fn canonical_default_strips_trailing_cast() {
        assert_eq!(canonical_default("'{}'::text[]"), "'{}'");
        assert_eq!(canonical_default("''::text"), "''");
        assert_eq!(canonical_default("'active'::config.status"), "'active'");
        assert_eq!(
            canonical_default("'2020-01-01'::timestamp with time zone"),
            "'2020-01-01'"
        );
        assert_eq!(canonical_default("0::numeric(10,2)"), "0");
        assert_eq!(canonical_default("  'x' :: text "), "'x'");
    }

    /// `normalize_common` does the representation normalization (types, defaults,
    /// enum qualification, PK/unique lifting) but must PRESERVE the attributes
    /// reconcile later strips: FK/CHECK constraints, indexes, and column comments.
    #[test]
    fn normalize_common_preserves_fk_check_index_comment() {
        use crate::entity::{ForeignKey, IndexColumn, IndexDef};
        let mut snap = snap(vec![TableSnapshot {
            name: "orders".to_string(),
            schema: "public".to_string(),
            columns: vec![ColumnDef { comment: Some("the total".to_string()), ..col("total", "int4") }],
            indexes: vec![IndexDef {
                name: Some("orders_total_idx".to_string()),
                columns: vec![IndexColumn { name: "total".to_string(), order: None }],
                unique: false,
                index_type: None,
            }],
            table_constraints: vec![
                TableConstraint::ForeignKey(ForeignKey {
                    name: Some("orders_cust_fk".to_string()),
                    columns: vec!["cust_id".to_string()],
                    ref_schema: None,
                    ref_table: "customers".to_string(),
                    ref_columns: vec!["id".to_string()],
                    on_delete: None,
                    on_update: None,
                }),
                TableConstraint::Check { name: Some("ck".to_string()), expression: "total > 0".to_string() },
            ],
        }]);
        normalize_common(&mut snap);
        let t = &snap.tables[0];
        assert_eq!(t.columns[0].data_type, "integer", "types must still be normalized");
        assert_eq!(t.columns[0].comment.as_deref(), Some("the total"), "comment preserved");
        assert_eq!(t.indexes.len(), 1, "indexes preserved");
        assert!(t.table_constraints.iter().any(|c| matches!(c, TableConstraint::ForeignKey(_))), "FK preserved");
        assert!(t.table_constraints.iter().any(|c| matches!(c, TableConstraint::Check { .. })), "CHECK preserved");
    }

    #[test]
    fn canonical_default_leaves_non_casts_intact() {
        // No top-level cast → unchanged.
        assert_eq!(canonical_default("now()"), "now()");
        assert_eq!(canonical_default("0"), "0");
        assert_eq!(canonical_default("false"), "false");
        // Cast lives inside the function args, not on the whole expression.
        assert_eq!(
            canonical_default("nextval('app.seq'::regclass)"),
            "nextval('app.seq'::regclass)"
        );
        // `::` embedded in a string literal must not be treated as a cast.
        assert_eq!(canonical_default("'a::b'"), "'a::b'");
    }

    // ── Materialized-view convergence (Task 13) ──────────────

    fn mv(name: &str, body: &str) -> Entity {
        let mut e = Entity::new(EntityType::MaterializedView, name);
        e.writes = vec![body.into()];
        e
    }

    /// The hash is deterministic: the same design entity hashes identically on
    /// repeated calls (SHA-256, so it is also stable across processes and
    /// toolchain versions — the property the on-disk sentinel relies on).
    #[test]
    fn matview_hash_is_deterministic() {
        let e = mv("a.m", "SELECT 1 AS x");
        assert_eq!(matview_hash(&e), matview_hash(&e));
    }

    /// A changed body yields a different hash.
    #[test]
    fn matview_hash_changes_with_body() {
        assert_ne!(
            matview_hash(&mv("a.m", "SELECT 1 AS x")),
            matview_hash(&mv("a.m", "SELECT 2 AS x")),
        );
    }

    /// Adding an index yields a different hash.
    #[test]
    fn matview_hash_changes_with_index() {
        use crate::entity::{IndexColumn, IndexDef, TableDef};
        let plain = mv("a.m", "SELECT 1 AS x");
        let mut indexed = mv("a.m", "SELECT 1 AS x");
        indexed.table_def = Some(TableDef {
            columns: vec![],
            constraints: vec![],
            indexes: vec![IndexDef {
                name: Some("m_x_idx".into()),
                columns: vec![IndexColumn { name: "x".into(), order: None }],
                unique: true,
                index_type: None,
            }],
            comments: Default::default(),
        });
        assert_ne!(matview_hash(&plain), matview_hash(&indexed));
    }

    /// Task 8 asymmetry: an index-less matview parsed as
    /// `Some(TableDef { indexes: [] })` and introspected as `table_def = None`
    /// must hash IDENTICALLY (bodies equal) — else every reconcile would recreate.
    #[test]
    fn matview_hash_indexless_none_and_some_empty_are_equal() {
        use crate::entity::TableDef;
        let mut some_empty = mv("a.m", "SELECT 1 AS x");
        some_empty.table_def = Some(TableDef {
            columns: vec![],
            constraints: vec![],
            indexes: vec![],
            comments: Default::default(),
        });
        let none = mv("a.m", "SELECT 1 AS x"); // table_def = None
        assert_eq!(
            matview_hash(&some_empty),
            matview_hash(&none),
            "None and Some(indexes:[]) must produce the same hash"
        );
    }

    /// The sentinel comment SQL and `parse_dbd_hash` round-trip: the payload
    /// stored (`dbd:hash=<hex>`) parses back to the same hash.
    #[test]
    fn matview_hash_comment_sql_roundtrips() {
        let sql = matview_hash_comment_sql("a.m", "deadbeefcafe0001");
        assert!(
            sql.contains("COMMENT ON MATERIALIZED VIEW \"a\".\"m\" IS 'dbd:hash=deadbeefcafe0001';"),
            "got: {sql}"
        );
        // obj_description returns the payload (without the surrounding quotes).
        assert_eq!(
            parse_dbd_hash(Some("dbd:hash=deadbeefcafe0001")),
            Some("deadbeefcafe0001".to_string())
        );
    }

    /// `parse_dbd_hash` returns `None` for a missing comment or a non-sentinel one.
    #[test]
    fn parse_dbd_hash_none_cases() {
        assert_eq!(parse_dbd_hash(None), None);
        assert_eq!(parse_dbd_hash(Some("just a user comment")), None);
        assert_eq!(parse_dbd_hash(Some("dbd:hash=")), None, "empty hash → None");
    }

    /// A create carries both the `CREATE MATERIALIZED VIEW … WITH DATA` and the
    /// hash sentinel comment.
    #[test]
    fn matview_create_sql_has_create_and_comment() {
        let sql = matview_create_sql(&mv("a.m", "SELECT 1 AS x"), "abc123");
        assert!(sql.contains("CREATE MATERIALIZED VIEW IF NOT EXISTS \"a\".\"m\" AS SELECT 1 AS x WITH DATA;"));
        assert!(sql.contains("COMMENT ON MATERIALIZED VIEW \"a\".\"m\" IS 'dbd:hash=abc123';"));
    }

    /// The decision core, hash-based, over all four states: dbd never auto-drops,
    /// so a drifted or unstamped matview is **Warn**, not recreate.
    #[test]
    fn decide_matview_action_covers_all_branches() {
        // Absent → Create.
        assert_eq!(decide_matview_action("h", None), MatviewAction::Create);
        // Present, matching hash → Skip.
        assert_eq!(
            decide_matview_action("h", Some(Some("h".into()))),
            MatviewAction::Skip
        );
        // Present, different hash → Warn (definition drifted).
        assert_eq!(
            decide_matview_action("h", Some(Some("other".into()))),
            MatviewAction::Warn
        );
        // Present, no dbd sentinel → Warn (cannot verify).
        assert_eq!(decide_matview_action("h", Some(None)), MatviewAction::Warn);
    }
}
