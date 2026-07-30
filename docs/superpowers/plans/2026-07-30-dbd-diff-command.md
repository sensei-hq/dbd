# `dbd diff` Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `dbd diff` command that shows the complete difference between the live database and the design — including the FK / CHECK / index / comment drift that `reconcile --dry-run` deliberately omits.

**Architecture:** Reuse the existing full diff engine (`diff::diff`, which already compares columns, PK/unique, FK, CHECK, indexes, enum values). The only new comparison logic is a normalization pass, `normalize_for_diff()`, that removes parsed-vs-introspected representation noise *without* stripping FK/CHECK/index/comment the way reconcile's `canonicalize()` does. A read-only `Design::diff_live()` wires introspection + design into a serializable `SchemaDiff`; a new CLI `cmd_diff` renders it (human or `--json`) with a terraform-style `--exit-code`.

**Tech Stack:** Rust, `dbd-core` (diff engine, `pg_query`/libpg_query for CHECK canonicalization, `serde`), `dbd-cli` (clap, `output` module).

**Spec:** `docs/superpowers/specs/2026-07-30-dbd-diff-command-design.md`

---

## File structure

**dbd-core**
- Modify `crates/dbd-core/src/reconcile.rs` — extract `normalize_common()` out of `canonicalize()` (behavior of `canonicalize` unchanged).
- Create `crates/dbd-core/src/schema_diff.rs` — `normalize_for_diff()`, `SchemaDiff`, helpers.
- Modify `crates/dbd-core/src/design.rs` — add `Design::diff_live()`.
- Modify `crates/dbd-core/src/lib.rs` — `pub mod schema_diff;` + re-export `SchemaDiff`.

**dbd-cli**
- Modify `crates/dbd-cli/src/cli.rs` — add `Commands::Diff { json, exit_code }`.
- Create `crates/dbd-cli/src/commands/diff.rs` — `cmd_diff`, pure `diff_report_lines()`, pure `diff_exit_code()`.
- Modify `crates/dbd-cli/src/commands/mod.rs` — `mod diff;` + dispatch arm.

**Docs**
- Modify `docs/guide/04-commands.md`, `docs/llms/llms-full.txt`, `docs/llms/llms.txt`.

Conventions: `cargo test -p <crate> <filter>` runs a subset. The repo has a pre-commit hook that runs `cargo test` + `cargo clippy -- -D warnings`; every commit must be warning-free.

---

## Task 1: Extract `normalize_common()` from `canonicalize()` (reconcile behavior unchanged)

Split reconcile's `canonicalize()` (`crates/dbd-core/src/reconcile.rs:124`) into a shared normalization half (`normalize_common`) plus the reconcile-only stripping half. `canonicalize` must produce byte-identical results — the existing reconcile tests are the guard.

