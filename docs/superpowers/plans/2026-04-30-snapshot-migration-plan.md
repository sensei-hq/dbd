# Snapshot, Schema Diff & Migration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add versioned schema snapshots with incremental migration generation and version-aware apply to dbd-rs.

**Architecture:** Pure logic functions (`diff`, `prepare_snapshot`, `build_execution_plan`) take version/snapshot/entity inputs and return data structures. Thin I/O wrappers handle DB/filesystem reads. All core logic is unit-testable without mocks.

**Tech Stack:** Rust, serde/serde_json, chrono, sha2, sqlx (existing), clap (existing)

**Spec:** `docs/superpowers/specs/2026-04-30-snapshot-migration-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/dbd-core/src/diff.rs` | Create | Diff types, diff logic, SQL generation |
| `crates/dbd-core/src/snapshot.rs` | Modify | Add EnumSnapshot, prepare_snapshot(), create_snapshot(), update types |
| `crates/dbd-core/src/entity.rs` | Modify | Add PartialEq to ColumnDef, TableConstraint, IndexDef, ForeignKey |
| `crates/dbd-core/src/config.rs` | Modify | Add version to ProjectConfig, add update_version() |
| `crates/dbd-core/src/design.rs` | Modify | Add build_execution_plan(), update apply() |
| `crates/dbd-core/src/lib.rs` | Modify | Export diff module |
| `crates/dbd-cli/src/commands.rs` | Modify | Wire snapshot create, migrate status/apply |

---

### Task 1: Add PartialEq to entity types

**Files:**
- Modify: `crates/dbd-core/src/entity.rs`

The diff engine needs to compare columns, constraints, and indexes by value. Currently only `EntityType`, `FkAction`, `IndexType`, and `SortOrder` derive `PartialEq`. We need it on the structural types too.

- [ ] **Step 1: Add PartialEq derive to entity types**

In `crates/dbd-core/src/entity.rs`, add `PartialEq` to these derives:

```rust
// Line 66 — ForeignKey
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ForeignKey {

// Line 88 — TableConstraint
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TableConstraint {

// Line 107 — ColumnDef
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {

// Line 121 — IndexDef
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDef {

// Line 129 — IndexColumn
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexColumn {

// Line 150 — TableComments
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableComments {

// Line 157 — TableDef
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDef {
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dbd-core`
Expected: compiles with no errors

- [ ] **Step 3: Run existing tests to ensure no regressions**

Run: `cargo test`
Expected: all 195 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/entity.rs
git commit -m "feat: add PartialEq to entity struct types for diff comparisons"
```

---

### Task 2: Diff engine types

**Files:**
- Create: `crates/dbd-core/src/diff.rs`
- Modify: `crates/dbd-core/src/lib.rs`

- [ ] **Step 1: Create diff.rs with all types**

Create `crates/dbd-core/src/diff.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::entity::{ColumnDef, EntityType, IndexDef, TableConstraint};

// ── Diff types ─────────────────────────────────────────

/// Top-level diff for a single entity (table or enum).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationDiff {
    pub entity_name: String,
    pub entity_type: EntityType,
    pub action: DiffAction,
}

/// What happened to the entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiffAction {
    /// New entity — no migration SQL needed, regular apply creates it.
    Add,
    /// Entity removed — generate DROP.
    Drop,
    /// Entity modified — generate ALTERs.
    Change(Vec<FieldChange>),
}

/// A single field-level change within an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldChange {
    pub field_name: String,
    pub field_type: FieldType,
    pub action: ChangeAction,
}

/// What kind of field changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Column,
    Constraint,
    Index,
    EnumValue,
}

/// What happened to the field.
/// Note: Constraints and Indexes only use Add/Drop (changed = Drop old, apply creates new).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeAction {
    Add(FieldDetail),
    Drop,
    /// Column and EnumValue only.
    Alter { old: FieldDetail, new: FieldDetail },
}

/// The detail payload for a field change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldDetail {
    Column(ColumnDef),
    Constraint(TableConstraint),
    Index(IndexDef),
    EnumValue(String),
}
```

- [ ] **Step 2: Export diff module from lib.rs**

In `crates/dbd-core/src/lib.rs`, add after `pub mod dependency;`:

```rust
pub mod diff;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p dbd-core`
Expected: compiles (types only, no logic yet)

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/diff.rs crates/dbd-core/src/lib.rs
git commit -m "feat: add diff engine types (MigrationDiff, FieldChange, ChangeAction)"
```

---

### Task 3: Diff engine — table diff tests (D1-D8)

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

Write failing tests for core table diff scenarios before implementation.

- [ ] **Step 1: Write test helpers and tests D1-D8**

Append to `crates/dbd-core/src/diff.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{ColumnDef, ForeignKey, IndexColumn, IndexDef, TableComments, TableConstraint};
    use crate::snapshot::{EnumSnapshot, Snapshot, TableSnapshot};

    // ── Helpers ────────────────────────────────────────

    fn col(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            is_identity: false,
            comment: None,
            inline_fk: None,
        }
    }

    fn col_not_null(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            nullable: false,
            ..col(name, data_type)
        }
    }

    fn col_with_default(name: &str, data_type: &str, default: &str) -> ColumnDef {
        ColumnDef {
            default_value: Some(default.to_string()),
            ..col(name, data_type)
        }
    }

    fn table(name: &str, schema: &str, columns: Vec<ColumnDef>) -> TableSnapshot {
        TableSnapshot {
            name: name.to_string(),
            schema: schema.to_string(),
            columns,
            indexes: vec![],
            table_constraints: vec![],
        }
    }

    fn snap(version: u32, tables: Vec<TableSnapshot>, enums: Vec<EnumSnapshot>) -> Snapshot {
        Snapshot {
            version,
            description: format!("v{version}"),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables,
            enums,
        }
    }

    // ── D1: Identical snapshots ────────────────────────

    #[test]
    fn d1_identical_snapshots_empty_diff() {
        let users = table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT")]);
        let a = snap(1, vec![users.clone()], vec![]);
        let b = snap(2, vec![users], vec![]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical snapshots should produce no diffs");
    }

    // ── D2: New table detected ─────────────────────────

    #[test]
    fn d2_new_table_detected() {
        let users = table("config.users", "config", vec![col("id", "BIGINT")]);
        let orders = table("config.orders", "config", vec![col("id", "BIGINT")]);
        let a = snap(1, vec![users.clone()], vec![]);
        let b = snap(2, vec![users, orders], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "config.orders");
        assert!(matches!(diffs[0].action, DiffAction::Add));
    }

    // ── D3: Dropped table detected ─────────────────────

    #[test]
    fn d3_dropped_table_detected() {
        let users = table("config.users", "config", vec![col("id", "BIGINT")]);
        let orders = table("config.orders", "config", vec![col("id", "BIGINT")]);
        let a = snap(1, vec![users.clone(), orders], vec![]);
        let b = snap(2, vec![users], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "config.orders");
        assert!(matches!(diffs[0].action, DiffAction::Drop));
    }

    // ── D4: Column added ───────────────────────────────

    #[test]
    fn d4_column_added() {
        let a = snap(1, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT")])], vec![]);
        let b = snap(2, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT"), col("email", "TEXT")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action {
            DiffAction::Change(c) => c,
            _ => panic!("expected Change"),
        };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "email");
        assert_eq!(changes[0].field_type, FieldType::Column);
        assert!(matches!(changes[0].action, ChangeAction::Add(FieldDetail::Column(_))));
    }

    // ── D5: Column dropped ─────────────────────────────

    #[test]
    fn d5_column_dropped() {
        let a = snap(1, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT"), col("email", "TEXT")])], vec![]);
        let b = snap(2, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action {
            DiffAction::Change(c) => c,
            _ => panic!("expected Change"),
        };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "email");
        assert!(matches!(changes[0].action, ChangeAction::Drop));
    }

    // ── D6: Column type changed ────────────────────────

    #[test]
    fn d6_column_type_changed() {
        let a = snap(1, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("email", "VARCHAR(100)")])], vec![]);
        let b = snap(2, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action {
            DiffAction::Change(c) => c,
            _ => panic!("expected Change"),
        };
        assert_eq!(changes[0].field_name, "email");
        match &changes[0].action {
            ChangeAction::Alter { old, new } => {
                let old_col = match old { FieldDetail::Column(c) => c, _ => panic!("expected Column") };
                let new_col = match new { FieldDetail::Column(c) => c, _ => panic!("expected Column") };
                assert_eq!(old_col.data_type, "VARCHAR(100)");
                assert_eq!(new_col.data_type, "TEXT");
            }
            _ => panic!("expected Alter"),
        }
    }

    // ── D7: Column nullable changed ────────────────────

    #[test]
    fn d7_column_nullable_changed() {
        let a = snap(1, vec![table("config.users", "config", vec![col_not_null("id", "BIGINT"), col_not_null("email", "TEXT")])], vec![]);
        let b = snap(2, vec![table("config.users", "config", vec![col_not_null("id", "BIGINT"), col("email", "TEXT")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action {
            DiffAction::Change(c) => c,
            _ => panic!("expected Change"),
        };
        assert_eq!(changes[0].field_name, "email");
        match &changes[0].action {
            ChangeAction::Alter { old, new } => {
                let old_col = match old { FieldDetail::Column(c) => c, _ => panic!("expected Column") };
                let new_col = match new { FieldDetail::Column(c) => c, _ => panic!("expected Column") };
                assert!(!old_col.nullable);
                assert!(new_col.nullable);
            }
            _ => panic!("expected Alter"),
        }
    }

    // ── D8: Column default changed ─────────────────────

    #[test]
    fn d8_column_default_changed() {
        let a = snap(1, vec![table("config.users", "config", vec![col("id", "BIGINT"), col("status", "TEXT")])], vec![]);
        let b = snap(2, vec![table("config.users", "config", vec![col("id", "BIGINT"), col_with_default("status", "TEXT", "'active'")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action {
            DiffAction::Change(c) => c,
            _ => panic!("expected Change"),
        };
        assert_eq!(changes[0].field_name, "status");
        match &changes[0].action {
            ChangeAction::Alter { old, new } => {
                let old_col = match old { FieldDetail::Column(c) => c, _ => panic!("expected Column") };
                let new_col = match new { FieldDetail::Column(c) => c, _ => panic!("expected Column") };
                assert_eq!(old_col.default_value, None);
                assert_eq!(new_col.default_value, Some("'active'".to_string()));
            }
            _ => panic!("expected Alter"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core diff::tests`
