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
    /// (`matview_create_sql`, then its stamp) and are detected during `--dry-run`
    /// so the preview can list them.
    pub matview_creates: Vec<String>,
    /// Materialized views live with a `v1` (unversioned) hash sentinel — written
    /// by a dbd that hashed a different body contract, so its value is not
    /// comparable to a current hash. Reconcile re-stamps these to `v2` silently
    /// (never a drift warning); carried here so `--dry-run` previews the write.
    pub matview_restamps: Vec<String>,
    /// Risky-change advisories (type changes, possible renames, enum value drops,
    /// orphaned enums that are not auto-dropped).
    pub warnings: Vec<String>,
    /// Whether the plan makes a change gated behind `--allow-destructive`:
    /// dropping a column or constraint from an existing table (data loss), or
    /// dropping a foreign key (see [`plan_fk_convergence`]) or a secondary index
    /// (see [`plan_index_convergence`]). Whole-table drops are separate — see
    /// [`Self::dropped`] — and gated by pruning, not by this flag.
    pub destructive: bool,
    /// What makes the plan [`Self::destructive`], one line each —
    /// `"app.a: drops column gone"` — for a refusal to name. Only those: the
    /// refusal used to list every altered table, a column added as much as one
    /// dropped.
    pub destructive_changes: Vec<String>,
    /// The enum type swaps the plan makes (see [`plan_enum_recreation`]), with
    /// the columns each moves — what the caller checks against the live
    /// database for a materialized view that would block one.
    pub enum_swaps: Vec<EnumSwap>,
    /// What the plan drops from an existing table or enum, part by part —
    /// [`Self::destructive_changes`] as data, for the uses guard to check
    /// against what other owners depend on (#7).
    pub dropped_parts: Vec<DroppedPart>,
}

impl ReconcilePlan {
    /// Refuse a destructive reconcile (dropped columns, constraints, foreign keys,
    /// or indexes) unless the caller explicitly opted in with `allow_destructive`.
    pub fn refuse_destructive(&self, allow_destructive: bool) -> crate::error::Result<()> {
        if !self.destructive || allow_destructive {
            return Ok(());
        }
        // What is destructive, and only that: listing every altered table named
        // ones that merely gained a column.
        let details: String = self
            .destructive_changes
            .iter()
            .map(|change| format!("  {change}"))
            .collect::<Vec<_>>()
            .join("\n");
        Err(crate::error::DbdError::Config(format!(
            "reconcile would make destructive changes:\n{details}\n\
             Re-run with --allow-destructive to proceed."
        )))
    }
}

/// One part of an existing table or enum a plan drops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedPart {
    /// The table or enum, `schema.name`.
    pub entity: String,
    pub kind: DroppedKind,
    /// The column, constraint, index or enum value; a foreign key's columns.
    pub name: String,
}

/// What kind of part a [`DroppedPart`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DroppedKind {
    Column,
    Constraint,
    Index,
    EnumValue,
    ForeignKey,
}

/// One enum's type swap: the values it loses and the `(table, column)` pairs
/// (`schema.table`) moved onto the new type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumSwap {
    pub enum_name: String,
    pub removed: Vec<String>,
    pub columns: Vec<(String, String)>,
}

impl ReconcilePlan {
    /// Record a change gated behind `--allow-destructive`, once.
    fn mark_destructive(&mut self, change: String) {
        self.destructive = true;
        if !self.destructive_changes.contains(&change) {
            self.destructive_changes.push(change);
        }
    }

    /// Record a part the plan drops — as a destructive change, and as data.
    fn mark_dropped(&mut self, change: String, entity: &str, kind: DroppedKind, name: &str) {
        self.mark_destructive(change);
        let part = DroppedPart {
            entity: entity.to_string(),
            kind,
            name: name.to_string(),
        };
        if !self.dropped_parts.contains(&part) {
            self.dropped_parts.push(part);
        }
    }

    /// No structural changes to make.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.altered.is_empty()
            && self.dropped.is_empty()
            && self.matview_creates.is_empty()
            && self.matview_restamps.is_empty()
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

/// [`snapshot_from_entities`] for a comparison with a live database whose
/// extensions own `extension_types`, spelled as [`qualify_extension_types`]
/// spells them.
pub(crate) fn compared_snapshot(entities: &[Entity], extension_types: &[(String, String)]) -> Snapshot {
    let mut snap = raw_snapshot_from_entities(entities);
    qualify_extension_types(&mut snap, extension_types);
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

/// Spell each column type an extension owns as `format_type` does — bare in
/// `public`, `schema.name` elsewhere — on a snapshot compared with the live
/// database whose extensions own `extension_types` (#41).
///
/// `format_type` qualifies a type wherever the session `search_path` does not
/// reach its schema, so introspection reads `extensions.citext`; DDL reaches it
/// through its own `set search_path` and writes `citext`. They are one type,
/// and compared as text they read as a change on every run. Given the same
/// spelling on both sides they match, and the `ALTER … TYPE` a real change
/// emits names the type wherever the session path points.
///
/// A bare name is left as written where the snapshot cannot say which type it
/// is: one the snapshot's own enums declare, which the file's path reaches
/// first, or one two extensions own in different schemas.
///
/// Runs on a raw snapshot, before [`canonicalize`] or
/// [`crate::schema_diff::normalize_for_diff`]: both leave a qualified name as
/// it is.
pub(crate) fn qualify_extension_types(snap: &mut Snapshot, extension_types: &[(String, String)]) {
    let enums: HashSet<String> = snap.enums.iter().map(|e| e.name.to_lowercase()).collect();
    // `None` for a name two extensions own: ambiguous without the file's path.
    let mut spelled: HashMap<String, Option<String>> = HashMap::new();
    for (schema, name) in extension_types {
        let short = name.to_lowercase();
        if enums.contains(&short) {
            continue;
        }
        let canonical = if schema.eq_ignore_ascii_case(DEFAULT_SCHEMA) {
            short.clone()
        } else {
            format!("{}.{short}", schema.to_lowercase())
        };
        spelled
            .entry(short)
            .and_modify(|seen| {
                if seen.as_ref() != Some(&canonical) {
                    *seen = None;
                }
            })
            .or_insert(Some(canonical));
    }
    if spelled.is_empty() {
        return;
    }
    for t in &mut snap.tables {
        for c in &mut t.columns {
            let lowered = c.data_type.trim().to_lowercase();
            let (element, array_suffix) = split_array_suffix(&lowered);
            // A modifier follows the name: `vector(384)`.
            let (base, modifier) = match element.split_once('(') {
                Some((base, rest)) => (base.trim(), format!("({rest}")),
                None => (element, String::new()),
            };
            if let Some(Some(canonical)) = spelled.get(base) {
                c.data_type = format!("{canonical}{modifier}{array_suffix}");
            }
        }
    }
}

/// Canonicalize a snapshot so a **parsed** (desired) table and an **introspected**
/// (live) table of the same shape compare equal for the column/PK/unique diff.
///
/// Reconcile does not diff check constraints on existing tables here — their
/// introspected/parsed forms differ too much to compare reliably (create them
/// via the initial `CREATE`, or use snapshots) — so after the shared
/// `normalize_common` pass this drops them from the diff entirely, along with
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
/// into structurally-deduped table constraints, leaving any FK/CHECK constraints
/// already present untouched (reconcile's `canonicalize` strips those afterward;
/// the diff path keeps them).
///
/// The constraint **name is preserved**, unlike the FK/CHECK normalizers which
/// clear it. Those match by shape and can afford to drop the name because their
/// convergence passes re-read it from the raw snapshot; PK/UNIQUE are diffed here,
/// so this is the only place the live name survives to reach `DROP CONSTRAINT`.
/// Stripping it is what made reconcile emit the matching key as an identifier
/// (`DROP CONSTRAINT pk:tenant_id,metric_id`), which blocked every PK replacement.
/// Matching stays name-agnostic regardless: `diff::constraint_key` keys PK/UNIQUE
/// on their columns and never on their name.
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
            TableConstraint::PrimaryKey { name, columns } => {
                has_table_pk = true;
                push(&mut kept, &mut seen, TableConstraint::PrimaryKey { name, columns })
            }
            // `nulls_not_distinct` rides along: the diff compares it, so dropping it
            // during canonicalization would hide the very drift it detects.
            TableConstraint::Unique {
                name,
                columns,
                nulls_not_distinct,
            } => push(
                &mut kept,
                &mut seen,
                TableConstraint::Unique {
                    name,
                    columns,
                    nulls_not_distinct,
                },
            ),
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
            push(
                &mut kept,
                &mut seen,
                TableConstraint::PrimaryKey {
                    name: None,
                    columns: vec![c.name.clone()],
                },
            );
        }
        if c.is_unique {
            push(
                &mut kept,
                &mut seen,
                TableConstraint::Unique {
                    name: None,
                    columns: vec![c.name.clone()],
                    // A column's `is_unique` flag cannot carry the clause, so an
                    // inline `unique nulls not distinct` reaches here only as a
                    // table-level constraint — where the arm above preserves it.
                    nulls_not_distinct: false,
                },
            );
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
        let ty = c.data_type.clone();
        c.default_value = c.default_value.as_deref().map(|d| canonical_default(d, &ty));
        c.generated = c.generated.as_deref().map(canonical_generated);
        c.is_pk = false;
        c.is_unique = false;
    }
}

/// Canonicalize a `GENERATED ALWAYS AS (…) STORED` expression so the authored
/// spelling and the one `pg_get_expr` reports compare equal.
///
/// NOT [`canonical_default`]: that strips a single *trailing* top-level cast,
/// which is the right shape for a default literal but leaves the casts Postgres
/// pushes *inside* a generation expression —
/// `to_tsvector('english'::regconfig, COALESCE(content, ''::text))` against an
/// authored `to_tsvector('english', coalesce(content,''))`. Those are erased by
/// the same parse/normalize/deparse round-trip an index expression key uses,
/// which also settles case and parens on both sides at once.
///
/// An expression that will not parse keeps its raw text, so it compares only
/// against an identical spelling: it may read as drift, never as falsely equal.
fn canonical_generated(raw: &str) -> String {
    crate::sql_expr::canonicalize_expression(raw).unwrap_or_else(|| raw.trim().to_string())
}

/// Normalize a column type for cross-representation comparison, toward the
/// `format_type` spelling introspection reports:
///
/// - lowercase, and drop a redundant `public.` / `pg_catalog.` prefix;
/// - collapse any array suffix to a single `[]` over the normalized element type
///   — Postgres records neither dimensionality nor a bound, so `text[][]` and
///   `text[3]` are both just `text[]`;
/// - re-spell the whole date/time family, which means ADDING a qualifier the
///   author never wrote: `time` → `time without time zone`;
/// - map common Postgres aliases (`int4` → `integer`, `varchar` → `character
///   varying`);
/// - schema-qualify a bare enum name using `enum_types`.
///
/// The last three are more than an alias swap, and the result is user-visible:
/// `diff::generate` emits `ALTER … TYPE {data_type}`, so a real change to a
/// `time` column now prints `TYPE time without time zone`.
///
/// Public for the same reason as [`crate::parser::pg_native_types`]: the parser
/// parity harness is an integration test in a separate crate, and comparing two
/// parsers' *type spellings* is exactly what this function is for. A local copy
/// there would be a second alias table free to drift from this one.
pub fn canonical_type(raw: &str, enum_types: &HashMap<String, String>) -> String {
    let mut s = raw.trim().to_lowercase();
    // `public.` and `pg_catalog.` are the implicit search path — a type named
    // through either is the same type as the bare form. Postgres rewrites
    // SQL-standard names into pg_catalog-qualified internals at parse time
    // (`int` → `pg_catalog.int4`), so without this the alias table below never
    // sees them and every such column reads as drifted.
    for prefix in ["public.", "pg_catalog."] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    // An array suffix has to come off before the alias table sees the name,
    // otherwise `int4[]` never matches `int4` and every array column reads as
    // drifted against the `integer[]` introspection reports.
    let (element, array_suffix) = split_array_suffix(&s);
    format!("{}{array_suffix}", canonical_element_type(element, enum_types))
}

/// Split a trailing array suffix off a type name, returning the element type and
/// the canonical suffix to put back (`"[]"` or `""`).
///
/// `format_type` spells every array as `element[]` regardless of how many
/// dimensions — or what bounds — were declared, because Postgres does not track
/// them: a column declared `text[][]` or `text[3]` is the same one-and-only
/// `text[]` type. So peel every `[…]` off and re-emit a single `[]`.
fn split_array_suffix(s: &str) -> (&str, &'static str) {
    let mut rest = s.trim_end();
    let mut is_array = false;
    while let Some(inner) = rest.strip_suffix(']') {
        // Only a dimension marker peels: `[]` and `[3]` are array syntax, but
        // anything else between the brackets is part of some other type's name.
        let Some(open) = inner.rfind('[') else { break };
        if !inner[open + 1..].chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        rest = inner[..open].trim_end();
        is_array = true;
    }
    (rest, if is_array { "[]" } else { "" })
}

/// Normalize a scalar (non-array) type name to its `format_type` spelling.
fn canonical_element_type(s: &str, enum_types: &HashMap<String, String>) -> String {
    // The date/time family needs more than a base-name swap — see below.
    if let Some(canonical) = canonical_datetime(s) {
        return canonical;
    }
    // Split "base(args)" so parameterized aliases (varchar(255)) normalize too.
    let (base, args) = match s.split_once('(') {
        Some((b, rest)) => (b.trim().to_string(), Some(format!("({rest}"))),
        None => (s.to_string(), None),
    };
    let base = match base.as_str() {
        "int" | "int4" => "integer".to_string(),
        "int8" => "bigint".to_string(),
        "int2" => "smallint".to_string(),
        "bool" => "boolean".to_string(),
        "float8" => "double precision".to_string(),
        "float4" => "real".to_string(),
        "varchar" => "character varying".to_string(),
        "char" | "bpchar" => "character".to_string(),
        "decimal" => "numeric".to_string(),
        // Qualify a bare enum reference to match introspection.
        other if !other.contains('.') => enum_types.get(other).cloned().unwrap_or_else(|| other.to_string()),
        other => other.to_string(),
    };
    match args {
        Some(a) => format!("{base}{a}"),
        None => base,
    }
}

