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
    /// Materialized views present in the design but absent from the DB. Reconcile
    /// CREATEs these (Postgres has no `CREATE OR REPLACE MATERIALIZED VIEW`);
    /// carried separately from `added` because they use a different code path
    /// (`matview_create_sql` + hash sentinel) and are detected during `--dry-run`
    /// so the preview can list them.
    pub matview_creates: Vec<String>,
    /// Risky-change advisories (type changes, possible renames, enum value drops,
    /// orphaned enums that are not auto-dropped).
    pub warnings: Vec<String>,
    /// Whether the plan makes a change gated behind `--allow-destructive`:
    /// dropping a column or constraint from an existing table (data loss), or
    /// dropping a foreign key (see [`plan_fk_convergence`]) or a secondary index
    /// (see [`plan_index_convergence`]). Whole-table drops are separate — see
    /// [`Self::dropped`] — and gated by pruning, not by this flag.
    pub destructive: bool,
}

impl ReconcilePlan {
    /// No structural changes to make.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.altered.is_empty()
            && self.dropped.is_empty()
            && self.matview_creates.is_empty()
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
/// Reconcile does not diff check constraints on existing tables here — their
/// introspected/parsed forms differ too much to compare reliably (create them
/// via the initial `CREATE`, or use snapshots) — so after the shared
/// [`normalize_common`] pass this drops them from the diff entirely, along with
/// column comments and inline FK. **Foreign keys and indexes are handled
/// separately** by [`plan_fk_convergence`] and [`plan_index_convergence`] from
/// the raw (un-canonicalized) snapshots, so they are stripped here too — keeping
/// this column/PK/unique diff free of FK and index noise.
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
///
/// Case is then folded outside quoted text ([`fold_unquoted_case`]), because
/// `pg_get_expr` re-spells keywords and function names in Postgres's own casing
/// rather than the author's: `current_date` comes back `CURRENT_DATE`, `NOW()`
/// comes back `now()`, `coalesce(…)` comes back `COALESCE(…)`. Comparing those
/// textually made reconcile emit a `SET DEFAULT` that Postgres immediately
/// re-spelled, so the next diff reported the same change — forever.
fn canonical_default(raw: &str) -> String {
    fold_unquoted_case(strip_trailing_cast(raw.trim()).trim())
}

/// Lowercase a default expression *outside* string literals and quoted
/// identifiers, so two spellings of the same keyword compare equal.
///
/// Unquoted SQL is case-insensitive, so folding it changes no meaning. Quoted text
/// is not: `'Active'` and `'active'` are different defaults and `"MyCol"` is a
/// different column from `"mycol"`, so both are copied through untouched — the
/// canonical form is emitted as DDL (`SET DEFAULT …`), and folding a literal would
/// silently change the value written to the database.
///
/// A dollar-quoted expression (`$$Hello$$`) is returned unchanged rather than
/// risking a fold inside its body. `pg_get_expr` never emits that form, so at
/// worst such a default keeps reading as drift — the safe direction.
fn fold_unquoted_case(s: &str) -> String {
    if s.contains('$') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    // The delimiter of the quoted run currently being copied verbatim. Postgres
    // escapes a quote by doubling it, which needs no special handling here: the
    // first closes the run and the second immediately reopens it.
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        match quote {
            Some(delim) => {
                out.push(ch);
                if ch == delim {
                    quote = None;
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    out.push(ch);
                } else {
                    out.extend(ch.to_lowercase());
                }
            }
        }
    }
    out
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

// ── CHECK convergence ───────────────────────────────────────

/// A table's CHECK constraints, keyed by canonical expression.
///
/// Postgres auto-names every CHECK and the design usually leaves inline ones
/// unnamed, so the expression is the only stable identity — canonicalized via
/// [`crate::sql_expr`] so an authored `status in ('a','b')` matches the
/// `status = ANY (ARRAY['a'::text,'b'::text])` `pg_get_constraintdef` returns.
///
/// An expression that will not canonicalize keeps its raw text as the key, so it
/// compares only against an identical spelling: it may read as drift, but it can
/// never be mistaken for a different constraint and dropped.
fn table_checks(t: &TableSnapshot) -> Vec<(String, &TableConstraint)> {
    t.table_constraints
        .iter()
        .filter_map(|con| match con {
            TableConstraint::Check { expression, .. } => {
                let key = crate::sql_expr::canonicalize_predicate(expression)
                    .unwrap_or_else(|| expression.clone());
                Some((key, con))
            }
            _ => None,
        })
        .collect()
}

/// Converge CHECK constraints into an existing reconcile `plan`.
///
/// Reconcile's [`canonicalize`] strips CHECKs from the snapshots the main diff
/// sees, and until now nothing put them back — so reconcile was blind to them and
/// `--allow-destructive` silently left every CHECK the read-only `dbd diff`
/// flagged in place. This closes that gap the same way [`plan_fk_convergence`]
/// does for foreign keys, from the RAW (un-canonicalized) snapshots. For every
/// table present in BOTH sides:
/// - a CHECK the design declares but the live DB lacks is **added**
///   (`ADD [CONSTRAINT name] CHECK (…)`, non-destructive), and
/// - a CHECK the live DB has but the design dropped is **removed** by its real
///   live name (`DROP CONSTRAINT <live-name>`, setting `plan.destructive` so it
///   is gated behind `--allow-destructive`).
///
/// CHECKs match by canonical expression, never by name, so a live auto-named
/// constraint and the design's unnamed inline one reconcile to no change. New and
/// pruned tables are skipped: their CHECKs ride along with the `CREATE`/`DROP
/// TABLE`.
pub fn plan_check_convergence(plan: &mut ReconcilePlan, live: &Snapshot, desired: &Snapshot) {
    use crate::diff::{FieldChange, FieldDetail, FieldType};

    let live_by_name: HashMap<String, &TableSnapshot> = live
        .tables
        .iter()
        .map(|t| (format!("{}.{}", t.schema, t.name), t))
        .collect();

    for dt in &desired.tables {
        let qname = format!("{}.{}", dt.schema, dt.name);
        let Some(lt) = live_by_name.get(&qname) else {
            continue;
        };

        let desired_checks = table_checks(dt);
        let live_checks = table_checks(lt);
        let desired_keys: HashSet<&str> = desired_checks.iter().map(|(k, _)| k.as_str()).collect();
        let live_keys: HashSet<&str> = live_checks.iter().map(|(k, _)| k.as_str()).collect();

        let mut changes: Vec<FieldChange> = Vec::new();

        // Drops first (destructive). `DROP CONSTRAINT` needs the live constraint's
        // real name — the reason CHECKs can't simply be name-stripped before
        // diffing, and why the read-only diff path used to emit an unusable
        // `DROP CONSTRAINT ck:<expression>`.
        for (key, con) in live_checks.iter().filter(|(k, _)| !desired_keys.contains(k.as_str())) {
            let TableConstraint::Check { name, .. } = con else {
                continue;
            };
            // Without a name there is no statement to issue; warn rather than
            // emit SQL that cannot run.
            let Some(name) = name else {
                plan.warnings.push(format!(
                    "CHECK ({key}) on {qname} is not in the design but has no constraint name — drop it manually"
                ));
                continue;
            };
            changes.push(FieldChange {
                field_name: format!("\"{name}\""),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop,
            });
            plan.destructive = true;
        }

        // Adds: a design CHECK the live DB lacks. An unnamed one is emitted
        // without a `CONSTRAINT` clause so Postgres auto-names it.
        for (key, con) in desired_checks.iter().filter(|(k, _)| !live_keys.contains(k.as_str())) {
            changes.push(FieldChange {
                field_name: format!("ck:{key}"),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint((*con).clone()))),
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

// ── Column-comment convergence ──────────────────────────────

/// Converge column comments into an existing reconcile `plan`.
///
/// Reconcile's [`canonicalize`] clears `ColumnDef::comment`, and until now nothing
/// put it back — so reconcile was blind to comment drift that the read-only
/// `dbd diff` reported on every run. This closes that gap the same way
/// [`plan_check_convergence`] does for CHECKs, from the RAW (un-canonicalized)
/// snapshots. For every column present in BOTH sides whose comment differs, emit
/// `COMMENT ON COLUMN … IS '…'`, or `IS NULL` when the design has no comment and
/// the live database does.
///
/// Comments are metadata, so a change is never destructive. Columns only on one
/// side are skipped: a new column's comment rides along with its `ADD COLUMN`, and
/// a dropped column takes its comment with it.
///
/// Table-level comments are not converged here because `TableSnapshot` does not
/// model them at all — they are invisible to both this pass and `dbd diff`, so
/// they cannot drift between the two.
pub fn plan_comment_convergence(plan: &mut ReconcilePlan, live: &Snapshot, desired: &Snapshot) {
    let live_by_name: HashMap<String, &TableSnapshot> = live
        .tables
        .iter()
        .map(|t| (format!("{}.{}", t.schema, t.name), t))
        .collect();

    for dt in &desired.tables {
        let qname = format!("{}.{}", dt.schema, dt.name);
        let Some(lt) = live_by_name.get(&qname) else {
            continue;
        };
        let live_comments: HashMap<&str, Option<&String>> = lt
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.comment.as_ref()))
            .collect();

        let mut lines: Vec<String> = Vec::new();
        for dc in &dt.columns {
            // Absent from the live table → its comment comes with the ADD COLUMN.
            let Some(live_comment) = live_comments.get(dc.name.as_str()) else {
                continue;
            };
            if *live_comment == dc.comment.as_ref() {
                continue;
            }
            lines.push(crate::emit::emit_column_comment_sql(
                &format!("\"{}\".\"{}\"", dt.schema, dt.name),
                &dc.name,
                dc.comment.as_deref(),
            ));
        }

        if lines.is_empty() {
            continue;
        }
        merge_altered_sql(plan, &qname, lines.join("\n"));
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

// ── Index convergence (issue #12) ───────────────────────────

/// Converge secondary indexes into an existing reconcile `plan`.
///
/// Reconcile's [`canonicalize`] strips indexes from the snapshots the main diff
/// sees, so — like foreign keys (issue #8) — index drift is handled here from
/// the RAW (un-canonicalized) `live` and `desired` snapshots. For every table
/// present in BOTH sides:
/// - an index the design declares but the live DB lacks is **added**
///   (`CREATE [UNIQUE] INDEX IF NOT EXISTS …`, non-destructive), and
/// - an index the live DB has but the design dropped is **removed**
///   (`DROP INDEX IF EXISTS …`, which sets `plan.destructive` so it is gated
///   behind `--allow-destructive`).
///
/// Indexes match by [`IndexShape`] (name-agnostic: unique flag, access method,
/// and ordered columns), so a live index and a design index of the same shape
/// under different names reconcile to no change — mirroring FK convergence. An
/// index whose shape changed (columns, uniqueness, or method) is a drop of the
/// old plus an add of the new; Postgres can't alter those in place.
///
/// Indexes that merely back a PRIMARY KEY / UNIQUE constraint are excluded on
/// both sides (introspection reports them; the parsed design does not), matching
/// `schema_diff::normalize_for_diff`. New and pruned tables are skipped — their
/// indexes ride along with the `CREATE`/`DROP TABLE`.
pub fn plan_index_convergence(plan: &mut ReconcilePlan, live: &Snapshot, desired: &Snapshot) {
    let live_by_name: HashMap<String, &TableSnapshot> = live
        .tables
        .iter()
        .map(|t| (format!("{}.{}", t.schema, t.name), t))
        .collect();

    for dt in &desired.tables {
        let qname = format!("{}.{}", dt.schema, dt.name);
        // Only tables present in both sides — new/pruned tables carry their
        // indexes via the full CREATE/DROP TABLE.
        let Some(lt) = live_by_name.get(&qname) else {
            continue;
        };

        let desired_ix = secondary_indexes(dt);
        let live_ix = secondary_indexes(lt);
        let desired_shapes: HashSet<IndexShape> = desired_ix.iter().map(|i| index_shape(i)).collect();
        let live_shapes: HashSet<IndexShape> = live_ix.iter().map(|i| index_shape(i)).collect();

        let mut lines: Vec<String> = Vec::new();

        // Drops first (destructive): a live index the design no longer declares.
        // Use the live index's real name — that's what `DROP INDEX` needs. An
        // index lives in its table's schema, so qualify the drop with it. Drops
        // precede adds so a shape-change on a shared name doesn't collide.
        for ix in live_ix.iter().filter(|ix| !desired_shapes.contains(&index_shape(ix))) {
            if let Some(name) = &ix.name {
                lines.push(format!("DROP INDEX IF EXISTS \"{}\".\"{}\";", dt.schema, name));
                plan.destructive = true;
            }
        }

        // Adds: a design index missing from the live DB. Rendered via the same
        // `emit::emit_index_sql` helper the initial CREATE uses (idempotent
        // `IF NOT EXISTS`, correct `USING <method>`), so a GIN/GiST index
        // converges as its real access method, not a plain btree.
        let qtable = format!("\"{}\".\"{}\"", dt.schema, dt.name);
        for ix in desired_ix.iter().filter(|ix| !live_shapes.contains(&index_shape(ix))) {
            lines.push(crate::emit::emit_index_sql(ix, &qtable, &dt.name, true));
        }

        if lines.is_empty() {
            continue;
        }
        merge_altered_sql(plan, &qname, lines.join("\n"));
    }
}

/// A secondary index's comparable shape — everything that defines it EXCEPT its
/// name. Used to match a live index against the design's, so same-shape indexes
/// under different names reconcile to no change.
///
/// Every field of [`crate::entity::IndexDef`] except the name is represented,
/// deliberately: a shape that ignores an attribute declares two different indexes
/// equal, and reconcile then leaves real drift in place — or, worse, matches an
/// index it is about to recreate differently and churns a `DROP`/`CREATE` pair on
/// every run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IndexShape {
    unique: bool,
    method: String,
    columns: Vec<IndexColumnShape>,
    predicate: Option<String>,
    include: Vec<String>,
    nulls_not_distinct: bool,
    with_options: Vec<(String, String)>,
}

/// One key entry's shape: the column name or expression text, plus every
/// modifier that changes which queries the index can answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IndexColumnShape {
    name: String,
    is_expression: bool,
    descending: bool,
    nulls_first: Option<bool>,
    opclass: Option<String>,
}

/// The name-agnostic shape of an index for cross-representation matching.
fn index_shape(ix: &crate::entity::IndexDef) -> IndexShape {
    use crate::entity::SortOrder;
    // Collapse the default spellings (`using btree`, `asc`) first, so the shape
    // of an authored index matches the shape of the introspected one.
    let mut ix = ix.clone();
    crate::schema_diff::normalize_index(&mut ix);

    let columns = ix
        .columns
        .iter()
        .map(|c| IndexColumnShape {
            // An expression is case-significant (it can contain string literals);
            // a column name is not.
            name: if c.is_expression { c.name.clone() } else { c.name.to_lowercase() },
            is_expression: c.is_expression,
            descending: matches!(c.order, Some(SortOrder::Desc)),
            nulls_first: c.nulls_first,
            opclass: c.opclass.as_ref().map(|o| o.to_lowercase()),
        })
        .collect();
    IndexShape {
        unique: ix.unique,
        method: ix.index_type.as_ref().map_or("btree".to_string(), |t| t.amname().to_string()),
        columns,
        predicate: ix.predicate.clone(),
        include: ix.include.iter().map(|c| c.to_lowercase()).collect(),
        nulls_not_distinct: ix.nulls_not_distinct,
        // BTreeMap iteration is already name-ordered, so an authored and a
        // `reloptions` ordering hash the same.
        with_options: ix.with_options.into_iter().collect(),
    }
}

/// The column-name sets (ordered, lowercased) that a table's PRIMARY KEY / UNIQUE
/// constraints cover — from both inline column flags and table-level constraints.
/// Used to drop PK/UNIQUE-backing indexes from index convergence.
///
/// A column's `is_pk` flag only contributes a single-column set when the table has
/// NO table-level PRIMARY KEY, exactly as [`lift_pk_unique_keep_others`] treats it
/// and for the same reason: the SQL parser emits a composite `primary key (a, b)`
/// BOTH as a table constraint AND as an `is_pk` flag on each member column. Taking
/// those flags at face value invents backing sets `[a]` and `[b]` that no index
/// backs, which suppressed a declared single-column index on a composite-PK member
/// (`create index on memory_links(child_id)`) from convergence — so `dbd diff` kept
/// asking for an index reconcile refused to create.
fn pk_unique_col_sets(t: &TableSnapshot) -> HashSet<Vec<String>> {
    let mut sets: HashSet<Vec<String>> = HashSet::new();
    let has_table_pk = t
        .table_constraints
        .iter()
        .any(|c| matches!(c, TableConstraint::PrimaryKey { .. }));
    for c in &t.columns {
        if (c.is_pk && !has_table_pk) || c.is_unique {
            sets.insert(vec![c.name.to_lowercase()]);
        }
    }
    for con in &t.table_constraints {
        match con {
            TableConstraint::PrimaryKey { columns, .. } | TableConstraint::Unique { columns, .. } => {
                sets.insert(columns.iter().map(|s| s.to_lowercase()).collect());
            }
            _ => {}
        }
    }
    sets
}

/// A table's secondary indexes: every index EXCEPT those merely backing a
/// PRIMARY KEY / UNIQUE constraint (introspection reports those, the parsed
/// design does not — matching by covered columns). Mirrors the suppression in
/// `schema_diff::normalize_for_diff`, including its rule that a partial or
/// expression index is always a real index, never constraint backing.
fn secondary_indexes(t: &TableSnapshot) -> Vec<&crate::entity::IndexDef> {
    let backing = pk_unique_col_sets(t);
    t.indexes
        .iter()
        .filter(|ix| {
            let lowered = crate::entity::IndexDef {
                columns: ix
                    .columns
                    .iter()
                    .map(|c| crate::entity::IndexColumn {
                        name: c.name.to_lowercase(),
                        ..c.clone()
                    })
                    .collect(),
                ..(*ix).clone()
            };
            !crate::schema_diff::backs_a_constraint(&lowered, &backing)
        })
        .collect()
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

    // ── Column-comment convergence ───────────────────────────

    fn commented(name: &str, comment: Option<&str>) -> ColumnDef {
        ColumnDef { comment: comment.map(str::to_string), ..col(name, "text") }
    }

    fn docs_with(columns: Vec<ColumnDef>) -> Snapshot {
        snap(vec![table("app", "docs", columns)])
    }

    /// The design sets a comment the live column lacks → `COMMENT ON COLUMN`.
    #[test]
    fn comment_convergence_adds_missing_comment() {
        let live = docs_with(vec![commented("title", None)]);
        let desired = docs_with(vec![commented("title", Some("Display name"))]);

        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        assert_eq!(
            plan.altered[0].sql,
            "COMMENT ON COLUMN \"app\".\"docs\".\"title\" IS 'Display name';"
        );
        assert!(!plan.destructive, "a comment is metadata, never destructive");
    }

    /// A changed comment is overwritten in place.
    #[test]
    fn comment_convergence_updates_changed_comment() {
        let live = docs_with(vec![commented("title", Some("old text"))]);
        let desired = docs_with(vec![commented("title", Some("new text"))]);

        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(&mut plan, &live, &desired);

        assert!(plan.altered[0].sql.contains("IS 'new text';"), "got: {}", plan.altered[0].sql);
    }

    /// The design dropped the comment → `IS NULL`, which is how Postgres removes
    /// one. Emitting nothing would leave the live comment in place and the next
    /// diff would report it again.
    #[test]
    fn comment_convergence_clears_dropped_comment() {
        let live = docs_with(vec![commented("title", Some("stale text"))]);
        let desired = docs_with(vec![commented("title", None)]);

        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(&mut plan, &live, &desired);

        assert_eq!(
            plan.altered[0].sql,
            "COMMENT ON COLUMN \"app\".\"docs\".\"title\" IS NULL;"
        );
    }

    /// Matching comments reconcile to no change — the convergence property.
    #[test]
    fn comment_convergence_in_sync_is_no_change() {
        let same = || docs_with(vec![commented("title", Some("Display name"))]);

        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(&mut plan, &same(), &same());

        assert!(plan.is_empty() && !plan.destructive, "got {plan:?}");

        // Both sides commentless is equally a no-change.
        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(
            &mut plan,
            &docs_with(vec![commented("title", None)]),
            &docs_with(vec![commented("title", None)]),
        );
        assert!(plan.is_empty(), "got {plan:?}");
    }

    /// A quote in the comment text must be escaped, or the statement won't parse.
    #[test]
    fn comment_convergence_escapes_quotes() {
        let live = docs_with(vec![commented("title", None)]);
        let desired = docs_with(vec![commented("title", Some("the project's root"))]);

        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(&mut plan, &live, &desired);

        assert!(
            plan.altered[0].sql.contains("IS 'the project''s root';"),
            "got: {}",
            plan.altered[0].sql
        );
    }

    /// A column only in the design gets its comment from the `ADD COLUMN`, and a
    /// table only in the design from its `CREATE TABLE` — neither belongs here.
    #[test]
    fn comment_convergence_skips_columns_and_tables_not_in_both_sides() {
        // New column: live table exists but lacks the column.
        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(
            &mut plan,
            &docs_with(vec![commented("title", None)]),
            &docs_with(vec![commented("title", None), commented("summary", Some("new col"))]),
        );
        assert!(plan.is_empty(), "a new column's comment rides its ADD COLUMN; got {plan:?}");

        // New table entirely.
        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(
            &mut plan,
            &snap(vec![]),
            &docs_with(vec![commented("title", Some("Display name"))]),
        );
        assert!(plan.is_empty(), "a new table's comments ride its CREATE; got {plan:?}");
    }

    /// Several drifted comments on one table collapse into that table's single
    /// altered statement rather than fighting over it.
    #[test]
    fn comment_convergence_merges_into_one_statement_per_table() {
        let live = docs_with(vec![commented("title", None), commented("body", Some("old"))]);
        let desired = docs_with(vec![
            commented("title", Some("Display name")),
            commented("body", Some("new")),
        ]);

        let mut plan = ReconcilePlan::default();
        plan_comment_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "one statement per table; got {plan:?}");
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("\"title\" IS 'Display name';"), "got: {sql}");
        assert!(sql.contains("\"body\" IS 'new';"), "got: {sql}");
    }