Expected: FAIL — `diff` function not found

- [ ] **Step 3: Commit failing tests**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "test: add failing diff tests D1-D8 (table add/drop/column changes)"
```

---

### Task 4: Diff engine — table diff implementation

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

- [ ] **Step 1: Implement diff() function for tables**

Add above the `#[cfg(test)]` block in `crates/dbd-core/src/diff.rs`:

```rust
use crate::snapshot::{EnumSnapshot, Snapshot, TableSnapshot};
use std::collections::HashMap;

// ── Diff logic ─────────────────────────────────────────

/// Compare two snapshots and produce a list of entity-level diffs.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Vec<MigrationDiff> {
    let mut diffs = Vec::new();

    // Table diffs
    diffs.extend(diff_tables(&old.tables, &new.tables));

    // Enum diffs
    diffs.extend(diff_enums(&old.enums, &new.enums));

    diffs
}

fn diff_tables(old: &[TableSnapshot], new: &[TableSnapshot]) -> Vec<MigrationDiff> {
    let old_map: HashMap<&str, &TableSnapshot> = old.iter().map(|t| (t.name.as_str(), t)).collect();
    let new_map: HashMap<&str, &TableSnapshot> = new.iter().map(|t| (t.name.as_str(), t)).collect();
    let mut diffs = Vec::new();

    // Dropped tables (in old, not in new)
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.to_string(),
                entity_type: EntityType::Table,
                action: DiffAction::Drop,
            });
        }
    }

    // Added or changed tables
    for (name, new_table) in &new_map {
        match old_map.get(name) {
            None => {
                diffs.push(MigrationDiff {
                    entity_name: name.to_string(),
                    entity_type: EntityType::Table,
                    action: DiffAction::Add,
                });
            }
            Some(old_table) => {
                let changes = diff_table_fields(old_table, new_table);
                if !changes.is_empty() {
                    diffs.push(MigrationDiff {
                        entity_name: name.to_string(),
                        entity_type: EntityType::Table,
                        action: DiffAction::Change(changes),
                    });
                }
            }
        }
    }

    diffs
}

fn diff_table_fields(old: &TableSnapshot, new: &TableSnapshot) -> Vec<FieldChange> {
    let mut changes = Vec::new();

    // Column diffs
    changes.extend(diff_columns(&old.columns, &new.columns));

    // Constraint diffs
    changes.extend(diff_constraints(&old.table_constraints, &new.table_constraints));

    // Index diffs
    changes.extend(diff_indexes(&old.indexes, &new.indexes));

    changes
}

fn diff_columns(old: &[ColumnDef], new: &[ColumnDef]) -> Vec<FieldChange> {
    let old_map: HashMap<&str, &ColumnDef> = old.iter().map(|c| (c.name.as_str(), c)).collect();
    let new_map: HashMap<&str, &ColumnDef> = new.iter().map(|c| (c.name.as_str(), c)).collect();
    let mut changes = Vec::new();

    // Dropped columns
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            changes.push(FieldChange {
                field_name: name.to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Drop,
            });
        }
    }

    // Added or altered columns
    for (name, new_col) in &new_map {
        match old_map.get(name) {
            None => {
                changes.push(FieldChange {
                    field_name: name.to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(FieldDetail::Column((*new_col).clone())),
                });
            }
            Some(old_col) => {
                if *old_col != *new_col {
                    changes.push(FieldChange {
                        field_name: name.to_string(),
                        field_type: FieldType::Column,
                        action: ChangeAction::Alter {
                            old: FieldDetail::Column((*old_col).clone()),
                            new: FieldDetail::Column((*new_col).clone()),
                        },
                    });
                }
            }
        }
    }

    changes
}

/// Get a stable identifier for a constraint (name or type+columns fallback).
fn constraint_key(c: &TableConstraint) -> String {
    match c {
        TableConstraint::PrimaryKey { name, columns } => {
            name.clone().unwrap_or_else(|| format!("pk:{}", columns.join(",")))
        }
        TableConstraint::Unique { name, columns } => {
            name.clone().unwrap_or_else(|| format!("uq:{}", columns.join(",")))
        }
        TableConstraint::ForeignKey(fk) => {
            fk.name.clone().unwrap_or_else(|| format!("fk:{}", fk.columns.join(",")))
        }
        TableConstraint::Check { name, expression } => {
            name.clone().unwrap_or_else(|| format!("ck:{expression}"))
        }
    }
}

fn diff_constraints(old: &[TableConstraint], new: &[TableConstraint]) -> Vec<FieldChange> {
    let old_map: HashMap<String, &TableConstraint> = old.iter().map(|c| (constraint_key(c), c)).collect();
    let new_map: HashMap<String, &TableConstraint> = new.iter().map(|c| (constraint_key(c), c)).collect();
    let mut changes = Vec::new();

    // Dropped constraints
    for (key, _) in &old_map {
        if !new_map.contains_key(key) {
            changes.push(FieldChange {
                field_name: key.clone(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop,
            });
        }
    }

    // Added constraints (or changed — changed = drop, since apply recreates)
    for (key, new_con) in &new_map {
        match old_map.get(key) {
            None => {
                changes.push(FieldChange {
                    field_name: key.clone(),
                    field_type: FieldType::Constraint,
                    action: ChangeAction::Add(FieldDetail::Constraint((*new_con).clone())),
                });
            }
            Some(old_con) => {
                // Changed constraint = drop old (apply recreates the new one)
                if *old_con != *new_con {
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Constraint,
                        action: ChangeAction::Drop,
                    });
                }
            }
        }
    }

    changes
}

/// Get a stable identifier for an index.
fn index_key(idx: &IndexDef) -> String {
    idx.name.clone().unwrap_or_else(|| {
        let cols: Vec<&str> = idx.columns.iter().map(|c| c.name.as_str()).collect();
        format!("idx:{}", cols.join(","))
    })
}

fn diff_indexes(old: &[IndexDef], new: &[IndexDef]) -> Vec<FieldChange> {
    let old_map: HashMap<String, &IndexDef> = old.iter().map(|i| (index_key(i), i)).collect();
    let new_map: HashMap<String, &IndexDef> = new.iter().map(|i| (index_key(i), i)).collect();
    let mut changes = Vec::new();

    // Dropped indexes
    for (key, _) in &old_map {
        if !new_map.contains_key(key) {
            changes.push(FieldChange {
                field_name: key.clone(),
                field_type: FieldType::Index,
                action: ChangeAction::Drop,
            });
        }
    }

    // Added or changed indexes
    for (key, new_idx) in &new_map {
        match old_map.get(key) {
            None => {
                changes.push(FieldChange {
                    field_name: key.clone(),
                    field_type: FieldType::Index,
                    action: ChangeAction::Add(FieldDetail::Index((*new_idx).clone())),
                });
            }
            Some(old_idx) => {
                if *old_idx != *new_idx {
                    changes.push(FieldChange {
                        field_name: key.clone(),
                        field_type: FieldType::Index,
                        action: ChangeAction::Drop,
                    });
                }
            }
        }
    }

    changes
}

fn diff_enums(_old: &[EnumSnapshot], _new: &[EnumSnapshot]) -> Vec<MigrationDiff> {
    // Stub — implemented in Task 5
    Vec::new()
}
```

- [ ] **Step 2: Run tests D1-D8**