/// Re-spell a date/time type the way `format_type` does, or `None` if this is
/// not one — in which case the caller falls back to the plain alias table.
///
/// This family cannot go through that table because its three parts move
/// independently. `format_type` always writes the base word, then the modifier,
/// then the time zone spelled out in full (`timetz(3)` → `time(3) with time
/// zone`), while the design side arrives in whatever libpg_query's deparser
/// produced. A base-name swap gets both halves wrong: it cannot express
/// `timetz` → `time … with time zone` with a modifier wedged in the middle
/// (yielding `time with time zone(3)`), and it never fires at all on the
/// already-spelled-out forms, whose "base name" is the entire string.
///
/// The case that made this load-bearing is plain `time`. The deparser renders
/// both `time` and `time without time zone` as bare `time`, so *no* DDL spelling
/// could match the `time without time zone` introspection reports — every `time`
/// column showed a permanent `ALTER COLUMN … TYPE` that changed nothing, and
/// `dbd diff --exit-code` could never come back clean.
///
/// `interval` is deliberately not in the family: its qualifier is a field list
/// (`interval hour to minute`), not a time zone, and both sides already spell it
/// the same way.
fn canonical_datetime(s: &str) -> Option<String> {
    // Peel the spelled-out qualifier first — it is the only part that can be
    // absent, and its absence means "without" for the SQL-standard names.
    let (head, spelled_tz) = if let Some(h) = s.strip_suffix("without time zone") {
        (h.trim_end(), Some(false))
    } else if let Some(h) = s.strip_suffix("with time zone") {
        (h.trim_end(), Some(true))
    } else {
        (s, None)
    };

    // Then the modifier. The deparser writes `time (3) with time zone` with a
    // space that `format_type` does not, so trim around the paren.
    let (word, modifier) = match head.split_once('(') {
        Some((w, rest)) => (w.trim_end(), format!("({rest}")),
        None => (head, String::new()),
    };

    // A `tz` base word carries its own answer; the spelled-out qualifier only
    // ever appears on the SQL-standard spellings.
    let (family, tz) = match word {
        "time" => ("time", spelled_tz.unwrap_or(false)),
        "timetz" => ("time", true),
        "timestamp" => ("timestamp", spelled_tz.unwrap_or(false)),
        "timestamptz" => ("timestamp", true),
        _ => return None,
    };

    let zone = if tz { "with time zone" } else { "without time zone" };
    Some(format!("{family}{modifier} {zone}"))
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
pub(crate) fn canonical_default(raw: &str, data_type: &str) -> String {
    // Postgres keeps `nextval('s')`'s argument as an OID and reports it as
    // `nextval('s'::regclass)`; a design without the cast read as a changed
    // default on every run. Both sides take the reported form — valid SQL with
    // the same meaning, as everything this returns must be (it is emitted).
    if let Some(sequence) = crate::parser::pg::common::nextval_sequence_of(raw.trim()) {
        return format!("nextval('{}'::regclass)", sequence.replace('\'', "''"));
    }
    let base = fold_unquoted_case(strip_trailing_cast(raw.trim()).trim());
    canonical_default_for_type(&base, data_type).unwrap_or(base)
}

/// Settle a literal the server rewrites when it stores it (#18).
///
/// Stripping the cast and folding case is not enough for the types where
/// Postgres does not keep what you wrote. Measured on a live server:
///
/// | written | stored |
/// |---|---|
/// | `'{"b":1,"a":2}'` (jsonb) | `'{"a": 2, "b": 1}'` |
/// | `'epoch'` (timestamptz) | `'1969-12-31 18:00:00-06'` |
/// | `'t'` (boolean) | `true` |
///
/// Each of these read as permanent drift: reconcile emitted a `SET DEFAULT`,
/// the server rewrote it again, and the next run saw the same change.
///
/// # The output has to be valid SQL
///
/// This value is *emitted* — `SET DEFAULT {default}` in both
/// [`crate::diff::generate`] and the type-change path below — so it cannot be
/// reduced to a comparison key. Every form produced here is a literal with the
/// same meaning as the input, in the spelling Postgres itself uses.
///
/// `None` means "nothing to settle": the caller keeps the text, which may read
/// as drift but can never read as falsely equal.
fn canonical_default_for_type(base: &str, data_type: &str) -> Option<String> {
    let ty = data_type.trim().to_ascii_lowercase();
    let ty = ty.split(['(', '[']).next().unwrap_or(&ty).trim();
    match ty {
        "json" | "jsonb" => canonical_json_default(base),
        "bool" | "boolean" => canonical_bool_default(base),
        "timestamptz" | "timestamp with time zone" => canonical_instant_default(base),
        _ => None,
    }
}

/// Re-render a JSON **object** literal with its keys sorted, matching how
/// `jsonb` stores and reports one.
///
/// Objects only: an array or scalar has no key order to settle, and
/// re-rendering one would risk changing a value for no gain. A literal that is
/// not valid JSON is left alone.
fn canonical_json_default(base: &str) -> Option<String> {
    let inner = base.strip_prefix('\'')?.strip_suffix('\'')?;
    let value: serde_json::Value = serde_json::from_str(&inner.replace("''", "'")).ok()?;
    if !value.is_object() {
        return None;
    }
    // `to_string` on a `serde_json::Map` is insertion-ordered; dbd builds
    // `serde_json` with `preserve_order` off, so parsing already sorted the
    // keys. Rendered with Postgres's `", "` and `": "` spacing so the two
    // sides are byte-identical rather than merely equivalent.
    let rendered = render_json_pg(&value);
    Some(format!("'{}'", rendered.replace('\'', "''")))
}

/// `serde_json` compact form, respaced the way `jsonb` prints it.
fn render_json_pg(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, val)| format!("{}: {}", serde_json::Value::String(k.clone()), render_json_pg(val)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(render_json_pg).collect();
            format!("[{}]", inner.join(", "))
        }
        other => other.to_string(),
    }
}

/// `'t'`, `'yes'`, `TRUE` and the rest all store as `true`.
fn canonical_bool_default(base: &str) -> Option<String> {
    let word = base.trim_matches('\'').trim();
    match word {
        "t" | "true" | "y" | "yes" | "on" | "1" => Some("true".to_string()),
        "f" | "false" | "n" | "no" | "off" | "0" => Some("false".to_string()),
        _ => None,
    }
}

/// Reduce a `timestamptz` literal to one instant, spelled in UTC.
///
/// Two reasons a text comparison cannot work here. `'epoch'` is a special
/// input the server resolves on store, and the stored value is reported **in
/// the session's timezone** — so the same database reports different text to
/// different connections. Rendering both sides as the same instant settles
/// both at once.
///
/// Deliberately NOT applied to `'now'`, `'today'` and friends. Those are
/// resolved at DDL time and frozen, so the live value is a particular moment
/// that will differ from the design on every later day — no normalisation can
/// make them agree, and choosing one would be inventing a value. They stay as
/// written and read as drift, which is the honest report; `now()` and
/// `current_date` are the forms that converge.
fn canonical_instant_default(base: &str) -> Option<String> {
    let inner = base.strip_prefix('\'')?.strip_suffix('\'')?.trim();
    let instant = if inner.eq_ignore_ascii_case("epoch") {
        chrono::DateTime::UNIX_EPOCH
    } else {
        parse_timestamptz(inner)?
    };
    Some(format!("'{}'", instant.format("%Y-%m-%d %H:%M:%S%.f+00")))
}

/// Parse the shapes Postgres emits and authors write for a `timestamptz`.
fn parse_timestamptz(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{DateTime, Utc};
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f%#z",
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(text, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    None
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
    let mut plan = ReconcilePlan {
        warnings,
        ..Default::default()
    };
    for d in &diffs {
        for (change, kind, name) in dropped_fields(d) {
            plan.mark_dropped(change, &d.entity_name, kind, &name);
        }
    }

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
            // Only a table becomes a prune target: a dropped enum is a warning,
            // already in `warnings`, and never auto-dropped. Named as the live
            // catalog spells it, quoted — unquoted, a mixed-case `"Report"`
            // dropped the `report` beside it (#7) — and only if still there: a
            // parent's `CASCADE` drops the tables inheriting from it first.
            DiffAction::Drop if d.entity_type == EntityType::Table => {
                let (schema, name) = crate::entity::split_qualified_name(&d.entity_name);
                let schema = schema.unwrap_or_else(|| DEFAULT_SCHEMA.to_string());
                plan.dropped.push(ReconcileStatement {
                    entity_name: d.entity_name.clone(),
                    sql: format!(
                        "DROP TABLE IF EXISTS {}.{} CASCADE;",
                        crate::sql_quote::ident(&schema),
                        crate::sql_quote::ident(&name)
                    ),
                });
            }
            DiffAction::Drop => {}
        }
    }

    // In a stable order, whatever order the diff found them in.
    plan.dropped.sort_by(|a, b| a.entity_name.cmp(&b.entity_name));
    plan
}

