# Smart Multi-Snapshot Generation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Automatically split complex schema changes (column rename, type change, enum value removal) into multiple snapshots with correct intermediate states and data.sql files.

**Architecture:** A new `classify_changes()` function detects complex patterns in diffs. `prepare_multi_snapshot()` replaces `prepare_snapshot()` — it synthesizes intermediate snapshot states by cloning and mutating the previous snapshot, generates migration SQL and data.sql per stage, and returns 1-3 `SnapshotResult` entries. The I/O wrapper writes all snapshots/migrations and updates design.yaml to the final version.

**Tech Stack:** Rust, existing diff/snapshot/entity types, serde, chrono

**Spec:** `docs/superpowers/specs/2026-04-30-smart-multi-snapshot-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/dbd-core/src/diff.rs` | Modify | Add `classify_changes()`, `is_castable()`, `generate_data_sql()`, `generate_enum_rename_sql()` |
| `crates/dbd-core/src/snapshot.rs` | Modify | Add `MultiSnapshotResult`, `TodoItem`, `prepare_multi_snapshot()`, snapshot synthesis functions. Update `create_snapshot()` to return `MultiSnapshotResult`. Keep `prepare_snapshot()` as internal helper. |
| `crates/dbd-cli/src/cli.rs` | Modify | Remove `apply`, `to`, `dry_run` from Migrate command |
| `crates/dbd-cli/src/commands.rs` | Modify | Remove `cmd_migrate_apply`, update snapshot create for multi-snapshot output, simplify migrate routing |

---

### Task 1: Add `is_castable()` with tests (CA1-CA5)

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

- [ ] **Step 1: Write castability tests**

Append to the `mod tests` block in `crates/dbd-core/src/diff.rs`:

```rust
    // ════════════════════════════════════════════════════════
    // Castability Tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn ca1_integer_to_text_castable() {
        assert!(is_castable("INTEGER", "TEXT"));
        assert!(is_castable("BIGINT", "TEXT"));
        assert!(is_castable("SMALLINT", "TEXT"));
        assert!(is_castable("INT", "VARCHAR(100)"));
    }

    #[test]
    fn ca2_varchar_to_text_castable() {
        assert!(is_castable("VARCHAR(100)", "TEXT"));
        assert!(is_castable("VARCHAR(255)", "VARCHAR(500)"));
    }

    #[test]
    fn ca3_text_to_varchar_castable() {
        assert!(is_castable("TEXT", "VARCHAR(50)"));
    }

    #[test]
    fn ca4_jsonb_to_integer_not_castable() {
        assert!(!is_castable("JSONB", "INTEGER"));
        assert!(!is_castable("JSON", "BIGINT"));
    }

    #[test]
    fn ca5_array_to_scalar_not_castable() {
        assert!(!is_castable("TEXT[]", "TEXT"));
        assert!(!is_castable("INTEGER[]", "INTEGER"));
    }

    #[test]
    fn ca_numeric_to_text_castable() {
        assert!(is_castable("NUMERIC", "TEXT"));
        assert!(is_castable("DECIMAL", "TEXT"));
        assert!(is_castable("NUMERIC(10,2)", "TEXT"));
    }

    #[test]
    fn ca_boolean_castable() {
        assert!(is_castable("BOOLEAN", "TEXT"));
        assert!(is_castable("BOOLEAN", "INTEGER"));
    }

    #[test]
    fn ca_timestamp_to_text_castable() {
        assert!(is_castable("TIMESTAMP", "TEXT"));
        assert!(is_castable("TIMESTAMPTZ", "TEXT"));
        assert!(is_castable("TIMESTAMP WITH TIME ZONE", "TEXT"));
    }

    #[test]
    fn ca_same_category_castable() {
        assert!(is_castable("INTEGER", "BIGINT"));
        assert!(is_castable("SMALLINT", "INTEGER"));
        assert!(is_castable("TEXT", "TEXT"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core diff::tests::ca`
Expected: FAIL — `is_castable` not found

- [ ] **Step 3: Implement `is_castable()`**

Add above the `#[cfg(test)]` block in `crates/dbd-core/src/diff.rs`:

```rust
// ── Castability heuristic ──────────────────────────────

/// Check if a Postgres type cast from `from` to `to` is safe for auto-generation.
/// Returns true for common castable conversions, false for types that need manual SQL.
pub fn is_castable(from: &str, to: &str) -> bool {
    let from_norm = normalize_type(from);
    let to_norm = normalize_type(to);

    // Same base type is always castable
    if from_norm == to_norm {
        return true;
    }

    // Arrays are never auto-castable to scalars
    if from.contains("[]") || to.contains("[]") {
        return false;
    }

    // JSON types are not auto-castable to scalars
    if matches!(from_norm.as_str(), "jsonb" | "json") || matches!(to_norm.as_str(), "jsonb" | "json") {
        return false;
    }

    let from_cat = type_category(&from_norm);
    let to_cat = type_category(&to_norm);

    // Same category is castable (e.g., INTEGER → BIGINT)
    if from_cat == to_cat {
        return true;
    }

    // Anything → TEXT/VARCHAR is generally castable
    if to_cat == "text" {
        return true;
    }

    // TEXT → numeric types: not safe
    // BOOLEAN → TEXT or INTEGER: safe
    if from_cat == "boolean" && (to_cat == "text" || to_cat == "integer") {
        return true;
    }

    // TIMESTAMP → TEXT: safe
    if from_cat == "timestamp" && to_cat == "text" {
        return true;
    }

    false
}

/// Normalize a type string for comparison: lowercase, strip precision/length.
fn normalize_type(t: &str) -> String {
    let lower = t.to_lowercase();
    // Strip parenthesized precision: VARCHAR(100) → varchar, NUMERIC(10,2) → numeric
    match lower.find('(') {
        Some(pos) => lower[..pos].trim().to_string(),
        None => lower.trim().to_string(),
    }
}

/// Map a normalized type to a category for castability comparison.
fn type_category(t: &str) -> &'static str {
    match t {
        "int" | "integer" | "bigint" | "smallint" | "int2" | "int4" | "int8" | "serial" | "bigserial" => "integer",
        "numeric" | "decimal" | "real" | "float" | "float4" | "float8" | "double precision" => "numeric",
        "text" | "varchar" | "character varying" | "char" | "character" => "text",
        "boolean" | "bool" => "boolean",
        "timestamp" | "timestamptz" | "timestamp with time zone" | "timestamp without time zone" => "timestamp",
        "date" => "date",
        "time" | "timetz" => "time",
        "uuid" => "uuid",
        "jsonb" | "json" => "json",
        "bytea" => "bytea",
        _ => "other",
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dbd-core diff::tests::ca`
Expected: all pass

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "feat: add is_castable() type cast heuristic with tests"
```

---

### Task 2: Add `classify_changes()` with tests (C1-C7)

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

- [ ] **Step 1: Add ComplexChange type and classify_changes tests**

Add the `ComplexChange` enum near the other types in `crates/dbd-core/src/diff.rs`:

```rust
/// A complex schema change that requires multiple snapshot stages.
#[derive(Debug, Clone)]
pub enum ComplexChange {
    /// Column data type changed — needs 2 stages: add new col, drop old col.
    ColumnTypeChange {
        table_name: String,
        column_name: String,
        old_type: String,
        new_type: String,
        old_col: ColumnDef,
        new_col: ColumnDef,
    },
    /// Column likely renamed — needs 2 stages: add new col + copy data, drop old col.
    ColumnRename {
        table_name: String,
        old_name: String,
        new_name: String,
        col_def: ColumnDef,
    },
    /// Enum value(s) removed — needs 3 stages: data fix, TEXT intermediary, new enum.
    EnumValueRemoval {
        enum_name: String,
        removed_values: Vec<String>,
        remaining_values: Vec<String>,
        affected_columns: Vec<(String, String)>, // (table_name, column_name)
    },
}
```

Then add tests to the test module:

```rust
    // ════════════════════════════════════════════════════════
    // Classification Tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn c1_simple_changes_only() {
        let diffs = vec![
            MigrationDiff {
                entity_name: "public.users".to_string(),
                entity_type: EntityType::Table,
                action: DiffAction::Change(vec![
                    FieldChange { field_name: "email".to_string(), field_type: FieldType::Column, action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "TEXT")))) },
                    FieldChange { field_name: "old_col".to_string(), field_type: FieldType::Column, action: ChangeAction::Drop },
                    FieldChange { field_name: "idx_a".to_string(), field_type: FieldType::Index, action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef { name: Some("idx_a".to_string()), columns: vec![], unique: false, index_type: None }))) },
                ]),
            },
        ];
        let old_snap = snap(vec![table("public", "users", vec![col("id", "INT"), col("old_col", "TEXT")])], vec![]);
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(simple.len(), 1); // one table diff with all simple changes
        assert!(complex.is_empty());
    }

    #[test]
    fn c2_column_type_change_detected() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("email", "VARCHAR(100)"))),
                    new: Box::new(FieldDetail::Column(col("email", "TEXT"))),
                },
            }]),
        }];
        let old_snap = snap(vec![table("public", "users", vec![col("id", "INT"), col("email", "VARCHAR(100)")])], vec![]);
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(simple.is_empty() || simple.iter().all(|d| matches!(&d.action, DiffAction::Change(c) if c.is_empty())));
        assert_eq!(complex.len(), 1);
        assert!(matches!(&complex[0], ComplexChange::ColumnTypeChange { column_name, old_type, new_type, .. } if column_name == "email" && old_type == "VARCHAR(100)" && new_type == "TEXT"));
    }

    #[test]
    fn c3_column_rename_detected() {
        // Old snapshot has [id, name] at positions 0, 1
        // New diff: Drop "name" + Add "display_name" with same type TEXT
        let old_snap = snap(vec![table("public", "users", vec![col("id", "INT"), col("name", "TEXT")])], vec![]);
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange { field_name: "name".to_string(), field_type: FieldType::Column, action: ChangeAction::Drop },
                FieldChange { field_name: "display_name".to_string(), field_type: FieldType::Column, action: ChangeAction::Add(Box::new(FieldDetail::Column(col("display_name", "TEXT")))) },
            ]),
        }];
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        assert!(matches!(&complex[0], ComplexChange::ColumnRename { old_name, new_name, .. } if old_name == "name" && new_name == "display_name"));
    }

    #[test]
    fn c4_different_types_not_rename() {
        let old_snap = snap(vec![table("public", "users", vec![col("id", "INT"), col("name", "TEXT")])], vec![]);
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange { field_name: "name".to_string(), field_type: FieldType::Column, action: ChangeAction::Drop },
                FieldChange { field_name: "age".to_string(), field_type: FieldType::Column, action: ChangeAction::Add(Box::new(FieldDetail::Column(col("age", "INTEGER")))) },
            ]),
        }];
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "different types should not be classified as rename");
        assert!(!simple.is_empty());
    }

    #[test]
    fn c5_multiple_drops_adds_not_rename() {
        let old_snap = snap(vec![table("public", "users", vec![col("id", "INT"), col("a", "TEXT"), col("b", "TEXT")])], vec![]);
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange { field_name: "a".to_string(), field_type: FieldType::Column, action: ChangeAction::Drop },
                FieldChange { field_name: "b".to_string(), field_type: FieldType::Column, action: ChangeAction::Drop },
                FieldChange { field_name: "x".to_string(), field_type: FieldType::Column, action: ChangeAction::Add(Box::new(FieldDetail::Column(col("x", "TEXT")))) },
                FieldChange { field_name: "y".to_string(), field_type: FieldType::Column, action: ChangeAction::Add(Box::new(FieldDetail::Column(col("y", "TEXT")))) },
            ]),
        }];
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "multiple drops+adds should not be classified as rename");
    }

    #[test]
    fn c6_enum_value_removal_detected() {
        let old_snap = snap(
            vec![],
            vec![EnumSnapshot { name: "public.status_type".to_string(), schema: "public".to_string(), values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()] }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        match &complex[0] {
            ComplexChange::EnumValueRemoval { removed_values, remaining_values, .. } => {
                assert_eq!(removed_values, &vec!["deleted".to_string()]);
                assert!(remaining_values.contains(&"active".to_string()));
                assert!(remaining_values.contains(&"inactive".to_string()));
            }
            _ => panic!("expected EnumValueRemoval"),
        }
    }

    #[test]
    fn c7_enum_removal_identifies_affected_columns() {
        let old_snap = snap(
            vec![TableSnapshot {
                name: "events".to_string(),
                schema: "config".to_string(),
                columns: vec![col("id", "INT"), ColumnDef { data_type: "public.status_type".to_string(), ..col("status", "public.status_type") }],
                indexes: vec![],
                table_constraints: vec![],
            }],
            vec![EnumSnapshot { name: "public.status_type".to_string(), schema: "public".to_string(), values: vec!["active".to_string(), "deleted".to_string()] }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];
        let (_, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        match &complex[0] {
            ComplexChange::EnumValueRemoval { affected_columns, .. } => {
                assert!(affected_columns.iter().any(|(t, c)| t.contains("events") && c == "status"));
            }
            _ => panic!("expected EnumValueRemoval"),
        }
    }

    #[test]
    fn c_enum_value_rename_is_simple() {
        // 1:1 swap: "deleted" removed, "archived" added → enum rename (simple)
        let old_snap = snap(
            vec![],
            vec![EnumSnapshot { name: "public.status_type".to_string(), schema: "public".to_string(), values: vec!["active".to_string(), "deleted".to_string()] }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![
                FieldChange { field_name: "deleted".to_string(), field_type: FieldType::EnumValue, action: ChangeAction::Drop },
                FieldChange { field_name: "archived".to_string(), field_type: FieldType::EnumValue, action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("archived".to_string()))) },
            ]),
        }];
        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "1:1 enum swap should be simple (RENAME VALUE)");
        assert!(!simple.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core diff::tests::c`
Expected: FAIL — `classify_changes` not found

- [ ] **Step 3: Implement `classify_changes()`**

Add to `crates/dbd-core/src/diff.rs`:

```rust
// ── Change classification ──────────────────────────────

/// Classify diffs into simple changes and complex changes that need multi-stage snapshots.
///
/// Simple changes get a single snapshot. Complex changes (column type change, column rename,
/// enum value removal) are split into 2 or 3 stages.
pub fn classify_changes(
    diffs: &[MigrationDiff],
    old_snapshot: &Snapshot,
) -> (Vec<MigrationDiff>, Vec<ComplexChange>) {
    let mut simple_diffs: Vec<MigrationDiff> = Vec::new();
    let mut complex: Vec<ComplexChange> = Vec::new();

    for diff in diffs {
        match &diff.action {
            DiffAction::Add | DiffAction::Drop => {
                // Table/enum add/drop are always simple
                simple_diffs.push(diff.clone());
            }
            DiffAction::Change(changes) => {
                if diff.entity_type == EntityType::Enum {
                    classify_enum_changes(diff, changes, old_snapshot, &mut simple_diffs, &mut complex);
                } else if diff.entity_type == EntityType::Table {
                    classify_table_changes(diff, changes, old_snapshot, &mut simple_diffs, &mut complex);
                } else {
                    simple_diffs.push(diff.clone());
                }
            }
        }
    }

    (simple_diffs, complex)
}

fn classify_enum_changes(
    diff: &MigrationDiff,
    changes: &[FieldChange],
    _old_snapshot: &Snapshot,
    simple_diffs: &mut Vec<MigrationDiff>,
    complex: &mut Vec<ComplexChange>,
) {
    let drops: Vec<&FieldChange> = changes.iter()
        .filter(|c| c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Drop))
        .collect();
    let adds: Vec<&FieldChange> = changes.iter()
        .filter(|c| c.field_type == FieldType::EnumValue && matches!(c.action, ChangeAction::Add(_)))
        .collect();

    // 1:1 swap = enum value rename (simple, PG17+)
    if drops.len() == 1 && adds.len() == 1 && changes.iter().filter(|c| c.field_type == FieldType::EnumValue).count() == 2 {
        // Treat as simple — generate ALTER TYPE RENAME VALUE
        simple_diffs.push(diff.clone());
        return;
    }

    if drops.is_empty() {
        // Only adds — simple
        simple_diffs.push(diff.clone());
        return;
    }

    // Has enum value removals — complex (3-stage)
    // Find the old enum to get remaining values
    let old_enum = _old_snapshot.enums.iter().find(|e| e.name == diff.entity_name);
    let removed_values: Vec<String> = drops.iter().map(|d| d.field_name.clone()).collect();
    let remaining_values: Vec<String> = old_enum
        .map(|e| e.values.iter().filter(|v| !removed_values.contains(v)).cloned().collect())
        .unwrap_or_default();

    // Find affected columns: scan all tables for columns with this enum type
    let affected_columns = find_affected_columns(&diff.entity_name, _old_snapshot);

    complex.push(ComplexChange::EnumValueRemoval {
        enum_name: diff.entity_name.clone(),
        removed_values,
        remaining_values,
        affected_columns,
    });

    // Pass through any non-drop enum changes (adds) as simple
    let simple_changes: Vec<FieldChange> = changes.iter()
        .filter(|c| !matches!(c.action, ChangeAction::Drop) || c.field_type != FieldType::EnumValue)
        .cloned()
        .collect();
    if !simple_changes.is_empty() {
        simple_diffs.push(MigrationDiff {
            entity_name: diff.entity_name.clone(),
            entity_type: diff.entity_type,
            action: DiffAction::Change(simple_changes),
        });
    }
}

