# Smart Multi-Snapshot Generation Design

**Date:** 2026-04-30
**Status:** Approved
**Scope:** Automatic multi-snapshot splitting for complex schema changes

---

## Overview

When `dbd snapshot` detects complex changes (enum value removal, column type change, column rename), it automatically generates multiple snapshots with correct intermediate states and data.sql files. DDL files always represent the final desired state. Each intermediate snapshot is synthesized programmatically.

**Invariant:** DDL files = final state. Migrations bridge any environment to that final state. Apply always runs all pending migrations then applies DDL.

## Design Decisions

- **Fully automatic:** No interactive prompts. Generate all snapshots, developer reviews before committing.
- **DDL files untouched:** Only snapshots/, migrations/, and design.yaml are written. DDL files stay at final state.
- **Enum value mappings are business logic:** Always generate TODO comments for enum value removal. Never guess.
- **Column type casts are best-effort:** Generate `UPDATE ... SET col = old_col::new_type` using Postgres CAST. Fall back to TODO comment for incompatible types.
- **Stage batching:** All changes batch by stage number, not by entity. One snapshot per stage.

---

## Change Classification

### Pure function: `classify_changes()`

```rust
fn classify_changes(
    diffs: &[MigrationDiff],
    old_snapshot: &Snapshot,
) -> (Vec<MigrationDiff>, Vec<ComplexChange>)
//    simple changes     complex changes
```

Returns simple changes (passed through to stage 1) and classified complex changes.

### Types

```rust
enum ComplexChange {
    ColumnTypeChange {
        table_name: String,
        column_name: String,
        old_type: String,
        new_type: String,
        old_col: ColumnDef,
        new_col: ColumnDef,
    },
    ColumnRename {
        table_name: String,
        old_name: String,
        new_name: String,
        col_def: ColumnDef,
    },
    EnumValueRemoval {
        enum_name: String,
        removed_values: Vec<String>,
        remaining_values: Vec<String>,
        affected_columns: Vec<(String, String)>,  // (table_name, column_name)
    },
}
```

### Detection Rules

| Pattern | Detection | Classification |
|---------|-----------|----------------|
| Column type change | `FieldChange` with `Alter` where `old.data_type != new.data_type` | `ColumnTypeChange` (2 stages) |
| Column rename | Same table has `Column Drop` + `Column Add` with same data type AND same position | `ColumnRename` (2 stages) |
| Enum value removal | `EnumValue Drop` in a `Change` diff | `EnumValueRemoval` (3 stages) |
| Enum value rename | `EnumValue Drop` + `EnumValue Add` in same enum (PG17+) | Simple: `ALTER TYPE RENAME VALUE` (1 stage) |

**Rename detection heuristic:** Within a single table's `Change` diff, if there's exactly one dropped column and one added column with the same data type AND the same ordinal position in the column list, treat as rename. If there are multiple drops+adds with same types, don't guess — treat each as independent drop/add (simple changes). Position is determined by the column's index in the old/new snapshot's column vec.

**Enum value rename (Postgres 17+):** If an enum loses one value and gains one value (1:1 swap), treat as a rename. Generate `ALTER TYPE ... RENAME VALUE 'old' TO 'new'` as a simple change (single snapshot). This requires Postgres 14+ which supports `RENAME VALUE`. We assume Postgres 17+ as minimum.

**Affected columns for enum removal:** Scan all table snapshots in the old snapshot for columns whose `data_type` matches the enum name (qualified or unqualified).

---

## Stage Batching

```
Max stages = 1 + (has_2_step ? 1 : 0) + (has_3_step ? 1 : 0)
```

| Stage | Simple | ColumnTypeChange | ColumnRename | EnumValueRemoval | EnumValueRename (PG17+) |
|-------|--------|------------------|--------------|------------------|------------------------|
| 1 | All ALTERs | ADD new_col + data.sql (CAST) | ADD new_col + data.sql (copy) | data.sql (TODO: value mapping) | `ALTER TYPE RENAME VALUE` |
| 2 | — | DROP old_col | DROP old_col | ALTER cols to TEXT + DROP TYPE | — |
| 3 | — | — | — | CREATE TYPE + ALTER cols back | — |

Enum value rename is a simple change (single stage) thanks to Postgres 17+ support for `ALTER TYPE ... RENAME VALUE`.

If no 3-step changes exist, max 2 snapshots. If no complex changes at all, 1 snapshot (current behavior — backward compatible).

---

## Intermediate Snapshot Synthesis

### Stage 1 snapshot (synthesized from previous + stage-1 changes)