/// What a diff drops from an existing table or enum — a column, a constraint,
/// an enum value: the data-loss cases gated by `allow_destructive` — one line
/// each, for [`ReconcilePlan::destructive_changes`]. Whole-object drops are
/// handled separately via pruning.
fn dropped_fields(d: &MigrationDiff) -> Vec<(String, DroppedKind, String)> {
    use crate::diff::{FieldDetail, FieldType};
    let DiffAction::Change(changes) = &d.action else {
        return Vec::new();
    };
    changes
        .iter()
        .filter(|c| matches!(c.action, ChangeAction::Drop(_)))
        .map(|c| {
            let (label, kind) = match c.field_type {
                FieldType::Column => ("column", DroppedKind::Column),
                FieldType::Constraint => ("constraint", DroppedKind::Constraint),
                FieldType::Index => ("index", DroppedKind::Index),
                FieldType::EnumValue => ("enum value", DroppedKind::EnumValue),
            };
            // The part's own name, as the database knows it — a constraint's
            // diff key is its shape (`uq:code`), not its name.
            let name = match &c.action {
                ChangeAction::Drop(detail) => match detail.as_ref() {
                    FieldDetail::Constraint(
                        TableConstraint::PrimaryKey { name, .. }
                        | TableConstraint::Unique { name, .. }
                        | TableConstraint::Check { name, .. }
                        | TableConstraint::Exclude { name, .. },
                    ) => name.clone(),
                    FieldDetail::Constraint(TableConstraint::ForeignKey(fk)) => fk.name.clone(),
                    FieldDetail::Index(ix) => ix.name.clone(),
                    _ => None,
                },
                _ => None,
            };
            (
                format!("{}: drops {label} {}", d.entity_name, c.field_name),
                kind,
                name.unwrap_or_else(|| c.field_name.clone()),
            )
        })
        .collect()
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
    match_full: bool,
    deferrable: bool,
    initially_deferred: bool,
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
        match_full: fk.match_full,
        // `INITIALLY DEFERRED` implies `DEFERRABLE`; one spelled without the
        // other is the same key.
        deferrable: fk.deferrable || fk.initially_deferred,
        initially_deferred: fk.initially_deferred,
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
/// FKs match by `FkShape` (name-agnostic), so a live auto-named FK and the
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
        // The drop carries the live constraint, so the emitter names it from the
        // real `conname` — the reason FKs can't simply be name-stripped before
        // diffing.
        for fk in live_fks.iter().filter(|fk| !desired_shapes.contains(&fk_shape(fk))) {
            changes.push(FieldChange {
                field_name: format!("fk:{}", fk.columns.join(",")),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop(Box::new(FieldDetail::Constraint(TableConstraint::ForeignKey(
                    fk.clone(),
                )))),
            });
            plan.mark_dropped(
                format!("{qname}: drops foreign key ({})", fk.columns.join(", ")),
                &qname,
                DroppedKind::ForeignKey,
                &fk.columns.join(", "),
            );
        }

        // Adds: a design FK missing from the live DB (unnamed → Postgres auto-names).
        for fk in desired_fks.iter().filter(|fk| !live_shapes.contains(&fk_shape(fk))) {
            changes.push(FieldChange {
                field_name: format!("fk:{}", fk.columns.join(",")),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(TableConstraint::ForeignKey(
                    fk.clone(),
                )))),
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
    let cols = coercible_columns(&t.columns);
    t.table_constraints
        .iter()
        .filter_map(|con| match con {
            TableConstraint::Check { expression, .. } => {
                let key = crate::sql_expr::canonicalize_predicate_for_columns(expression, &cols)
                    .unwrap_or_else(|| expression.clone());
                Some((key, con))
            }
            _ => None,
        })
        .collect()
}

/// The columns of `columns` whose declared type Postgres **binary-coerces** to
/// `text`, lower-cased — the input to
/// [`crate::sql_expr::canonicalize_predicate_for_columns`].
///
/// `varchar` alone: `pg_cast` records `varchar → text` as `castmethod = 'b'`, so
/// the cast Postgres inserts is a runtime no-op and erasing it cannot change what
/// a predicate accepts. Deliberately NOT here:
///
/// - **`char(n)`/`bpchar`** — `castmethod = 'f'`, running `rtrim1`. The cast
///   strips trailing spaces, so it is load-bearing. A CHECK on a `char` column
///   keeps reading as drift, which is the safe failure.
/// - **`varchar[]`** — an array cast changes the operand's shape, and
///   `canonical_type` keeps the `[]` suffix so it never matches here.
/// - **`text`** — Postgres inserts no cast in the first place.
pub(crate) fn coercible_columns(columns: &[crate::entity::ColumnDef]) -> crate::sql_expr::CoercibleColumns {
    let no_enums = HashMap::new();
    columns
        .iter()
        .filter(|c| {
            let ty = canonical_type(&c.data_type, &no_enums);
            // `canonical_type` always spells an array with a trailing `[]`, and it
            // lands AFTER the length modifier — `character varying(10)[]` — so the
            // array test has to come before the base-name one, not after it.
            !ty.ends_with("[]")
                && ty
                    .split('(')
                    .next()
                    .is_some_and(|base| base.trim_end() == "character varying")
        })
        .map(|c| c.name.to_lowercase())
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
        // `DROP CONSTRAINT ck:<expression>`. The drop carries the live constraint
        // so the emitter reads that name off it.
        for (key, con) in live_checks.iter().filter(|(k, _)| !desired_keys.contains(k.as_str())) {
            let TableConstraint::Check { name, .. } = con else {
                continue;
            };
            // Without a name there is no statement to issue; warn rather than
            // emit SQL that cannot run.
            if name.is_none() {
                plan.warnings.push(format!(
                    "CHECK ({key}) on {qname} is not in the design but has no constraint name — drop it manually"
                ));
                continue;
            }
            changes.push(FieldChange {
                field_name: format!("ck:{key}"),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop(Box::new(FieldDetail::Constraint((*con).clone()))),
            });
            plan.mark_dropped(
                format!(
                    "{qname}: drops CHECK constraint {}",
                    name.as_deref().unwrap_or_default()
                ),
                &qname,
                DroppedKind::Constraint,
                name.as_deref().unwrap_or_default(),
            );
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

// ── EXCLUDE convergence ─────────────────────────────────────

/// Converge `EXCLUDE` constraints into an existing reconcile `plan`, as
/// [`plan_check_convergence`] does CHECKs: [`canonicalize`] strips them, so
/// they are matched here from the RAW snapshots by their definition read back
/// through [`crate::sql_expr::canonical_exclusion`] — Postgres auto-names an
/// unnamed one (`<table>_<columns>_excl`), so a name cannot be the identity.
/// One the design declares and the database lacks is added; one the database
/// has and the design dropped is removed by its live name, destructively.
pub fn plan_exclusion_convergence(plan: &mut ReconcilePlan, live: &Snapshot, desired: &Snapshot) {
    use crate::diff::{FieldChange, FieldDetail, FieldType};

    fn exclusions(t: &TableSnapshot) -> Vec<(String, &TableConstraint)> {
        t.table_constraints
            .iter()
            .filter_map(|con| match con {
                TableConstraint::Exclude { definition, .. } => Some((
                    crate::sql_expr::canonical_exclusion(definition).unwrap_or_else(|| definition.clone()),
                    con,
                )),
                _ => None,
            })
            .collect()
    }

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
        let desired_ex = exclusions(dt);
        let live_ex = exclusions(lt);
        let desired_keys: HashSet<&str> = desired_ex.iter().map(|(k, _)| k.as_str()).collect();
        let live_keys: HashSet<&str> = live_ex.iter().map(|(k, _)| k.as_str()).collect();

        let mut changes: Vec<FieldChange> = Vec::new();
        for (key, con) in live_ex.iter().filter(|(k, _)| !desired_keys.contains(k.as_str())) {
            let TableConstraint::Exclude { name: Some(_), .. } = con else {
                plan.warnings.push(format!(
                    "EXCLUDE {key} on {qname} is not in the design but has no constraint name — drop it manually"
                ));
                continue;
            };
            changes.push(FieldChange {
                field_name: format!("ex:{key}"),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop(Box::new(FieldDetail::Constraint((*con).clone()))),
            });
            plan.destructive = true;
        }
        for (key, con) in desired_ex.iter().filter(|(k, _)| !live_keys.contains(k.as_str())) {
            changes.push(FieldChange {
                field_name: format!("ex:{key}"),
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

// ── Enum value removal (issue #12) ──────────────────────────

/// Suffix the displaced enum type carries between the rename and the drop.
///
/// A fixed suffix rather than a generated one: the whole recreation runs in a
/// single implicit transaction, so the name exists only inside it, and a fixed
/// name is what an operator reading a failed run's SQL can act on.
pub(crate) const ENUM_RECREATE_SUFFIX: &str = "__dbd_old";

/// Converge an enum whose declared value set **lost** a member.
///
/// Postgres has no `ALTER TYPE … DROP VALUE`, so `generate_field_sql` emitted
/// nothing for an enum-value drop. `plan_reconcile` then skipped the empty SQL,
/// reconcile reported `0 altered`, and `dbd diff` reported the same drift on
/// every subsequent run — reconcile could not converge and never said so.
///
/// The convergent form is the type-swap Postgres does leave open:
///
/// ```text
/// DROP VIEW   … ;                 -- managed views over the column, deepest first
/// DROP INDEX  … ;                 -- indexes whose WHERE or expression reads it
/// ALTER TABLE … DROP DEFAULT;     -- the default is typed against the old type
/// ALTER TYPE  … RENAME TO x__dbd_old;
/// CREATE TYPE … AS ENUM (…);      -- exactly the declared values, in order
/// ALTER TABLE … TYPE … USING c::text::…;
/// ALTER TABLE … SET DEFAULT …;
/// CREATE INDEX …;                 -- the dropped indexes, against the new type
/// DROP TYPE   x__dbd_old;
/// ```
///
/// `ALTER COLUMN … TYPE` rebuilds every index on the column from its stored
/// definition, and one that compares the column with an enum literal is stored
/// as `mood <> 'bad'::app.mood` — after the rename, a comparison with the old
/// type, which fails with `operator does not exist: mood <> mood__dbd_old` and
/// names no index. So an index whose predicate or expression key reads the
/// column is dropped first and created after, as the design declares it when it
/// declares one of that name, else as it was. The indexes come from the tables
/// of `live` and `desired`: a canonicalized snapshot has none (`canonicalize`
/// strips them), so a caller passes ones that carry them — see
/// `with_indexes_of`.
///
/// Every failure this can hit is a *loud* one, inside the single implicit
/// transaction `execute_script` runs the batch in: a row holding a removed value
/// fails the `USING` cast, a default naming a removed value fails the
/// `SET DEFAULT`, an unmanaged dependent view fails the `ALTER … TYPE`, and a
/// routine whose *signature* uses the type follows the rename and so fails the
/// final `DROP TYPE` (`function app.p(app.x__dbd_old) depends on type …`).
/// None of them can half-apply, and all of them name the object. Tables and
/// routines that merely *reference* the enum's table are never dropped: only a
/// view over the column stands in the way of the swap.
///
/// A dependent **materialized view** is the one blocker it does not clear:
/// dbd never drops one (a `DROP … CASCADE` would repopulate it and take its
/// dependents with it). Whether one blocks is a fact about the database — one
/// dropped by hand blocks nothing, one the design does not declare still does —
/// so the swap is recorded in [`ReconcilePlan::enum_swaps`] with the columns it
/// moves, and the caller asks the database before any write (see
/// `Design::reconcile`). Deciding from the design meant a matview declared
/// there blocked forever: dropped by hand as told, it was re-created by the
/// same run, and the next declined again.
///
/// The type's comment is carried from the type renamed aside to its
/// replacement (`enum_comment_carry`); it used to go with the old type.
///
/// Dropped views are not re-created here — reconcile re-applies every managed
/// view on every run, after the enum and tables it depends on, which is exactly
/// what restores them.
pub fn plan_enum_recreation(
    plan: &mut ReconcilePlan,
    live: &Snapshot,
    desired: &Snapshot,
    desired_entities: &[Entity],
) {
    use crate::sql_quote::literal;

    let desired_by_name: HashMap<String, &crate::snapshot::EnumSnapshot> = desired
        .enums
        .iter()
        .map(|e| (format!("{}.{}", e.schema, e.name), e))
        .collect();

    for le in &live.enums {
        let qname = format!("{}.{}", le.schema, le.name);
        let Some(de) = desired_by_name.get(&qname) else {
            // Absent from the design entirely — that is an orphan, reported by
            // `plan_reconcile`'s warnings and never auto-dropped.
            continue;
        };
        let kept: HashSet<&str> = de.values.iter().map(String::as_str).collect();
        let removed: Vec<&str> = le
            .values
            .iter()
            .map(String::as_str)
            .filter(|v| !kept.contains(v))
            .collect();
        if removed.is_empty() {
            // Pure additions are `ALTER TYPE … ADD VALUE`, which needs no rewrite
            // of the dependent columns.
            continue;
        }

        let columns = enum_columns(live, &qname);
        let roots: HashSet<String> = columns.iter().map(|c| c.table.clone()).collect();
        // The views over the columns' tables, which the swap drops and the
        // write pass re-creates. Materialized views are left to the caller's
        // live check: `DROP VIEW` fails on one that exists.
        let dependents: Vec<(usize, &Entity)> = dependent_views(desired_entities, &roots)
            .into_iter()
            .filter(|(_, e)| e.entity_type == EntityType::View)
            .collect();
        plan.enum_swaps.push(EnumSwap {
            enum_name: qname.clone(),
            removed: removed.iter().map(|v| v.to_string()).collect(),
            columns: columns.iter().map(|c| (c.table.clone(), c.column.clone())).collect(),
        });

        let old_type = format!("{qname}{ENUM_RECREATE_SUFFIX}");
        let mut lines: Vec<String> = vec![format!(
            "-- {qname}: Postgres has no ALTER TYPE … DROP VALUE, so the type is recreated \
             (dropping {}).",
            removed.join(", ")
        )];

        // Deepest dependents first: a view over a view has to go before the view
        // it selects from. Every dependent left is a plain view, so `DROP VIEW`
        // is the right keyword.
        for (_, e) in &dependents {
            lines.push(format!("DROP VIEW IF EXISTS {};", qualified_entity_name(e)));
        }

        let rebuilt = indexes_to_rebuild(live, &columns);
        for (table, ix) in &rebuilt {
            if let Some(name) = &ix.name {
                lines.push(format!(
                    "DROP INDEX IF EXISTS {}.{};",
                    crate::sql_quote::ident(&table.schema),
                    crate::sql_quote::ident(name)
                ));
            }
        }

        for c in columns.iter().filter(|c| c.default_value.is_some()) {
            lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT;",
                c.table, c.column
            ));
        }

        lines.push(format!(
            "ALTER TYPE {qname} RENAME TO {}{ENUM_RECREATE_SUFFIX};",
            le.name
        ));
        lines.push(format!(
            "CREATE TYPE {qname} AS ENUM ({});",
            de.values.iter().map(|v| literal(v)).collect::<Vec<_>>().join(", ")
        ));
        lines.push(enum_comment_carry(&qname, &old_type));

        for c in &columns {
            // An array column cannot cast element-wise through the scalar text
            // type — `c::text::app.colour` is a type error on `app.colour[]`.
            let (via, to) = if c.is_array {
                (format!("{}::text[]", c.column), format!("{qname}[]"))
            } else {
                (format!("{}::text", c.column), qname.clone())
            };
            lines.push(column_type_swap(
                &format!(
                    "ALTER TABLE {} ALTER COLUMN {} TYPE {to} USING {via}::{to};",
                    c.table, c.column
                ),
                &format!("{}.{}", c.table, c.column),
                &qname,
            ));
        }

        for c in &columns {
            if let Some(default) = &c.default_value {
                lines.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {default};",
                    c.table, c.column
                ));
            }
        }

        for (table, live_ix) in &rebuilt {
            // The design's definition when it declares one of that name: the
            // live one may compare with a value this swap just removed.
            let ix = desired
                .tables
                .iter()
                .find(|d| d.schema == table.schema && d.name == table.name)
                .and_then(|d| d.indexes.iter().find(|d| d.name.is_some() && d.name == live_ix.name))
                .unwrap_or(live_ix);
            let qtable = format!(
                "{}.{}",
                crate::sql_quote::ident(&table.schema),
                crate::sql_quote::ident(&table.name)
            );
            lines.push(crate::emit::emit_index_sql(ix, &qtable, &table.name, false));
        }

        lines.push(format!("DROP TYPE {old_type};"));

        // The recreation IS the whole change for this enum: `CREATE TYPE` already
        // carries any added values, so an `ALTER TYPE … ADD VALUE` that
        // `plan_reconcile` planned would now run against a type that already has
        // them. Replace rather than merge.
        let sql = lines.join("\n");
        match plan.altered.iter_mut().find(|s| s.entity_name == qname) {
            Some(stmt) => stmt.sql = sql,
            None => plan.altered.push(ReconcileStatement {
                entity_name: qname.clone(),
                sql,
            }),
        }

        // Losing a value can reject rows the table already holds, so this is
        // gated behind `--allow-destructive` like any other data-losing change.
        // `plan_reconcile` named the dropped values already, from the diff.
        for value in &removed {
            let change = format!("{qname}: drops enum value {value}");
            plan.mark_dropped(change, &qname, DroppedKind::EnumValue, value);
        }
    }
}

/// `alter` — one column's `ALTER … TYPE … USING` in an enum swap — wrapped so
/// a row holding a removed value fails naming the column.
///
/// The cast fails with `invalid input value for enum app.mood: "meh"`, which
/// names neither table nor column, and the step reporting it is the enum's —
/// one swap moves columns in every table using the type. The wrapper adds the
/// column and what to do, and keeps the SQLSTATE; it runs inside the same
/// implicit transaction, so the failure still leaves nothing half-applied.
fn column_type_swap(alter: &str, column: &str, enum_name: &str) -> String {
    // The message is a RAISE format string inside a `$$` body: `%` is its
    // placeholder and `'` closes it.
    let escape = |s: &str| s.replace('\'', "''").replace('%', "%%");
    format!(
        "DO $$ BEGIN\n  {alter}\nEXCEPTION WHEN invalid_text_representation THEN\n  \
         RAISE EXCEPTION '{} holds a value {} no longer has (%) — update those rows first', SQLERRM \
         USING ERRCODE = 'invalid_text_representation';\nEND $$;",
        escape(column),
        escape(enum_name),
    )
}

/// Copy the comment of `old_type` — the enum renamed aside by a type swap — to
/// `qname`, its replacement, so the swap does not lose it.
///
/// Read from the database rather than the design: an enum's comment is not
/// part of any snapshot or diff, so the one it had is the one to keep. A type
/// with no comment copies `NULL`, which sets none.
pub(crate) fn enum_comment_carry(qname: &str, old_type: &str) -> String {
    use crate::sql_quote::literal;
    let (schema, name) = crate::entity::split_qualified_name(qname);
    format!(
        "DO $$ BEGIN\n  EXECUTE format('COMMENT ON TYPE %I.%I IS %L', {}, {}, \
         obj_description(to_regtype({}), 'pg_type'));\nEND $$;",
        literal(schema.as_deref().unwrap_or(DEFAULT_SCHEMA)),
        literal(&name),
        literal(old_type),
    )
}

/// The live indexes an enum swap over `columns` has to drop and create again —
/// those whose `WHERE` predicate or expression key reads one of the columns
/// (see [`plan_enum_recreation`]) — with their table, in table then index order
/// so the SQL is byte-stable run to run.
///
/// Taken from `live` alone: what blocks the swap is what the database has.
fn indexes_to_rebuild<'a>(
    live: &'a Snapshot,
    columns: &[EnumColumn],
) -> Vec<(&'a TableSnapshot, &'a crate::entity::IndexDef)> {
    let mut out = Vec::new();
    for t in &live.tables {
        let qtable = format!("{}.{}", t.schema, t.name);
        let read: Vec<&str> = columns
            .iter()
            .filter(|c| c.table == qtable)
            .map(|c| c.column.as_str())
            .collect();
        if read.is_empty() {
            continue;
        }
        out.extend(indexes_reading_columns(t, &read).into_iter().map(|ix| (t, ix)));
    }
    out.sort_by(|a, b| (&a.0.schema, &a.0.name, &a.1.name).cmp(&(&b.0.schema, &b.0.name, &b.1.name)));
    out
}

/// The indexes on `table` whose `WHERE` predicate or expression key reads any
/// of `columns` — the ones Postgres cannot rebuild across an enum type swap of
/// those columns. Shared by reconcile's swap and the migration `dbd snapshot`
/// writes for the same change.
pub(crate) fn indexes_reading_columns<'a>(
    table: &'a TableSnapshot,
    columns: &[&str],
) -> Vec<&'a crate::entity::IndexDef> {
    let reads = |expr: &str| columns.iter().any(|c| crate::sql_expr::reads_column(expr, c));
    table
        .indexes
        .iter()
        .filter(|ix| {
            ix.predicate.as_deref().is_some_and(reads) || ix.columns.iter().any(|k| k.is_expression && reads(&k.name))
        })
        .collect()
}