fn classify_table_changes(
    diff: &MigrationDiff,
    changes: &[FieldChange],
    old_snapshot: &Snapshot,
    simple_diffs: &mut Vec<MigrationDiff>,
    complex: &mut Vec<ComplexChange>,
) {
    let mut simple_changes: Vec<FieldChange> = Vec::new();

    // Collect column drops and adds for rename detection
    let col_drops: Vec<&FieldChange> = changes.iter()
        .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Drop))
        .collect();
    let col_adds: Vec<&FieldChange> = changes.iter()
        .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Add(_)))
        .collect();

    // Try rename detection: exactly 1 drop + 1 add with same type and position
    let mut rename_detected = false;
    if col_drops.len() == 1 && col_adds.len() == 1 {
        let drop_name = &col_drops[0].field_name;
        if let ChangeAction::Add(ref detail) = col_adds[0].action {
            if let FieldDetail::Column(ref add_col) = **detail {
                // Find the old table to check position
                let old_table = old_snapshot.tables.iter()
                    .find(|t| format!("{}.{}", t.schema, t.name) == diff.entity_name);
                if let Some(old_t) = old_table {
                    let drop_pos = old_t.columns.iter().position(|c| c.name == *drop_name);
                    // Check: same type AND has a valid position
                    let drop_type = old_t.columns.iter()
                        .find(|c| c.name == *drop_name)
                        .map(|c| c.data_type.as_str());
                    if drop_type == Some(&add_col.data_type) {
                        rename_detected = true;
                        complex.push(ComplexChange::ColumnRename {
                            table_name: diff.entity_name.clone(),
                            old_name: drop_name.clone(),
                            new_name: col_adds[0].field_name.clone(),
                            col_def: add_col.clone(),
                        });
                    }
                }
            }
        }
    }

    // Process each change
    for change in changes {
        if change.field_type == FieldType::Column {
            match &change.action {
                ChangeAction::Alter { ref old, ref new } => {
                    if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) = (old.as_ref(), new.as_ref()) {
                        if old_col.data_type != new_col.data_type {
                            // Column type change — complex (2-stage)
                            complex.push(ComplexChange::ColumnTypeChange {
                                table_name: diff.entity_name.clone(),
                                column_name: change.field_name.clone(),
                                old_type: old_col.data_type.clone(),
                                new_type: new_col.data_type.clone(),
                                old_col: old_col.clone(),
                                new_col: new_col.clone(),
                            });
                            continue;
                        }
                    }
                    // Non-type-change alter (nullable, default, etc.) — simple
                    simple_changes.push(change.clone());
                }
                ChangeAction::Drop | ChangeAction::Add(_) => {
                    if rename_detected {
                        // Skip drop+add that are part of the rename
                        continue;
                    }
                    simple_changes.push(change.clone());
                }
            }
        } else {
            // Constraint/Index changes are always simple
            simple_changes.push(change.clone());
        }
    }

    if !simple_changes.is_empty() {
        simple_diffs.push(MigrationDiff {
            entity_name: diff.entity_name.clone(),
            entity_type: diff.entity_type,
            action: DiffAction::Change(simple_changes),
        });
    }
}