Run: `cargo test -p dbd-core diff::tests`
Expected: all 8 tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "feat: implement table diff logic (columns, constraints, indexes)"
```

---

### Task 5: Diff engine — constraint, index, enum diff tests (D9-D21)

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

- [ ] **Step 1: Write tests D9-D21**

Append to the `mod tests` block in `crates/dbd-core/src/diff.rs`:

```rust
    // ── D9: Constraint added ───────────────────────────

    #[test]
    fn d9_constraint_added() {
        let a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        let mut b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        b_table.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "uq_email");
        assert_eq!(changes[0].field_type, FieldType::Constraint);
        assert!(matches!(changes[0].action, ChangeAction::Add(_)));
    }

    // ── D10: Constraint dropped ────────────────────────

    #[test]
    fn d10_constraint_dropped() {
        let mut a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        a_table.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });
        let b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "uq_email");
        assert!(matches!(changes[0].action, ChangeAction::Drop));
    }

    // ── D11: Index added ───────────────────────────────

    #[test]
    fn d11_index_added() {
        let a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        let mut b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        b_table.indexes.push(IndexDef {
            name: Some("idx_email".to_string()),
            columns: vec![IndexColumn { name: "email".to_string(), order: None }],
            unique: false,
            index_type: None,
        });
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "idx_email");
        assert!(matches!(changes[0].action, ChangeAction::Add(_)));
    }

    // ── D12: Index dropped ─────────────────────────────

    #[test]
    fn d12_index_dropped() {
        let mut a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        a_table.indexes.push(IndexDef {
            name: Some("idx_email".to_string()),
            columns: vec![IndexColumn { name: "email".to_string(), order: None }],
            unique: false,
            index_type: None,
        });
        let b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "idx_email");
        assert!(matches!(changes[0].action, ChangeAction::Drop));
    }

    // ── D13: Constraint changed (drop old) ─────────────

    #[test]
    fn d13_constraint_changed_drops_old() {
        let mut a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT"), col("name", "TEXT")]);
        a_table.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });
        let mut b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT"), col("name", "TEXT")]);
        b_table.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string(), "name".to_string()],
        });
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "uq_email");
        assert!(matches!(changes[0].action, ChangeAction::Drop));
    }

    // ── D14: Index changed (drop old) ──────────────────

    #[test]
    fn d14_index_changed_drops_old() {
        let mut a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        a_table.indexes.push(IndexDef {
            name: Some("idx_email".to_string()),
            columns: vec![IndexColumn { name: "email".to_string(), order: None }],
            unique: false,
            index_type: Some(crate::entity::IndexType::Btree),
        });
        let mut b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]);
        b_table.indexes.push(IndexDef {
            name: Some("idx_email".to_string()),
            columns: vec![IndexColumn { name: "email".to_string(), order: None }],
            unique: false,
            index_type: Some(crate::entity::IndexType::Hash),
        });
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "idx_email");
        assert!(matches!(changes[0].action, ChangeAction::Drop));
    }

    // ── D15: Enum value added ──────────────────────────

    #[test]
    fn d15_enum_value_added() {
        let old_enum = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string(), "female".to_string()] };
        let new_enum = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string(), "female".to_string(), "other".to_string()] };
        let a = snap(1, vec![], vec![old_enum]);
        let b = snap(2, vec![], vec![new_enum]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "other");
        assert_eq!(changes[0].field_type, FieldType::EnumValue);
        assert!(matches!(changes[0].action, ChangeAction::Add(FieldDetail::EnumValue(_))));
    }

    // ── D16: Enum value dropped ────────────────────────

    #[test]
    fn d16_enum_value_dropped() {
        let old_enum = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string(), "female".to_string(), "other".to_string()] };
        let new_enum = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string(), "female".to_string()] };
        let a = snap(1, vec![], vec![old_enum]);
        let b = snap(2, vec![], vec![new_enum]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_name, "other");
        assert!(matches!(changes[0].action, ChangeAction::Drop));
    }

    // ── D17: New enum detected ─────────────────────────

    #[test]
    fn d17_new_enum_detected() {
        let new_enum = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string()] };
        let a = snap(1, vec![], vec![]);
        let b = snap(2, vec![], vec![new_enum]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.gender_type");
        assert!(matches!(diffs[0].action, DiffAction::Add));
    }

    // ── D18: Enum dropped ──────────────────────────────

    #[test]
    fn d18_enum_dropped() {
        let old_enum = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string()] };
        let a = snap(1, vec![], vec![old_enum]);
        let b = snap(2, vec![], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.gender_type");
        assert!(matches!(diffs[0].action, DiffAction::Drop));
    }

    // ── D19: Multiple changes on same table ────────────

    #[test]
    fn d19_multiple_changes_same_table() {
        let mut a_table = table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT"), col("old_col", "TEXT")]);
        a_table.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_name".to_string()),
            columns: vec!["name".to_string()],
        });
        let mut b_table = table("config.users", "config", vec![col("id", "BIGINT"), col("name", "TEXT"), col("email", "TEXT")]);
        b_table.indexes.push(IndexDef {
            name: Some("idx_email".to_string()),
            columns: vec![IndexColumn { name: "email".to_string(), order: None }],
            unique: false,
            index_type: None,
        });
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        let changes = match &diffs[0].action { DiffAction::Change(c) => c, _ => panic!("expected Change") };
        // old_col dropped, email added, uq_name dropped, idx_email added = 4 changes
        assert_eq!(changes.len(), 4);
    }

    // ── D20: Multiple tables changed ───────────────────

    #[test]
    fn d20_multiple_tables_changed() {
        let a = snap(1, vec![
            table("config.users", "config", vec![col("id", "BIGINT")]),
            table("config.orders", "config", vec![col("id", "BIGINT")]),
        ], vec![]);
        let b = snap(2, vec![
            table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]),
            table("config.orders", "config", vec![col("id", "BIGINT"), col("total", "NUMERIC")]),
        ], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 2);
    }

    // ── D21: Mixed add/alter/drop across entities ──────

    #[test]
    fn d21_mixed_add_alter_drop() {
        let a = snap(1, vec![
            table("config.users", "config", vec![col("id", "BIGINT")]),
            table("config.orders", "config", vec![col("id", "BIGINT")]),
            table("staging.temp", "staging", vec![col("id", "BIGINT")]),
        ], vec![]);
        let b = snap(2, vec![
            table("config.users", "config", vec![col("id", "BIGINT"), col("email", "TEXT")]),
            table("config.orders", "config", vec![col("id", "BIGINT")]),
            table("config.payments", "config", vec![col("id", "BIGINT")]),
        ], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 3);
        let names: Vec<&str> = diffs.iter().map(|d| d.entity_name.as_str()).collect();
        assert!(names.contains(&"staging.temp"));
        assert!(names.contains(&"config.users"));
        assert!(names.contains(&"config.payments"));
    }

    // ── E2: Unnamed constraint matching ────────────────

    #[test]
    fn e2_unnamed_constraint_matching() {
        let mut a_table = table("config.users", "config", vec![col("id", "BIGINT")]);
        a_table.table_constraints.push(TableConstraint::PrimaryKey { name: None, columns: vec!["id".to_string()] });
        let mut b_table = table("config.users", "config", vec![col("id", "BIGINT")]);
        b_table.table_constraints.push(TableConstraint::PrimaryKey { name: None, columns: vec!["id".to_string()] });
        let a = snap(1, vec![a_table], vec![]);
        let b = snap(2, vec![b_table], vec![]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical unnamed constraints should not produce diffs");
    }

    // ── E4: Enum with no changes ───────────────────────

    #[test]
    fn e4_enum_no_changes() {
        let e = EnumSnapshot { name: "public.gender_type".to_string(), schema: "public".to_string(), values: vec!["male".to_string(), "female".to_string()] };
        let a = snap(1, vec![], vec![e.clone()]);
        let b = snap(2, vec![], vec![e]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify D15-D18 fail (enum diff not implemented)**

Run: `cargo test -p dbd-core diff::tests`
Expected: D9-D14, D19-D21, E2 pass; D15-D18, E4 fail (enum stub returns empty)

- [ ] **Step 3: Implement enum diff**

Replace the `diff_enums` stub in `crates/dbd-core/src/diff.rs`:

```rust
fn diff_enums(old: &[EnumSnapshot], new: &[EnumSnapshot]) -> Vec<MigrationDiff> {
    let old_map: HashMap<&str, &EnumSnapshot> = old.iter().map(|e| (e.name.as_str(), e)).collect();
    let new_map: HashMap<&str, &EnumSnapshot> = new.iter().map(|e| (e.name.as_str(), e)).collect();
    let mut diffs = Vec::new();

    // Dropped enums
    for name in old_map.keys() {
        if !new_map.contains_key(name) {
            diffs.push(MigrationDiff {
                entity_name: name.to_string(),
                entity_type: EntityType::Enum,
                action: DiffAction::Drop,
            });
        }
    }

    // Added or changed enums
    for (name, new_enum) in &new_map {
        match old_map.get(name) {
            None => {
                diffs.push(MigrationDiff {
                    entity_name: name.to_string(),
                    entity_type: EntityType::Enum,
                    action: DiffAction::Add,
                });
            }
            Some(old_enum) => {
                let changes = diff_enum_values(&old_enum.values, &new_enum.values);
                if !changes.is_empty() {
                    diffs.push(MigrationDiff {
                        entity_name: name.to_string(),
                        entity_type: EntityType::Enum,
                        action: DiffAction::Change(changes),
                    });
                }
            }
        }
    }

    diffs
}

fn diff_enum_values(old: &[String], new: &[String]) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    let old_set: std::collections::HashSet<&str> = old.iter().map(|s| s.as_str()).collect();
    let new_set: std::collections::HashSet<&str> = new.iter().map(|s| s.as_str()).collect();

    for val in &old_set {
        if !new_set.contains(val) {
            changes.push(FieldChange {
                field_name: val.to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            });
        }
    }

    for val in new {
        if !old_set.contains(val.as_str()) {
            changes.push(FieldChange {
                field_name: val.clone(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Add(FieldDetail::EnumValue(val.clone())),
            });
        }
    }

    changes
}
```

- [ ] **Step 4: Run all diff tests**

Run: `cargo test -p dbd-core diff::tests`
Expected: all D1-D21, E2, E4 pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "feat: complete diff engine with enum support and tests D1-D21"
```

---

### Task 6: SQL generation — tests and implementation (S1-S14)

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

- [ ] **Step 1: Write SQL generation tests S1-S14**

Append to the `mod tests` block in `crates/dbd-core/src/diff.rs`:

```rust
    // ── SQL Generation Tests ───────────────────────────

    #[test]
    fn s1_column_add_generates_alter() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(FieldDetail::Column(col("email", "TEXT"))),
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert_eq!(sql.trim(), "ALTER TABLE config.users ADD COLUMN email TEXT;");
    }

    #[test]
    fn s2_column_add_not_null_default() {
        let c = ColumnDef {
            nullable: false,
            default_value: Some("'active'".to_string()),
            ..col("status", "VARCHAR(20)")
        };
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(FieldDetail::Column(c)),
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ADD COLUMN status VARCHAR(20) NOT NULL DEFAULT 'active'"));
    }

    #[test]
    fn s3_column_drop() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Drop,
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert_eq!(sql.trim(), "ALTER TABLE config.users DROP COLUMN email;");
    }

    #[test]
    fn s4_column_type_change() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: FieldDetail::Column(col("email", "VARCHAR(100)")),
                    new: FieldDetail::Column(col("email", "TEXT")),
                },
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ALTER TABLE config.users ALTER COLUMN email TYPE TEXT;"));
    }

    #[test]
    fn s5_nullable_change() {
        // NOT NULL -> nullable
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: FieldDetail::Column(col_not_null("email", "TEXT")),
                    new: FieldDetail::Column(col("email", "TEXT")),
                },
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ALTER TABLE config.users ALTER COLUMN email DROP NOT NULL;"));
    }

    #[test]
    fn s5b_set_not_null() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: FieldDetail::Column(col("email", "TEXT")),
                    new: FieldDetail::Column(col_not_null("email", "TEXT")),
                },
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ALTER TABLE config.users ALTER COLUMN email SET NOT NULL;"));
    }

    #[test]
    fn s6_default_change() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: FieldDetail::Column(col("status", "TEXT")),
                    new: FieldDetail::Column(col_with_default("status", "TEXT", "'active'")),
                },
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ALTER TABLE config.users ALTER COLUMN status SET DEFAULT 'active';"));
    }

    #[test]
    fn s6b_drop_default() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: FieldDetail::Column(col_with_default("status", "TEXT", "'active'")),
                    new: FieldDetail::Column(col("status", "TEXT")),
                },
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ALTER TABLE config.users ALTER COLUMN status DROP DEFAULT;"));
    }

    #[test]
    fn s7_constraint_add() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "uq_email".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(FieldDetail::Constraint(TableConstraint::Unique {
                    name: Some("uq_email".to_string()),
                    columns: vec!["email".to_string()],
                })),
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ALTER TABLE config.users ADD CONSTRAINT uq_email UNIQUE (email);"));
    }

    #[test]
    fn s8_constraint_drop() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "uq_email".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop,
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert_eq!(sql.trim(), "ALTER TABLE config.users DROP CONSTRAINT uq_email;");
    }

    #[test]
    fn s9_fk_constraint_add() {
        let fk = ForeignKey {
            name: Some("fk_orders_users".to_string()),
            columns: vec!["user_id".to_string()],
            ref_schema: Some("config".to_string()),
            ref_table: "users".to_string(),
            ref_columns: vec!["id".to_string()],
            on_delete: None,
            on_update: None,
        };
        let diff = MigrationDiff {
            entity_name: "config.orders".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "fk_orders_users".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(FieldDetail::Constraint(TableConstraint::ForeignKey(fk))),
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("ADD CONSTRAINT fk_orders_users FOREIGN KEY (user_id) REFERENCES config.users(id)"));
    }

    #[test]
    fn s10_index_add() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(FieldDetail::Index(IndexDef {
                    name: Some("idx_email".to_string()),
                    columns: vec![IndexColumn { name: "email".to_string(), order: None }],
                    unique: true,
                    index_type: None,
                })),
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert!(sql.contains("CREATE UNIQUE INDEX idx_email ON config.users (email);"));
    }

    #[test]
    fn s11_index_drop() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Drop,
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert_eq!(sql.trim(), "DROP INDEX idx_email;");
    }

    #[test]
    fn s12_enum_value_add() {
        let diff = MigrationDiff {
            entity_name: "public.gender_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "other".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Add(FieldDetail::EnumValue("other".to_string())),
            }]),
        };
        let sql = generate_migration_sql(&diff);
        assert_eq!(sql.trim(), "ALTER TYPE public.gender_type ADD VALUE 'other';");
    }

    #[test]
    fn s13_table_drop() {
        let diff = MigrationDiff {
            entity_name: "staging.lookups".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Drop,
        };
        let sql = generate_migration_sql(&diff);
        assert_eq!(sql.trim(), "DROP TABLE staging.lookups CASCADE;");
    }

    #[test]
    fn s14_multiple_field_changes() {
        let diff = MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "email".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(FieldDetail::Column(col("email", "TEXT"))),
                },
                FieldChange {
                    field_name: "old_col".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "idx_email".to_string(),
                    field_type: FieldType::Index,
                    action: ChangeAction::Add(FieldDetail::Index(IndexDef {
                        name: Some("idx_email".to_string()),
                        columns: vec![IndexColumn { name: "email".to_string(), order: None }],
                        unique: false,
                        index_type: None,
                    })),
                },
            ]),
        };
        let sql = generate_migration_sql(&diff);
        let lines: Vec<&str> = sql.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core diff::tests::s`
Expected: FAIL — `generate_migration_sql` not found

- [ ] **Step 3: Implement generate_migration_sql()**

Add above the `#[cfg(test)]` block in `crates/dbd-core/src/diff.rs`:

```rust
// ── SQL generation ─────────────────────────────────────

/// Generate migration SQL from a diff. Returns empty string for Add actions
/// (new entities are created by regular apply).
pub fn generate_migration_sql(diff: &MigrationDiff) -> String {
    match &diff.action {
        DiffAction::Add => String::new(),
        DiffAction::Drop => match diff.entity_type {
            EntityType::Table => format!("DROP TABLE {} CASCADE;\n", diff.entity_name),
            EntityType::Enum => format!("-- WARNING: manual migration required for dropped enum {}\n", diff.entity_name),
            _ => String::new(),
        },
        DiffAction::Change(changes) => {
            let mut lines = Vec::new();
            for change in changes {
                let sql = generate_field_sql(&diff.entity_name, &diff.entity_type, change);
                if !sql.is_empty() {
                    lines.push(sql);
                }
            }
            lines.join("\n")
        }
    }
}

fn generate_field_sql(entity_name: &str, entity_type: &EntityType, change: &FieldChange) -> String {
    match (&change.field_type, &change.action) {
        // Column changes
        (FieldType::Column, ChangeAction::Add(FieldDetail::Column(col))) => {
            let mut sql = format!("ALTER TABLE {} ADD COLUMN {} {}", entity_name, col.name, col.data_type);
            if !col.nullable { sql.push_str(" NOT NULL"); }
            if let Some(ref default) = col.default_value { sql.push_str(&format!(" DEFAULT {default}")); }
            sql.push(';');
            sql
        }
        (FieldType::Column, ChangeAction::Drop) => {
            format!("ALTER TABLE {} DROP COLUMN {};", entity_name, change.field_name)
        }
        (FieldType::Column, ChangeAction::Alter { old, new }) => {
            let (old_col, new_col) = match (old, new) {
                (FieldDetail::Column(o), FieldDetail::Column(n)) => (o, n),
                _ => return String::new(),
            };
            let mut stmts = Vec::new();
            if old_col.data_type != new_col.data_type {
                stmts.push(format!("ALTER TABLE {} ALTER COLUMN {} TYPE {};", entity_name, new_col.name, new_col.data_type));
            }
            if old_col.nullable != new_col.nullable {
                if new_col.nullable {
                    stmts.push(format!("ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL;", entity_name, new_col.name));
                } else {
                    stmts.push(format!("ALTER TABLE {} ALTER COLUMN {} SET NOT NULL;", entity_name, new_col.name));
                }
            }
            if old_col.default_value != new_col.default_value {
                match &new_col.default_value {
                    Some(val) => stmts.push(format!("ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {};", entity_name, new_col.name, val)),
                    None => stmts.push(format!("ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT;", entity_name, new_col.name)),
                }
            }
            stmts.join("\n")
        }

        // Constraint changes
        (FieldType::Constraint, ChangeAction::Add(FieldDetail::Constraint(con))) => {
            generate_add_constraint_sql(entity_name, con)
        }
        (FieldType::Constraint, ChangeAction::Drop) => {
            format!("ALTER TABLE {} DROP CONSTRAINT {};", entity_name, change.field_name)
        }

        // Index changes
        (FieldType::Index, ChangeAction::Add(FieldDetail::Index(idx))) => {
            generate_add_index_sql(entity_name, idx)
        }
        (FieldType::Index, ChangeAction::Drop) => {
            format!("DROP INDEX {};", change.field_name)
        }

        // Enum value changes
        (FieldType::EnumValue, ChangeAction::Add(FieldDetail::EnumValue(val))) => {
            match entity_type {
                EntityType::Enum => format!("ALTER TYPE {} ADD VALUE '{}';", entity_name, val),
                _ => String::new(),
            }
        }
        (FieldType::EnumValue, ChangeAction::Drop) => {
            // Warning only — no SQL generated for enum value drops
            String::new()
        }

        _ => String::new(),
    }
}

fn generate_add_constraint_sql(table_name: &str, constraint: &TableConstraint) -> String {
    match constraint {
        TableConstraint::PrimaryKey { name, columns } => {
            let cols = columns.join(", ");
            match name {
                Some(n) => format!("ALTER TABLE {} ADD CONSTRAINT {} PRIMARY KEY ({});", table_name, n, cols),
                None => format!("ALTER TABLE {} ADD PRIMARY KEY ({});", table_name, cols),
            }
        }
        TableConstraint::Unique { name, columns } => {
            let cols = columns.join(", ");
            match name {
                Some(n) => format!("ALTER TABLE {} ADD CONSTRAINT {} UNIQUE ({});", table_name, n, cols),
                None => format!("ALTER TABLE {} ADD UNIQUE ({});", table_name, cols),
            }
        }
        TableConstraint::ForeignKey(fk) => {
            let cols = fk.columns.join(", ");
            let ref_table = match &fk.ref_schema {
                Some(s) => format!("{}.{}", s, fk.ref_table),
                None => fk.ref_table.clone(),
            };
            let ref_cols = fk.ref_columns.join(", ");
            match &fk.name {
                Some(n) => format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}({});", table_name, n, cols, ref_table, ref_cols),
                None => format!("ALTER TABLE {} ADD FOREIGN KEY ({}) REFERENCES {}({});", table_name, cols, ref_table, ref_cols),
            }
        }
        TableConstraint::Check { name, expression } => {
            match name {
                Some(n) => format!("ALTER TABLE {} ADD CONSTRAINT {} CHECK ({});", table_name, n, expression),
                None => format!("ALTER TABLE {} ADD CHECK ({});", table_name, expression),
            }
        }
    }
}

fn generate_add_index_sql(table_name: &str, idx: &IndexDef) -> String {
    let unique = if idx.unique { "UNIQUE " } else { "" };
    let cols: Vec<&str> = idx.columns.iter().map(|c| c.name.as_str()).collect();
    let cols_str = cols.join(", ");
    match &idx.name {
        Some(n) => format!("CREATE {}INDEX {} ON {} ({});", unique, n, table_name, cols_str),
        None => format!("CREATE {}INDEX ON {} ({});", unique, table_name, cols_str),
    }
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p dbd-core diff::tests`
Expected: all tests pass (D1-D21, S1-S14, E2, E4)

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "feat: SQL generation from diffs with tests S1-S14"
```

---

### Task 7: Update Snapshot types (EnumSnapshot, MigrationGraph.added, backward compat)

**Files:**
- Modify: `crates/dbd-core/src/snapshot.rs`

- [ ] **Step 1: Add EnumSnapshot, update Snapshot and MigrationGraph**

In `crates/dbd-core/src/snapshot.rs`, after `use crate::entity::{ColumnDef, IndexDef, TableConstraint};` add:

```rust
use crate::entity::EntityType;
use crate::diff::{MigrationDiff, DiffAction};
```

Add `EnumSnapshot` struct after `TableSnapshot`:

```rust
/// Snapshot of a single enum type's values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumSnapshot {
    pub name: String,
    pub schema: String,
    pub values: Vec<String>,
}
```

Update `Snapshot` to include enums with backward-compatible default:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub description: String,
    pub timestamp: String,
    pub tables: Vec<TableSnapshot>,
    #[serde(default)]
    pub enums: Vec<EnumSnapshot>,
}
```