/// `canonical` with each table's indexes taken from the same table in `raw`.
///
/// Reconcile diffs canonicalized snapshots, and `canonicalize` strips indexes
/// (they converge separately, from the raw ones). [`plan_enum_recreation`]
/// reads columns in their canonical form but has to see the indexes too.
pub(crate) fn with_indexes_of(canonical: &Snapshot, raw: &Snapshot) -> Snapshot {
    let mut out = canonical.clone();
    for t in &mut out.tables {
        if let Some(r) = raw.tables.iter().find(|r| r.schema == t.schema && r.name == t.name) {
            t.indexes = r.indexes.clone();
        }
    }
    out
}

/// A live column whose type is the enum being recreated.
struct EnumColumn {
    /// Qualified table name (`schema.table`).
    table: String,
    column: String,
    default_value: Option<String>,
    /// Whether the column is `enum[]` rather than plain `enum`.
    is_array: bool,
}

/// Every live column typed by `qname`, scalar or array.
///
/// Reads the canonicalized snapshot, where an enum column's type is already
/// schema-qualified and lowercased and an array collapsed to a single `[]` — so
/// an exact match is enough and there is no spelling to re-derive here.
fn enum_columns(live: &Snapshot, qname: &str) -> Vec<EnumColumn> {
    let array = format!("{qname}[]");
    let mut out = Vec::new();
    for t in &live.tables {
        for c in &t.columns {
            let is_array = c.data_type == array;
            if !is_array && c.data_type != qname {
                continue;
            }
            out.push(EnumColumn {
                table: format!("{}.{}", t.schema, t.name),
                column: c.name.clone(),
                default_value: c.default_value.clone(),
                is_array,
            });
        }
    }
    out
}