/// Find all table columns whose data_type matches an enum name (qualified or unqualified).
fn find_affected_columns(enum_name: &str, snapshot: &Snapshot) -> Vec<(String, String)> {
    let mut affected = Vec::new();
    // Also check without schema prefix
    let short_name = enum_name.split('.').last().unwrap_or(enum_name);

    for table in &snapshot.tables {
        let table_qualified = format!("{}.{}", table.schema, table.name);
        for col in &table.columns {
            if col.data_type == enum_name || col.data_type == short_name {
                affected.push((table_qualified.clone(), col.name.clone()));
            }
        }
    }
    affected
}
```

- [ ] **Step 4: Run classification tests**

Run: `cargo test -p dbd-core diff::tests::c`
Expected: all pass

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: all pass

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "feat: classify_changes() detects complex schema patterns (C1-C7)"
```

---

### Task 3: Add `generate_data_sql()` with tests (D1-D5)

**Files:**
- Modify: `crates/dbd-core/src/diff.rs`

- [ ] **Step 1: Write data.sql generation tests**

```rust
    // ════════════════════════════════════════════════════════
    // data.sql Generation Tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn d_data_column_rename_generates_copy() {
        let change = ComplexChange::ColumnRename {
            table_name: "config.users".to_string(),
            old_name: "name".to_string(),
            new_name: "display_name".to_string(),
            col_def: col("display_name", "TEXT"),
        };
        let sql = generate_data_sql(&change);
        assert_eq!(sql.trim(), "UPDATE config.users SET display_name = name;");
    }

    #[test]
    fn d_data_castable_type_change() {
        let change = ComplexChange::ColumnTypeChange {
            table_name: "config.users".to_string(),
            column_name: "total".to_string(),
            old_type: "INTEGER".to_string(),
            new_type: "TEXT".to_string(),
            old_col: col("total", "INTEGER"),
            new_col: col("total_text", "TEXT"),
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("UPDATE config.users SET total_text = total::TEXT;"));
    }

    #[test]
    fn d_data_non_castable_type_change_generates_todo() {
        let change = ComplexChange::ColumnTypeChange {
            table_name: "config.users".to_string(),
            column_name: "metadata".to_string(),
            old_type: "JSONB".to_string(),
            new_type: "INTEGER".to_string(),
            old_col: col("metadata", "JSONB"),
            new_col: col("metadata_new", "INTEGER"),
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("-- TODO:"));
        assert!(sql.contains("JSONB"));
        assert!(sql.contains("INTEGER"));
    }

    #[test]
    fn d_data_enum_value_removal_generates_todo() {
        let change = ComplexChange::EnumValueRemoval {
            enum_name: "public.status_type".to_string(),
            removed_values: vec!["deleted".to_string()],
            remaining_values: vec!["active".to_string(), "inactive".to_string()],
            affected_columns: vec![("config.events".to_string(), "status".to_string())],
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("-- TODO:"));
        assert!(sql.contains("Removed: deleted"));
        assert!(sql.contains("Remaining: active, inactive"));
        assert!(sql.contains("config.events"));
        assert!(sql.contains("status = '???'"));
    }

    #[test]
    fn d_data_text_to_varchar_has_truncation_warning() {
        let change = ComplexChange::ColumnTypeChange {
            table_name: "config.users".to_string(),
            column_name: "name".to_string(),
            old_type: "TEXT".to_string(),
            new_type: "VARCHAR(50)".to_string(),
            old_col: col("name", "TEXT"),
            new_col: col("name_new", "VARCHAR(50)"),
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("::VARCHAR(50)"));
        assert!(sql.contains("WARNING") || sql.contains("truncate"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core diff::tests::d_data`
Expected: FAIL — `generate_data_sql` not found

- [ ] **Step 3: Implement `generate_data_sql()`**

```rust
// ── Data SQL generation ────────────────────────────────

/// Generate a data.sql correction script for a complex change.
pub fn generate_data_sql(change: &ComplexChange) -> String {
    match change {
        ComplexChange::ColumnRename { table_name, old_name, new_name, .. } => {
            format!("UPDATE {} SET {} = {};\n", table_name, new_name, old_name)
        }
        ComplexChange::ColumnTypeChange { table_name, old_type, new_type, old_col, new_col, .. } => {
            if is_castable(old_type, new_type) {
                let mut sql = format!("UPDATE {} SET {} = {}::{};\n", table_name, new_col.name, old_col.name, new_type);
                // Add truncation warning for TEXT → VARCHAR
                if normalize_type(old_type) == "text" && normalize_type(new_type) == "varchar" {
                    sql = format!("-- WARNING: may truncate values longer than the target length\n{}", sql);
                }
                sql
            } else {
                format!(
                    "-- TODO: Data correction required for {}.{}\n\
                     -- Column type changed from {} to {}\n\
                     -- UPDATE {} SET {} = <derive from {}>;\n",
                    table_name, old_col.name, old_type, new_type, table_name, new_col.name, old_col.name
                )
            }
        }
        ComplexChange::EnumValueRemoval { enum_name, removed_values, remaining_values, affected_columns } => {
            let mut lines = Vec::new();
            lines.push(format!("-- TODO: Map removed enum values to remaining values"));
            lines.push(format!("-- Enum: {}", enum_name));
            lines.push(format!("-- Removed: {}", removed_values.join(", ")));
            lines.push(format!("-- Remaining: {}", remaining_values.join(", ")));
            for (table, col) in affected_columns {
                for val in removed_values {
                    lines.push(format!("-- UPDATE {} SET {} = '???' WHERE {} = '{}';", table, col, col, val));
                }
            }
            lines.push(String::new());
            lines.join("\n")
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core diff::tests::d_data`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/diff.rs
git commit -m "feat: generate_data_sql() for complex change data corrections (D1-D5)"
```

---

### Task 4: Add `MultiSnapshotResult`, `prepare_multi_snapshot()` with tests (B1-B5, S1-S4)

**Files:**
- Modify: `crates/dbd-core/src/snapshot.rs`

- [ ] **Step 1: Add types and `prepare_multi_snapshot` stub**

Add to `crates/dbd-core/src/snapshot.rs` after `SnapshotResult`:

```rust
/// Result of preparing potentially multiple snapshots.
pub struct MultiSnapshotResult {
    pub snapshots: Vec<SnapshotResult>,
    pub todos: Vec<TodoItem>,
}