**Files:**
- Modify: `crates/dbd-core/src/reconcile.rs` (`canonicalize` at line 124, `lift_pk_unique_constraints` at 149, `normalize_columns` at 197)
- Test: `crates/dbd-core/src/reconcile.rs` (existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** — add to `reconcile.rs` tests module:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core normalize_common_preserves -- --nocapture`
Expected: FAIL to compile — `cannot find function normalize_common in this scope`.

- [ ] **Step 3: Refactor `reconcile.rs`.** Replace `canonicalize` and split the two helpers. New code:

```rust
/// Representation normalization shared by reconcile's `canonicalize` and the
/// full `schema_diff::normalize_for_diff`. Makes a parsed (desired) and an
/// introspected (live) form of the *same* object compare equal for the
/// attributes both paths care about — column types/defaults, enum
/// qualification, and PK/unique constraints — while PRESERVING FK/CHECK
/// constraints, indexes, and column comments for callers that diff them.
pub(crate) fn normalize_common(snap: &mut Snapshot) {
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

/// Canonicalize a snapshot so a parsed and an introspected table of the same
/// shape compare equal for what reconcile manages. Reconcile does NOT manage
/// FK/CHECK/indexes/comments on existing tables (their introspected/parsed
/// forms differ too much), so after the shared normalization it strips them.
pub fn canonicalize(snap: &mut Snapshot) {
    normalize_common(snap);
    for t in &mut snap.tables {
        t.indexes.clear();
        t.table_constraints.retain(|c| {
            matches!(c, TableConstraint::PrimaryKey { .. } | TableConstraint::Unique { .. })
        });
        for c in &mut t.columns {
            c.inline_fk = None;
            c.comment = None;
        }
    }
}

/// Lift inline PK/unique flags into name-stripped, deduped table constraints,
/// keeping any FK/CHECK constraints already present untouched. (Reconcile's
/// `canonicalize` strips FK/CHECK afterward; the diff path keeps them.)
fn lift_pk_unique_keep_others(t: &mut snapshot::TableSnapshot) {
    let mut kept: Vec<TableConstraint> = Vec::new();
    let mut seen: HashSet<(char, String)> = HashSet::new();
    let mut push = |kept: &mut Vec<TableConstraint>, seen: &mut HashSet<(char, String)>, c: TableConstraint| {
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
```

Delete the old `lift_pk_unique_constraints` and `normalize_columns` (their logic now lives in the two new fns + `canonicalize`'s strip loop). Keep `canonical_type`, `canonical_default`, `strip_trailing_cast`, `is_plausible_type` unchanged.

- [ ] **Step 4: Run the whole reconcile suite to verify unchanged behavior + new test passes**

Run: `cargo test -p dbd-core reconcile && cargo test -p dbd-core normalize_common`
Expected: PASS — all existing `canonicalize_*` tests green (byte-identical behavior) plus `normalize_common_preserves_fk_check_index_comment`.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reconcile.rs
git commit -m "refactor(reconcile): extract normalize_common from canonicalize"
```

---

## Task 2: `SchemaDiff` type + `normalize_for_diff` skeleton (columns/PK/unique/enums + comments)

Create `schema_diff.rs` with the result type and the first slice of `normalize_for_diff` — reuse `normalize_common`, keep comments (do not strip). FK/CHECK/index normalization arrive in Tasks 3–5.

**Files:**
- Create: `crates/dbd-core/src/schema_diff.rs`
- Modify: `crates/dbd-core/src/lib.rs`
- Test: `crates/dbd-core/src/schema_diff.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Wire the module.** In `crates/dbd-core/src/lib.rs`, after `pub mod reconcile;` (line 18) add `pub mod schema_diff;` and after the reconcile re-export (line 31) add `pub use schema_diff::SchemaDiff;`.

- [ ] **Step 2: Write the failing test** — in `schema_diff.rs`:

```rust
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p dbd-core schema_diff`
Expected: FAIL to compile — `SchemaDiff` not found.

- [ ] **Step 4: Implement the type + normalize entry point** — top of `schema_diff.rs`:

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core schema_diff`
Expected: PASS — both tests. (`normalize_common` already normalizes `int4`→`integer`; comments are retained so the comment change surfaces.)

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/schema_diff.rs crates/dbd-core/src/lib.rs
git commit -m "feat(schema_diff): SchemaDiff type + normalize_for_diff skeleton"
```

---

## Task 3: FK normalization (lift inline FKs, normalize NO ACTION)

Introspection reports FKs as table constraints; the parser may carry them as `ColumnDef.inline_fk`. Lift inline FKs into table constraints and normalize action defaults so an unchanged FK reconciles to no diff.

**Files:**
- Modify: `crates/dbd-core/src/schema_diff.rs`
- Test: `crates/dbd-core/src/schema_diff.rs` tests

- [ ] **Step 1: Write the failing test:**

```rust
use crate::entity::{FkAction, ForeignKey, TableConstraint};

fn fk(name: &str, col: &str, reft: &str, refc: &str, on_delete: Option<FkAction>) -> ForeignKey {
    ForeignKey { name: Some(name.into()), columns: vec![col.into()], ref_schema: None,
        ref_table: reft.into(), ref_columns: vec![refc.into()], on_delete, on_update: None }
}

/// Desired carries the FK inline on the column; live carries it as a table
/// constraint (as introspection reports). After normalization they match.
#[test]
fn inline_fk_matches_table_constraint_fk() {
    let live = snap(TableSnapshot {
        table_constraints: vec![TableConstraint::ForeignKey(fk("users_org_fk", "org_id", "org", "id", Some(FkAction::NoAction)))],
        ..table(vec![col("org_id", "integer")])
    });
    let desired = snap(TableSnapshot {
        // NO ACTION is the default → introspection may omit it; must still match.
        table_constraints: vec![TableConstraint::ForeignKey(fk("users_org_fk", "org_id", "org", "id", None))],
        ..table(vec![col("org_id", "integer")])
    });
    let d = SchemaDiff::compute(live, desired);
    assert!(d.is_empty(), "unchanged FK must not diff, got {:?}", d.changes);
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-core schema_diff::tests::inline_fk`
Expected: FAIL — `inline_fk_matches_table_constraint_fk` fails: `NoAction` vs `None` differ (and inline FK not lifted).

- [ ] **Step 3: Implement FK normalization** in `normalize_for_diff`, after `normalize_common(snap)`:

```rust
    for t in &mut snap.tables {
        // Lift any inline column FK into a table constraint (introspection form).
        let inline: Vec<TableConstraint> = t.columns.iter_mut()
            .filter_map(|c| c.inline_fk.take().map(TableConstraint::ForeignKey))
            .collect();
        t.table_constraints.extend(inline);
        // Normalize FK action defaults: NO ACTION is Postgres's default and is
        // often omitted by one side. Treat None and NoAction as equal.
        for con in &mut t.table_constraints {
            if let TableConstraint::ForeignKey(fk) = con {
                normalize_fk_action(&mut fk.on_delete);
                normalize_fk_action(&mut fk.on_update);
            }
        }
    }
```

Add the helper (module level):

```rust
use crate::entity::{FkAction, TableConstraint};

/// `NO ACTION` is the Postgres default; collapse it to `None` so an explicit
/// and an omitted default compare equal.
fn normalize_fk_action(a: &mut Option<FkAction>) {
    if *a == Some(FkAction::NoAction) {
        *a = None;
    }
}
```

(Adjust the `use` at the top of the file to include `FkAction, TableConstraint` alongside the existing imports.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core schema_diff`
Expected: PASS — both new FK tests and all prior tests.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/schema_diff.rs
git commit -m "feat(schema_diff): normalize FKs (lift inline, collapse NO ACTION)"
```

---

## Task 4: Index normalization + backing-index suppression

Introspection reports the implicit index that backs a PK/UNIQUE constraint; the parsed design does not. Suppress those so they don't show as phantom index adds/drops. Keep genuine secondary indexes.

**Files:**
- Modify: `crates/dbd-core/src/schema_diff.rs`
- Test: `crates/dbd-core/src/schema_diff.rs` tests

- [ ] **Step 1: Write the failing test:**

```rust
use crate::entity::{IndexColumn, IndexDef};

fn idx(name: &str, cols: &[&str], unique: bool) -> IndexDef {
    IndexDef { name: Some(name.into()),
        columns: cols.iter().map(|c| IndexColumn { name: (*c).into(), order: None }).collect(),
        unique, index_type: None }
}

/// The implicit index backing a PK (live-only, from introspection) is not a
/// real drift and must be suppressed.
#[test]
fn pk_backing_index_is_suppressed() {
    let live = snap(TableSnapshot {
        table_constraints: vec![TableConstraint::PrimaryKey { name: Some("users_pkey".into()), columns: vec!["id".into()] }],
        indexes: vec![idx("users_pkey", &["id"], true)], // introspection reports the backing index
        ..table(vec![ColumnDef { nullable: false, is_pk: true, ..col("id", "integer") }])
    });
    let desired = snap(TableSnapshot {
        table_constraints: vec![],
        indexes: vec![],
        ..table(vec![ColumnDef { nullable: false, is_pk: true, ..col("id", "integer") }])
    });
    let d = SchemaDiff::compute(live, desired);
    assert!(d.is_empty(), "PK-backing index must not surface as a diff, got {:?}", d.changes);
}

/// A genuine secondary index add is still detected.
#[test]
fn secondary_index_add_is_detected() {
    let live = snap(table(vec![col("email", "text")]));
    let desired = snap(TableSnapshot { indexes: vec![idx("users_email_idx", &["email"], false)], ..table(vec![col("email", "text")]) });
    let d = SchemaDiff::compute(live, desired);
    assert!(!d.is_empty(), "new index must surface");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-core schema_diff::tests::pk_backing`
Expected: FAIL — the live-only `users_pkey` index surfaces as an index drop.

- [ ] **Step 3: Implement backing-index suppression** — inside the `for t in &mut snap.tables` loop in `normalize_for_diff` (after the FK block), add:

```rust
        // Drop indexes that merely back a PK/UNIQUE constraint — introspection
        // reports them, the parsed design does not. Match by covered columns.
        let constraint_cols: std::collections::HashSet<Vec<String>> = t.table_constraints.iter()
            .filter_map(|c| match c {
                TableConstraint::PrimaryKey { columns, .. } | TableConstraint::Unique { columns, .. } => Some(columns.clone()),
                _ => None,
            })
            .collect();
        t.indexes.retain(|i| {
            let cols: Vec<String> = i.columns.iter().map(|c| c.name.clone()).collect();
            !constraint_cols.contains(&cols)
        });
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core schema_diff`
Expected: PASS — backing index suppressed, secondary index still detected.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/schema_diff.rs
git commit -m "feat(schema_diff): suppress PK/UNIQUE-backing indexes"
```

---

## Task 5: CHECK expression normalization via `pg_query` (+ advisory fallback)

`pg_get_constraintdef` re-formats CHECK expressions (extra parens, casts). Canonicalize both sides by parsing `SELECT 1 WHERE (<expr>)` with `pg_query` and re-deparsing; on parse failure fall back to a paren/whitespace-normalized text form and record an advisory.

**Files:**
- Modify: `crates/dbd-core/src/schema_diff.rs`
- Test: `crates/dbd-core/src/schema_diff.rs` tests

- [ ] **Step 1: Write the failing test:**

```rust
fn check(name: &str, expr: &str) -> TableConstraint {
    TableConstraint::Check { name: Some(name.into()), expression: expr.into() }
}

/// The same CHECK written with different (but equivalent) parenthesization
/// canonicalizes to the same form → no diff.
#[test]
fn equivalent_check_exprs_do_not_diff() {
    let live = snap(TableSnapshot { table_constraints: vec![check("ck_total", "((total > 0))")], ..table(vec![col("total", "integer")]) });
    let desired = snap(TableSnapshot { table_constraints: vec![check("ck_total", "total > 0")], ..table(vec![col("total", "integer")]) });
    let d = SchemaDiff::compute(live, desired);
    assert!(d.is_empty(), "equivalent CHECK exprs must not diff, got {:?}", d.changes);
}

/// A genuinely different CHECK predicate is still detected.
#[test]
fn changed_check_expr_is_detected() {
    let live = snap(TableSnapshot { table_constraints: vec![check("ck_total", "total > 0")], ..table(vec![col("total", "integer")]) });
    let desired = snap(TableSnapshot { table_constraints: vec![check("ck_total", "total >= 0")], ..table(vec![col("total", "integer")]) });
    let d = SchemaDiff::compute(live, desired);
    assert!(!d.is_empty(), "changed CHECK predicate must surface");
}

/// An unparseable CHECK is surfaced with an advisory rather than hidden.
#[test]
fn unparseable_check_records_advisory() {
    let mut adv = Vec::new();
    let mut s = snap(TableSnapshot { table_constraints: vec![check("ck", "%%% not sql %%%")], ..table(vec![col("x", "integer")]) });
    normalize_for_diff(&mut s, &mut adv);
    assert!(!adv.is_empty(), "unparseable CHECK must record an advisory");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-core schema_diff::tests::equivalent_check`
Expected: FAIL — `((total > 0))` ≠ `total > 0` textually.

- [ ] **Step 3: Implement CHECK canonicalization.** In the `for t in &mut snap.tables` loop (after the index block), add:

```rust
        for con in &mut t.table_constraints {
            if let TableConstraint::Check { name, expression } = con {
                match canonicalize_check_expr(expression) {
                    Some(canon) => *expression = canon,
                    None => advisories.push(format!(
                        "CHECK {} on {}.{} couldn't be normalized — shown as changed; verify manually",
                        name.as_deref().unwrap_or("(unnamed)"), t.schema, t.name
                    )),
                }
            }
        }
```

Add the helper (module level):

```rust
/// Canonicalize a CHECK expression by parsing `SELECT 1 WHERE (<expr>)` with
/// libpg_query and re-deparsing, so equivalent spellings (extra parens, casts)
/// converge. Returns `None` if the expression can't be parsed (caller records
/// an advisory and leaves the raw text, so a real diff is never hidden).
fn canonicalize_check_expr(expr: &str) -> Option<String> {
    let wrapped = format!("SELECT 1 WHERE ({expr})");
    let parsed = pg_query::parse(&wrapped).ok()?;
    let deparsed = parsed.deparse().ok()?;
    Some(deparsed)
}
```

> Note: `pg_query::parse` returns a result whose `.deparse()` yields the canonical SQL string (same crate/API used in `crates/dbd-core/src/parser/extractors.rs:236`). If the v6 method name differs, the Step-2 compile failure will point to the right one; the parser module is the reference for the exact signature.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core schema_diff`
Expected: PASS — equivalent CHECKs collapse, changed CHECK detected, unparseable records advisory.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/schema_diff.rs
git commit -m "feat(schema_diff): canonicalize CHECK exprs via pg_query (+advisory)"
```

---

## Task 6: `Design::diff_live()` — read-only introspection wiring

Mirror `Design::reconcile` (`crates/dbd-core/src/design.rs:1140`) but read-only: introspect, build desired snapshot, restrict live to managed schemas, return a `SchemaDiff`.

**Files:**
- Modify: `crates/dbd-core/src/design.rs`
- Test: `crates/dbd-core/src/design.rs` tests (use the mock adapter, as reconcile tests do)

- [ ] **Step 1: Write the failing test** — add near the existing `deploy_dry_run_*` / reconcile tests in `design.rs` (they already build a `MockAdapter`; follow that pattern):

```rust
/// diff_live is read-only and reports drift: a mock live DB missing a desired
/// table yields a non-empty diff and applies/executes nothing.
#[tokio::test]
async fn diff_live_reports_drift_read_only() {
    let design = Design::from_config_with_dir(&fixture_config(), "dev", Some(&fixtures())).unwrap();
    let mock = crate::adapter::mock::MockAdapter::new(); // empty live DB
    let d = design.diff_live(&mock, None).await.unwrap();
    assert!(!d.is_empty(), "empty live DB vs a non-empty design must show drift");
    assert!(mock.applied_names().is_empty(), "diff_live must not apply anything");
}
```

> Use whatever fixture/mock constructors the surrounding reconcile/deploy tests use in `design.rs`; match their imports. If a helper like `fixture_config()`/`fixtures()` isn't in scope in this module, copy the setup the adjacent `deploy_dry_run_returns_ok_and_applies_nothing` test uses.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-core diff_live_reports_drift`
Expected: FAIL to compile — `no method named diff_live`.

- [ ] **Step 3: Implement `diff_live`** — add to the `impl Design` block near `reconcile` in `design.rs`:

```rust
    /// Read-only: introspect the live database and return the complete
    /// difference against the design. Never writes. Unlike `reconcile`, this is
    /// available even after the project is released.
    pub async fn diff_live(
        &self,
        adapter: &dyn DatabaseAdapter,
        scope: Option<&ResolvedScope>,
    ) -> Result<crate::SchemaDiff> {
        use crate::reconcile::{snapshot_from_entities, DEFAULT_SCHEMA};
        use std::collections::HashSet;

        if adapter.prefers_batch_apply() {
            return Err(DbdError::Config(
                "diff is not supported for this target (no live SQL schema to diff)".to_string(),
            ));
        }

        let working_set: Option<HashSet<String>> = match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                Some(self.working_set(s)?)
            }
            _ => None,
        };

        let desired_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| match (&working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect();

        let desired_owned: Vec<Entity> = desired_entities.iter().map(|e| (*e).clone()).collect();
        let desired = snapshot_from_entities(&desired_owned);

        let managed_schemas: HashSet<String> = desired_entities
            .iter()
            .map(|e| match e.entity_type {
                EntityType::Schema => e.name.clone(),
                _ => {
                    let s = e.schema.clone().unwrap_or_default();
                    if s.is_empty() { DEFAULT_SCHEMA.to_string() } else { s }
                }
            })
            .collect();

        let live_entities = adapter.introspect().await?;
        let live_full = snapshot_from_entities(&live_entities);
        let live = restrict_snapshot_to_schemas(live_full, &managed_schemas);

        Ok(crate::SchemaDiff::compute(live, desired))
    }
```

(`restrict_snapshot_to_schemas`, `ResolvedScope`, `DbdError`, `EntityType`, `Entity` are already in scope in `design.rs` — they're used by `reconcile`.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core diff_live_reports_drift`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(core): Design::diff_live — read-only live↔design SchemaDiff"
```

---

## Task 7: CLI command — `Commands::Diff`, dispatch, `cmd_diff`, human rendering

Add the subcommand, dispatch it, and render the human report through a pure `diff_report_lines()` seam (unit-testable without a DB), mirroring `reconcile_plan_lines`.

**Files:**
- Modify: `crates/dbd-cli/src/cli.rs` (Commands enum ~line 269)
- Create: `crates/dbd-cli/src/commands/diff.rs`
- Modify: `crates/dbd-cli/src/commands/mod.rs` (add `mod diff;` line 1-6 area; dispatch arm ~line 175)
- Test: `crates/dbd-cli/src/commands/diff.rs` tests

- [ ] **Step 1: Add the subcommand.** In `crates/dbd-cli/src/cli.rs`, add to `enum Commands` (place before `Reconcile`):

```rust
    /// Show the complete difference between the live database and the design
    /// (read-only). Covers columns, PK/unique, foreign keys, CHECK constraints,
    /// indexes, comments and enums — everything `reconcile --dry-run` omits.
    Diff {
        /// Emit the diff as JSON for tooling/CI
        #[arg(long)]
        json: bool,
        /// Exit 2 when differences exist, 0 when in sync (errors exit 1)
        #[arg(long)]
        exit_code: bool,
    },
```

Add a parse test in `cli.rs` tests:

```rust
    #[test]
    fn diff_flags_parse() {
        let cli = Cli::try_parse_from(["dbd", "diff"]).expect("diff parses");
        assert!(matches!(&cli.command, Commands::Diff { json: false, exit_code: false }));
        let cli = Cli::try_parse_from(["dbd", "diff", "--json", "--exit-code"]).expect("diff flags parse");
        assert!(matches!(&cli.command, Commands::Diff { json: true, exit_code: true }));
    }
```

- [ ] **Step 2: Write the failing rendering test** — create `crates/dbd-cli/src/commands/diff.rs`:

```rust
use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::Design;
use dbd_core::SchemaDiff;

use crate::output::{self, Verbosity};

use super::get_adapter;

#[cfg(test)]
mod tests {
    use super::*;
    use dbd_core::diff::{DiffAction, MigrationDiff};
    use dbd_core::entity::EntityType;

    fn add(name: &str) -> MigrationDiff {
        MigrationDiff { entity_name: name.into(), entity_type: EntityType::Table, action: DiffAction::Add }
    }

    /// In-sync diff renders the friendly "no differences" line.
    #[test]
    fn renders_in_sync() {
        let d = SchemaDiff::default();
        let out = diff_report_lines(&d).join("\n");
        assert!(out.contains("in sync"), "got: {out}");
    }

    /// A create shows a `+ create` line and advisories are surfaced.
    #[test]
    fn renders_changes_and_advisories() {
        let d = SchemaDiff { changes: vec![add("public.audit_log")], warnings: vec![],
            advisories: vec!["CHECK ck on public.orders couldn't be normalized".into()] };
        let out = diff_report_lines(&d).join("\n");
        assert!(out.contains("+ create public.audit_log"), "got: {out}");
        assert!(out.contains("advisory"), "advisory must render; got: {out}");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p dbd-cli diff`
Expected: FAIL to compile — `diff_report_lines` not found.

- [ ] **Step 4: Implement rendering + command.** Add above the test module in `diff.rs`:

```rust
/// Build the human-readable lines for a schema diff. Pure so it can be
/// unit-tested. Each changed entity gets one summary line and, for alters, the
/// generated SQL indented beneath; advisories and warnings follow.
pub(crate) fn diff_report_lines(d: &SchemaDiff) -> Vec<String> {
    use dbd_core::diff::{self, DiffAction};
    if d.is_empty() {
        return vec!["Live database is in sync with the design — no differences.".to_string()];
    }
    let mut lines = Vec::new();
    for c in &d.changes {
        match &c.action {
            DiffAction::Add => lines.push(format!("  + create {}", c.entity_name)),
            DiffAction::Drop => lines.push(format!("  - drop   {}", c.entity_name)),
            DiffAction::Change(_) => {
                lines.push(format!("  ~ alter  {}", c.entity_name));
                let sql = diff::generate_migration_sql(std::slice::from_ref(c));
                for l in sql.lines().filter(|l| !l.trim().is_empty()) {
                    lines.push(format!("      {l}"));
                }
            }
        }
    }
    for w in &d.warnings {
        lines.push(format!("  ⚠ {w}"));
    }
    for a in &d.advisories {
        lines.push(format!("  ⚠ advisory: {a}"));
    }
    lines
}

/// `dbd diff` — read-only full diff of the live database against the design.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_diff(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    json: bool,
    exit_code: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let adapter = get_adapter(config, database_url).await?;

    let diff = design.diff_live(&*adapter, Some(&resolved)).await.context("Diff failed")?;

    if json {
        let doc = serde_json::to_string_pretty(&diff).context("Failed to serialize diff")?;
        output::always(&doc);
    } else {
        for line in diff_report_lines(&diff) {
            output::always(&line);
        }
    }

    if let Some(code) = diff_exit_code(diff.is_empty(), exit_code) {
        std::process::exit(code);
    }
    let _ = verbosity;
    Ok(())
}
```

Add `serde_json` if not already a `dbd-cli` dependency: check `crates/dbd-cli/Cargo.toml`; the `diagram --json` path already serializes, so it is almost certainly present. If absent, `cargo add serde_json -p dbd-cli`.

- [ ] **Step 5: Wire dispatch.** In `crates/dbd-cli/src/commands/mod.rs`: add `mod diff;` to the module list (lines 1–6), and add a dispatch arm next to `Reconcile` (~line 175):

```rust
        Commands::Diff { json, exit_code } => {
            diff::cmd_diff(config, env, project_dir, database_url, *json, *exit_code, scope, deps, verbosity).await
        }
```

- [ ] **Step 6: Run to verify pass**

Run: `cargo test -p dbd-cli diff && cargo build -p dbd-cli`
Expected: PASS + clean build. (`diff_exit_code` is defined in Task 8; add a temporary `fn diff_exit_code(_: bool, _: bool) -> Option<i32> { None }` stub now, replaced with the real one + test in Task 8.)

- [ ] **Step 7: Commit**

```bash
git add crates/dbd-cli/src/cli.rs crates/dbd-cli/src/commands/diff.rs crates/dbd-cli/src/commands/mod.rs
git commit -m "feat(cli): dbd diff command — read-only full schema diff (human output)"
```

---

## Task 8: `--exit-code` semantics (terraform-style)

Replace the Task-7 stub with a tested pure function: `0` in sync, `2` drift, `None` (no forced exit) when the flag is off.

**Files:**
- Modify: `crates/dbd-cli/src/commands/diff.rs`
- Test: `crates/dbd-cli/src/commands/diff.rs` tests

- [ ] **Step 1: Write the failing test:**

```rust
    #[test]
    fn exit_code_semantics() {
        assert_eq!(diff_exit_code(true, false), None);   // flag off → no forced exit
        assert_eq!(diff_exit_code(false, false), None);  // flag off → no forced exit
        assert_eq!(diff_exit_code(true, true), Some(0));  // --exit-code, in sync
        assert_eq!(diff_exit_code(false, true), Some(2)); // --exit-code, drift
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-cli diff::tests::exit_code_semantics`
Expected: FAIL — the stub returns `None` for all inputs.

- [ ] **Step 3: Implement** — replace the stub with:

```rust
/// terraform-`plan` style exit codes for `--exit-code`: `Some(0)` in sync,
/// `Some(2)` when differences exist, `None` when the flag is off (caller keeps
/// the normal `0`). Errors are handled separately by the caller (exit 1).
pub(crate) fn diff_exit_code(is_empty: bool, exit_code_flag: bool) -> Option<i32> {
    if !exit_code_flag {
        return None;
    }
    Some(if is_empty { 0 } else { 2 })
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-cli diff`
Expected: PASS — all diff tests including exit-code.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli/src/commands/diff.rs
git commit -m "feat(cli): dbd diff --exit-code (0 in-sync / 2 drift)"
```

---

## Task 9: `--json` output shape test

Lock the JSON shape with an assertion so it stays stable for tooling.

**Files:**
- Modify: `crates/dbd-cli/src/commands/diff.rs`
- Test: `crates/dbd-cli/src/commands/diff.rs` tests

- [ ] **Step 1: Write the failing test:**

```rust
    #[test]
    fn json_shape_is_stable() {
        let d = SchemaDiff { changes: vec![add("public.audit_log")], warnings: vec![], advisories: vec![] };
        let json = serde_json::to_string(&d).unwrap();
        // Top-level keys tooling depends on.
        assert!(json.contains("\"changes\""), "got: {json}");
        assert!(json.contains("\"warnings\""), "got: {json}");
        assert!(json.contains("\"advisories\""), "got: {json}");
        assert!(json.contains("public.audit_log"), "got: {json}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-cli diff::tests::json_shape`
Expected: FAIL if `serde_json` is not yet a `dbd-cli` dev/dependency (add it) — otherwise PASS immediately, in which case tighten the assertion to also require the entity to serialize (it already does). If it passes on first run, add a `DiffAction` variant check: `assert!(json.contains("Add"))`.

- [ ] **Step 3: Ensure `serde_json` is available** — `grep serde_json crates/dbd-cli/Cargo.toml`; if missing, `cargo add serde_json -p dbd-cli`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-cli diff`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli/src/commands/diff.rs crates/dbd-cli/Cargo.toml
git commit -m "test(cli): lock dbd diff --json output shape"
```

---

## Task 10: Read-only integration test (embedded/sqlite)

Mirror `reconcile_dry_run_is_read_only` (`crates/dbd-core/tests/embedded_test.rs:1289`): a drifted DB yields the expected diff, an in-sync DB yields empty, and `diff_live` never writes.

**Files:**
- Modify: `crates/dbd-core/tests/embedded_test.rs` (follow its existing harness/imports)
- Test: same file

- [ ] **Step 1: Write the failing test** — model it on the existing `reconcile_dry_run_is_read_only` test in the same file (reuse its DB setup helpers and design fixture):

```rust
/// `diff_live` is read-only and reports drift. Build a table in the design that
/// the live DB lacks; diff must report it and must not create it.
#[tokio::test]
async fn diff_live_reports_drift_and_writes_nothing() {
    // (reuse the same harness the reconcile_dry_run_is_read_only test uses:
    //  a live adapter/connection + a Design loaded from the embedded fixture)
    let (design, adapter) = setup_embedded_design_and_db().await; // ← use the file's actual helper

    let before = adapter.introspect().await.unwrap();
    let diff = design.diff_live(&adapter, None).await.expect("diff_live");
    let after = adapter.introspect().await.unwrap();

    assert!(!diff.is_empty(), "a design table absent from the live DB must show drift");
    assert_eq!(before.len(), after.len(), "diff_live must not create or drop anything");
}
```

> If `embedded_test.rs` has no reusable setup helper, copy the exact setup block from `reconcile_dry_run_is_read_only` (same file) — that test already introspects and asserts read-only, so its scaffolding is the template.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p dbd-core diff_live_reports_drift_and_writes_nothing`
Expected: FAIL first for a compile/setup reason, then (once setup matches the file's harness) PASS. If the embedded harness requires a feature flag, run it the same way the reconcile embedded test is run (check the top of `embedded_test.rs` for any `#![cfg(...)]` or required env).

- [ ] **Step 3: Adjust setup to match the file's harness** until the test compiles and the assertions hold.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core diff_live_reports_drift_and_writes_nothing`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/tests/embedded_test.rs
git commit -m "test(core): diff_live is read-only and reports drift (embedded)"
```

---

## Task 11: Docs

Document the command in the guide and llms references.

**Files:**
- Modify: `docs/guide/04-commands.md` (add a `## dbd diff` section, e.g. after `## dbd reconcile`)
- Modify: `docs/llms/llms-full.txt` (add a `### dbd diff` section near `### dbd reconcile`)
- Modify: `docs/llms/llms.txt` (one bullet near the `dbd reconcile` bullet, line ~75)

- [ ] **Step 1: Add the guide section** to `docs/guide/04-commands.md`:

````markdown
---

## `dbd diff`

Read-only. Show the complete difference between the live database and the design —
**everything `reconcile --dry-run` omits**: foreign keys, CHECK constraints, indexes,
and column comments, in addition to columns, PK/unique, and enums. Never writes, and
available even after `dbd release`.

```sh
dbd diff -d $DATABASE_URL                 # Human report (summary + SQL)
dbd diff -d $DATABASE_URL --json          # Structured diff for tooling/CI
dbd diff -d $DATABASE_URL --exit-code     # Exit 0 in sync, 2 on drift, 1 on error
dbd diff --scope hub -d $HUB_URL          # Restrict to a scope's working set
```

The desired schema is parsed from your DDL; the live schema is introspected. dbd normalizes
representation noise (type aliases, default casts, enum qualification, PK/UNIQUE-backing
indexes, FK `NO ACTION` defaults, and CHECK-expression parenthesization via the Postgres
parser) so only real drift is reported. A CHECK expression that can't be parsed is still
shown, flagged `advisory` so you verify it by hand. Diff covers **tables and enums**;
views/functions/roles/sequences are not structurally diffed.
````

- [ ] **Step 2: Add the llms-full section** to `docs/llms/llms-full.txt` (mirror the guide, terser).

- [ ] **Step 3: Add the terse bullet** to `docs/llms/llms.txt` near line 75:

```
- `dbd diff` — read-only: show the complete live-DB↔design difference (columns, PK/unique, FK, CHECK, indexes, comments, enums) — everything `reconcile --dry-run` omits. `--json` for tooling, `--exit-code` (0 in sync / 2 drift / 1 error). Tables+enums only; always available (even after release)
```

- [ ] **Step 4: Commit**

```bash
git add docs/guide/04-commands.md docs/llms/llms-full.txt docs/llms/llms.txt
git commit -m "docs: document dbd diff"
```

---

## Task 12: Verify, release, merge

- [ ] **Step 1: Full green gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all tests pass, zero clippy warnings.

- [ ] **Step 2: Manual smoke** (if a dev DB is available)

Run: `cargo run -p dbd-cli -- diff -d $DATABASE_URL` and `... diff --json` and `... diff --exit-code; echo $?`
Expected: human report, valid JSON, exit `0`/`2` as appropriate.

- [ ] **Step 3: Push develop, cut patch, merge to main** (repo release flow)

```bash
git push origin develop
make bump patch          # bumps to next patch, commits, tags, pushes develop + tag, cargo clean
git fetch origin main && git checkout main && git merge --no-ff develop \
  -m "Merge branch 'develop' into main: dbd diff — read-only full schema diff (vX.Y.Z)" \
  && git push origin main && git checkout develop
```

(Replace `vX.Y.Z` with the version `make bump` prints.)

---

## Self-review

**Spec coverage:**
- Read-only live↔design diff → Tasks 6, 10. ✅
- Covers FK/CHECK/index/comment beyond reconcile → Tasks 3 (FK), 4 (index), 5 (CHECK), 2 (comment). ✅
- Normalization split (`normalize_common` + reconcile behavior unchanged) → Task 1. ✅
- `--json` → Tasks 7, 9. ✅
- `--exit-code` (0/2/1) → Task 8. ✅
- Advisories for unparseable CHECK → Task 5. ✅
- Available post-release (not gated on `project.released`) → Task 6 (no released check in `diff_live`). ✅
- Scope/deps honored → Task 6 (working_set + check_scope_gaps) + Task 7 (resolve_scope). ✅
- Tables+enums only; views/etc out → reflected in Task 11 docs + inherent to `snapshot_from_entities`. ✅
- Docs (guide + llms-full + llms) → Task 11. ✅
- Release flow → Task 12. ✅

**Placeholder scan:** Two tasks (6, 10) intentionally defer to "the harness the adjacent reconcile test uses" rather than reproducing an unknown fixture verbatim — the referenced tests (`design.rs` reconcile/deploy tests, `embedded_test.rs:1289`) are named exactly so the implementer copies real, existing scaffolding. All other code steps carry complete code.

**Type consistency:** `SchemaDiff { changes, warnings, advisories }` + `is_empty()` + `compute()` used consistently across Tasks 2, 6, 7, 8, 9. `normalize_for_diff(&mut Snapshot, &mut Vec<String>)` signature consistent Tasks 2/3/4/5. `diff_report_lines(&SchemaDiff)` and `diff_exit_code(bool,bool)->Option<i32>` consistent Tasks 7/8. `diff_live(&dyn DatabaseAdapter, Option<&ResolvedScope>)` consistent Tasks 6/7.