/// Managed views and materialized views over any of `roots` — directly, or
/// through another such view — deepest first.
///
/// Only a view is a dependent here. `ALTER COLUMN … TYPE` is refused while a
/// view (or rule) selects the column; nothing else dbd manages blocks it. A
/// table with a foreign key to a root and a routine whose body reads one hold
/// no dependency on the column, so they are neither returned nor walked
/// through: every transitive referrer used to come back, and the caller
/// emitted each as `DROP VIEW` — failing on the first table (`"order_notes" is
/// not a view`), and naming tables and routines that must never be dropped.
///
/// Depth is the longest path from a root, so a view over a view sorts ahead of
/// the view it selects from — which is the order they have to be dropped in.
/// The iteration is bounded by the entity count so a `refers` cycle terminates
/// rather than spinning.
pub(crate) fn dependent_views<'a>(entities: &'a [Entity], roots: &HashSet<String>) -> Vec<(usize, &'a Entity)> {
    let views: Vec<&Entity> = entities
        .iter()
        .filter(|e| matches!(e.entity_type, EntityType::View | EntityType::MaterializedView))
        .collect();

    // A `refers` entry may or may not be schema-qualified, so each entity is
    // reachable under both spellings — matching how `build_dependency_map`
    // compares refs against `Entity::name` verbatim.
    let mut depth: HashMap<String, usize> = roots.iter().map(|r| (r.clone(), 0)).collect();
    for root in roots {
        if let Some((_, short)) = root.split_once('.') {
            depth.entry(short.to_string()).or_insert(0);
        }
    }

    for _ in 0..views.len() {
        let mut changed = false;
        for e in &views {
            let qname = qualified_entity_name(e);
            let schema = e.schema.clone().unwrap_or_else(|| DEFAULT_SCHEMA.to_string());
            let deepest = e
                .refers()
                .filter(|r| *r != qname && *r != e.name)
                .filter_map(|r| depth.get(r).or_else(|| depth.get(format!("{schema}.{r}").as_str())))
                .max()
                .copied();
            let Some(d) = deepest else { continue };
            if depth.get(&qname).is_none_or(|existing| *existing < d + 1) {
                depth.insert(qname, d + 1);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut out: Vec<(usize, &Entity)> = views
        .into_iter()
        .filter_map(|e| depth.get(&qualified_entity_name(e)).map(|d| (*d, e)))
        .filter(|(d, _)| *d > 0)
        .collect();
    // Deepest first; name breaks ties so the emitted SQL is byte-stable run to run.
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    out
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
/// Indexes match by `IndexShape` (name-agnostic: unique flag, access method,
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
        // Each side's predicate is canonicalized against its OWN declared types.
        // They agree whenever the column has not drifted, which is the case the
        // matching is for; when it has, the column diff reports it and the index
        // reading as changed is the conservative answer.
        let desired_cols = coercible_columns(&dt.columns);
        let live_cols = coercible_columns(&lt.columns);
        let desired_shapes: HashSet<IndexShape> = desired_ix.iter().map(|i| index_shape(i, &desired_cols)).collect();
        let live_shapes: HashSet<IndexShape> = live_ix.iter().map(|i| index_shape(i, &live_cols)).collect();

        let mut lines: Vec<String> = Vec::new();

        // Drops first (destructive): a live index the design no longer declares.
        // Use the live index's real name — that's what `DROP INDEX` needs. An
        // index lives in its table's schema, so qualify the drop with it. Drops
        // precede adds so a shape-change on a shared name doesn't collide.
        for ix in live_ix
            .iter()
            .filter(|ix| !desired_shapes.contains(&index_shape(ix, &live_cols)))
        {
            if let Some(name) = &ix.name {
                lines.push(format!("DROP INDEX IF EXISTS \"{}\".\"{}\";", dt.schema, name));
                let table = format!("{}.{}", dt.schema, dt.name);
                plan.mark_dropped(format!("{table}: drops index {name}"), &table, DroppedKind::Index, name);
            }
        }

        // Adds: a design index missing from the live DB. Rendered via the same
        // `emit::emit_index_sql` helper the initial CREATE uses (idempotent
        // `IF NOT EXISTS`, correct `USING <method>`), so a GIN/GiST index
        // converges as its real access method, not a plain btree.
        let qtable = format!("\"{}\".\"{}\"", dt.schema, dt.name);
        for ix in desired_ix
            .iter()
            .filter(|ix| !live_shapes.contains(&index_shape(ix, &desired_cols)))
        {
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
///
/// `cols` carries the table's binary-coercible columns so a partial index over a
/// `varchar` column matches the predicate Postgres reports back — see
/// [`coercible_columns`].
fn index_shape(ix: &crate::entity::IndexDef, cols: &crate::sql_expr::CoercibleColumns) -> IndexShape {
    use crate::entity::SortOrder;
    // Collapse the default spellings (`using btree`, `asc`) first, so the shape
    // of an authored index matches the shape of the introspected one.
    let mut ix = ix.clone();
    crate::schema_diff::normalize_index(&mut ix, cols);

    let columns = ix
        .columns
        .iter()
        .map(|c| IndexColumnShape {
            // An expression is case-significant (it can contain string literals);
            // a column name is not.
            name: if c.is_expression {
                c.name.clone()
            } else {
                c.name.to_lowercase()
            },
            is_expression: c.is_expression,
            descending: matches!(c.order, Some(SortOrder::Desc)),
            nulls_first: c.nulls_first,
            opclass: c.opclass.as_ref().map(|o| o.to_lowercase()),
        })
        .collect();
    IndexShape {
        unique: ix.unique,
        method: ix
            .index_type
            .as_ref()
            .map_or("btree".to_string(), |t| t.amname().to_string()),
        columns,
        predicate: ix.predicate.clone(),
        include: ix.include.iter().map(|c| c.to_lowercase()).collect(),
        nulls_not_distinct: ix.nulls_not_distinct,
        // BTreeMap iteration is already name-ordered, so an authored and a
        // `reloptions` ordering hash the same.
        with_options: ix.with_options.into_iter().collect(),
    }
}

/// A table's secondary indexes: every index EXCEPT those merely backing a
/// PRIMARY KEY / UNIQUE constraint (a live snapshot may carry those, the
/// parsed design never does — matched by the constraint's name, which its
/// backing index bears). Mirrors the suppression in
/// `schema_diff::normalize_for_diff` through the same
/// [`crate::schema_diff::backs_a_constraint`].
fn secondary_indexes(t: &TableSnapshot) -> Vec<&crate::entity::IndexDef> {
    let backing = crate::schema_diff::constraint_backing_keys(&t.table_constraints, str::to_lowercase);
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
    /// Materialized views whose live `v1` hash sentinel was silently re-stamped
    /// to `v2`. Counted separately from `reapplied` so this one-time upgrade
    /// write — to a live object, during what looks like a routine reconcile —
    /// is visible in the summary rather than folded into an unrelated count.
    pub restamped: u32,
    /// `apply.before` hook scripts run.
    pub before_scripts: u32,
    /// `apply.after` hook scripts run.
    pub after_scripts: u32,
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
// Drift is detected by recording a deterministic hash of the DESIGN as a
// `dbd:hash=…` stamp for the live object at CREATE time — in the bookkeeping
// schema (`DatabaseAdapter::stamp_matview`), not the view's comment, which is
// its author's — then comparing the stored hash to a freshly computed one on
// later runs. The hash is over the design, not Postgres's deparsed
// `pg_matviews.definition`, so it is exact and deparser-independent.

/// What a reconcile should do with one design materialized view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatviewAction {
    /// Absent live → create it and stamp the hash sentinel.
    Create,
    /// Live hash matches the design → nothing to do.
    Skip,
    /// Live sentinel is `v1` (written under a superseded body contract, so its
    /// hash isn't comparable to a current one) → re-stamp to `v2` silently.
    /// Never a `Warn`: a v1/v2 mismatch says nothing about whether the design
    /// actually drifted.
    Restamp,
    /// Exists but the design differs (or it carries no dbd sentinel) → warn and
    /// leave it untouched. dbd never auto-drops a materialized view.
    Warn,
}

/// Decide the action for a design matview from its wanted hash and the live
/// sentinel state:
/// - `None` — the matview does not exist live → **Create**.
/// - `Some(Some(Sentinel::V2(h)))` — it exists with stored hash `h`; **Skip**
///   iff `h == want`, else **Warn** (definition drifted).
/// - `Some(Some(Sentinel::V1(_)))` — stamped under the superseded body
///   contract → **Restamp** to `v2` (not comparable, so never a drift signal).
/// - `Some(None)` — it exists but carries no `dbd:hash` sentinel (created outside
///   dbd, or before this feature) → **Warn** (cannot verify its definition).
pub(crate) fn decide_matview_action(want_hash: &str, live: Option<Option<Sentinel>>) -> MatviewAction {
    match live {
        None => MatviewAction::Create,
        Some(Some(Sentinel::V2(h))) if h == want_hash => MatviewAction::Skip,
        Some(Some(Sentinel::V1(_))) => MatviewAction::Restamp,
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

/// The drift stamp for `hash`: `dbd:hash=v2:<hash>`, recorded through
/// [`crate::adapter::DatabaseAdapter::stamp_matview`].
///
/// Versioned (`v2:`) because the hash input is the matview's `writes[0]`, so
/// any change to what that field holds changes every stamp already written.
/// Without a version, such a change makes every existing matview report drift
/// once on upgrade — indistinguishable from real drift, with a warning that
/// tells users to `DROP … CASCADE`. See [`Sentinel`] and
/// [`decide_matview_action`] for how an unversioned (`v1`) stamp is handled:
/// re-stamped silently, never read as a drift signal.
pub(crate) fn matview_stamp(hash: &str) -> String {
    format!("dbd:hash=v2:{hash}")
}

/// A parsed `dbd:hash` stamp.
///
/// Versioned because the hash input is the matview's `writes[0]`, so any
/// change to what that field holds changes every stamp already written. A
/// bare hash predates versioning and is recognised as `V1`: its value hashes a
/// different body contract than the current code computes, so it must never
/// be compared against a current hash — see [`decide_matview_action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sentinel {
    /// Written by a dbd that hashed the pre-versioning body contract. Its hash
    /// is not comparable with a current one, so it is re-stamped rather than
    /// read.
    V1(String),
    /// Current format (`dbd:hash=v2:<hash>`).
    V2(String),
}

/// Extract the sentinel from a `dbd:hash=…` comment, or `None` when the
/// comment is absent or carries no such sentinel. Inverse of
/// [`matview_stamp`]'s payload; tolerant of trailing text after the
/// token.
pub(crate) fn parse_dbd_hash(comment: Option<&str>) -> Option<Sentinel> {
    let rest = comment?.split("dbd:hash=").nth(1)?;
    // Alphanumeric-or-`:` so a versioned `v2:<hash>` token is captured whole.
    // Plain `is_ascii_alphanumeric` stops at the `:` and would silently yield
    // the literal "v2" as the hash — wrong, and it would never match a real
    // hash, turning every matview into a *permanent* false-drift warning.
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == ':')
        .collect();
    match token.strip_prefix("v2:") {
        Some(hash) if !hash.is_empty() => Some(Sentinel::V2(hash.to_string())),
        // An unversioned token is a v1 stamp. Reject anything else carrying a
        // colon — an unrecognised future version must not be misread as v1.
        _ if !token.is_empty() && !token.contains(':') => Some(Sentinel::V1(token)),
        _ => None,
    }
}

/// `CREATE MATERIALIZED VIEW … WITH DATA;` (+ any index statements, via
/// [`crate::emit::emit_entity`]). The caller records its stamp separately.
pub(crate) fn matview_create_sql(entity: &Entity) -> String {
    crate::emit::emit_entity(entity).unwrap_or_default()
}

/// Whether a deployed materialized view (`live`, as introspected) is the
/// design's: the same query and the same index key set.
///
/// The queries are compared as libpg_query parse trees, deparsed — so layout,
/// keyword case and the formatter's leading commas do not count, while any
/// difference in what the query does does. Postgres prints a deployed
/// definition its own way (qualified names, explicit casts), so a hand-written
/// body often reads differently from it; that is a `false`, which leaves the
/// matview unverified rather than vouching for it.
pub(crate) fn same_matview_definition(design: &Entity, live: &Entity) -> bool {
    fn canonical(e: &Entity) -> Option<String> {
        let body = e.body.first()?.trim().trim_end_matches(';');
        pg_query::deparse(&pg_query::parse(body).ok()?.protobuf).ok()
    }
    matview_index_keys(design) == matview_index_keys(live)
        && canonical(design).is_some_and(|d| canonical(live).is_some_and(|l| l == d))
}

/// Normalize a matview's `SELECT` body for a stable hash input: first `body`
/// entry, trimmed, a single trailing `;` removed, internal whitespace collapsed
/// to single spaces, lowercased.
fn normalize_matview_body(e: &Entity) -> String {
    let raw = e.body.first().map(String::as_str).unwrap_or_default();
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
        .map(|ix| (ix.unique, ix.columns.iter().map(|c| c.name.to_lowercase()).collect()))
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
            generated: None,
            comment: None,
            inline_fk: None,
            collation: None,
        }
    }

    // ── issue #17: varchar's implicit ::text cast ────────────────

    /// A `varchar` column and one CHECK — the shape of the repro.
    fn checked_varchar_table(expression: &str) -> TableSnapshot {
        let mut t = table("app", "chk", vec![col("name", "varchar(100)"), col("n", "integer")]);
        t.table_constraints = vec![TableConstraint::Check {
            name: Some("chk_name_lower".to_string()),
            expression: expression.to_string(),
        }];
        t
    }

    fn only_key(t: &TableSnapshot) -> String {
        table_checks(t).first().expect("one check").0.clone()
    }

    /// The property #17 is about, asserted on the function reconcile actually
    /// calls: the authored spelling and the one `pg_get_constraintdef` reports
    /// must produce one key, or the CHECK is dropped and re-added forever.
    #[test]
    fn a_varchar_check_keys_identically_from_both_sides() {
        assert_eq!(
            only_key(&checked_varchar_table("name = lower(name)")),
            only_key(&checked_varchar_table("name::text = lower(name::text)")),
        );
    }

    /// `n` is `integer`, so its `::text` is the author's and stays — keying on
    /// the declared type is what separates the two cases.
    #[test]
    fn a_check_on_a_non_coercible_column_keeps_its_cast() {
        assert!(only_key(&checked_varchar_table("n::text <> ''")).contains("::text"));
    }

    // ── issue #16: STORED generated columns ──────────────────────

    /// Issue #16: the authored generation expression and the analyzed form
    /// `pg_attrdef` reports must converge. Without this, moving the expression
    /// off `default_value` only trades the `DROP DEFAULT` abort for perpetual
    /// drift on the expression instead.
    ///
    /// The introspected string is real `pg_get_expr` output from PG 17 for the
    /// authored column above it.
    #[test]
    fn a_generated_expression_converges_with_its_introspected_form() {
        let authored = canonical_generated("to_tsvector('english', coalesce(content,''))");
        let introspected = canonical_generated("to_tsvector('english'::regconfig, COALESCE(content, ''::text))");
        assert_eq!(authored, introspected);
    }

    /// A genuinely different expression must still read as changed.
    #[test]
    fn a_changed_generated_expression_stays_different() {
        assert_ne!(canonical_generated("upper(name)"), canonical_generated("lower(name)"));
    }

    #[test]
    fn only_binary_coercible_columns_qualify() {
        let cols = vec![
            col("name", "varchar(100)"),
            col("alias", "character varying"),
            col("code", "char(8)"),
            col("txt", "text"),
            col("n", "integer"),
            col("tags", "varchar(10)[]"),
        ];
        let set = coercible_columns(&cols);
        assert!(set.contains("name"), "varchar(n) is binary-coercible");
        assert!(set.contains("alias"), "unparameterized varchar too");
        // bpchar → text runs `rtrim1`, which strips trailing spaces. Erasing that
        // cast would change what the predicate accepts.
        assert!(!set.contains("code"), "char(n) must NOT qualify");
        assert!(!set.contains("txt"), "text needs no cast to begin with");
        assert!(!set.contains("n"));
        // An array cast relabels the shape, not just the element type.
        assert!(!set.contains("tags"), "varchar[] must NOT qualify");
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

    // ── pg_catalog-qualified type names ─────────────────────────────────────

    /// Postgres rewrites SQL-standard type names at parse time, so libpg_query
    /// reports `pg_catalog.int4` where sqlparser reports `INT`. Both name the
    /// same type; the alias table below already knows `int4`, it just never saw
    /// it because the qualified form does not reach the match.
    #[test]
    fn pg_catalog_qualified_types_normalize_like_their_bare_form() {
        let e = std::collections::HashMap::new();
        for (qualified, bare) in [
            ("pg_catalog.int4", "int"),
            ("pg_catalog.int8", "bigint"),
            ("pg_catalog.int2", "smallint"),
            ("pg_catalog.bool", "boolean"),
            ("pg_catalog.varchar(30)", "varchar(30)"),
            ("pg_catalog.bpchar(2)", "char(2)"),
            ("pg_catalog.numeric(10,2)", "numeric(10,2)"),
            ("pg_catalog.timestamptz", "timestamp with time zone"),
        ] {
            assert_eq!(
                canonical_type(qualified, &e),
                canonical_type(bare, &e),
                "{qualified} and {bare} must canonicalize alike"
            );
        }
    }

    /// A column's type reaches dbd in two spellings that have to converge or the
    /// diff never comes back clean. The **design** side is whatever libpg_query's
    /// deparser emits for the authored DDL (`time`, `timestamptz(3)`); the
    /// **live** side is `format_type`, which always spells the time zone out and
    /// puts the modifier on the base word (`time without time zone`,
    /// `timestamp(3) with time zone`). Both columns below were read off a real
    /// parse and a real `pg_attribute` row.
    ///
    /// `time` is the case that bit: the deparser renders BOTH `time` and
    /// `time without time zone` as plain `time`, so no DDL spelling could ever
    /// match the live side — every `time` column read as a permanent
    /// `ALTER COLUMN … TYPE` that changed nothing.
    #[test]
    fn datetime_spellings_converge_on_the_format_type_form() {
        let e = std::collections::HashMap::new();
        for (design, live) in [
            ("time", "time without time zone"),
            ("time(3)", "time(3) without time zone"),
            ("timestamp", "timestamp without time zone"),
            ("timestamp(6)", "timestamp(6) without time zone"),
            ("timetz", "time with time zone"),
            ("timetz(3)", "time(3) with time zone"),
            ("timestamptz", "timestamp with time zone"),
            ("timestamptz(3)", "timestamp(3) with time zone"),
            // The deparser leaves a space before the modifier on the spelled-out
            // form: `time(3) with time zone` comes back `time (3) with time zone`.
            ("time (3) with time zone", "time(3) with time zone"),
            ("timestamp (3) with time zone", "timestamp(3) with time zone"),
        ] {
            assert_eq!(
                canonical_type(design, &e),
                live,
                "{design} must canonicalize to the format_type spelling"
            );
            assert_eq!(
                canonical_type(live, &e),
                live,
                "{live} is already canonical and must survive unchanged"
            );
        }
    }

    /// Time zone awareness is a real type difference, not a spelling — collapsing
    /// it would hide a genuine `ALTER COLUMN … TYPE` instead of a phantom one.
    #[test]
    fn time_zone_awareness_is_not_normalized_away() {
        let e = std::collections::HashMap::new();
        assert_ne!(canonical_type("time", &e), canonical_type("timetz", &e));
        assert_ne!(canonical_type("timestamp", &e), canonical_type("timestamptz", &e));
        assert_ne!(canonical_type("time", &e), canonical_type("timestamp", &e));
        assert_ne!(canonical_type("time(3)", &e), canonical_type("time(6)", &e));
    }

    /// The alias table has to reach through an array suffix too. `format_type`
    /// spells an array as its normalized element type plus `[]`, so an authored
    /// `int[]` has to become `integer[]` — and because Postgres does not track
    /// array dimensionality, a declared `text[][]` IS just `text[]`.
    #[test]
    fn array_types_normalize_through_their_element_type() {
        let e = std::collections::HashMap::new();
        for (design, live) in [
            ("int[]", "integer[]"),
            ("int4[]", "integer[]"),
            ("bigint[]", "bigint[]"),
            ("varchar(30)[]", "character varying(30)[]"),
            ("boolean[]", "boolean[]"),
            ("numeric(10,2)[]", "numeric(10,2)[]"),
            ("smallint[]", "smallint[]"),
            ("time[]", "time without time zone[]"),
            ("timestamp[]", "timestamp without time zone[]"),
            ("timestamptz[]", "timestamp with time zone[]"),
            ("timetz[]", "time with time zone[]"),
            ("text[][]", "text[]"),
            // A declared BOUND is as inert as a declared dimension — Postgres
            // records neither, and dbd's own parser emits the bounded spelling
            // (`create table t (a text[3])` parses to `text[3]`), so without
            // these the digit branch in `split_array_suffix` never sees a digit
            // and every bounded-array column reads as permanently drifted.
            ("text[3]", "text[]"),
            ("int[3]", "integer[]"),
            ("varchar(4)[2]", "character varying(4)[]"),
        ] {
            assert_eq!(
                canonical_type(design, &e),
                live,
                "{design} must canonicalize to the format_type spelling"
            );
            assert_eq!(
                canonical_type(live, &e),
                live,
                "{live} is already canonical and must survive unchanged"
            );
        }
    }

    /// An array of an enum still gets the bare name schema-qualified — the array
    /// suffix must not shadow the enum lookup that plain columns get.
    #[test]
    fn array_of_enum_is_still_schema_qualified() {
        let mut e = std::collections::HashMap::new();
        e.insert("status_t".to_string(), "app.status_t".to_string());
        assert_eq!(canonical_type("status_t[]", &e), "app.status_t[]");
        assert_eq!(canonical_type("status_t", &e), "app.status_t");
    }

    /// `interval` carries its qualifier as fields, not as a time-zone suffix, and
    /// both sides already agree on it — the datetime rewrite must not touch it.
    #[test]
    fn interval_is_left_alone() {
        let e = std::collections::HashMap::new();
        assert_eq!(canonical_type("interval", &e), "interval");
        assert_eq!(canonical_type("interval hour to minute", &e), "interval hour to minute");
    }

    /// Only the two system schemas are stripped. A user type keeps its schema —
    /// `app.status_t` and `other.status_t` are different types.
    #[test]
    fn a_user_schema_qualification_is_preserved() {
        let e = std::collections::HashMap::new();
        assert_eq!(canonical_type("app.status_t", &e), "app.status_t");
        assert_ne!(canonical_type("app.status_t", &e), canonical_type("other.status_t", &e));
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
            columns: vec![ColumnDef {
                inline_fk: Some(fk),
                ..col("org_id", "uuid")
            }],
            constraints: vec![],
            indexes: vec![],
            comments: TableComments::default(),
        });
        let entities = vec![entity];

        let raw = raw_snapshot_from_entities(&entities);
        assert!(
            raw.tables[0].columns[0].inline_fk.is_some(),
            "raw builder must retain the inline FK"
        );

        let canon = snapshot_from_entities(&entities);
        assert!(
            canon.tables[0].columns[0].inline_fk.is_none(),
            "canonicalize builder must strip the inline FK"
        );
    }

    // ── Foreign-key convergence (issue #8) ──────────────────

    fn ref_fk(
        name: Option<&str>,
        col: &str,
        ref_schema: Option<&str>,
        ref_table: &str,
        on_delete: Option<FkAction>,
    ) -> ForeignKey {
        ForeignKey {
            name: name.map(str::to_string),
            columns: vec![col.to_string()],
            ref_schema: ref_schema.map(str::to_string),
            ref_table: ref_table.to_string(),
            ref_columns: vec!["id".to_string()],
            on_delete,
            on_update: None,
            ..Default::default()
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
            plan.altered[0]
                .sql
                .contains("ADD FOREIGN KEY (parent_id) REFERENCES app.parents(id)"),
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
            Some("children_parent_id_fkey"),
            "parent_id",
            Some("app"),
            "parents",
            None,
        )));
        let live = snap(vec![live_t]);
        let desired = snap(vec![table("app", "children", vec![col("parent_id", "uuid")])]);

        let mut plan = ReconcilePlan::default();
        plan_fk_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        assert!(
            plan.altered[0]
                .sql
                .contains("DROP CONSTRAINT IF EXISTS \"children_parent_id_fkey\""),
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
            Some("children_parent_id_fkey"),
            "parent_id",
            Some("app"),
            "parents",
            Some(FkAction::NoAction),
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

        assert!(
            plan.is_empty() && !plan.destructive,
            "in-sync FK must reconcile to no change; got {plan:?}"
        );
    }

    /// A changed FK action (design adds `ON DELETE CASCADE`) drops the old and
    /// adds the new — Postgres can't alter FK actions in place.
    #[test]
    fn fk_convergence_changed_action_replaces() {
        let mut live_t = table("app", "children", vec![col("parent_id", "uuid")]);
        live_t.table_constraints.push(TableConstraint::ForeignKey(ref_fk(
            Some("children_parent_id_fkey"),
            "parent_id",
            Some("app"),
            "parents",
            None,
        )));
        let live = snap(vec![live_t]);
        let desired = snap(vec![TableSnapshot {
            columns: vec![ColumnDef {
                inline_fk: Some(ref_fk(
                    None,
                    "parent_id",
                    Some("app"),
                    "parents",
                    Some(FkAction::Cascade),
                )),
                ..col("parent_id", "uuid")
            }],
            ..table("app", "children", vec![])
        }]);

        let mut plan = ReconcilePlan::default();
        plan_fk_convergence(&mut plan, &live, &desired);

        assert_eq!(plan.altered.len(), 1);
        let sql = &plan.altered[0].sql;
        assert!(
            sql.contains("DROP CONSTRAINT IF EXISTS \"children_parent_id_fkey\""),
            "must drop old FK; got: {sql}"
        );
        assert!(
            sql.contains("ADD FOREIGN KEY (parent_id) REFERENCES app.parents(id) ON DELETE CASCADE"),
            "must add the new FK with the action; got: {sql}"
        );
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
        ColumnDef {
            comment: comment.map(str::to_string),
            ..col(name, "text")
        }
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

        assert!(
            plan.altered[0].sql.contains("IS 'new text';"),
            "got: {}",
            plan.altered[0].sql
        );
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
        assert!(
            plan.is_empty(),
            "a new column's comment rides its ADD COLUMN; got {plan:?}"
        );

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
        assert!(
            !sql.contains("unnamed"),
            "an unnamed CHECK must not be named \"unnamed\"; got: {sql}"
        );
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
        assert!(
            sql.contains("DROP CONSTRAINT IF EXISTS \"docs_status_check\""),
            "got: {sql}"
        );
        assert!(
            !sql.contains("ck:"),
            "the expression key must never be used as a name; got: {sql}"
        );
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
        assert!(
            sql.contains("DROP CONSTRAINT IF EXISTS \"docs_status_check\""),
            "got: {sql}"
        );
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

        assert!(
            plan.is_empty(),
            "a new table's CHECKs come with its CREATE; got {plan:?}"
        );
    }

    // ── Index convergence (issue #12) ────────────────────────

    fn idx(name: &str, cols: &[&str], unique: bool) -> crate::entity::IndexDef {
        use crate::entity::{IndexColumn, IndexDef};
        IndexDef {
            name: Some(name.to_string()),
            columns: cols
                .iter()
                .map(|c| IndexColumn {
                    name: (*c).to_string(),
                    ..Default::default()
                })
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
                IndexColumn {
                    name: "project_id".to_string(),
                    ..Default::default()
                },
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
                ..table(
                    "app",
                    "runs",
                    vec![col("project_id", "uuid"), col("started_at", "timestamptz")],
                )
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
        assert!(
            sql.contains("WHERE folder_id IS NULL"),
            "the new predicate must be created; got: {sql}"
        );
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
                    ColumnDef {
                        is_pk: true,
                        nullable: false,
                        ..col("parent_id", "uuid")
                    },
                    ColumnDef {
                        is_pk: true,
                        nullable: false,
                        ..col("child_id", "uuid")
                    },
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

        assert_eq!(
            plan.altered.len(),
            1,
            "composite-PK member index must converge; got {plan:?}"
        );
        assert!(
            plan.altered[0].sql.contains("memory_links_child_id_idx"),
            "got: {}",
            plan.altered[0].sql
        );
    }

    /// The index that genuinely backs a composite PRIMARY KEY is still suppressed —
    /// should a live snapshot report it, the design never declares it. A backing
    /// index carries its constraint's name (Postgres names it so), and a live
    /// constraint always has one.
    #[test]
    fn index_convergence_still_ignores_composite_pk_backing_index() {
        let mut live_t = table(
            "app",
            "memory_links",
            vec![col("parent_id", "uuid"), col("child_id", "uuid")],
        );
        live_t.table_constraints.push(TableConstraint::PrimaryKey {
            name: Some("memory_links_pkey".to_string()),
            columns: vec!["parent_id".to_string(), "child_id".to_string()],
        });
        let mut desired_t = live_t.clone();
        live_t
            .indexes
            .push(idx("memory_links_pkey", &["parent_id", "child_id"], true));
        desired_t.indexes.clear();

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &snap(vec![live_t]), &snap(vec![desired_t]));

        assert!(
            plan.is_empty() && !plan.destructive,
            "a PK-backing index must never be dropped as an orphan; got {plan:?}"
        );
    }

    /// A design index over the primary key's columns is an index of its own.
    /// Suppressed as constraint backing on both sides, one dropped from the
    /// database was never re-created. The live constraint is named, as
    /// introspection names it; the design's is not.
    #[test]
    fn index_convergence_recreates_a_dropped_index_over_the_primary_key_columns() {
        let mut desired_t = table("app", "t", vec![col("id", "integer")]);
        desired_t.table_constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["id".to_string()],
        });
        let mut live_t = desired_t.clone();
        live_t.table_constraints = vec![TableConstraint::PrimaryKey {
            name: Some("t_pkey".to_string()),
            columns: vec!["id".to_string()],
        }];
        desired_t.indexes.push(idx("t_id_idx", &["id"], false));

        let mut plan = ReconcilePlan::default();
        plan_index_convergence(&mut plan, &snap(vec![live_t]), &snap(vec![desired_t]));

        assert!(
            plan.altered.first().is_some_and(|a| a.sql.contains("\"t_id_idx\"")),
            "the dropped index is re-created; got {plan:?}"
        );
    }

    /// A partial UNIQUE index is a real index, never PK/UNIQUE constraint backing:
    /// `unique (a)` and `unique index (a) where b is null` enforce different things,
    /// so the partial one must not be suppressed as "already a constraint".
    #[test]
    fn index_convergence_keeps_partial_unique_index_over_constrained_columns() {
        let mut live_t = table(
            "app",
            "libs",
            vec![col("library_id", "uuid"), col("project_id", "uuid")],
        );
        live_t.table_constraints.push(TableConstraint::Unique {
            name: None,
            columns: vec!["library_id".to_string()],
            nulls_not_distinct: false,
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
            plan.altered[0]
                .sql
                .contains("DROP INDEX IF EXISTS \"app\".\"children_parent_id_idx\";"),
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
            table_constraints: vec![TableConstraint::PrimaryKey {
                name: None,
                columns: vec!["id".to_string()],
            }],
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
        assert!(
            sql.contains("DROP INDEX IF EXISTS \"app\".\"users_email_idx\";"),
            "must drop old index; got: {sql}"
        );
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
            columns: vec![IndexColumn {
                name: "tags".to_string(),
                order: None,
                ..Default::default()
            }],
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
        ColumnDef {
            is_pk: true,
            nullable: false,
            ..col(name, ty)
        }
    }
    fn not_null(name: &str, ty: &str) -> ColumnDef {
        ColumnDef {
            nullable: false,
            ..col(name, ty)
        }
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

    /// A table whose PRIMARY KEY covers `pk_cols`, named `pk_name` when the live
    /// database named it (introspection always does; the parsed design does not).
    fn table_with_pk(pk_name: Option<&str>, pk_cols: &[&str]) -> TableSnapshot {
        TableSnapshot {
            table_constraints: vec![TableConstraint::PrimaryKey {
                name: pk_name.map(str::to_string),
                columns: pk_cols.iter().map(|c| c.to_string()).collect(),
            }],
            ..table(
                "app",
                "metrics",
                vec![
                    col("tenant_id", "uuid"),
                    col("metric_id", "uuid"),
                    col("bucket", "date"),
                ],
            )
        }
    }

    /// Replacing a composite PRIMARY KEY must produce SQL Postgres can actually
    /// run: the drop names the constraint's REAL live name, and it runs BEFORE the
    /// add. The bug this pins emitted the internal matching key as an identifier
    /// (`DROP CONSTRAINT pk:tenant_id,metric_id`) — the same defect
    /// [`plan_check_convergence`] already fixed for `ck:<expression>` — and ordered
    /// the add first, which Postgres rejects with "multiple primary keys".
    #[test]
    fn pk_replacement_drops_by_real_live_name_before_adding() {
        let mut live = snap(vec![table_with_pk(Some("metrics_pkey"), &["tenant_id", "metric_id"])]);
        let mut desired = snap(vec![table_with_pk(None, &["tenant_id", "metric_id", "bucket"])]);

        canonicalize(&mut live);
        canonicalize(&mut desired);
        let plan = plan_reconcile(&live, &desired);

        assert_eq!(plan.altered.len(), 1, "got {plan:?}");
        let sql = &plan.altered[0].sql;
        assert!(
            !sql.contains("pk:"),
            "the internal matching key must never be used as an identifier; got: {sql}"
        );
        assert!(
            !sql.contains("unnamed"),
            "an unnamed PK must not be named \"unnamed\"; got: {sql}"
        );
        let drop_at = sql
            .find("DROP CONSTRAINT")
            .unwrap_or_else(|| panic!("no DROP CONSTRAINT; got: {sql}"));
        let add_at = sql
            .find("PRIMARY KEY (tenant_id, metric_id, bucket)")
            .unwrap_or_else(|| panic!("no PK add; got: {sql}"));
        assert!(
            sql[drop_at..].starts_with("DROP CONSTRAINT IF EXISTS \"metrics_pkey\""),
            "drop must name the real live constraint; got: {sql}"
        );
        assert!(
            drop_at < add_at,
            "the old PK must be dropped before the new one is added; got: {sql}"
        );
        assert!(plan.destructive, "replacing a PK is gated as destructive");
    }

    /// The matching guarantee that makes the drop-by-real-name fix safe: a live PK
    /// that Postgres auto-named and the design's unnamed PK over the SAME columns
    /// are the same constraint. Preserving the live name for the emitter must not
    /// make a name difference read as drift and churn a destructive drop+add.
    #[test]
    fn pk_name_difference_alone_is_not_drift() {
        let mut live = snap(vec![table_with_pk(Some("metrics_pkey"), &["tenant_id", "metric_id"])]);
        let mut desired = snap(vec![table_with_pk(None, &["tenant_id", "metric_id"])]);

        canonicalize(&mut live);
        canonicalize(&mut desired);
        let plan = plan_reconcile(&live, &desired);

        assert!(
            plan.is_empty() && !plan.destructive,
            "an auto-named live PK and the design's unnamed PK over the same columns \
             are the same constraint; got {plan:?}"
        );
    }

    /// Name-agnostic matching must not swallow a rename the design asked for.
    /// The DDL parser captures an explicit `constraint <name> primary key (…)`, so
    /// when BOTH sides name the constraint a difference is deliberate and has to be
    /// expressed as a drop of the live name plus an add under the declared one.
    #[test]
    fn explicitly_renamed_pk_is_still_replaced() {
        let mut live = snap(vec![table_with_pk(Some("metrics_pkey"), &["tenant_id", "metric_id"])]);
        let mut desired = snap(vec![table_with_pk(Some("metrics_pk"), &["tenant_id", "metric_id"])]);

        canonicalize(&mut live);
        canonicalize(&mut desired);
        let plan = plan_reconcile(&live, &desired);

        assert_eq!(
            plan.altered.len(),
            1,
            "an explicit PK rename must be acted on; got {plan:?}"
        );
        let sql = &plan.altered[0].sql;
        assert!(sql.contains("DROP CONSTRAINT IF EXISTS \"metrics_pkey\""), "got: {sql}");
        assert!(sql.contains("ADD CONSTRAINT metrics_pk PRIMARY KEY"), "got: {sql}");
    }

    /// A live PK with no name at all cannot be dropped by any statement. Reconcile
    /// must say so rather than emit SQL that fails the whole transactional apply —
    /// mirroring [`plan_check_convergence`]'s unnamed-CHECK warning.
    #[test]
    fn pk_replacement_warns_when_live_constraint_has_no_name() {
        let mut live = snap(vec![table_with_pk(None, &["tenant_id", "metric_id"])]);
        let mut desired = snap(vec![table_with_pk(None, &["tenant_id", "metric_id", "bucket"])]);

        canonicalize(&mut live);
        canonicalize(&mut desired);
        let plan = plan_reconcile(&live, &desired);

        let sql: String = plan.altered.iter().map(|s| s.sql.as_str()).collect();
        assert!(
            !sql.contains("DROP CONSTRAINT"),
            "no DROP statement is possible for a constraint that has no name; got: {sql}"
        );
        assert!(
            plan.warnings.iter().any(|w| w.contains("no constraint name")),
            "an undroppable live PK must be warned about; got {:?}",
            plan.warnings
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
        let mut desired = snap(vec![table("public", "counter", vec![col_default("n", "integer", "1")])]);
        let mut live = snap(vec![table("public", "counter", vec![col_default("n", "integer", "0")])]);

        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.altered.len(), 1, "changed default must be planned; got {plan:?}");
        assert!(plan.altered[0].sql.contains("SET DEFAULT 1"));
    }

    // ── Defaults the server rewrites on store (#18) ─────────────────────

    /// `jsonb` does not preserve key order or spacing, so a text comparison of
    /// an object default never converges. Measured on a live server:
    /// `'{"b":1,"a":2}'` is stored as `'{"a": 2, "b": 1}'::jsonb`.
    ///
    /// Both sides canonicalise to sorted keys with Postgres's own spacing,
    /// which is still a valid literal — it has to be, because this value is
    /// what `SET DEFAULT` emits.
    #[test]
    fn a_jsonb_object_default_converges_regardless_of_key_order() {
        let design = canonical_default(r#"'{"b":1,"a":2}'"#, "jsonb");
        let live = canonical_default(r#"'{"a": 2, "b": 1}'::jsonb"#, "jsonb");
        assert_eq!(design, live);
        assert_eq!(design, r#"'{"a": 2, "b": 1}'"#, "and it is Postgres's own spelling");
    }

    #[test]
    fn nested_jsonb_keys_are_ordered_too() {
        let design = canonical_default(r#"'{"z":{"b":1,"a":2}}'"#, "jsonb");
        let live = canonical_default(r#"'{"z": {"a": 2, "b": 1}}'::jsonb"#, "jsonb");
        assert_eq!(design, live);
    }

    /// A `jsonb` default that is not an object is left alone — there is no key
    /// order to settle, and re-rendering risks changing it.
    #[test]
    fn a_non_object_jsonb_default_is_untouched() {
        assert_eq!(canonical_default("'[]'", "jsonb"), "'[]'");
        assert_eq!(canonical_default("'null'", "jsonb"), "'null'");
    }

    /// `'epoch'` is a special input Postgres resolves on store — and renders
    /// back **in the session timezone**, so the live text differs by
    /// connection as well as by spelling. Measured: `'1969-12-31 18:00:00-06'`
    /// for a `-06` session. Both sides reduce to the same instant.
    #[test]
    fn a_timestamptz_special_input_converges_to_its_instant() {
        let design = canonical_default("'epoch'", "timestamptz");
        let live = canonical_default("'1969-12-31 18:00:00-06'::timestamp with time zone", "timestamptz");
        assert_eq!(design, live, "same instant, different spelling and timezone");
    }

    #[test]
    fn two_spellings_of_one_instant_converge() {
        let a = canonical_default("'2024-03-01 12:00:00+00'", "timestamptz");
        let b = canonical_default("'2024-03-01 07:00:00-05'::timestamp with time zone", "timestamptz");
        assert_eq!(a, b);
    }

    /// `'infinity'` already round-trips unchanged and must stay that way — it
    /// is not an instant and must not be coerced into one.
    #[test]
    fn infinity_is_left_as_written() {
        assert_eq!(canonical_default("'infinity'", "timestamptz"), "'infinity'");
        assert_eq!(canonical_default("'-infinity'", "timestamptz"), "'-infinity'");
    }

    /// **Cannot converge, by construction.** `'now'` and `'today'` are
    /// resolved at DDL time and frozen, so the live value is a fixed timestamp
    /// that differs from the design text on every later day. Normalising them
    /// would mean inventing a value. They are left alone and read as drift,
    /// which is the honest answer — the fix is to write `now()` or
    /// `current_date`, which are re-evaluated and do converge.
    #[test]
    fn a_frozen_special_input_is_not_normalised() {
        assert_eq!(canonical_default("'today'", "date"), "'today'");
        assert_eq!(canonical_default("'now'", "timestamptz"), "'now'");
        assert_ne!(
            canonical_default("'today'", "date"),
            canonical_default("'2026-09-26'::date", "date"),
            "and it must NOT be made to match some particular day"
        );
    }

    /// Postgres stores `'t'`, `'true'` and `TRUE` alike as `true`.
    #[test]
    fn boolean_default_spellings_converge() {
        for written in ["'t'", "'true'", "true", "'y'", "'yes'", "'1'"] {
            assert_eq!(canonical_default(written, "boolean"), "true", "{written}");
        }
        for written in ["'f'", "'false'", "false", "'n'", "'no'", "'0'"] {
            assert_eq!(canonical_default(written, "boolean"), "false", "{written}");
        }
    }

    /// A default the normaliser does not understand keeps its text, so it can
    /// read as drift but never as falsely equal.
    #[test]
    fn an_unrecognised_default_is_passed_through() {
        // An inner cast stays — only a single TRAILING top-level one is
        // stripped, which is what keeps `nextval('s'::regclass)` intact.
        assert_eq!(
            canonical_default("nextval('s'::regclass)", "integer"),
            "nextval('s'::regclass)"
        );
        assert_eq!(canonical_default("'{oops'", "jsonb"), "'{oops'");
        assert_eq!(canonical_default("'not a date'", "timestamptz"), "'not a date'");
    }

    /// Type-awareness must not change what the old text-only rules did for
    /// every other type.
    #[test]
    fn canonical_default_strips_trailing_cast() {
        assert_eq!(canonical_default("'{}'::text[]", "text"), "'{}'");
        assert_eq!(canonical_default("''::text", "text"), "''");
        assert_eq!(canonical_default("'active'::config.status", "text"), "'active'");
        assert_eq!(
            canonical_default("'2020-01-01'::timestamp with time zone", "text"),
            "'2020-01-01'"
        );
        assert_eq!(canonical_default("0::numeric(10,2)", "text"), "0");
        assert_eq!(canonical_default("  'x' :: text ", "text"), "'x'");
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
            columns: vec![ColumnDef {
                comment: Some("the total".to_string()),
                ..col("total", "int4")
            }],
            indexes: vec![IndexDef {
                name: Some("orders_total_idx".to_string()),
                columns: vec![IndexColumn {
                    name: "total".to_string(),
                    order: None,
                    ..Default::default()
                }],
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
                    ..Default::default()
                }),
                TableConstraint::Check {
                    name: Some("ck".to_string()),
                    expression: "total > 0".to_string(),
                },
            ],
        }]);
        normalize_common(&mut snap);
        let t = &snap.tables[0];
        assert_eq!(t.columns[0].data_type, "integer", "types must still be normalized");
        assert_eq!(t.columns[0].comment.as_deref(), Some("the total"), "comment preserved");
        assert_eq!(t.indexes.len(), 1, "indexes preserved");
        assert!(
            t.table_constraints
                .iter()
                .any(|c| matches!(c, TableConstraint::ForeignKey(_))),
            "FK preserved"
        );
        assert!(
            t.table_constraints
                .iter()
                .any(|c| matches!(c, TableConstraint::Check { .. })),
            "CHECK preserved"
        );
    }

    #[test]
    fn canonical_default_leaves_non_casts_intact() {
        // No top-level cast → unchanged.
        assert_eq!(canonical_default("now()", "text"), "now()");
        assert_eq!(canonical_default("0", "text"), "0");
        assert_eq!(canonical_default("false", "text"), "false");
        // Cast lives inside the function args, not on the whole expression.
        assert_eq!(
            canonical_default("nextval('app.seq'::regclass)", "text"),
            "nextval('app.seq'::regclass)"
        );
        // `::` embedded in a string literal must not be treated as a cast.
        assert_eq!(canonical_default("'a::b'", "text"), "'a::b'");
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
                canonical_default(authored, "text"),
                canonical_default(introspected, "text"),
                "{authored} and {introspected} are the same default"
            );
        }
        // Catalog functions: Postgres stores these lowercase whatever was authored.
        assert_eq!(canonical_default("NOW()", "text"), canonical_default("now()", "text"));
        assert_eq!(canonical_default("Gen_Random_Uuid()", "text"), "gen_random_uuid()");
        // SQL constructs: stored uppercase, and the literal arguments keep their case.
        assert_eq!(
            canonical_default("coalesce('X', 'y')", "text"),
            canonical_default("COALESCE('X', 'y')", "text")
        );
    }

    /// Folding case must never reach inside quoted text: the canonical form is
    /// emitted as `SET DEFAULT` DDL, so folding a literal would change the value
    /// actually written to the database.
    #[test]
    fn canonical_default_preserves_quoted_text_case() {
        assert_eq!(canonical_default("'Mixed Case'", "text"), "'Mixed Case'");
        assert_eq!(canonical_default("'Mixed Case'::text", "text"), "'Mixed Case'");
        assert_eq!(canonical_default("upper('aB')", "text"), "upper('aB')");
        assert_eq!(canonical_default("UPPER('aB')", "text"), "upper('aB')");
        // A quoted identifier is case-significant too.
        assert_eq!(canonical_default("\"MyFunc\"()", "text"), "\"MyFunc\"()");
        // Doubled quotes escape: the run continues, so inner case survives.
        assert_eq!(canonical_default("'It''s A Value'", "text"), "'It''s A Value'");
        // Two defaults differing only inside a literal must NOT converge.
        assert_ne!(
            canonical_default("'Active'", "text"),
            canonical_default("'active'", "text")
        );
    }

    /// A dollar-quoted default is left exactly as authored rather than risking a
    /// fold inside its body — at worst it keeps reading as drift.
    #[test]
    fn canonical_default_leaves_dollar_quoted_alone() {
        assert_eq!(canonical_default("$$Hello World$$", "text"), "$$Hello World$$");
        assert_eq!(canonical_default("$tag$Mixed$tag$", "text"), "$tag$Mixed$tag$");
    }

    /// Non-ASCII text survives folding intact (byte-wise lowercasing would corrupt
    /// a multibyte sequence).
    #[test]
    fn canonical_default_handles_non_ascii() {
        assert_eq!(canonical_default("'café'", "text"), "'café'");
        assert_eq!(canonical_default("'CAFÉ'", "text"), "'CAFÉ'");
    }

    // ── Materialized-view convergence (Task 13) ──────────────

    fn mv(name: &str, body: &str) -> Entity {
        let mut e = Entity::new(EntityType::MaterializedView, name);
        e.body = vec![body.into()];
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
                columns: vec![IndexColumn {
                    name: "x".into(),
                    order: None,
                    ..Default::default()
                }],
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

    // ── Sentinel versioning ─────────────────────────────────────────────────

    /// The stamp SQL and `parse_dbd_hash` round-trip through the current (`v2`)
    /// format: the payload stored (`dbd:hash=v2:<hex>`) parses back to the same
    /// hash.
    #[test]
    fn v2_stamp_round_trips() {
        let stamp = matview_stamp("deadbeefcafe0001");
        assert_eq!(stamp, "dbd:hash=v2:deadbeefcafe0001");
        assert_eq!(
            parse_dbd_hash(Some(&stamp)),
            Some(Sentinel::V2("deadbeefcafe0001".to_string()))
        );
    }

    /// A stamp written by an older dbd has no version prefix. It must be
    /// recognised as v1, not misparsed — `take_while(is_ascii_alphanumeric)`
    /// would stop at the `:` of a v2 stamp and yield "v2".
    #[test]
    fn unversioned_stamp_parses_as_v1() {
        assert_eq!(
            parse_dbd_hash(Some("dbd:hash=deadbeefcafe0001")),
            Some(Sentinel::V1("deadbeefcafe0001".to_string()))
        );
    }

    #[test]
    fn absent_or_foreign_comment_has_no_sentinel() {
        assert_eq!(parse_dbd_hash(None), None);
        assert_eq!(parse_dbd_hash(Some("just a comment")), None);
    }

    /// An empty hash token (`dbd:hash=` with nothing after it) is also `None`,
    /// distinct from no sentinel at all — both must not crash the parser.
    #[test]
    fn parse_dbd_hash_empty_hash_is_none() {
        assert_eq!(parse_dbd_hash(Some("dbd:hash=")), None, "empty hash → None");
    }

    /// A create is the `CREATE MATERIALIZED VIEW … WITH DATA` alone — the stamp
    /// is recorded apart, and the view's comment is left to its author.
    #[test]
    fn matview_create_sql_creates_and_leaves_the_comment_alone() {
        let sql = matview_create_sql(&mv("a.m", "SELECT 1 AS x"));
        assert!(sql.contains("CREATE MATERIALIZED VIEW IF NOT EXISTS \"a\".\"m\" AS SELECT 1 AS x WITH DATA;"));
        assert!(!sql.contains("COMMENT ON"), "{sql}");
    }

    /// A v1 stamp means "written by a dbd that hashed the old body contract",
    /// so its hash is not comparable — re-stamp silently rather than warn.
    #[test]
    fn a_v1_stamp_is_restamped_not_warned() {
        assert!(matches!(
            decide_matview_action("want", Some(Some(Sentinel::V1("anything".into())))),
            MatviewAction::Restamp
        ));
    }

    #[test]
    fn a_matching_v2_stamp_is_skipped() {
        assert!(matches!(
            decide_matview_action("want", Some(Some(Sentinel::V2("want".into())))),
            MatviewAction::Skip
        ));
    }

    #[test]
    fn a_differing_v2_stamp_is_real_drift() {
        assert!(matches!(
            decide_matview_action("want", Some(Some(Sentinel::V2("other".into())))),
            MatviewAction::Warn
        ));
    }

    #[test]
    fn an_unstamped_matview_still_warns() {
        assert!(matches!(decide_matview_action("want", Some(None)), MatviewAction::Warn));
    }

    #[test]
    fn an_absent_matview_is_created() {
        assert!(matches!(decide_matview_action("want", None), MatviewAction::Create));
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

    /// A plan carrying only a matview restamp is NOT empty either — it is a
    /// real write to a live object, so `--dry-run` must preview it rather than
    /// claiming "Already in sync".
    #[test]
    fn is_empty_false_when_only_matview_restamps() {
        let plan = ReconcilePlan {
            matview_restamps: vec!["analytics.daily_sales".to_string()],
            ..Default::default()
        };
        assert!(!plan.is_empty());
    }
}

#[cfg(test)]
mod enum_recreation_tests {
    use super::*;
    use crate::entity::{ColumnDef, Entity};
    use crate::snapshot::{EnumSnapshot, TableSnapshot};

    fn col(name: &str, data_type: &str, default_value: Option<&str>) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: default_value.map(str::to_string),
            is_pk: false,
            is_unique: false,
            identity: None,
            generated: None,
            comment: None,
            inline_fk: None,
            collation: None,
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

    fn enum_snap(schema: &str, name: &str, values: &[&str]) -> EnumSnapshot {
        EnumSnapshot {
            name: name.to_string(),
            schema: schema.to_string(),
            values: values.iter().map(|v| v.to_string()).collect(),
        }
    }

    fn snap(tables: Vec<TableSnapshot>, enums: Vec<EnumSnapshot>) -> Snapshot {
        Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables,
            enums,
        }
    }

    fn entity(entity_type: EntityType, name: &str, refers: &[&str]) -> Entity {
        let mut e = Entity::new(entity_type, name);
        e.refs = refers
            .iter()
            .map(|r| crate::entity::Ref::stated(*r, crate::entity::RefKind::Reads))
            .collect();
        e
    }

    /// The whole point of issue #12.2: Postgres has no `ALTER TYPE … DROP VALUE`,
    /// so reconcile has to recreate the type. Before this, `generate_field_sql`
    /// emitted nothing for an enum-value drop, `plan_reconcile` skipped the empty
    /// SQL, and reconcile reported `0 altered` while `diff` reported the same
    /// drift forever.
    #[test]
    fn removing_an_enum_value_plans_a_type_recreation() {
        let live = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green", "blue"])],
        );
        let desired = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green"])],
        );
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &[]);

        let stmt = plan
            .altered
            .iter()
            .find(|s| s.entity_name == "app.colour")
            .expect("the enum must get a recreation statement");
        let sql = &stmt.sql;

        // Rename out of the way, create the declared type, move the column across
        // by text, drop the old type. Order is load-bearing.
        let rename = sql.find("ALTER TYPE app.colour RENAME TO").expect("rename");
        let create = sql
            .find("CREATE TYPE app.colour AS ENUM ('red', 'green')")
            .expect("create with exactly the declared values, in declared order");
        let alter = sql
            .find("ALTER TABLE app.t ALTER COLUMN c TYPE app.colour USING c::text::app.colour")
            .expect("column moved across by text");
        let drop = sql.find("DROP TYPE app.colour__dbd_old").expect("old type dropped");
        assert!(rename < create && create < alter && alter < drop, "wrong order:\n{sql}");

        assert!(plan.destructive, "losing an enum value can reject existing rows");
    }

    /// A column default is typed against the old enum, so it has to come off
    /// before the swap and go back on after — otherwise the `ALTER … TYPE` fails
    /// with "default for column cannot be cast automatically".
    #[test]
    fn a_defaulted_column_has_its_default_dropped_and_restored() {
        let live = snap(
            vec![table(
                "app",
                "t",
                vec![col("c", "app.colour", Some("'red'::app.colour"))],
            )],
            vec![enum_snap("app", "colour", &["red", "green", "blue"])],
        );
        let desired = snap(
            vec![table(
                "app",
                "t",
                vec![col("c", "app.colour", Some("'red'::app.colour"))],
            )],
            vec![enum_snap("app", "colour", &["red", "green"])],
        );
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &[]);
        let sql = &plan.altered[0].sql;

        let drop_default = sql
            .find("ALTER TABLE app.t ALTER COLUMN c DROP DEFAULT")
            .expect("default dropped");
        let type_swap = sql.find("ALTER TABLE app.t ALTER COLUMN c TYPE").expect("type swap");
        let set_default = sql
            .find("ALTER TABLE app.t ALTER COLUMN c SET DEFAULT 'red'::app.colour")
            .expect("default restored");
        assert!(
            drop_default < type_swap && type_swap < set_default,
            "the default must come off before the swap and go back after:\n{sql}"
        );
    }

    /// An array column holds the type too, and `c::text::app.colour` is a type
    /// error on `app.colour[]`. It has to round-trip through `text[]`.
    #[test]
    fn an_array_column_casts_through_the_array_text_type() {
        let live = snap(
            vec![table("app", "t", vec![col("c", "app.colour[]", None)])],
            vec![enum_snap("app", "colour", &["red", "green", "blue"])],
        );
        let desired = snap(
            vec![table("app", "t", vec![col("c", "app.colour[]", None)])],
            vec![enum_snap("app", "colour", &["red", "green"])],
        );
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &[]);
        let sql = &plan.altered[0].sql;
        assert!(
            sql.contains("ALTER COLUMN c TYPE app.colour[] USING c::text[]::app.colour[]"),
            "an array column must cast through text[]:\n{sql}"
        );
    }

    /// `ALTER COLUMN … TYPE` rebuilds every index on the column from its stored
    /// definition. One whose `WHERE` or expression key compares the column with
    /// a literal of the enum — stored as `'bad'::app.mood` — is re-read after the
    /// rename as a comparison with the *old* type, and the swap failed with
    /// `operator does not exist: mood <> mood__dbd_old`, naming no index. Those
    /// are dropped before the swap and created again after it; an index that
    /// does not read the column is left alone.
    #[test]
    fn an_index_reading_the_column_is_rebuilt_around_the_swap() {
        use crate::entity::{IndexColumn, IndexDef};
        let index = |name: &str, key: &str, is_expression: bool, predicate: Option<&str>| IndexDef {
            name: Some(name.to_string()),
            columns: vec![IndexColumn {
                name: key.to_string(),
                is_expression,
                ..Default::default()
            }],
            predicate: predicate.map(str::to_string),
            ..Default::default()
        };
        let mut people = table(
            "app",
            "people",
            vec![col("id", "integer", None), col("mood", "app.mood", None)],
        );
        // Spelled as introspection reports them (`pg_get_expr`, `pg_get_indexdef`).
        people.indexes = vec![
            index("people_not_bad", "id", false, Some("(mood <> 'bad'::app.mood)")),
            index("people_is_ok", "(mood = 'ok'::app.mood)", true, None),
            index("people_by_id", "id", false, Some("(id > 0)")),
        ];
        let live = snap(vec![people], vec![enum_snap("app", "mood", &["ok", "meh", "bad"])]);
        let desired = snap(
            vec![table(
                "app",
                "people",
                vec![col("id", "integer", None), col("mood", "app.mood", None)],
            )],
            vec![enum_snap("app", "mood", &["ok", "bad"])],
        );
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &[]);
        let sql = &plan.altered[0].sql;

        let swap = sql
            .find("ALTER TABLE app.people ALTER COLUMN mood TYPE")
            .expect("type swap");
        for name in ["people_not_bad", "people_is_ok"] {
            let drop = sql
                .find(&format!("DROP INDEX IF EXISTS \"app\".\"{name}\";"))
                .unwrap_or_else(|| panic!("{name} is dropped:\n{sql}"));
            let create = sql
                .find(&format!("INDEX \"{name}\" ON \"app\".\"people\""))
                .unwrap_or_else(|| panic!("{name} is created again:\n{sql}"));
            assert!(drop < swap && swap < create, "{name} around the swap:\n{sql}");
        }
        assert!(
            !sql.contains("people_by_id"),
            "an index that does not read the column is left alone:\n{sql}"
        );
    }

    /// A view over the affected table blocks `ALTER TABLE … ALTER COLUMN … TYPE`
    /// ("cannot alter type of a column used by a view or rule"). Managed views are
    /// dropped first and re-created by reconcile's write pass, which re-applies
    /// every view on every run anyway. Transitive dependents count: a view over a view.
    #[test]
    fn managed_views_over_the_affected_table_are_dropped_first() {
        let live = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green", "blue"])],
        );
        let desired = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green"])],
        );
        let entities = vec![
            entity(EntityType::Table, "app.t", &[]),
            entity(EntityType::View, "app.v", &["app.t"]),
            entity(EntityType::View, "app.v2", &["app.v"]),
            entity(EntityType::View, "app.unrelated", &["app.other"]),
        ];
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &entities);
        let sql = &plan.altered[0].sql;

        // Anchored on the terminator: `…app.v` is also a prefix of `…app.v2`.
        let v2 = sql.find("DROP VIEW IF EXISTS app.v2;").expect("transitive dependent");
        let v = sql.find("DROP VIEW IF EXISTS app.v;").expect("direct dependent");
        let swap = sql.find("ALTER TABLE app.t ALTER COLUMN c TYPE").expect("type swap");
        assert!(v2 < v, "dependents must be dropped before what they depend on:\n{sql}");
        assert!(v < swap, "views must be gone before the column type changes:\n{sql}");
        assert!(
            !sql.contains("app.unrelated"),
            "a view that does not depend on the table must be left alone:\n{sql}"
        );
    }

    /// Only a view stands in the way of `ALTER COLUMN … TYPE`. A table with a
    /// foreign key to the affected table, and a routine whose body reads it, do
    /// not — Postgres records no dependency on the column for either — and they
    /// must never be dropped: every dependent used to be emitted as `DROP VIEW`,
    /// which failed with `"order_notes" is not a view` and would have destroyed a
    /// table's rows had it been the right keyword. Nor does a view or matview
    /// over such a table depend on the column, so neither is dropped or blocks.
    #[test]
    fn only_views_over_the_affected_table_are_dropped_never_tables_or_routines() {
        let live = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green", "blue"])],
        );
        let desired = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green"])],
        );
        let entities = vec![
            entity(EntityType::Table, "app.t", &[]),
            entity(EntityType::Table, "app.notes", &["app.t"]),
            entity(EntityType::Procedure, "app.p", &["app.t"]),
            entity(EntityType::Function, "app.f", &["app.t"]),
            entity(EntityType::View, "app.v", &["app.t"]),
            entity(EntityType::View, "app.notes_v", &["app.notes"]),
            entity(EntityType::MaterializedView, "app.notes_mv", &["app.notes"]),
        ];
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &entities);

        assert!(
            plan.warnings.iter().all(|w| !w.contains("app.notes_mv")),
            "a matview over a referencing table does not depend on the column: {:?}",
            plan.warnings
        );
        let sql = &plan
            .altered
            .iter()
            .find(|s| s.entity_name == "app.colour")
            .expect("the recreation is planned")
            .sql;
        let drops: Vec<&str> = sql.lines().filter(|l| l.starts_with("DROP ")).collect();
        assert_eq!(
            drops,
            vec!["DROP VIEW IF EXISTS app.v;", "DROP TYPE app.colour__dbd_old;"],
            "only the view over the column may be dropped:\n{sql}"
        );
    }

    /// Whether a materialized view blocks the swap is a fact about the
    /// database: one dropped by hand blocks nothing, and one the design does
    /// not declare still does. The design's matviews decided it, so dropping
    /// the matview as told and re-running declined again, forever. The planner
    /// now plans the swap and records the columns it moves, which the caller
    /// checks against the live database (`Design::reconcile`); and it never
    /// emits `DROP VIEW` for a matview, which fails on one that exists.
    #[test]
    fn a_design_matview_over_the_column_is_left_to_the_live_check() {
        let live = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green", "blue"])],
        );
        let desired = snap(
            vec![table("app", "t", vec![col("c", "app.colour", None)])],
            vec![enum_snap("app", "colour", &["red", "green"])],
        );
        let entities = vec![
            entity(EntityType::Table, "app.t", &[]),
            entity(EntityType::MaterializedView, "app.mv", &["app.t"]),
            entity(EntityType::View, "app.v", &["app.t"]),
        ];
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &entities);

        let sql = &plan
            .altered
            .iter()
            .find(|s| s.entity_name == "app.colour")
            .expect("the swap is planned")
            .sql;
        assert!(sql.contains("DROP VIEW IF EXISTS app.v;"), "{sql}");
        assert!(!sql.contains("app.mv"), "a matview is never dropped by the swap: {sql}");
        assert_eq!(
            plan.enum_swaps,
            vec![EnumSwap {
                enum_name: "app.colour".to_string(),
                removed: vec!["blue".to_string()],
                columns: vec![("app.t".to_string(), "c".to_string())],
            }]
        );
    }

    /// Adding values needs no recreation — `ALTER TYPE … ADD VALUE` handles it,
    /// and recreating would be a needless rewrite of every dependent column.
    #[test]
    fn adding_an_enum_value_plans_no_recreation() {
        let live = snap(vec![], vec![enum_snap("app", "colour", &["red"])]);
        let desired = snap(vec![], vec![enum_snap("app", "colour", &["red", "green"])]);
        let mut plan = ReconcilePlan::default();
        plan_enum_recreation(&mut plan, &live, &desired, &[]);
        assert!(plan.altered.is_empty(), "got {:?}", plan.altered);
        assert!(!plan.destructive);
    }

    /// When values are both added and removed, the recreation is the whole
    /// change: `CREATE TYPE` already carries the added value. Leaving the
    /// `ALTER TYPE … ADD VALUE` that `plan_reconcile` emitted would then fail
    /// against the freshly-created type, which already has it.
    #[test]
    fn a_recreation_replaces_the_add_value_statement_for_the_same_enum() {
        let live = snap(vec![], vec![enum_snap("app", "colour", &["red", "blue"])]);
        let desired = snap(vec![], vec![enum_snap("app", "colour", &["red", "green"])]);
        let mut plan = plan_reconcile(&live, &desired);
        assert!(
            plan.altered.iter().any(|s| s.sql.contains("ADD VALUE")),
            "precondition: the value add is planned first"
        );
        plan_enum_recreation(&mut plan, &live, &desired, &[]);

        let stmt = plan
            .altered
            .iter()
            .find(|s| s.entity_name == "app.colour")
            .expect("recreation statement");
        assert!(
            !stmt.sql.contains("ADD VALUE"),
            "the recreation subsumes the add; got:\n{}",
            stmt.sql
        );
        assert!(stmt.sql.contains("CREATE TYPE app.colour AS ENUM ('red', 'green')"));
    }
}