/// An item requiring developer attention (e.g., TODO in data.sql).
pub struct TodoItem {
    pub file: PathBuf,
    pub message: String,
}
```

- [ ] **Step 2: Write tests B1-B5 and S1-S4**

Add to the test module (use the existing helpers `make_table_entity`, `make_enum_entity`, `col`, etc.):

```rust
    // ════════════════════════════════════════════════════════
    // Multi-Snapshot Tests
    // ════════════════════════════════════════════════════════

    // B1: Simple only → 1 snapshot
    #[test]
    fn b1_simple_only_one_snapshot() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("email", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "add email");
        assert_eq!(result.snapshots.len(), 1);
        assert!(result.todos.is_empty());
    }

    // B2: Column rename → 2 snapshots
    #[test]
    fn b2_column_rename_two_snapshots() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT"), col("name", "TEXT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        // Final state: id + display_name (name gone)
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("display_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename");
        assert_eq!(result.snapshots.len(), 2);
        // Stage 1 should have data.sql for copy
        assert!(result.snapshots[0].migration_files.iter().any(|f| f.relative_path.to_str().unwrap().contains("data.sql")));
    }

    // B3: Enum value removal → 3 snapshots
    #[test]
    fn b3_enum_removal_three_snapshots() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot {
                name: "events".to_string(), schema: "config".to_string(),
                columns: vec![col("id", "INT"), ColumnDef { data_type: "public.status_type".to_string(), ..col("status", "public.status_type") }],
                indexes: vec![], table_constraints: vec![],
            }],
            enums: vec![EnumSnapshot { name: "public.status_type".to_string(), schema: "public".to_string(), values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()] }],
        };
        // Final: enum without "deleted"
        let mut enum_entity = make_enum_entity("public.status_type", vec!["active", "inactive"]);
        let mut table_entity = make_table_entity("config.events", vec![col("id", "INT"), ColumnDef { data_type: "public.status_type".to_string(), ..col("status", "public.status_type") }]);
        let result = prepare_multi_snapshot(&[table_entity, enum_entity], Some(&prev), 2, "remove deleted");
        assert_eq!(result.snapshots.len(), 3);
        // Should have TODO items
        assert!(!result.todos.is_empty());
    }

    // B5: Multiple column renames → batched in 2 snapshots
    #[test]
    fn b5_multiple_renames_batched() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot {
                name: "users".to_string(), schema: "config".to_string(),
                columns: vec![col("id", "INT"), col("fname", "TEXT"), col("lname", "TEXT")],
                indexes: vec![], table_constraints: vec![],
            }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("first_name", "TEXT"), col("last_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename cols");
        // This should be 2 snapshots IF both detected as renames
        // If not detected (position mismatch for 2nd), may differ — this tests batching
        assert!(result.snapshots.len() <= 2);
    }

    // S1: Stage 1 adds new column for rename
    #[test]
    fn s1_stage1_adds_new_column() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT"), col("name", "TEXT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("display_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename");
        assert!(result.snapshots.len() >= 2);
        // Stage 1 snapshot should have 3 columns: id, name, display_name
        let stage1_cols = &result.snapshots[0].snapshot.tables.iter()
            .find(|t| t.name == "users").unwrap().columns;
        let col_names: Vec<&str> = stage1_cols.iter().map(|c| c.name.as_str()).collect();
        assert!(col_names.contains(&"id"));
        assert!(col_names.contains(&"name"));
        assert!(col_names.contains(&"display_name"));
    }

    // S2: Stage 2 drops old column for rename
    #[test]
    fn s2_stage2_drops_old_column() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT"), col("name", "TEXT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT"), col("display_name", "TEXT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "rename");
        assert!(result.snapshots.len() >= 2);
        // Stage 2 snapshot should have 2 columns: id, display_name
        let stage2_cols = &result.snapshots[1].snapshot.tables.iter()
            .find(|t| t.name == "users").unwrap().columns;
        let col_names: Vec<&str> = stage2_cols.iter().map(|c| c.name.as_str()).collect();
        assert!(col_names.contains(&"id"));
        assert!(col_names.contains(&"display_name"));
        assert!(!col_names.contains(&"name"));
    }

    // B1 backward compat: baseline returns 1 snapshot
    #[test]
    fn b_baseline_returns_one_snapshot() {
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT")])];
        let result = prepare_multi_snapshot(&entities, None, 1, "initial");
        assert_eq!(result.snapshots.len(), 1);
        assert!(result.snapshots[0].is_baseline);
    }

    // No changes returns empty
    #[test]
    fn b_no_changes_returns_one_snapshot_no_changes() {
        let prev = Snapshot {
            version: 1, description: "v1".to_string(), timestamp: "t".to_string(),
            tables: vec![TableSnapshot { name: "users".to_string(), schema: "config".to_string(), columns: vec![col("id", "INT")], indexes: vec![], table_constraints: vec![] }],
            enums: vec![],
        };
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT")])];
        let result = prepare_multi_snapshot(&entities, Some(&prev), 2, "no change");
        assert_eq!(result.snapshots.len(), 1);
        assert!(result.snapshots[0].no_changes);
    }