Update `MigrationGraph` to include `added`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationGraph {
    #[serde(rename = "fromVersion")]
    pub from_version: u32,
    #[serde(rename = "toVersion")]
    pub to_version: u32,
    #[serde(default)]
    pub added: Vec<String>,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
}
```

Update `PendingMigration` to include `added`:

```rust
#[derive(Debug, Clone)]
pub struct PendingMigration {
    pub from_version: u32,
    pub to_version: u32,
    pub migration_dir: PathBuf,
    pub added: Vec<String>,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
    pub checksum: String,
}
```

Update the `pending_migrations` function where `PendingMigration` is constructed (around line 186):

```rust
            Some(PendingMigration {
                from_version: graph.from_version,
                to_version,
                migration_dir,
                added: graph.added,
                altered: graph.altered,
                dropped: graph.dropped,
                checksum,
            })
```

- [ ] **Step 2: Fix any compilation errors from updated types**

Run: `cargo check`
Expected: compiles (may need to update test fixtures that construct `MigrationGraph` without `added`)

Update test helper `create_migration_dir` in snapshot.rs tests to include `added: vec![]`:

```rust
        let graph = MigrationGraph {
            from_version: 1,
            to_version: 2,
            added: vec![],
            altered: vec!["config.lookup_values".to_string()],
            dropped: vec![],
        };
```

- [ ] **Step 3: Add E6 backward compatibility test**

Append to snapshot.rs test module:

```rust
    #[test]
    fn e6_snapshot_backward_compat_no_enums() {
        let json = r#"{"version":1,"description":"old","timestamp":"2026-01-01T00:00:00Z","tables":[]}"#;
        let snap: Snapshot = serde_json::from_str(json).unwrap();
        assert!(snap.enums.is_empty());
    }

    #[test]
    fn migration_graph_backward_compat_no_added() {
        let json = r#"{"fromVersion":1,"toVersion":2,"altered":["t"],"dropped":[]}"#;
        let graph: MigrationGraph = serde_json::from_str(json).unwrap();
        assert!(graph.added.is_empty());
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/snapshot.rs
git commit -m "feat: add EnumSnapshot, MigrationGraph.added, backward-compatible serde"
```

---

### Task 8: Add version to ProjectConfig

**Files:**
- Modify: `crates/dbd-core/src/config.rs`

- [ ] **Step 1: Add version field to ProjectConfig**

In `crates/dbd-core/src/config.rs`, update `ProjectConfig`:

```rust
#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub note: Option<String>,
    pub version: Option<u32>,
}
```

- [ ] **Step 2: Add update_version function**

Add to `crates/dbd-core/src/config.rs`:

```rust
/// Update the project.version field in design.yaml.
/// Reads the file, modifies the version, writes it back.
pub fn update_version(config_path: &Path, version: u32) -> Result<()> {
    let content = std::fs::read_to_string(config_path).map_err(|e| {
        DbdError::Config(format!("Cannot read {}: {}", config_path.display(), e))
    })?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&content)?;
    if let Some(project) = doc.get_mut("project") {
        project["version"] = serde_yaml::Value::Number(serde_yaml::Number::from(version as u64));
    }
    let updated = serde_yaml::to_string(&doc)?;
    std::fs::write(config_path, updated).map_err(|e| {
        DbdError::Config(format!("Cannot write {}: {}", config_path.display(), e))
    })?;
    Ok(())
}
```

- [ ] **Step 3: Add test for version parsing and update**

Append a test:

```rust
#[cfg(test)]
mod version_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_version_from_config() {
        let yaml = "project:\n  name: test\n  version: 5\ntarget: {}\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.project.version, Some(5));
    }

    #[test]
    fn parses_missing_version_as_none() {
        let yaml = "project:\n  name: test\ntarget: {}\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.project.version, None);
    }

    #[test]
    fn update_version_writes_to_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("design.yaml");
        std::fs::write(&path, "project:\n  name: test\ntarget: {}\n").unwrap();
        update_version(&path, 3).unwrap();
        let config: DesignConfig = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.project.version, Some(3));
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core config::version_tests`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/config.rs
git commit -m "feat: add version to ProjectConfig with update_version()"
```

---

### Task 9: Snapshot prepare (pure logic) — tests and implementation

**Files:**
- Modify: `crates/dbd-core/src/snapshot.rs`

- [ ] **Step 1: Write tests SC1-SC10, E5, SC7-SC9**

Add to snapshot.rs, in the test module:

```rust
    use crate::entity::{Entity, EntityType, EnumValue, TableDef, TableComments};
    use crate::diff::{DiffAction, FieldType};

    fn make_table_entity(name: &str, schema: &str, columns: Vec<crate::entity::ColumnDef>) -> Entity {
        Entity {
            entity_type: EntityType::Table,
            name: name.to_string(),
            schema: Some(schema.to_string()),
            file: None,
            format: None,
            refers: vec![],
            references: vec![],
            search_paths: vec![],
            errors: vec![],
            warnings: vec![],
            reads: vec![],
            writes: vec![],
            table_def: Some(TableDef {
                columns,
                constraints: vec![],
                indexes: vec![],
                comments: TableComments::default(),
            }),
            enum_values: vec![],
        }
    }

    fn make_enum_entity(name: &str, schema: &str, values: Vec<&str>) -> Entity {
        Entity {
            entity_type: EntityType::Enum,
            name: name.to_string(),
            schema: Some(schema.to_string()),
            file: None,
            format: None,
            refers: vec![],
            references: vec![],
            search_paths: vec![],
            errors: vec![],
            warnings: vec![],
            reads: vec![],
            writes: vec![],
            table_def: None,
            enum_values: values.into_iter().map(|v| EnumValue { name: v.to_string(), note: None }).collect(),
        }
    }

    fn test_col(name: &str, data_type: &str) -> crate::entity::ColumnDef {
        crate::entity::ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            is_identity: false,
            comment: None,
            inline_fk: None,
        }
    }

    // ── SC7: Entity to TableSnapshot ───────────────────

    #[test]
    fn sc7_entity_to_table_snapshot() {
        let entity = make_table_entity("config.users", "config", vec![test_col("id", "BIGINT"), test_col("name", "TEXT")]);
        let snap = entity_to_table_snapshot(&entity).unwrap();
        assert_eq!(snap.name, "config.users");
        assert_eq!(snap.schema, "config");
        assert_eq!(snap.columns.len(), 2);
    }

    // ── SC8: Entity to EnumSnapshot ────────────────────

    #[test]
    fn sc8_entity_to_enum_snapshot() {
        let entity = make_enum_entity("public.gender_type", "public", vec!["male", "female"]);
        let snap = entity_to_enum_snapshot(&entity);
        assert_eq!(snap.name, "public.gender_type");
        assert_eq!(snap.schema, "public");
        assert_eq!(snap.values, vec!["male", "female"]);
    }

    // ── SC9: Snapshot round-trip ───────────────────────

    #[test]
    fn sc9_snapshot_serialization_roundtrip() {
        let snapshot = Snapshot {
            version: 1,
            description: "test".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "config.users".to_string(),
                schema: "config".to_string(),
                columns: vec![test_col("id", "BIGINT")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![EnumSnapshot {
                name: "public.status".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string()],
            }],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, 1);
        assert_eq!(deserialized.tables.len(), 1);
        assert_eq!(deserialized.enums.len(), 1);
    }

    // ── SC1: First snapshot baseline ───────────────────

    #[test]
    fn sc1_first_snapshot_baseline() {
        let entities = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT")]),
            make_table_entity("config.orders", "config", vec![test_col("id", "BIGINT")]),
            make_enum_entity("public.status", "public", vec!["active", "inactive"]),
        ];
        let result = prepare_snapshot(&entities, None, 1, "initial");
        assert!(result.is_baseline);
        assert!(!result.no_changes);
        assert_eq!(result.snapshot.version, 1);
        assert_eq!(result.snapshot.tables.len(), 2);
        assert_eq!(result.snapshot.enums.len(), 1);
        assert!(result.diffs.is_empty());
        assert!(result.graph.is_none());
        assert!(result.migration_files.is_empty());
    }

    // ── SC3: No changes skipped ────────────────────────

    #[test]
    fn sc3_no_changes_skipped() {
        let entities = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT")]),
        ];
        let prev = Snapshot {
            version: 1,
            description: "v1".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "config.users".to_string(),
                schema: "config".to_string(),
                columns: vec![test_col("id", "BIGINT")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![],
        };
        let result = prepare_snapshot(&entities, Some(&prev), 2, "no change");
        assert!(result.no_changes);
        assert!(result.diffs.is_empty());
    }

    // ── SC2: Second snapshot with changes ──────────────

    #[test]
    fn sc2_second_snapshot_with_changes() {
        let entities = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT"), test_col("email", "TEXT")]),
        ];
        let prev = Snapshot {
            version: 1,
            description: "v1".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "config.users".to_string(),
                schema: "config".to_string(),
                columns: vec![test_col("id", "BIGINT")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![],
        };
        let result = prepare_snapshot(&entities, Some(&prev), 2, "add email");
        assert!(!result.no_changes);
        assert!(!result.is_baseline);
        assert_eq!(result.snapshot.version, 2);
        assert_eq!(result.diffs.len(), 1);
        assert!(result.graph.is_some());
        let graph = result.graph.unwrap();
        assert_eq!(graph.from_version, 1);
        assert_eq!(graph.to_version, 2);
        assert!(graph.altered.contains(&"config.users".to_string()));
        assert!(!result.migration_files.is_empty());
    }

    // ── SC4: Snapshot with new table ───────────────────

    #[test]
    fn sc4_snapshot_new_table() {
        let entities = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT")]),
            make_table_entity("config.orders", "config", vec![test_col("id", "BIGINT")]),
        ];
        let prev = Snapshot {
            version: 1,
            description: "v1".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![TableSnapshot {
                name: "config.users".to_string(),
                schema: "config".to_string(),
                columns: vec![test_col("id", "BIGINT")],
                indexes: vec![],
                table_constraints: vec![],
            }],
            enums: vec![],
        };
        let result = prepare_snapshot(&entities, Some(&prev), 2, "add orders");
        let graph = result.graph.unwrap();
        assert!(graph.added.contains(&"config.orders".to_string()));
        assert!(graph.altered.is_empty());
        // No migration SQL file for new tables
        assert!(result.migration_files.is_empty());
    }

    // ── SC5: Snapshot with dropped table ───────────────

    #[test]
    fn sc5_snapshot_dropped_table() {
        let entities = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT")]),
        ];
        let prev = Snapshot {
            version: 1,
            description: "v1".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![
                TableSnapshot { name: "config.users".to_string(), schema: "config".to_string(), columns: vec![test_col("id", "BIGINT")], indexes: vec![], table_constraints: vec![] },
                TableSnapshot { name: "staging.temp".to_string(), schema: "staging".to_string(), columns: vec![test_col("id", "BIGINT")], indexes: vec![], table_constraints: vec![] },
            ],
            enums: vec![],
        };
        let result = prepare_snapshot(&entities, Some(&prev), 2, "drop temp");
        let graph = result.graph.unwrap();
        assert!(graph.dropped.contains(&"staging.temp".to_string()));
        // Should have migration SQL for the drop
        assert!(!result.migration_files.is_empty());
        let drop_file = result.migration_files.iter().find(|f| f.relative_path.to_str().unwrap().contains("temp")).unwrap();
        assert!(drop_file.content.contains("DROP TABLE"));
    }

    // ── E5: Empty project ──────────────────────────────

    #[test]
    fn e5_empty_project_snapshot() {
        let entities: Vec<Entity> = vec![];
        let result = prepare_snapshot(&entities, None, 1, "empty");
        assert!(result.is_baseline);
        assert!(result.snapshot.tables.is_empty());
        assert!(result.snapshot.enums.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core snapshot::tests::sc`