Start with a clone of the previous snapshot's tables/enums. Apply:
- Simple column adds → push column to table's columns vec
- Simple column drops → remove column from table's columns vec
- ColumnRename step 1 → push new column (old column stays)
- ColumnTypeChange step 1 → push new column with new type (old column stays)
- EnumValueRemoval step 1 → no schema change (data-only)
- Simple constraint/index changes → apply add/drop
- Enum value adds → add to enum's values vec
- New tables → add full TableSnapshot
- Dropped tables → remove from tables vec

### Stage 2 snapshot (synthesized from stage 1)

Start with stage 1 snapshot. Apply:
- ColumnRename step 2 → remove old column
- ColumnTypeChange step 2 → remove old column
- EnumValueRemoval step 2 → change affected columns' data_type to "TEXT", remove enum from enums vec

### Stage 3 snapshot = final state

Built directly from the developer's entities (same as current `prepare_snapshot` behavior). This always matches the DDL files.

---

## data.sql Generation

### Auto-generated (best-effort)

**Column rename:**
```sql
UPDATE config.users SET display_name = name;
```

**Column type change (castable):**
```sql
UPDATE config.users SET total_text = total::TEXT;
```

**Column type change (non-castable, e.g., JSONB → INTEGER):**
```sql
-- TODO: Data correction required for config.users.metadata
-- Column type changed from JSONB to INTEGER
-- UPDATE config.users SET metadata_new = <derive from metadata>;
```

### TODO-only (business logic required)

**Enum value removal:**
```sql
-- TODO: Map removed enum values to remaining values
-- Enum: public.status_type
-- Removed: deleted
-- Remaining: active, inactive
-- UPDATE config.events SET status = '???' WHERE status = 'deleted';
```

### Castability heuristic

A type change is "castable" if the Postgres `::` operator would work. Simple heuristic based on type categories:

| From | To | Castable? |
|------|----|-----------|
| Any integer | TEXT/VARCHAR | Yes |
| NUMERIC/DECIMAL | TEXT/VARCHAR | Yes |
| VARCHAR(N) | TEXT | Yes |
| TEXT | VARCHAR(N) | Yes (may truncate — add comment) |
| BOOLEAN | TEXT/INTEGER | Yes |
| TIMESTAMP | TEXT | Yes |
| Any → same category | — | Yes |
| JSONB → scalar | — | No (TODO) |
| Array → scalar | — | No (TODO) |

Implemented as a simple function: `fn is_castable(from: &str, to: &str) -> bool`

---

## Multi-Snapshot Result

### Types

```rust
struct MultiSnapshotResult {
    pub snapshots: Vec<SnapshotResult>,  // 1, 2, or 3 entries
    pub todos: Vec<TodoItem>,            // items requiring developer attention
}

struct TodoItem {
    pub file: PathBuf,                   // relative path to data.sql
    pub message: String,                 // console-friendly description
}
```

### Pure function

```rust
fn prepare_multi_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> MultiSnapshotResult
```

Logic:
1. Build final snapshot from entities (same as current)
2. If no previous → baseline, return single SnapshotResult
3. Diff previous vs final
4. `classify_changes(diffs, previous)` → simple + complex
5. If no complex changes → single snapshot (current behavior)
6. Determine max stages (2 or 3)
7. For each stage: synthesize intermediate snapshot, generate migration SQL + data.sql
8. Return `MultiSnapshotResult` with all snapshots and TODO items

### I/O wrapper

```rust
fn create_snapshot(
    entities: &[Entity],
    project_dir: &Path,
    config_path: &Path,
    description: &str,
) -> Result<MultiSnapshotResult>
```

Replaces the current single-snapshot `create_snapshot`. Writes all snapshot JSONs, migration folders, and data.sql files. Updates design.yaml version to the final version.

---

## CLI Output

```
$ dbd snapshot --name "restructure"

Snapshot v002 created (stage 1 of 3)
  10 simple changes applied
  + config.users.display_name (TEXT) — rename step 1
  + config.orders.total_text (TEXT) — type change step 1
  data.sql: migrations/002/config/users.data.sql (auto: copy from name)
  data.sql: migrations/002/config/orders.data.sql (auto: cast INTEGER → TEXT)

Snapshot v003 created (stage 2 of 3)
  - config.users.name — rename step 2 (drop old column)
  - config.orders.total — type change step 2 (drop old column)
  ALTER config.events.status → TEXT (enum intermediary)
  DROP TYPE public.status_type

Snapshot v004 created (stage 3 of 3)
  CREATE TYPE public.status_type (active, inactive)
  ALTER config.events.status → public.status_type

Action required:
  migrations/002/config/events.data.sql — fill in enum value mapping:
    Removed: deleted
    Remaining: active, inactive

design.yaml version updated to 4
```

For simple-only changes (backward compatible):
```
$ dbd snapshot --name "add email column"

Snapshot v002 created
  1 table altered
  1 migration file generated

design.yaml version updated to 2
```