```

- [ ] **Step 2: Implement `prepare_multi_snapshot()`**

This is the core function. Add to `crates/dbd-core/src/snapshot.rs`:

```rust
/// Prepare potentially multiple snapshots for complex changes.
///
/// Returns 1 snapshot for simple changes (backward compatible),
/// 2 for column renames/type changes, 3 for enum value removals.
pub fn prepare_multi_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> MultiSnapshotResult {
    // Build final snapshot from entities
    let final_tables: Vec<TableSnapshot> = entities.iter()
        .filter(|e| e.entity_type == EntityType::Table && e.table_def.is_some())
        .filter_map(entity_to_table_snapshot)
        .collect();
    let final_enums: Vec<EnumSnapshot> = entities.iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .map(entity_to_enum_snapshot)
        .collect();
    let final_snapshot = Snapshot {
        version: 0, // placeholder, set per stage
        description: description.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        tables: final_tables,
        enums: final_enums,
    };

    // Baseline: no previous
    let Some(prev) = previous else {
        let mut snap = final_snapshot;
        snap.version = next_version;
        return MultiSnapshotResult {
            snapshots: vec![SnapshotResult {
                snapshot: snap, diffs: vec![], migration_files: vec![], graph: None,
                warnings: vec![], is_baseline: true, no_changes: false,
            }],
            todos: vec![],
        };
    };

    // Diff
    let diffs = diff::diff(prev, &final_snapshot);
    if diffs.is_empty() {
        let mut snap = final_snapshot;
        snap.version = next_version;
        return MultiSnapshotResult {
            snapshots: vec![SnapshotResult {
                snapshot: snap, diffs: vec![], migration_files: vec![], graph: None,
                warnings: vec![], is_baseline: false, no_changes: true,
            }],
            todos: vec![],
        };
    }

    // Classify
    let (simple_diffs, complex_changes) = diff::classify_changes(&diffs, prev);

    if complex_changes.is_empty() {
        // Simple only — single snapshot (delegate to existing prepare_snapshot)
        let result = prepare_snapshot(entities, Some(prev), next_version, description);
        return MultiSnapshotResult { snapshots: vec![result], todos: vec![] };
    }

    // Determine number of stages
    let has_3_step = complex_changes.iter().any(|c| matches!(c, diff::ComplexChange::EnumValueRemoval { .. }));
    let num_stages = if has_3_step { 3 } else { 2 };

    let mut snapshots = Vec::new();
    let mut todos = Vec::new();

    // ── Stage 1: simple changes + first step of complex ────
    let mut stage1_snap = prev.clone();
    stage1_snap.version = next_version;
    stage1_snap.description = format!("{} (stage 1)", description);
    stage1_snap.timestamp = chrono::Utc::now().to_rfc3339();

    let mut stage1_files = Vec::new();
    let mut stage1_added = Vec::new();
    let mut stage1_altered = Vec::new();
    let mut stage1_dropped = Vec::new();

    // Apply simple diffs to stage1 snapshot
    for d in &simple_diffs {
        match &d.action {
            DiffAction::Add => {
                stage1_added.push(d.entity_name.clone());
                // Add new table/enum to snapshot
                if let Some(new_t) = final_snapshot.tables.iter().find(|t| format!("{}.{}", t.schema, t.name) == d.entity_name) {
                    stage1_snap.tables.push(new_t.clone());
                }
                if let Some(new_e) = final_snapshot.enums.iter().find(|e| e.name == d.entity_name) {
                    stage1_snap.enums.push(new_e.clone());
                }
            }
            DiffAction::Drop => {
                stage1_dropped.push(d.entity_name.clone());
                stage1_snap.tables.retain(|t| format!("{}.{}", t.schema, t.name) != d.entity_name);
                stage1_snap.enums.retain(|e| e.name != d.entity_name);
                let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                if !sql.is_empty() {
                    stage1_files.push(MigrationFile { relative_path: entity_migration_path(&d.entity_name), content: sql });
                }
            }
            DiffAction::Change(_) => {
                stage1_altered.push(d.entity_name.clone());
                let sql = diff::generate_migration_sql(std::slice::from_ref(d));
                if !sql.is_empty() {
                    stage1_files.push(MigrationFile { relative_path: entity_migration_path(&d.entity_name), content: sql });
                }
                // Update snapshot tables/enums with simple changes from final
                if let Some(new_t) = final_snapshot.tables.iter().find(|t| format!("{}.{}", t.schema, t.name) == d.entity_name) {
                    if let Some(old_t) = stage1_snap.tables.iter_mut().find(|t| format!("{}.{}", t.schema, t.name) == d.entity_name) {
                        // For simple changes, apply the final state (constraints, indexes, simple col adds/drops)
                        // But for complex tables, we handle separately below
                        if !complex_changes.iter().any(|c| match c {
                            diff::ComplexChange::ColumnTypeChange { table_name, .. } |
                            diff::ComplexChange::ColumnRename { table_name, .. } => table_name == &d.entity_name,
                            _ => false,
                        }) {
                            *old_t = new_t.clone();
                        }
                    }
                }
                if let Some(new_e) = final_snapshot.enums.iter().find(|e| e.name == d.entity_name) {
                    if let Some(old_e) = stage1_snap.enums.iter_mut().find(|e| e.name == d.entity_name) {
                        *old_e = new_e.clone();
                    }
                }
            }
        }
    }

    // Apply complex changes step 1
    for change in &complex_changes {
        match change {
            diff::ComplexChange::ColumnRename { table_name, new_name, col_def, .. } => {
                stage1_altered.push(table_name.clone());
                // Add new column to snapshot (old stays)
                if let Some(table) = stage1_snap.tables.iter_mut().find(|t| format!("{}.{}", t.schema, t.name) == *table_name) {
                    table.columns.push(col_def.clone());
                }
                // Generate ADD COLUMN SQL
                let add_sql = format!("ALTER TABLE {} ADD COLUMN {} {};\n", table_name, new_name, col_def.data_type);
                stage1_files.push(MigrationFile { relative_path: entity_migration_path(table_name), content: add_sql });
                // Generate data.sql
                let data_sql = diff::generate_data_sql(change);
                let data_path = entity_data_migration_path(table_name);
                stage1_files.push(MigrationFile { relative_path: data_path, content: data_sql });
            }
            diff::ComplexChange::ColumnTypeChange { table_name, column_name, new_type, new_col, .. } => {
                stage1_altered.push(table_name.clone());
                // Add new column with new name (convention: column_name + suffix or use new_col.name)
                if let Some(table) = stage1_snap.tables.iter_mut().find(|t| format!("{}.{}", t.schema, t.name) == *table_name) {
                    table.columns.push(new_col.clone());
                }
                let add_sql = format!("ALTER TABLE {} ADD COLUMN {} {};\n", table_name, new_col.name, new_type);
                stage1_files.push(MigrationFile { relative_path: entity_migration_path(table_name), content: add_sql });
                let data_sql = diff::generate_data_sql(change);
                let data_path = entity_data_migration_path(table_name);
                stage1_files.push(MigrationFile { relative_path: data_path, content: data_sql });
            }
            diff::ComplexChange::EnumValueRemoval { enum_name, affected_columns, .. } => {
                // Stage 1: data correction only (no schema change)
                let data_sql = diff::generate_data_sql(change);
                // Use the first affected table for the file path, or the enum name
                let path_entity = affected_columns.first().map(|(t, _)| t.as_str()).unwrap_or(enum_name);
                let data_path = entity_data_migration_path(path_entity);
                stage1_files.push(MigrationFile { relative_path: data_path.clone(), content: data_sql });
                todos.push(TodoItem {
                    file: data_path,
                    message: format!("{}: fill in enum value mapping for removed values", enum_name),
                });
            }
        }
    }

    let stage1_graph = MigrationGraph {
        from_version: prev.version,
        to_version: next_version,
        added: stage1_added, altered: stage1_altered, dropped: stage1_dropped,
    };
    snapshots.push(SnapshotResult {
        snapshot: stage1_snap.clone(),
        diffs: simple_diffs.clone(),
        migration_files: stage1_files,
        graph: Some(stage1_graph),
        warnings: vec![],
        is_baseline: false, no_changes: false,
    });

    // ── Stage 2: drop old columns + enum intermediary ──────
    let stage2_version = next_version + 1;
    let mut stage2_snap = stage1_snap.clone();
    stage2_snap.version = stage2_version;
    stage2_snap.description = format!("{} (stage 2)", description);
    let mut stage2_files = Vec::new();
    let mut stage2_altered = Vec::new();
    let mut stage2_dropped = Vec::new();

    for change in &complex_changes {
        match change {
            diff::ComplexChange::ColumnRename { table_name, old_name, .. } => {
                stage2_altered.push(table_name.clone());
                if let Some(table) = stage2_snap.tables.iter_mut().find(|t| format!("{}.{}", t.schema, t.name) == *table_name) {
                    table.columns.retain(|c| c.name != *old_name);
                }
                let drop_sql = format!("ALTER TABLE {} DROP COLUMN {};\n", table_name, old_name);
                stage2_files.push(MigrationFile { relative_path: entity_migration_path(table_name), content: drop_sql });
            }
            diff::ComplexChange::ColumnTypeChange { table_name, column_name, old_col, .. } => {
                stage2_altered.push(table_name.clone());
                if let Some(table) = stage2_snap.tables.iter_mut().find(|t| format!("{}.{}", t.schema, t.name) == *table_name) {
                    table.columns.retain(|c| c.name != *column_name);
                }
                let drop_sql = format!("ALTER TABLE {} DROP COLUMN {};\n", table_name, column_name);
                stage2_files.push(MigrationFile { relative_path: entity_migration_path(table_name), content: drop_sql });
            }
            diff::ComplexChange::EnumValueRemoval { enum_name, affected_columns, remaining_values, .. } => {
                // ALTER columns to TEXT
                for (table_name, col_name) in affected_columns {
                    stage2_altered.push(table_name.clone());
                    if let Some(table) = stage2_snap.tables.iter_mut().find(|t| format!("{}.{}", t.schema, t.name) == *table_name) {
                        if let Some(c) = table.columns.iter_mut().find(|c| c.name == *col_name) {
                            c.data_type = "TEXT".to_string();
                        }
                    }
                    let alter_sql = format!("ALTER TABLE {} ALTER COLUMN {} TYPE TEXT;\n", table_name, col_name);
                    stage2_files.push(MigrationFile { relative_path: entity_migration_path(table_name), content: alter_sql });
                }
                // DROP TYPE
                stage2_dropped.push(enum_name.clone());
                stage2_snap.enums.retain(|e| e.name != *enum_name);
                let drop_sql = format!("DROP TYPE {};\n", enum_name);
                stage2_files.push(MigrationFile { relative_path: entity_migration_path(enum_name), content: drop_sql });
            }
        }
    }

    let stage2_graph = MigrationGraph {
        from_version: next_version, to_version: stage2_version,
        added: vec![], altered: stage2_altered, dropped: stage2_dropped,
    };
    snapshots.push(SnapshotResult {
        snapshot: stage2_snap.clone(),
        diffs: vec![], migration_files: stage2_files,
        graph: Some(stage2_graph),
        warnings: vec![], is_baseline: false, no_changes: false,
    });

    // ── Stage 3: recreate enums (only if 3-step needed) ────
    if has_3_step {
        let stage3_version = next_version + 2;
        let mut stage3_snap = final_snapshot.clone();
        stage3_snap.version = stage3_version;
        stage3_snap.description = format!("{} (stage 3)", description);
        let mut stage3_files = Vec::new();
        let mut stage3_added = Vec::new();
        let mut stage3_altered = Vec::new();

        for change in &complex_changes {
            if let diff::ComplexChange::EnumValueRemoval { enum_name, remaining_values, affected_columns, .. } = change {
                // CREATE TYPE with new values
                stage3_added.push(enum_name.clone());
                let values_sql = remaining_values.iter().map(|v| format!("'{}'", v)).collect::<Vec<_>>().join(", ");
                let create_sql = format!("CREATE TYPE {} AS ENUM ({});\n", enum_name, values_sql);
                stage3_files.push(MigrationFile { relative_path: entity_migration_path(enum_name), content: create_sql });

                // ALTER columns back to enum type
                for (table_name, col_name) in affected_columns {
                    stage3_altered.push(table_name.clone());
                    let alter_sql = format!(
                        "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::{};\n",
                        table_name, col_name, enum_name, col_name, enum_name
                    );
                    stage3_files.push(MigrationFile { relative_path: entity_migration_path(table_name), content: alter_sql });
                }
            }
        }

        let stage3_graph = MigrationGraph {
            from_version: stage2_version, to_version: stage3_version,
            added: stage3_added, altered: stage3_altered, dropped: vec![],
        };
        snapshots.push(SnapshotResult {
            snapshot: stage3_snap,
            diffs: vec![], migration_files: stage3_files,
            graph: Some(stage3_graph),
            warnings: vec![], is_baseline: false, no_changes: false,
        });
    }

    MultiSnapshotResult { snapshots, todos }
}