Expected: FAIL — `prepare_snapshot`, `entity_to_table_snapshot`, `entity_to_enum_snapshot` not found

- [ ] **Step 3: Implement conversion functions and prepare_snapshot**

Add to `crates/dbd-core/src/snapshot.rs`, above the I/O section:

```rust
use crate::diff::{self, DiffAction, MigrationDiff};
use crate::entity::{Entity, EntityType};
use std::path::PathBuf;

// ── Entity conversion ──────────────────────────────────

/// Convert a table entity to a TableSnapshot.
pub fn entity_to_table_snapshot(entity: &Entity) -> Option<TableSnapshot> {
    let table_def = entity.table_def.as_ref()?;
    Some(TableSnapshot {
        name: entity.name.clone(),
        schema: entity.schema.clone().unwrap_or_default(),
        columns: table_def.columns.clone(),
        indexes: table_def.indexes.clone(),
        table_constraints: table_def.constraints.clone(),
    })
}

/// Convert an enum entity to an EnumSnapshot.
pub fn entity_to_enum_snapshot(entity: &Entity) -> EnumSnapshot {
    EnumSnapshot {
        name: entity.name.clone(),
        schema: entity.schema.clone().unwrap_or_default(),
        values: entity.enum_values.iter().map(|v| v.name.clone()).collect(),
    }
}

// ── Snapshot preparation (pure logic) ──────────────────

/// Result of preparing a snapshot — no I/O performed.
pub struct SnapshotResult {
    pub snapshot: Snapshot,
    pub diffs: Vec<MigrationDiff>,
    pub migration_files: Vec<MigrationFile>,
    pub graph: Option<MigrationGraph>,
    pub is_baseline: bool,
    pub no_changes: bool,
}

/// A migration file to be written.
pub struct MigrationFile {
    pub relative_path: PathBuf,
    pub content: String,
}

/// Build a snapshot and compute diffs from entities. No filesystem I/O.
pub fn prepare_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> SnapshotResult {
    let tables: Vec<TableSnapshot> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Table && e.table_def.is_some())
        .filter_map(|e| entity_to_table_snapshot(e))
        .collect();

    let enums: Vec<EnumSnapshot> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .map(|e| entity_to_enum_snapshot(e))
        .collect();

    let snapshot = Snapshot {
        version: next_version,
        description: description.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        tables,
        enums,
    };

    match previous {
        None => SnapshotResult {
            snapshot,
            diffs: Vec::new(),
            migration_files: Vec::new(),
            graph: None,
            is_baseline: true,
            no_changes: false,
        },
        Some(prev) => {
            let diffs = diff::diff(prev, &snapshot);
            if diffs.is_empty() {
                return SnapshotResult {
                    snapshot,
                    diffs: Vec::new(),
                    migration_files: Vec::new(),
                    graph: None,
                    is_baseline: false,
                    no_changes: true,
                };
            }

            let mut added = Vec::new();
            let mut altered = Vec::new();
            let mut dropped = Vec::new();
            let mut migration_files = Vec::new();

            for d in &diffs {
                match &d.action {
                    DiffAction::Add => added.push(d.entity_name.clone()),
                    DiffAction::Drop => {
                        dropped.push(d.entity_name.clone());
                        let sql = diff::generate_migration_sql(d);
                        if !sql.is_empty() {
                            let parts: Vec<&str> = d.entity_name.split('.').collect();
                            let path = if parts.len() > 1 {
                                PathBuf::from(parts[0]).join(format!("{}.sql", parts[1]))
                            } else {
                                PathBuf::from(format!("{}.sql", d.entity_name))
                            };
                            migration_files.push(MigrationFile { relative_path: path, content: sql });
                        }
                    }
                    DiffAction::Change(_) => {
                        altered.push(d.entity_name.clone());
                        let sql = diff::generate_migration_sql(d);
                        if !sql.is_empty() {
                            let parts: Vec<&str> = d.entity_name.split('.').collect();
                            let path = if parts.len() > 1 {
                                PathBuf::from(parts[0]).join(format!("{}.sql", parts[1]))
                            } else {
                                PathBuf::from(format!("{}.sql", d.entity_name))
                            };
                            migration_files.push(MigrationFile { relative_path: path, content: sql });
                        }
                    }
                }
            }

            let graph = MigrationGraph {
                from_version: prev.version,
                to_version: next_version,
                added,
                altered,
                dropped,
            };

            SnapshotResult {
                snapshot,
                diffs,
                migration_files,
                graph: Some(graph),
                is_baseline: false,
                no_changes: false,
            }
        }
    }
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/snapshot.rs
git commit -m "feat: prepare_snapshot pure logic with entity conversion and tests SC1-SC9"
```

---

### Task 10: Snapshot I/O — create_snapshot and write files

**Files:**
- Modify: `crates/dbd-core/src/snapshot.rs`

- [ ] **Step 1: Implement create_snapshot I/O wrapper and write helpers**

Add to `crates/dbd-core/src/snapshot.rs`:

```rust
// ── Snapshot I/O (create) ──────────────────────────────

/// Create a snapshot: reads previous from disk, computes diff, writes output files.
pub fn create_snapshot(
    entities: &[Entity],
    project_dir: &Path,
    config_path: &Path,
    description: &str,
) -> Result<SnapshotResult> {
    let prev = latest_snapshot(project_dir)?;
    let version = next_version(project_dir);
    let result = prepare_snapshot(entities, prev.as_ref(), version, description);

    if result.no_changes {
        return Ok(result);
    }

    // Write snapshot JSON
    let snapshots_dir = project_dir.join(SNAPSHOTS_DIR);
    std::fs::create_dir_all(&snapshots_dir)?;
    let snap_file = snapshots_dir.join(format!("{}.json", pad_version(version)));
    let json = serde_json::to_string_pretty(&result.snapshot)?;
    std::fs::write(&snap_file, json)?;

    // Write migration files (if any)
    if let Some(ref graph) = result.graph {
        let migration_dir = project_dir.join(MIGRATIONS_DIR).join(pad_version(version));
        std::fs::create_dir_all(&migration_dir)?;

        // Write graph.json
        let graph_json = serde_json::to_string_pretty(graph)?;
        std::fs::write(migration_dir.join("graph.json"), graph_json)?;

        // Write SQL files
        for mf in &result.migration_files {
            let full_path = migration_dir.join(&mf.relative_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full_path, &mf.content)?;
        }
    }

    // Update design.yaml version
    crate::config::update_version(config_path, version)?;

    Ok(result)
}
```

- [ ] **Step 2: Add SC10 integration test**

Add to snapshot tests:

```rust
    #[test]
    fn sc10_create_snapshot_writes_files() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("design.yaml");
        fs::write(&config_path, "project:\n  name: test\ntarget: {}\n").unwrap();

        let entities = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT")]),
        ];

        // First snapshot — baseline
        let result = create_snapshot(&entities, tmp.path(), &config_path, "initial").unwrap();
        assert!(result.is_baseline);
        assert!(tmp.path().join("snapshots/001.json").exists());

        // Verify design.yaml updated
        let config_content = fs::read_to_string(&config_path).unwrap();
        assert!(config_content.contains("version: 1") || config_content.contains("version: '1'"));

        // Second snapshot with changes
        let entities_v2 = vec![
            make_table_entity("config.users", "config", vec![test_col("id", "BIGINT"), test_col("email", "TEXT")]),
        ];
        let result2 = create_snapshot(&entities_v2, tmp.path(), &config_path, "add email").unwrap();
        assert!(!result2.no_changes);
        assert!(tmp.path().join("snapshots/002.json").exists());
        assert!(tmp.path().join("migrations/002/graph.json").exists());
        assert!(tmp.path().join("migrations/002/config/users.sql").exists());
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p dbd-core snapshot::tests::sc10`
Expected: pass

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/snapshot.rs
git commit -m "feat: create_snapshot I/O with file writing and SC10 integration test"
```

---

### Task 11: Execution plan — tests and implementation (A1-A8)

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Add execution plan types to design.rs**

Add after the existing imports in `crates/dbd-core/src/design.rs`:

```rust
use crate::snapshot::PendingMigration;