---

## Backward Compatibility

- Single-snapshot case (no complex changes) behaves identically to current implementation
- `SnapshotResult` struct gains `warnings` field (already done) but is otherwise unchanged
- `create_snapshot` return type changes from `SnapshotResult` to `MultiSnapshotResult` — callers updated
- `apply` and `migrate` commands consume migrations the same way — they see individual versioned migration folders regardless of whether they were generated as a batch

---

## Test Scenarios

### Classification Tests

#### C1: Simple changes only → no complex classification
```
Given: diff with column add + column drop + index add
When:  classify_changes()
Then:  all changes in simple list, empty complex list
```

#### C2: Column type change detected
```
Given: diff with column email changed from VARCHAR(100) to TEXT
When:  classify_changes()
Then:  ColumnTypeChange { table: "config.users", column: "email", old: "VARCHAR(100)", new: "TEXT" }
```

#### C3: Column rename detected (drop + add same type)
```
Given: diff with column "name" dropped and "display_name" added, both TEXT
When:  classify_changes()
Then:  ColumnRename { table: "config.users", old: "name", new: "display_name", type: "TEXT" }
```

#### C4: Ambiguous drop+add (different types) → not rename
```
Given: diff with column "name" (TEXT) dropped and "age" (INTEGER) added
When:  classify_changes()
Then:  both in simple list, no ColumnRename
```

#### C5: Multiple drop+add same type → not rename (ambiguous)
```
Given: diff with 2 TEXT columns dropped and 2 TEXT columns added
When:  classify_changes()
Then:  all in simple list, no ColumnRename
```

#### C6: Enum value removal detected
```
Given: diff with enum "status_type" losing value "deleted"
When:  classify_changes()
Then:  EnumValueRemoval { enum: "public.status_type", removed: ["deleted"], remaining: ["active", "inactive"] }
```

#### C7: Enum value removal identifies affected columns
```
Given: old snapshot has table config.events with column status of type "public.status_type"
When:  classify_changes() for enum value removal
Then:  affected_columns includes ("config.events", "status")
```

### Stage Batching Tests

#### B1: Simple only → 1 snapshot
```
Given: 5 simple changes, no complex
When:  prepare_multi_snapshot()
Then:  snapshots.len() == 1
```

#### B2: 2-step changes → 2 snapshots
```
Given: 3 simple + 1 column rename
When:  prepare_multi_snapshot()
Then:  snapshots.len() == 2
       snapshots[0] has simple changes + new column
       snapshots[1] has old column drop
```

#### B3: 3-step changes → 3 snapshots
```
Given: 1 simple + 1 enum value removal
When:  prepare_multi_snapshot()
Then:  snapshots.len() == 3
```

#### B4: Mix of 2-step and 3-step → 3 snapshots
```
Given: 1 column rename + 1 enum value removal
When:  prepare_multi_snapshot()
Then:  snapshots.len() == 3
       stage 1: add new col + enum data.sql
       stage 2: drop old col + ALTER to TEXT + DROP TYPE
       stage 3: CREATE TYPE + ALTER back
```

#### B5: Multiple complex changes of same type → batched
```
Given: 3 column renames
When:  prepare_multi_snapshot()
Then:  snapshots.len() == 2
       stage 1: 3 new columns added
       stage 2: 3 old columns dropped
```

### Intermediate Snapshot Synthesis Tests

#### S1: Stage 1 snapshot adds new column for rename
```
Given: previous has table users with [id, name]
       rename: name → display_name
When:  synthesize stage 1
Then:  snapshot has users with [id, name, display_name]
```

#### S2: Stage 2 snapshot drops old column for rename
```
Given: stage 1 has users with [id, name, display_name]
When:  synthesize stage 2
Then:  snapshot has users with [id, display_name]
```

#### S3: Stage 2 snapshot changes enum column to TEXT
```
Given: stage 1 has table events with status (public.status_type)
       enum removal in progress
When:  synthesize stage 2
Then:  events.status has data_type "TEXT"
       public.status_type not in enums list
```

#### S4: Stage 3 snapshot matches final entities
```
Given: developer entities have events.status as new_status_type
When:  build stage 3 snapshot
Then:  matches entity-derived snapshot exactly
```

### data.sql Generation Tests

#### D1: Column rename generates auto copy
```
Given: ColumnRename { table: "config.users", old: "name", new: "display_name" }
When:  generate data.sql for stage 1
Then:  "UPDATE config.users SET display_name = name;"
```

#### D2: Castable type change generates CAST
```
Given: ColumnTypeChange { old: "INTEGER", new: "TEXT" }
When:  generate data.sql
Then:  "UPDATE config.users SET total_text = total::TEXT;"
```