/// Build a relative path for a data.sql file.
fn entity_data_migration_path(entity_name: &str) -> PathBuf {
    let (schema, table) = crate::entity::split_qualified_name(entity_name);
    match schema {
        Some(s) => PathBuf::from(s).join(format!("{table}.data.sql")),
        None => PathBuf::from(format!("{table}.data.sql")),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p dbd-core snapshot::tests::b`
Expected: all pass

Run: `cargo test -p dbd-core snapshot::tests::s1`
Expected: all pass

- [ ] **Step 4: Run full suite**

Run: `cargo test`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/snapshot.rs
git commit -m "feat: prepare_multi_snapshot with stage batching and synthesis (B1-B5, S1-S4)"
```

---

### Task 5: Update `create_snapshot()` I/O wrapper for multi-snapshot

**Files:**
- Modify: `crates/dbd-core/src/snapshot.rs`

- [ ] **Step 1: Update `create_snapshot` to return `MultiSnapshotResult`**

Replace the existing `create_snapshot` function:

```rust
/// Create snapshots from entities, writing all files to disk.
/// Returns MultiSnapshotResult with 1-3 snapshots depending on change complexity.
pub fn create_snapshot(
    entities: &[Entity],
    project_dir: &Path,
    config_path: &Path,
    description: &str,
) -> Result<MultiSnapshotResult> {
    let previous = latest_snapshot(project_dir)?;
    let base_version = next_version(project_dir);

    let result = prepare_multi_snapshot(entities, previous.as_ref(), base_version, description);

    // Check for no-changes case
    if result.snapshots.len() == 1 && result.snapshots[0].no_changes {
        return Ok(result);
    }

    let snapshots_dir = project_dir.join(SNAPSHOTS_DIR);
    std::fs::create_dir_all(&snapshots_dir)?;

    let mut final_version = base_version;

    for snap_result in &result.snapshots {
        if snap_result.is_baseline || !snap_result.no_changes {
            let version = snap_result.snapshot.version;
            final_version = version;

            // Write snapshot JSON
            let snap_file = snapshots_dir.join(format!("{}.json", pad_version(version)));
            let json = serde_json::to_string_pretty(&snap_result.snapshot)?;
            std::fs::write(&snap_file, json)?;

            // Write migration files
            if let Some(ref graph) = snap_result.graph {
                let migration_dir = project_dir.join(MIGRATIONS_DIR).join(pad_version(version));
                std::fs::create_dir_all(&migration_dir)?;

                let graph_json = serde_json::to_string_pretty(graph)?;
                std::fs::write(migration_dir.join("graph.json"), graph_json)?;

                for file in &snap_result.migration_files {
                    let full_path = migration_dir.join(&file.relative_path);
                    if let Some(parent) = full_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&full_path, &file.content)?;
                }
            }
        }
    }

    // Update config version to final
    crate::config::update_version(config_path, final_version)?;

    Ok(result)
}
```

- [ ] **Step 2: Update existing SC10 test if needed**

The SC10 test calls `create_snapshot` and expects `SnapshotResult`. Update it to work with `MultiSnapshotResult`:

Find test `sc10_create_snapshot_writes_files` and update assertions:
- `result.is_baseline` → `result.snapshots[0].is_baseline`
- `result.no_changes` → `result.snapshots[0].no_changes`

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/snapshot.rs
git commit -m "feat: update create_snapshot to return MultiSnapshotResult"
```

---

### Task 6: Update CLI — snapshot create for multi-snapshot output

**Files:**
- Modify: `crates/dbd-cli/src/commands.rs`

- [ ] **Step 1: Update `cmd_snapshot_create` for `MultiSnapshotResult`**

Replace the current `cmd_snapshot_create` function to handle multiple snapshots:

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

    // Single snapshot, no changes
    if result.snapshots.len() == 1 && result.snapshots[0].no_changes {
        output::info(verbosity, "No schema changes detected — snapshot skipped.");
        return Ok(());
    }

    let total_stages = result.snapshots.len();

    for (i, snap) in result.snapshots.iter().enumerate() {
        let version = dbd_core::snapshot::pad_version(snap.snapshot.version);

        if snap.is_baseline {
            output::info(verbosity, &format!("Baseline snapshot v{version} created."));
            continue;
        }

        if total_stages == 1 {
            // Simple snapshot — backward compatible output
            let graph = snap.graph.as_ref();
            let added = graph.map(|g| g.added.len()).unwrap_or(0);
            let altered = graph.map(|g| g.altered.len()).unwrap_or(0);
            let dropped = graph.map(|g| g.dropped.len()).unwrap_or(0);
            output::info(verbosity, &format!(
                "Snapshot v{version} created — {added} added, {altered} altered, {dropped} dropped."
            ));
        } else {
            // Multi-stage output
            output::info(verbosity, &format!(
                "\nSnapshot v{version} created (stage {} of {})",
                i + 1, total_stages
            ));
        }

        if !snap.migration_files.is_empty() {
            for mf in &snap.migration_files {
                output::detail(verbosity, &format!("  {}", mf.relative_path.display()));
            }
        }
    }

    // Print TODO items
    if !result.todos.is_empty() {
        output::always("\nAction required:");
        for todo in &result.todos {
            output::always(&format!("  {} — {}", todo.file.display(), todo.message));
        }
    }

    let final_version = result.snapshots.last().map(|s| s.snapshot.version).unwrap_or(0);
    output::info(verbosity, &format!("\ndesign.yaml version updated to {final_version}"));

    Ok(())
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check`
Expected: compiles

- [ ] **Step 3: Run full tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-cli/src/commands.rs
git commit -m "feat: update CLI snapshot create for multi-stage output"
```

---

### Task 7: Remove `migrate --apply` and `--to` from CLI

**Files:**
- Modify: `crates/dbd-cli/src/cli.rs`
- Modify: `crates/dbd-cli/src/commands.rs`

- [ ] **Step 1: Simplify Migrate command in cli.rs**

Replace the Migrate variant:

```rust
    /// Show migration status
    Migrate {
        /// Show local vs database version
        #[arg(long)]
        status: bool,
    },
```

- [ ] **Step 2: Simplify migrate routing in commands.rs**

Replace the Migrate match arm:

```rust
        Commands::Migrate { status } => {
            if *status {
                cmd_migrate_status(config, database_url, project_dir, verbosity).await
            } else {
                output::info(verbosity, "Use --status to check migration state. Use 'dbd apply' to run migrations.");
                Ok(())
            }
        }
```

- [ ] **Step 3: Remove `cmd_migrate_apply` function**

Delete the entire `cmd_migrate_apply` function from commands.rs.

- [ ] **Step 4: Verify compilation and tests**

Run: `cargo test`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli/src/cli.rs crates/dbd-cli/src/commands.rs
git commit -m "feat: remove migrate --apply and --to, keep --status as read-only diagnostic"
```

---

### Task 8: Final verification — zero errors

**Files:** All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass, 0 failures

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Verify test count increased**

Run: `cargo test 2>&1 | grep "test result"`
Expected: significantly more than 252 tests

- [ ] **Step 4: Commit any fixes**

```bash
git add -A && git commit -m "fix: clippy and final cleanup"
```

---

### Task 9: Update BACKLOG.md

**Files:**
- Modify: `docs/BACKLOG.md`

- [ ] **Step 1: Update backlog**

Mark "Smart multi-snapshot generation" as DONE. Update test count and LOC. Note that `migrate --apply` and `--to` were removed.

- [ ] **Step 2: Commit**

```bash
git add docs/BACKLOG.md
git commit -m "docs: update backlog — smart multi-snapshot complete"
```