#[cfg(test)]
mod extension_type_tests {
    use super::*;
    use crate::entity::ColumnDef;
    use crate::snapshot::EnumSnapshot;

    fn col(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            identity: None,
            generated: None,
            comment: None,
            inline_fk: None,
            collation: None,
        }
    }

    fn snap(columns: Vec<ColumnDef>, enums: Vec<EnumSnapshot>) -> Snapshot {
        Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables: vec![TableSnapshot {
                name: "docs".to_string(),
                schema: "app".to_string(),
                columns,
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums,
        }
    }

    fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(s, n)| (s.to_string(), n.to_string())).collect()
    }

    fn types_of(snap: &Snapshot) -> Vec<&str> {
        snap.tables[0].columns.iter().map(|c| c.data_type.as_str()).collect()
    }

    /// pgvector's type carries a modifier: `format_type` reads
    /// `extensions.vector(384)` where DDL wrote `vector(384)` — #41's own
    /// column. The bare spelling gets the schema and keeps the modifier, in an
    /// array too, and the two sides plan nothing.
    #[test]
    fn an_extension_type_keeps_its_modifier() {
        let types = owned(&[("extensions", "vector")]);
        let mut desired = snap(
            vec![col("embedding", "vector(384)"), col("history", "vector(3)[]")],
            vec![],
        );
        let mut live = snap(
            vec![
                col("embedding", "extensions.vector(384)"),
                col("history", "extensions.vector(3)[]"),
            ],
            vec![],
        );
        qualify_extension_types(&mut desired, &types);
        qualify_extension_types(&mut live, &types);
        assert_eq!(types_of(&desired), ["extensions.vector(384)", "extensions.vector(3)[]"]);

        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert!(plan.altered.is_empty(), "{:?}", plan.altered);
    }

    /// A database whose own path reaches the schema — Supabase's does — reads
    /// the type bare, and so does a pooled connection that ran a file's `set
    /// search_path`. The live side is qualified the same way as the design's.
    #[test]
    fn a_live_type_read_bare_is_qualified_too() {
        let types = owned(&[("extensions", "vector"), ("extensions", "citext")]);
        let mut live = snap(vec![col("embedding", "vector(384)"), col("tags", "citext[]")], vec![]);
        qualify_extension_types(&mut live, &types);
        assert_eq!(types_of(&live), ["extensions.vector(384)", "extensions.citext[]"]);
    }

    /// A real change to an extension type names it qualified, so the ALTER
    /// resolves under a path that does not reach the extension's schema.
    #[test]
    fn a_real_change_to_an_extension_type_alters_to_the_qualified_type() {
        let types = owned(&[("extensions", "citext")]);
        let mut desired = snap(vec![col("note", "citext")], vec![]);
        let mut live = snap(vec![col("note", "text")], vec![]);
        qualify_extension_types(&mut desired, &types);
        qualify_extension_types(&mut live, &types);
        canonicalize(&mut desired);
        canonicalize(&mut live);
        let plan = plan_reconcile(&live, &desired);
        assert_eq!(plan.altered.len(), 1, "{:?}", plan.altered);
        assert!(
            plan.altered[0]
                .sql
                .contains("TYPE extensions.citext USING note::extensions.citext"),
            "{}",
            plan.altered[0].sql
        );
    }

    /// Two extensions that each own a `ltree`, in different schemas: which one
    /// a bare `ltree` names depends on the file's path, which the snapshot does
    /// not carry. It is left as written rather than guessed at.
    #[test]
    fn a_name_two_extensions_own_is_left_as_written() {
        let types = owned(&[("ext_a", "ltree"), ("ext_b", "ltree"), ("extensions", "citext")]);
        let mut desired = snap(vec![col("path", "ltree"), col("email", "citext")], vec![]);
        qualify_extension_types(&mut desired, &types);
        assert_eq!(types_of(&desired), ["ltree", "extensions.citext"]);
    }

    /// A project enum named like an extension's type is the project's: a file
    /// declaring `app.cube` reaches it before `extensions.cube`. The bare name
    /// is left to the enum's own qualification.
    #[test]
    fn a_name_the_projects_enum_declares_stays_the_enums() {
        let types = owned(&[("extensions", "cube")]);
        let enums = vec![EnumSnapshot {
            name: "cube".to_string(),
            schema: "app".to_string(),
            values: vec!["small".to_string()],
        }];
        let mut desired = snap(vec![col("size", "cube")], enums);
        qualify_extension_types(&mut desired, &types);
        canonicalize(&mut desired);
        assert_eq!(types_of(&desired), ["app.cube"]);
    }
}