    /// Comment SQL appends to a table's existing ALTER statement, so the column an
    /// `ADD COLUMN` just created is present before its comment is set.
    #[test]
    fn comment_convergence_appends_after_existing_alter() {
        let live = docs_with(vec![commented("title", None)]);
        let desired = docs_with(vec![commented("title", Some("Display name"))]);

        let mut plan = ReconcilePlan::default();
        plan.altered.push(ReconcileStatement {
            entity_name: "app.docs".to_string(),
            sql: "ALTER TABLE app.docs ADD COLUMN body text;".to_string(),
        });
        plan_comment_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "must merge, not duplicate; got {plan:?}");
        let sql = &plan.altered[0].sql;
        let add = sql.find("ADD COLUMN").expect("ADD COLUMN present");
        let comment = sql.find("COMMENT ON COLUMN").expect("COMMENT present");
        assert!(add < comment, "ADD COLUMN must precede the comment; got: {sql}");
    }

    // ── CHECK convergence ────────────────────────────────────

    fn chk(name: Option<&str>, expression: &str) -> TableConstraint {
        TableConstraint::Check {
            name: name.map(str::to_string),
            expression: expression.to_string(),
        }
    }

    fn table_with_checks(checks: Vec<TableConstraint>) -> TableSnapshot {
        TableSnapshot {
            table_constraints: checks,
            ..table("app", "docs", vec![col("status", "text")])
        }
    }

    /// The design declares a CHECK the live DB lacks → non-destructive add. The
    /// design's inline CHECK is unnamed, so no `CONSTRAINT` clause is emitted and
    /// Postgres auto-names it.
    #[test]
    fn check_convergence_adds_missing_check() {
        let live = snap(vec![table_with_checks(vec![])]);
        let desired = snap(vec![table_with_checks(vec![chk(None, "status <> ''")])]);

        let mut plan = ReconcilePlan::default();
        plan_check_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("ADD CHECK (status <> '')"), "got: {sql}");
        assert!(!sql.contains("unnamed"), "an unnamed CHECK must not be named \"unnamed\"; got: {sql}");
        assert!(!plan.destructive, "adding a CHECK is not destructive");
    }

    /// A live CHECK the design dropped is removed by its REAL name — the bug that
    /// made the read-only diff emit an unusable `DROP CONSTRAINT ck:<expression>`.
    #[test]
    fn check_convergence_drops_extra_check_by_its_real_name() {
        let live = snap(vec![table_with_checks(vec![chk(
            Some("docs_status_check"),
            "status <> ''",
        )])]);
        let desired = snap(vec![table_with_checks(vec![])]);

        let mut plan = ReconcilePlan::default();
        plan_check_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("DROP CONSTRAINT \"docs_status_check\""), "got: {sql}");
        assert!(!sql.contains("ck:"), "the expression key must never be used as a name; got: {sql}");
        assert!(plan.destructive, "dropping a CHECK is gated as destructive");
    }

    /// The core convergence property: the design's authored spelling and the
    /// analyzed form Postgres reports for the SAME constraint must reconcile to no
    /// change. Without this, reconcile churns a destructive drop+add every run.
    #[test]
    fn check_convergence_in_sync_across_spellings_is_no_change() {
        let live = snap(vec![table_with_checks(vec![chk(
            Some("docs_status_check"),
            "status = ANY (ARRAY['active'::text, 'archived'::text])",
        )])]);
        let desired = snap(vec![table_with_checks(vec![chk(
            None,
            "status in ('active', 'archived')",
        )])]);

        let mut plan = ReconcilePlan::default();
        plan_check_convergence(&mut plan, &live, &desired);

        assert!(
            plan.is_empty() && !plan.destructive,
            "an authored `IN` and the analyzed `= ANY (ARRAY[…])` are the same CHECK; got {plan:?}"
        );
    }

    /// A genuinely changed CHECK is a drop of the old plus an add of the new —
    /// Postgres cannot alter a CHECK expression in place.
    #[test]
    fn check_convergence_changed_expression_replaces() {
        let live = snap(vec![table_with_checks(vec![chk(
            Some("docs_status_check"),
            "status in ('active')",
        )])]);
        let desired = snap(vec![table_with_checks(vec![chk(
            None,
            "status in ('active', 'archived')",
        )])]);

        let mut plan = ReconcilePlan::default();
        plan_check_convergence(&mut plan, &live, &desired);

        let sql = &plan.altered[0].sql;
        assert!(sql.contains("DROP CONSTRAINT \"docs_status_check\""), "got: {sql}");
        assert!(sql.contains("ADD CHECK"), "got: {sql}");
        assert!(plan.destructive);
    }

    /// A live-only CHECK with no name cannot be dropped by any statement, so it
    /// must surface as a warning rather than as unrunnable SQL.
    #[test]
    fn check_convergence_warns_on_unnamed_live_check() {
        let live = snap(vec![table_with_checks(vec![chk(None, "status <> ''")])]);
        let desired = snap(vec![table_with_checks(vec![])]);

        let mut plan = ReconcilePlan::default();
        plan_check_convergence(&mut plan, &live, &desired);

        assert!(plan.altered.is_empty(), "no SQL for a constraint that can't be named");
        assert_eq!(plan.warnings.len(), 1, "got {:?}", plan.warnings);
        assert!(plan.warnings[0].contains("drop it manually"), "got {:?}", plan.warnings);
    }

    /// Tables absent from one side are skipped — their CHECKs ride along with the
    /// CREATE/DROP TABLE.
    #[test]
    fn check_convergence_skips_tables_not_in_both_sides() {
        let live = snap(vec![]);
        let desired = snap(vec![table_with_checks(vec![chk(None, "status <> ''")])]);

        let mut plan = ReconcilePlan::default();
        plan_check_convergence(&mut plan, &live, &desired);

        assert!(plan.is_empty(), "a new table's CHECKs come with its CREATE; got {plan:?}");
    }

    // ── Index convergence (issue #12) ────────────────────────

    fn idx(name: &str, cols: &[&str], unique: bool) -> crate::entity::IndexDef {
        use crate::entity::{IndexColumn, IndexDef};
        IndexDef {
            name: Some(name.to_string()),
            columns: cols
                .iter()
                .map(|c| IndexColumn { name: (*c).to_string(), ..Default::default() })
                .collect(),
            unique,
            ..Default::default()
        }
    }

    /// A `DESC` index that already exists must reconcile to NO change.
    ///
    /// Introspection used to report every column as unordered, so an authored
    /// `(project_id, started_at desc)` never matched the live index and reconcile
    /// emitted a `DROP`/`CREATE` pair — then reported success while the very next
    /// `dbd diff` showed the same pair again, forever.
    #[test]
    fn index_convergence_desc_column_in_sync_is_no_change() {
        use crate::entity::{IndexColumn, SortOrder};
        let descending = || crate::entity::IndexDef {
            name: Some("runs_project_idx".to_string()),
            columns: vec![
                IndexColumn { name: "project_id".to_string(), ..Default::default() },
                IndexColumn {
                    name: "started_at".to_string(),
                    order: Some(SortOrder::Desc),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let table_with = |ix| {
            snap(vec![TableSnapshot {
                indexes: vec![ix],
                ..table("app", "runs", vec![col("project_id", "uuid"), col("started_at", "timestamptz")])
            }])
        };

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &table_with(descending()), &table_with(descending()));

        assert!(
            plan.is_empty() && !plan.destructive,
            "a DESC index present on both sides must not churn; got {plan:?}"
        );
    }

    /// A partial index that already exists must reconcile to NO change.
    ///
    /// Introspection used to skip partial indexes entirely, so the design's copy
    /// looked missing and reconcile issued `CREATE INDEX IF NOT EXISTS` — which
    /// silently no-ops against the existing name. Reconcile "succeeded" without
    /// converging anything.
    #[test]
    fn index_convergence_partial_index_in_sync_is_no_change() {
        let partial = || crate::entity::IndexDef {
            predicate: Some("file_path IS NOT NULL".to_string()),
            ..idx("nodes_identity", &["folder_id", "file_path"], true)
        };
        let table_with = |ix| {
            snap(vec![TableSnapshot {
                indexes: vec![ix],
                ..table("app", "nodes", vec![col("folder_id", "uuid"), col("file_path", "text")])
            }])
        };

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &table_with(partial()), &table_with(partial()));

        assert!(
            plan.is_empty() && !plan.destructive,
            "a partial index present on both sides must not churn; got {plan:?}"
        );
    }

    /// Two indexes on the same column that differ ONLY in their predicate are
    /// different indexes: the shape must tell them apart, or reconcile leaves real
    /// drift in place.
    #[test]
    fn index_convergence_differing_predicate_replaces() {
        let with_predicate = |pred: &str| {
            snap(vec![TableSnapshot {
                indexes: vec![crate::entity::IndexDef {
                    predicate: Some(pred.to_string()),
                    ..idx("nodes_folder_idx", &["folder_id"], false)
                }],
                ..table("app", "nodes", vec![col("folder_id", "uuid")])
            }])
        };

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(
            &mut plan,
            &with_predicate("folder_id IS NOT NULL"),
            &with_predicate("folder_id IS NULL"),
        );

        let sql = &plan.altered[0].sql;
        assert!(sql.contains("DROP INDEX IF EXISTS"), "got: {sql}");
        assert!(sql.contains("WHERE folder_id IS NULL"), "the new predicate must be created; got: {sql}");
        assert!(plan.destructive);
    }

    /// An extension-method index round-trips its access method, operator class,
    /// storage parameters and predicate — so it matches itself, and when it IS
    /// created the statement rebuilds the real index rather than a plain btree.
    #[test]
    fn index_convergence_preserves_extension_method_and_options() {
        use crate::entity::{IndexColumn, IndexType};
        let hnsw = crate::entity::IndexDef {
            name: Some("nodes_embedding_hnsw".to_string()),
            columns: vec![IndexColumn {
                name: "embedding".to_string(),
                opclass: Some("vector_cosine_ops".to_string()),
                ..Default::default()
            }],
            index_type: Some(IndexType::Other("hnsw".to_string())),
            predicate: Some("embedding IS NOT NULL".to_string()),
            with_options: [("m".to_string(), "16".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let with_index = |ixs: Vec<crate::entity::IndexDef>| {
            snap(vec![TableSnapshot {
                indexes: ixs,
                ..table("app", "nodes", vec![col("embedding", "vector")])
            }])
        };

        // Present on both sides → no change.
        let mut plan = ReconcilePlan::default();
        plan_index_convergence(
            &mut plan,
            &with_index(vec![hnsw.clone()]),
            &with_index(vec![hnsw.clone()]),
        );
        assert!(plan.is_empty(), "an hnsw index must match itself; got {plan:?}");

        // Missing from the live DB → the CREATE carries every clause.
        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &with_index(vec![]), &with_index(vec![hnsw]));
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("USING hnsw"), "got: {sql}");
        assert!(sql.contains("vector_cosine_ops"), "got: {sql}");
        assert!(sql.contains("WITH (m = 16)"), "got: {sql}");
        assert!(sql.contains("WHERE embedding IS NOT NULL"), "got: {sql}");
    }

    /// An index on ONE member of a composite PRIMARY KEY must still be created.
    ///
    /// The parser flags every member column `is_pk`, so treating those flags as
    /// backing sets invented `[parent_id]` and `[child_id]` covers that no index
    /// provides. Convergence then suppressed the declared single-column index while
    /// `dbd diff` kept reporting it — a `CREATE INDEX` that reappeared on every run.
    #[test]
    fn index_convergence_creates_index_on_a_composite_pk_member() {
        let base = || {
            let mut t = table(
                "app",
                "memory_links",
                vec![
                    ColumnDef { is_pk: true, nullable: false, ..col("parent_id", "uuid") },
                    ColumnDef { is_pk: true, nullable: false, ..col("child_id", "uuid") },
                ],
            );
            t.table_constraints.push(TableConstraint::PrimaryKey {
                name: None,
                columns: vec!["parent_id".to_string(), "child_id".to_string()],
            });
            t
        };
        let live = snap(vec![base()]);
        let desired = snap(vec![TableSnapshot {
            indexes: vec![idx("memory_links_child_id_idx", &["child_id"], false)],
            ..base()
        }]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "composite-PK member index must converge; got {plan:?}");
        assert!(
            plan.altered[0].sql.contains("memory_links_child_id_idx"),
            "got: {}",
            plan.altered[0].sql
        );
    }

    /// The index that genuinely backs a composite PRIMARY KEY is still suppressed —
    /// introspection reports it, the design never declares it.
    #[test]
    fn index_convergence_still_ignores_composite_pk_backing_index() {
        let mut live_t = table(
            "app",
            "memory_links",
            vec![col("parent_id", "uuid"), col("child_id", "uuid")],
        );
        live_t.table_constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["parent_id".to_string(), "child_id".to_string()],
        });
        let mut desired_t = live_t.clone();
        live_t.indexes.push(idx("memory_links_pkey", &["parent_id", "child_id"], true));
        desired_t.indexes.clear();

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &snap(vec![live_t]), &snap(vec![desired_t]));

        assert!(
            plan.is_empty() && !plan.destructive,
            "a PK-backing index must never be dropped as an orphan; got {plan:?}"
        );
    }

    /// A partial UNIQUE index is a real index, never PK/UNIQUE constraint backing:
    /// `unique (a)` and `unique index (a) where b is null` enforce different things,
    /// so the partial one must not be suppressed as "already a constraint".
    #[test]
    fn index_convergence_keeps_partial_unique_index_over_constrained_columns() {
        let mut live_t = table("app", "libs", vec![col("library_id", "uuid"), col("project_id", "uuid")]);
        live_t.table_constraints.push(TableConstraint::Unique {
            name: None,
            columns: vec!["library_id".to_string()],
        });
        let mut desired_t = live_t.clone();
        desired_t.indexes.push(crate::entity::IndexDef {
            predicate: Some("project_id IS NULL".to_string()),
            ..idx("libs_global_uniq", &["library_id"], true)
        });

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &snap(vec![live_t]), &snap(vec![desired_t]));

        let sql = &plan.altered[0].sql;
        assert!(
            sql.contains("libs_global_uniq") && sql.contains("WHERE project_id IS NULL"),
            "the partial unique index must still be created; got: {sql}"
        );
    }

    /// The design declares a secondary index the live DB lacks → non-destructive,
    /// idempotent `CREATE INDEX IF NOT EXISTS`. This is the reported bug's core
    /// case: a `CREATE INDEX` added to an existing table's `.ddl` that reconcile
    /// silently skipped (canonicalize strips indexes; there was no convergence).
    #[test]
    fn index_convergence_adds_missing_index() {
        let desired = snap(vec![TableSnapshot {
            indexes: vec![idx("children_parent_id_idx", &["parent_id"], false)],
            ..table("app", "children", vec![col("parent_id", "uuid")])
        }]);
        let live = snap(vec![table("app", "children", vec![col("parent_id", "uuid")])]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "expected one altered table; got {plan:?}");
        assert_eq!(plan.altered[0].entity_name, "app.children");
        assert!(
            plan.altered[0].sql.contains(
                "CREATE INDEX IF NOT EXISTS \"children_parent_id_idx\" ON \"app\".\"children\" (\"parent_id\");"
            ),
            "expected idempotent CREATE INDEX; got: {}",
            plan.altered[0].sql
        );
        assert!(!plan.destructive, "adding an index is not destructive");
    }

    /// The live DB has a secondary index the design dropped → schema-qualified
    /// `DROP INDEX IF EXISTS <live-name>`, gated as destructive.
    #[test]
    fn index_convergence_drops_extra_index_is_destructive() {
        let live = snap(vec![TableSnapshot {
            indexes: vec![idx("children_parent_id_idx", &["parent_id"], false)],
            ..table("app", "children", vec![col("parent_id", "uuid")])
        }]);
        let desired = snap(vec![table("app", "children", vec![col("parent_id", "uuid")])]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        assert!(
            plan.altered[0].sql.contains("DROP INDEX IF EXISTS \"app\".\"children_parent_id_idx\";"),
            "expected schema-qualified DROP INDEX with the live name; got: {}",
            plan.altered[0].sql
        );
        assert!(plan.destructive, "dropping an index is gated as destructive");
    }

    /// A live index and a design index of the same shape under DIFFERENT names
    /// reconcile to no change — no phantom drop/create churn on an in-sync DB.
    #[test]
    fn index_convergence_in_sync_by_shape_is_no_change() {
        let live = snap(vec![TableSnapshot {
            indexes: vec![idx("live_auto_name", &["email"], false)],
            ..table("app", "users", vec![col("email", "text")])
        }]);
        let desired = snap(vec![TableSnapshot {
            indexes: vec![idx("users_email_idx", &["email"], false)],
            ..table("app", "users", vec![col("email", "text")])
        }]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert!(
            plan.is_empty() && !plan.destructive,
            "same-shape index must reconcile to no change; got {plan:?}"
        );
    }

    /// A PK-backing index (introspection reports it; the parsed design does not)
    /// must be excluded on both sides — never dropped, never re-created.
    #[test]
    fn index_convergence_ignores_pk_backing_index() {
        let live = snap(vec![TableSnapshot {
            indexes: vec![idx("users_pkey", &["id"], true)],
            table_constraints: vec![TableConstraint::PrimaryKey {
                name: Some("users_pkey".to_string()),
                columns: vec!["id".to_string()],
            }],
            ..table("app", "users", vec![not_null("id", "uuid")])
        }]);
        let desired = snap(vec![TableSnapshot {
            table_constraints: vec![TableConstraint::PrimaryKey { name: None, columns: vec!["id".to_string()] }],
            ..table("app", "users", vec![pk_col("id", "uuid")])
        }]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert!(
            plan.is_empty() && !plan.destructive,
            "PK-backing index must not be dropped or re-created; got {plan:?}"
        );
    }

    /// An index whose shape changed (here uniqueness) drops the old and creates
    /// the new — Postgres can't alter it in place — with the drop before the
    /// create so the shared name doesn't collide.
    #[test]
    fn index_convergence_changed_shape_replaces() {
        let live = snap(vec![TableSnapshot {
            indexes: vec![idx("users_email_idx", &["email"], false)],
            ..table("app", "users", vec![col("email", "text")])
        }]);
        let desired = snap(vec![TableSnapshot {
            indexes: vec![idx("users_email_idx", &["email"], true)],
            ..table("app", "users", vec![col("email", "text")])
        }]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("DROP INDEX IF EXISTS \"app\".\"users_email_idx\";"), "must drop old index; got: {sql}");
        assert!(
            sql.contains("CREATE UNIQUE INDEX IF NOT EXISTS \"users_email_idx\" ON \"app\".\"users\" (\"email\");"),
            "must create the new unique index; got: {sql}"
        );
        assert!(plan.destructive, "replacing an index drops the old one → destructive");
        assert!(
            sql.find("DROP INDEX").unwrap() < sql.find("CREATE UNIQUE INDEX").unwrap(),
            "drop must precede create; got: {sql}"
        );
    }

    /// Index add SQL is appended AFTER any existing ALTER (e.g. the `ADD COLUMN`
    /// that created the indexed column), so the column exists before the index.
    #[test]
    fn index_convergence_appends_after_existing_alter() {
        let desired = snap(vec![TableSnapshot {
            indexes: vec![idx("users_email_idx", &["email"], false)],
            ..table("app", "users", vec![col("email", "text")])
        }]);
        let live = snap(vec![table("app", "users", vec![])]);

        let mut plan = ReconcilePlan::default();
        plan.altered.push(ReconcileStatement {
            entity_name: "app.users".to_string(),
            sql: "ALTER TABLE app.users ADD COLUMN email text;".to_string(),
        });
        plan_index_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1, "index merges into the existing statement");
        let sql = &plan.altered[0].sql;
        let col_pos = sql.find("ADD COLUMN email").expect("ADD COLUMN present");
        let idx_pos = sql.find("CREATE INDEX").expect("CREATE INDEX present");
        assert!(col_pos < idx_pos, "ADD COLUMN must precede CREATE INDEX; got: {sql}");
    }

    /// A non-btree index converges with its real access method — the previous
    /// diff-engine emitter dropped `USING <method>`, silently turning a GIN index
    /// into a btree. Reconcile renders via `emit::emit_index_sql`, which keeps it.
    #[test]
    fn index_convergence_add_emits_using_method() {
        use crate::entity::{IndexColumn, IndexDef, IndexType};
        let gin = IndexDef {
            name: Some("docs_tags_gin".to_string()),
            columns: vec![IndexColumn { name: "tags".to_string(), order: None, ..Default::default() }],
            unique: false,
            index_type: Some(IndexType::Gin),
            ..Default::default()
        };
        let desired = snap(vec![TableSnapshot {
            indexes: vec![gin],
            ..table("app", "docs", vec![col("tags", "text[]")])
        }]);
        let live = snap(vec![table("app", "docs", vec![col("tags", "text[]")])]);

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        assert!(
            plan.altered[0].sql.contains("USING gin"),
            "a GIN index must converge with its access method, not as btree; got: {}",
            plan.altered[0].sql
        );
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
                columns: vec![IndexColumn { name: "total".to_string(), order: None, ..Default::default() }],
                unique: false,
                index_type: None,
                ..Default::default()
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

    /// The authored spelling of a keyword default and the spelling `pg_get_expr`
    /// hands back must converge. Postgres re-spells in ITS casing, not the
    /// author's — uppercase for SQL keywords and constructs, lowercase for catalog
    /// functions — so reconcile used to emit a `SET DEFAULT` that Postgres
    /// immediately re-spelled, and the next diff reported it again.
    #[test]
    fn canonical_default_converges_keyword_casing() {
        // Keyword functions: Postgres stores these uppercase whatever was authored.
        for (authored, introspected) in [
            ("current_date", "CURRENT_DATE"),
            ("current_timestamp", "CURRENT_TIMESTAMP"),
            ("current_time", "CURRENT_TIME"),
            ("localtimestamp", "LOCALTIMESTAMP"),
            ("localtime", "LOCALTIME"),
            ("current_user", "CURRENT_USER"),
            ("session_user", "SESSION_USER"),
            ("current_role", "CURRENT_ROLE"),
            ("current_catalog", "CURRENT_CATALOG"),
            ("current_schema", "CURRENT_SCHEMA"),
            ("user", "USER"),
            ("CURRENT_TIMESTAMP(3)", "current_timestamp(3)"),
        ] {
            assert_eq!(
                canonical_default(authored),
                canonical_default(introspected),
                "{authored} and {introspected} are the same default"
            );
        }
        // Catalog functions: Postgres stores these lowercase whatever was authored.
        assert_eq!(canonical_default("NOW()"), canonical_default("now()"));
        assert_eq!(canonical_default("Gen_Random_Uuid()"), "gen_random_uuid()");
        // SQL constructs: stored uppercase, and the literal arguments keep their case.
        assert_eq!(
            canonical_default("coalesce('X', 'y')"),
            canonical_default("COALESCE('X', 'y')")
        );
    }

    /// Folding case must never reach inside quoted text: the canonical form is
    /// emitted as `SET DEFAULT` DDL, so folding a literal would change the value
    /// actually written to the database.
    #[test]
    fn canonical_default_preserves_quoted_text_case() {
        assert_eq!(canonical_default("'Mixed Case'"), "'Mixed Case'");
        assert_eq!(canonical_default("'Mixed Case'::text"), "'Mixed Case'");
        assert_eq!(canonical_default("upper('aB')"), "upper('aB')");
        assert_eq!(canonical_default("UPPER('aB')"), "upper('aB')");
        // A quoted identifier is case-significant too.
        assert_eq!(canonical_default("\"MyFunc\"()"), "\"MyFunc\"()");
        // Doubled quotes escape: the run continues, so inner case survives.
        assert_eq!(canonical_default("'It''s A Value'"), "'It''s A Value'");
        // Two defaults differing only inside a literal must NOT converge.
        assert_ne!(canonical_default("'Active'"), canonical_default("'active'"));
    }

    /// A dollar-quoted default is left exactly as authored rather than risking a
    /// fold inside its body — at worst it keeps reading as drift.
    #[test]
    fn canonical_default_leaves_dollar_quoted_alone() {
        assert_eq!(canonical_default("$$Hello World$$"), "$$Hello World$$");
        assert_eq!(canonical_default("$tag$Mixed$tag$"), "$tag$Mixed$tag$");
    }

    /// Non-ASCII text survives folding intact (byte-wise lowercasing would corrupt
    /// a multibyte sequence).
    #[test]
    fn canonical_default_handles_non_ascii() {
        assert_eq!(canonical_default("'café'"), "'café'");
        assert_eq!(canonical_default("'CAFÉ'"), "'CAFÉ'");
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
                columns: vec![IndexColumn { name: "x".into(), order: None, ..Default::default() }],
                unique: true,
                index_type: None,
                ..Default::default()
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

    /// A plan carrying only a matview create is NOT empty — otherwise the CLI's
    /// "Already in sync" guard would hide a pending matview CREATE in `--dry-run`.
    #[test]
    fn is_empty_false_when_only_matview_creates() {
        let plan = ReconcilePlan {
            matview_creates: vec!["analytics.daily_sales".to_string()],
            ..Default::default()
        };
        assert!(!plan.is_empty());
    }
}