// ── Execution plan types ───────────────────────────────

/// Strategy for how apply should proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// DB version == 0: apply all entities, mark latest version
    Fresh,
    /// DB version < latest: run migrations then apply
    Migrate,
    /// DB version == latest: idempotent apply only
    Current,
}

/// A single step in the execution plan.
#[derive(Debug, Clone)]
pub enum ExecutionStep {
    /// New entity from a migration's `added` list — needs CREATE before ALTERs.
    CreateEntity(String),
    /// Altered entity — run migration SQL, then re-apply DDL.
    MigrateEntity {
        entity_name: String,
        migration_sql_path: std::path::PathBuf,
        migration_version: u32,
    },
    /// Unchanged entity — idempotent apply.
    ApplyEntity(String),
    /// Dropped entity — run DROP SQL.
    DropEntity {
        entity_name: String,
        drop_sql_path: std::path::PathBuf,
        migration_version: u32,
    },
    /// Record a migration as applied.
    RecordMigration {
        version: u32,
        checksum: String,
    },
    /// Update _dbd_meta version.
    SetVersion(u32),
}

/// The complete execution plan for an apply operation.
#[derive(Debug)]
pub struct ExecutionPlan {
    pub strategy: ApplyStrategy,
    pub steps: Vec<ExecutionStep>,
}
```

- [ ] **Step 2: Implement build_execution_plan (pure logic)**

Add to `crates/dbd-core/src/design.rs`:

```rust
/// Build an execution plan from current state. Pure function — no I/O.
pub fn build_execution_plan(
    entities: &[Entity],
    db_version: u32,
    latest_version: u32,
    pending_migrations: &[PendingMigration],
) -> ExecutionPlan {
    let valid_entity_names: Vec<String> = entities
        .iter()
        .filter(|e| e.errors.is_empty() && e.entity_type != EntityType::External)
        .map(|e| e.name.clone())
        .collect();

    if db_version == 0 {
        // Fresh env — apply everything, mark latest
        let mut steps: Vec<ExecutionStep> = valid_entity_names
            .iter()
            .map(|name| ExecutionStep::ApplyEntity(name.clone()))
            .collect();
        if latest_version > 0 {
            steps.push(ExecutionStep::SetVersion(latest_version));
        }
        return ExecutionPlan {
            strategy: ApplyStrategy::Fresh,
            steps,
        };
    }

    if db_version >= latest_version || pending_migrations.is_empty() {
        // Current — idempotent apply
        let steps = valid_entity_names
            .iter()
            .map(|name| ExecutionStep::ApplyEntity(name.clone()))
            .collect();
        return ExecutionPlan {
            strategy: ApplyStrategy::Current,
            steps,
        };
    }

    // Behind — need migrations
    let mut steps = Vec::new();

    // Collect all added/altered/dropped from pending migrations
    let mut all_added: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_altered: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut all_dropped: std::collections::HashSet<String> = std::collections::HashSet::new();

    for m in pending_migrations {
        for name in &m.added { all_added.insert(name.clone()); }
        for name in &m.altered { all_altered.insert(name.clone()); }
        for name in &m.dropped { all_dropped.insert(name.clone()); }
    }

    // For each entity in dependency order:
    // - If it's in added → CreateEntity
    // - If it's in altered → MigrateEntity (per migration version)
    // - If it's in dropped → skip (will be dropped separately)
    // - Otherwise → ApplyEntity
    for entity_name in &valid_entity_names {
        if all_dropped.contains(entity_name) {
            continue; // handled below
        }
        if all_added.contains(entity_name) {
            steps.push(ExecutionStep::CreateEntity(entity_name.clone()));
        } else if all_altered.contains(entity_name) {
            // Add migration steps for each version that alters this entity
            for m in pending_migrations {
                if m.altered.contains(entity_name) {
                    let parts: Vec<&str> = entity_name.split('.').collect();
                    let sql_path = if parts.len() > 1 {
                        m.migration_dir.join(parts[0]).join(format!("{}.sql", parts[1]))
                    } else {
                        m.migration_dir.join(format!("{}.sql", entity_name))
                    };
                    steps.push(ExecutionStep::MigrateEntity {
                        entity_name: entity_name.clone(),
                        migration_sql_path: sql_path,
                        migration_version: m.to_version,
                    });
                }
            }
            // Then re-apply the entity DDL
            steps.push(ExecutionStep::ApplyEntity(entity_name.clone()));
        } else {
            steps.push(ExecutionStep::ApplyEntity(entity_name.clone()));
        }
    }

    // Drop steps
    for m in pending_migrations {
        for name in &m.dropped {
            let parts: Vec<&str> = name.split('.').collect();
            let sql_path = if parts.len() > 1 {
                m.migration_dir.join(parts[0]).join(format!("{}.sql", parts[1]))
            } else {
                m.migration_dir.join(format!("{}.sql", name))
            };
            steps.push(ExecutionStep::DropEntity {
                entity_name: name.clone(),
                drop_sql_path: sql_path,
                migration_version: m.to_version,
            });
        }
    }

    // Record migrations
    for m in pending_migrations {
        steps.push(ExecutionStep::RecordMigration {
            version: m.to_version,
            checksum: m.checksum.clone(),
        });
    }

    steps.push(ExecutionStep::SetVersion(latest_version));

    ExecutionPlan {
        strategy: ApplyStrategy::Migrate,
        steps,
    }
}
```

- [ ] **Step 3: Write tests A1-A8**

Add to the test module in `crates/dbd-core/src/design.rs`:

```rust
    use crate::snapshot::PendingMigration;
    use std::path::PathBuf;

    fn test_entity(name: &str) -> Entity {
        Entity::new(EntityType::Table, name)
    }

    fn test_migration(from: u32, to: u32, added: Vec<&str>, altered: Vec<&str>, dropped: Vec<&str>) -> PendingMigration {
        PendingMigration {
            from_version: from,
            to_version: to,
            migration_dir: PathBuf::from(format!("migrations/{:03}", to)),
            added: added.into_iter().map(String::from).collect(),
            altered: altered.into_iter().map(String::from).collect(),
            dropped: dropped.into_iter().map(String::from).collect(),
            checksum: format!("checksum_v{to}"),
        }
    }

    // ── A1: Fresh env ──────────────────────────────────

    #[test]
    fn a1_fresh_env_apply_all_mark_latest() {
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];
        let plan = build_execution_plan(&entities, 0, 3, &[]);
        assert_eq!(plan.strategy, ApplyStrategy::Fresh);
        let apply_count = plan.steps.iter().filter(|s| matches!(s, ExecutionStep::ApplyEntity(_))).count();
        assert_eq!(apply_count, 2);
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(3))));
        // No migration steps
        assert!(!plan.steps.iter().any(|s| matches!(s, ExecutionStep::MigrateEntity { .. })));
    }

    // ── A2: Current env ────────────────────────────────

    #[test]
    fn a2_current_env_idempotent() {
        let entities = vec![test_entity("config.users")];
        let plan = build_execution_plan(&entities, 3, 3, &[]);
        assert_eq!(plan.strategy, ApplyStrategy::Current);
        assert!(!plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(_))));
    }

    // ── A3: Behind by one version ──────────────────────

    #[test]
    fn a3_behind_one_version() {
        let entities = vec![test_entity("config.users")];
        let migrations = vec![test_migration(1, 2, vec![], vec!["config.users"], vec![])];
        let plan = build_execution_plan(&entities, 1, 2, &migrations);
        assert_eq!(plan.strategy, ApplyStrategy::Migrate);
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::MigrateEntity { entity_name, .. } if entity_name == "config.users")));
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(2))));
    }

    // ── A4: Behind by multiple versions ────────────────

    #[test]
    fn a4_behind_multiple_versions() {
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];
        let migrations = vec![
            test_migration(1, 2, vec![], vec!["config.users"], vec![]),
            test_migration(2, 3, vec![], vec!["config.orders"], vec![]),
        ];
        let plan = build_execution_plan(&entities, 1, 3, &migrations);
        assert_eq!(plan.strategy, ApplyStrategy::Migrate);
        let record_count = plan.steps.iter().filter(|s| matches!(s, ExecutionStep::RecordMigration { .. })).count();
        assert_eq!(record_count, 2);
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(3))));
    }

    // ── A5: New table dependency — interleaved ─────────

    #[test]
    fn a5_new_table_dependency_interleaved() {
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];
        let migrations = vec![test_migration(1, 2, vec!["config.users"], vec!["config.orders"], vec![])];
        let plan = build_execution_plan(&entities, 1, 2, &migrations);
        // users should be CreateEntity, orders should be MigrateEntity
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::CreateEntity(n) if n == "config.users")));
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::MigrateEntity { entity_name, .. } if entity_name == "config.orders")));
    }

    // ── A6: Table drop ─────────────────────────────────

    #[test]
    fn a6_migration_with_table_drop() {
        let entities = vec![test_entity("config.users")];
        let migrations = vec![test_migration(1, 2, vec![], vec![], vec!["staging.temp"])];
        let plan = build_execution_plan(&entities, 1, 2, &migrations);
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::DropEntity { entity_name, .. } if entity_name == "staging.temp")));
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core design::tests`
Expected: all pass (including existing tests + A1-A6)

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat: build_execution_plan with tests A1-A6"
```

---

### Task 12: Wire CLI — snapshot create command

**Files:**
- Modify: `crates/dbd-cli/src/commands.rs`