#### D3: Non-castable type change generates TODO
```
Given: ColumnTypeChange { old: "JSONB", new: "INTEGER" }
When:  generate data.sql
Then:  contains "-- TODO: Data correction required"
       contains "JSONB to INTEGER"
```

#### D4: Enum value removal generates TODO with values listed
```
Given: EnumValueRemoval { removed: ["deleted"], remaining: ["active", "inactive"] }
When:  generate data.sql
Then:  contains "-- TODO: Map removed enum values"
       contains "Removed: deleted"
       contains "Remaining: active, inactive"
       contains "UPDATE config.events SET status = '???' WHERE status = 'deleted'"
```

#### D5: TEXT→VARCHAR generates CAST with truncation warning
```
Given: ColumnTypeChange { old: "TEXT", new: "VARCHAR(50)" }
When:  generate data.sql
Then:  contains "::VARCHAR(50)"
       contains "-- WARNING: may truncate values longer than 50 characters"
```

### Castability Tests

#### CA1: Integer to TEXT is castable
```
assert!(is_castable("INTEGER", "TEXT"));
assert!(is_castable("BIGINT", "TEXT"));
```

#### CA2: VARCHAR to TEXT is castable
```
assert!(is_castable("VARCHAR(100)", "TEXT"));
```

#### CA3: TEXT to VARCHAR is castable (with warning)
```
assert!(is_castable("TEXT", "VARCHAR(50)"));
```

#### CA4: JSONB to INTEGER is not castable
```
assert!(!is_castable("JSONB", "INTEGER"));
```

#### CA5: Array to scalar is not castable
```
assert!(!is_castable("TEXT[]", "TEXT"));
```

### Integration Tests

#### I1: Full round-trip: rename column
```
Given: project with users table [id, name], snapshot v1
       DDL changed to [id, display_name]
When:  create_snapshot()
Then:  2 snapshots written (002, 003)
       migrations/002/config/users.sql has ADD COLUMN display_name
       migrations/002/config/users.data.sql has UPDATE SET display_name = name
       migrations/003/config/users.sql has DROP COLUMN name
       design.yaml version = 3
```

#### I2: Full round-trip: enum value removal
```
Given: project with status_type [active, inactive, deleted], snapshot v1
       DDL changed to [active, inactive]
       Table events has column status of type status_type
When:  create_snapshot()
Then:  3 snapshots written (002, 003, 004)
       migrations/002/config/events.data.sql has TODO comment
       migrations/003/ has ALTER to TEXT + DROP TYPE SQL
       migrations/004/ has CREATE TYPE + ALTER back SQL
       design.yaml version = 4
```

#### I3: Mixed simple + complex
```
Given: snapshot v1, DDL has: add column email (simple) + rename name→display_name (complex)
When:  create_snapshot()
Then:  2 snapshots
       v2 has both: ADD email + ADD display_name + data.sql for copy
       v3 has: DROP name
```

#### I4: No complex changes → backward compatible single snapshot
```
Given: snapshot v1, DDL only adds a column
When:  create_snapshot()
Then:  1 snapshot (same as before)
```

---

## Files Modified/Created

| File | Action | Purpose |
|------|--------|---------|
| `crates/dbd-core/src/diff.rs` | Modify | Add `classify_changes()`, `is_castable()`, `generate_data_sql()`. Keep `migration_warnings()` for inspect use. |
| `crates/dbd-core/src/snapshot.rs` | Modify | Replace `prepare_snapshot` with `prepare_multi_snapshot`, update `create_snapshot`, add `MultiSnapshotResult`, synthesis functions |
| `crates/dbd-cli/src/commands.rs` | Modify | Update `cmd_snapshot_create` for `MultiSnapshotResult`, print stage output + TODO items |

## CLI Changes

### `dbd migrate` simplification

Remove `dbd migrate --apply` and `--to N`. `apply` is the only command that modifies the database.

Keep `dbd migrate --status` as a read-only diagnostic:
```
$ dbd migrate --status
DB: v1, Latest: v4
Pending: v2, v3, v4
  v2: 10 simple changes + rename config.users.name
  v3: drop config.users.name + enum intermediary
  v4: enum recreation
```

`--dry-run` stays on `dbd apply` to preview what would execute.

## Assumptions

- **Postgres 17+ minimum** — enables `ALTER TYPE RENAME VALUE` for enum value renames
- Enum value DROP is not supported by Postgres — TEXT intermediary pattern required

## Future Considerations

- data.sql validation: `dbd inspect` could verify all TODO comments have been resolved before allowing apply
- Postgres future: if `ALTER TYPE DROP VALUE` is added, enum removal becomes a single-snapshot operation