- [ ] **Step 1: Implement cmd_snapshot_create**

In `crates/dbd-cli/src/commands.rs`, replace the snapshot stub:

```rust
        Commands::Snapshot { list, name } => {
            if *list {
                cmd_snapshot_list(project_dir, verbosity);
                return Ok(());
            }
            cmd_snapshot_create(config, env, project_dir, name.as_deref(), verbosity)
        }
```

Add the implementation function:

```rust
fn cmd_snapshot_create(
    config: &Path,
    env: &str,
    project_dir: &Path,
    description: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    let desc = description.unwrap_or("snapshot");
    let result = dbd_core::snapshot::create_snapshot(design.entities(), project_dir, config, desc)
        .context("Failed to create snapshot")?;

    if result.no_changes {
        output::info(verbosity, "No changes detected since last snapshot");
        return Ok(());
    }

    if result.is_baseline {
        output::info(verbosity, &format!(
            "Baseline snapshot v{} created ({} tables, {} enums)",
            result.snapshot.version,
            result.snapshot.tables.len(),
            result.snapshot.enums.len(),
        ));
    } else {
        let graph = result.graph.as_ref().unwrap();
        output::info(verbosity, &format!(
            "Snapshot v{} created — {} added, {} altered, {} dropped",
            result.snapshot.version,
            graph.added.len(),
            graph.altered.len(),
            graph.dropped.len(),
        ));
        for mf in &result.migration_files {
            output::detail(verbosity, &format!("  wrote: migrations/{}/{}", dbd_core::snapshot::pad_version(result.snapshot.version), mf.relative_path.display()));
        }
    }

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add crates/dbd-cli/src/commands.rs
git commit -m "feat: wire dbd snapshot create command in CLI"
```

---

### Task 13: Wire CLI — migrate command

**Files:**
- Modify: `crates/dbd-cli/src/commands.rs`

- [ ] **Step 1: Implement migrate status and apply**

Replace the migrate stub in `crates/dbd-cli/src/commands.rs`:

```rust
        Commands::Migrate { status, apply, to, dry_run } => {
            if *status {
                cmd_migrate_status(config, env, project_dir, database_url, verbosity).await
            } else if *apply {
                cmd_migrate_apply(config, env, project_dir, database_url, *to, *dry_run, verbosity).await
            } else {
                output::info(verbosity, "Use --status or --apply");
                Ok(())
            }
        }
```

Add implementations:

```rust
async fn cmd_migrate_status(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let latest_version = design.config().project.version.unwrap_or(0);

    let mut adapter = get_adapter(config, database_url).await?;
    adapter.ensure_meta_table().await.context("Failed to ensure meta table")?;
    let db_version = adapter.get_db_version().await.context("Failed to get DB version")?;

    if db_version >= latest_version {
        output::info(verbosity, &format!("Up to date at v{db_version}"));
    } else {
        let pending = dbd_core::snapshot::pending_migrations(db_version, project_dir);
        output::info(verbosity, &format!("DB: v{db_version}, Latest: v{latest_version}"));
        output::info(verbosity, &format!("Pending: {}", pending.iter().map(|m| format!("v{}", m.to_version)).collect::<Vec<_>>().join(", ")));
        for m in &pending {
            if !m.altered.is_empty() {
                output::detail(verbosity, &format!("  v{}: altered [{}]", m.to_version, m.altered.join(", ")));
            }
            if !m.dropped.is_empty() {
                output::detail(verbosity, &format!("  v{}: dropped [{}]", m.to_version, m.dropped.join(", ")));
            }
            if !m.added.is_empty() {
                output::detail(verbosity, &format!("  v{}: added [{}]", m.to_version, m.added.join(", ")));
            }
        }
    }

    adapter.disconnect().await.ok();
    Ok(())
}

async fn cmd_migrate_apply(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    to: Option<u32>,
    dry_run: bool,
    verbosity: Verbosity,
) -> Result<()> {
    let mut adapter = get_adapter(config, database_url).await?;
    adapter.ensure_meta_table().await.context("Failed to ensure meta table")?;
    adapter.ensure_migrations_table().await.context("Failed to ensure migrations table")?;
    let db_version = adapter.get_db_version().await.context("Failed to get DB version")?;

    let mut pending = dbd_core::snapshot::pending_migrations(db_version, project_dir);
    if let Some(limit) = to {
        pending.retain(|m| m.to_version <= limit);
    }

    if pending.is_empty() {
        output::info(verbosity, &format!("No pending migrations (at v{db_version})"));
        adapter.disconnect().await.ok();
        return Ok(());
    }

    for m in &pending {
        output::info(verbosity, &format!("-- Migration v{} -> v{}", m.from_version, m.to_version));

        // Read and execute migration SQL files
        for table_name in &m.altered {
            let parts: Vec<&str> = table_name.split('.').collect();
            let sql_file = if parts.len() > 1 {
                m.migration_dir.join(parts[0]).join(format!("{}.sql", parts[1]))
            } else {
                m.migration_dir.join(format!("{}.sql", table_name))
            };
            if sql_file.exists() {
                let sql = std::fs::read_to_string(&sql_file)
                    .context(format!("Failed to read migration {}", sql_file.display()))?;
                output::info(verbosity, &format!("-- [ALTER] {table_name}"));
                if dry_run {
                    output::always(&sql);
                } else {
                    adapter.execute_script(&sql).await
                        .context(format!("Failed to apply migration for {table_name}"))?;
                }
            }
        }

        for table_name in &m.dropped {
            let parts: Vec<&str> = table_name.split('.').collect();
            let sql_file = if parts.len() > 1 {
                m.migration_dir.join(parts[0]).join(format!("{}.sql", parts[1]))
            } else {
                m.migration_dir.join(format!("{}.sql", table_name))
            };
            if sql_file.exists() {
                let sql = std::fs::read_to_string(&sql_file)
                    .context(format!("Failed to read drop migration {}", sql_file.display()))?;
                output::info(verbosity, &format!("-- [DROP] {table_name}"));
                if dry_run {
                    output::always(&sql);
                } else {
                    adapter.execute_script(&sql).await
                        .context(format!("Failed to drop {table_name}"))?;
                }
            }
        }

        if !dry_run {
            let desc = format!("migration v{} to v{}", m.from_version, m.to_version);
            adapter.apply_migration(m.to_version, "", &desc, &m.checksum).await
                .context("Failed to record migration")?;
        }
    }

    let final_version = pending.last().map(|m| m.to_version).unwrap_or(db_version);
    if !dry_run {
        let design = Design::from_config_with_dir(config, env, Some(project_dir))
            .context("Failed to load design")?;
        let project_name = &design.config().project.name;
        adapter.set_project_meta(env, final_version).await
            .context("Failed to update _dbd_meta")?;
        output::info(verbosity, &format!("Migrations applied. DB now at v{final_version}"));
    }

    adapter.disconnect().await.ok();
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add crates/dbd-cli/src/commands.rs
git commit -m "feat: wire dbd migrate --status and --apply commands"
```

---

### Task 14: Update apply() to use execution plan

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Refactor apply() to use build_execution_plan**

Replace the existing `apply()` method in `Design` (lines ~239-337) with:

```rust
    pub async fn apply(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
    ) -> Result<()> {
        let valid_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .collect();

        if dry_run {
            return Ok(());
        }

        if adapter.prefers_batch_apply() {
            let owned: Vec<Entity> = valid_entities.into_iter().cloned().collect();
            adapter.apply_entities(&owned).await?;
            return Ok(());
        }

        // Get version state
        adapter.ensure_meta_table().await?;
        let db_version = adapter.get_db_version().await?;
        let latest_version = self.config.project.version.unwrap_or(0);
        let pending = snapshot::pending_migrations(db_version, &self.project_dir);

        let plan = build_execution_plan(
            &valid_entities.iter().map(|e| (*e).clone()).collect::<Vec<_>>(),
            db_version,
            latest_version,
            &pending,
        );

        if !pending.is_empty() {
            adapter.ensure_migrations_table().await?;
        }

        // Execute the plan
        for step in &plan.steps {
            match step {
                ExecutionStep::CreateEntity(entity_name) | ExecutionStep::ApplyEntity(entity_name) => {
                    if let Some(entity) = valid_entities.iter().find(|e| &e.name == entity_name) {
                        adapter.apply_entity(entity).await?;
                    }
                }
                ExecutionStep::MigrateEntity { migration_sql_path, .. } => {
                    if migration_sql_path.exists() {
                        let sql = std::fs::read_to_string(migration_sql_path)?;
                        adapter.execute_script(&sql).await?;
                    }
                }
                ExecutionStep::DropEntity { drop_sql_path, .. } => {
                    if drop_sql_path.exists() {
                        let sql = std::fs::read_to_string(drop_sql_path)?;
                        adapter.execute_script(&sql).await?;
                    }
                }
                ExecutionStep::RecordMigration { version, checksum } => {
                    let desc = format!("migration to v{version}");
                    adapter.apply_migration(*version, "", &desc, checksum).await?;
                }
                ExecutionStep::SetVersion(version) => {
                    adapter.set_project_meta(&self.env, *version).await?;
                }
            }
        }

        Ok(())
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles

- [ ] **Step 3: Run all tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat: refactor apply() to use build_execution_plan"
```

---

### Task 15: Final verification — zero errors

**Files:** All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass, 0 failures

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Verify test count increased**

Run: `cargo test 2>&1 | grep "test result"`
Expected: significantly more than 195 tests (should be ~240+)

- [ ] **Step 4: Commit any clippy fixes if needed**

```bash
git add -A
git commit -m "fix: clippy and final cleanup"
```

---

### Task 16: Update BACKLOG.md

**Files:**
- Modify: `docs/BACKLOG.md`

- [ ] **Step 1: Update backlog to reflect completed work**

Move `dbd snapshot (create)`, schema diff, and `dbd migrate --apply` from P0 to the "Working commands" table. Update test count.

- [ ] **Step 2: Commit**

```bash
git add docs/BACKLOG.md
git commit -m "docs: update backlog — snapshot, diff, migrate complete"
```
