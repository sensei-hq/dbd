# Snapshot, Schema Diff & Migration Design

**Date:** 2026-04-30
**Status:** Approved
**Scope:** `dbd snapshot` (create), schema diff engine, `dbd apply` with migrations, `dbd migrate` command

---

## Overview

Add versioned schema snapshots and incremental migration generation to dbd-rs. Snapshots capture the current DDL state as a versioned JSON file. Comparing consecutive snapshots produces a diff that generates ALTER/DROP SQL migration scripts. The `apply` command consumes these migrations to bring any environment up to date.

## Data Flow

```
dbd snapshot:
  DDL files -> parse -> entities -> Snapshot(N)
  Snapshot(N-1) + Snapshot(N) -> diff -> Vec<MigrationDiff>
  MigrationDiff -> SQL generation -> migrations/NNN/*.sql + graph.json
  Update design.yaml version -> N

dbd apply:
  _dbd_meta version -> current_db_version
  design.yaml version -> latest_version
  Fresh env: apply all entities, mark latest_version
  Behind: load pending migrations + entities ->
    build combined dependency graph (added + altered + unchanged) ->
    topological sort -> interleaved execute -> update _dbd_meta

dbd migrate:
  --status: show current vs latest, list pending
  --apply: run migration pass only (no full entity apply)
  --to N: limit to version N
  --dry-run: print SQL with version headers, highlight drops
```

---

## Architecture: I/O Boundary Separation

All core logic is implemented as pure functions that take version numbers, snapshots, and
entity lists as inputs and return data structures (plans, diffs, SQL strings) as outputs.
No database access, no filesystem reads inside core logic. This enables full unit testing
without mocks.

**Pure logic functions (no I/O):**

```rust
// Diff: compare two snapshots
fn diff(old: &Snapshot, new: &Snapshot) -> Vec<MigrationDiff>

// SQL gen: produce SQL from a diff
fn generate_migration_sql(entity_name: &str, diff: &MigrationDiff) -> String

// Snapshot: build snapshot + diffs from entities
fn prepare_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> SnapshotResult   // Snapshot, Vec<MigrationDiff>, generated SQL per entity

// Apply: build execution plan from version state
fn build_execution_plan(
    entities: &[Entity],
    db_version: u32,
    latest_version: u32,
    pending_migrations: &[PendingMigration],
) -> ExecutionPlan    // ordered list of steps: Create, Migrate, Drop
```

**I/O boundary (thin wrappers):**

```rust
// Reads DB version, calls build_execution_plan, executes steps via adapter
pub async fn apply(&self, adapter, name, dry_run) -> Result<()>

// Reads previous snapshot from disk, calls prepare_snapshot, writes files
pub fn create_snapshot(design: &Design, description: &str) -> Result<SnapshotResult>

// Reads DB version via adapter, calls pending_migrations, prints status
pub async fn migrate_status(adapter, project_dir, latest_version) -> Result<()>
```

**Testing benefit:** Every test scenario (D1-D21, S1-S14, SC1-SC10, A1-A8) runs as a
pure unit test. Construct inputs, call the function, assert outputs. No mock adapter needed
for core logic. Only the thin I/O wrappers need integration tests.

---

## Module 1: Schema Diff Engine (`diff.rs`)

### Types

```rust
/// Top-level diff for a single entity (table or enum).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MigrationDiff {
    entity_name: String,       // "config.lookups"
    entity_type: EntityType,   // Table, Enum
    action: DiffAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum DiffAction {
    Add,                       // new entity — no migration SQL needed
    Drop,                      // generate DROP TABLE/TYPE
    Change(Vec<FieldChange>),  // generate ALTER statements
}

/// A single field-level change within an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldChange {
    field_name: String,        // "email", "pk_users", "idx_email"
    field_type: FieldType,     // Column, Constraint, Index, EnumValue
    action: ChangeAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum FieldType {
    Column,
    Constraint,
    Index,
    EnumValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ChangeAction {
    Add(FieldDetail),
    Drop,
    Alter { old: FieldDetail, new: FieldDetail },  // Column and EnumValue only
}
// Note: Constraints and Indexes do NOT use Alter.
// A changed constraint/index = Drop (migration) + Add (regular apply).
// The migration only needs the Drop; the new version is created by
// CREATE TABLE / CREATE INDEX during the normal entity apply phase.

#[derive(Debug, Clone, Serialize, Deserialize)]
enum FieldDetail {
    Column(ColumnDef),
    Constraint(TableConstraint),
    Index(IndexDef),
    EnumValue(String),
}
```

### Diff Logic

`diff(old: &Snapshot, new: &Snapshot) -> Vec<MigrationDiff>`

**Tables:** Match by qualified name (`schema.table`).
- Present in new, absent in old -> `DiffAction::Add`
- Present in old, absent in new -> `DiffAction::Drop`
- Present in both -> compare fields:
  - **Columns:** Match by name. Added/dropped/altered (type, nullable, default, identity, unique, inline_fk).
  - **Constraints:** Match by name (or by type+columns if unnamed). Added/dropped only.
    Changed constraint (same name, different definition) = Drop old + Add happens via regular apply.
  - **Indexes:** Match by name. Added/dropped only.
    Changed index (same name, different columns/type) = Drop old + Add happens via regular apply.
- If no field changes -> entity excluded from diff output.

**Enums:** Match by qualified name.
- Present in new, absent in old -> `DiffAction::Add`
- Present in old, absent in new -> `DiffAction::Drop` (warning: manual migration required)
- Present in both -> compare values list:
  - New values -> `ChangeAction::Add(FieldDetail::EnumValue(v))`
  - Removed values -> `ChangeAction::Drop` (warning only, no SQL generated)

### SQL Generation

`generate_migration_sql(diff: &MigrationDiff) -> String`

| FieldChange | SQL |
|-------------|-----|
| Column Add | `ALTER TABLE schema.name ADD COLUMN col_name type [NOT NULL] [DEFAULT val]` |
| Column Drop | `ALTER TABLE schema.name DROP COLUMN col_name` |
| Column Alter (type) | `ALTER TABLE schema.name ALTER COLUMN col_name TYPE new_type` |
| Column Alter (nullable) | `ALTER TABLE ... ALTER COLUMN ... SET/DROP NOT NULL` |
| Column Alter (default) | `ALTER TABLE ... ALTER COLUMN ... SET DEFAULT/DROP DEFAULT` |
| Constraint Add | `ALTER TABLE ... ADD CONSTRAINT ...` |
| Constraint Drop | `ALTER TABLE ... DROP CONSTRAINT name` |
| Index Add | `CREATE [UNIQUE] INDEX name ON schema.table (cols)` |
| Index Drop | `DROP INDEX name` |
| Enum Value Add | `ALTER TYPE schema.name ADD VALUE 'val'` |
| Enum Value Drop | Warning only, no SQL |
| Table Drop | `DROP TABLE schema.name CASCADE` |
| Enum Drop | Warning: "manual migration required" |

---

## Module 2: Snapshot Create (extend `snapshot.rs`)

### Updated Types

```rust
struct Snapshot {
    version: u32,
    description: String,
    timestamp: String,
    tables: Vec<TableSnapshot>,
    enums: Vec<EnumSnapshot>,       // NEW
}

struct EnumSnapshot {
    name: String,                   // "public.gender_type"
    schema: String,
    values: Vec<String>,            // ["male", "female", "other"]
}

// MigrationGraph updated
struct MigrationGraph {
    from_version: u32,
    to_version: u32,
    added: Vec<String>,             // NEW — entities created in this version
    altered: Vec<String>,
    dropped: Vec<String>,
}
```

### Config Update

`ProjectConfig` gets a `version` field:

```yaml
project:
  name: daemon
  version: 3          # updated by `dbd snapshot`
```

```rust
struct ProjectConfig {
    name: String,
    version: Option<u32>,   // NEW — None means pre-snapshot project
    // ...existing fields
}
```

### Pure Logic: `prepare_snapshot()`

```rust
/// Builds snapshot and computes diffs. No filesystem I/O.
fn prepare_snapshot(
    entities: &[Entity],
    previous: Option<&Snapshot>,
    next_version: u32,
    description: &str,
) -> SnapshotResult

struct SnapshotResult {
    snapshot: Snapshot,
    diffs: Vec<MigrationDiff>,
    migration_files: Vec<MigrationFile>,  // path + SQL content pairs
    graph: Option<MigrationGraph>,        // None for v1 baseline
    is_baseline: bool,
    no_changes: bool,                     // true if N>1 and diff is empty
}

struct MigrationFile {
    relative_path: PathBuf,               // e.g. "config/users.sql"
    content: String,                      // ALTER TABLE ...
}
```

**Logic:**
1. Extract table entities with `table_def` -> `Vec<TableSnapshot>`
2. Extract enum entities -> `Vec<EnumSnapshot>`
3. Build `Snapshot { version: next_version, tables, enums, timestamp, description }`
4. **If previous is None** (baseline): return with `is_baseline=true`, empty diffs
5. **If previous is Some**: `diff(previous, &snapshot)` -> diffs
   - If empty: return with `no_changes=true`
   - Generate migration SQL + graph from diffs

### I/O Boundary: `create_snapshot()`

```rust
/// Reads previous snapshot, calls prepare_snapshot, writes output files.
pub fn create_snapshot(design: &Design, description: &str) -> Result<SnapshotResult> {
    let prev = latest_snapshot(&design.project_dir)?;
    let version = next_version(&design.project_dir);
    let result = prepare_snapshot(&design.entities, prev.as_ref(), version, description);
    if result.no_changes { return Ok(result); }  // nothing to write
    write_snapshot_files(&design.project_dir, &result)?;
    update_design_version(&design.config_path, version)?;
    Ok(result)
}
```

### File Layout

```
snapshots/
  001.json              # baseline
  002.json              # after first change
  003.json

migrations/
  002/
    graph.json          # { fromVersion: 1, toVersion: 2, added: [...], altered: [...], dropped: [...] }
    config/
      lookup_values.sql # ALTER TABLE config.lookup_values ADD COLUMN notes TEXT
  003/
    graph.json
    staging/
      lookups.sql       # DROP TABLE staging.lookups CASCADE
```

---

## Module 3: Apply with Migrations (modify `design.rs`)

### Pure Logic: `build_execution_plan()`

```rust
/// Determines what to do given current state. No I/O.
fn build_execution_plan(
    entities: &[Entity],
    db_version: u32,
    latest_version: u32,
    pending_migrations: &[PendingMigration],
) -> ExecutionPlan

struct ExecutionPlan {
    strategy: ApplyStrategy,
    steps: Vec<ExecutionStep>,      // dependency-sorted
}

enum ApplyStrategy {
    Fresh,                          // db_version == 0: apply all, mark latest
    Migrate,                        // db_version < latest: run migrations + apply
    Current,                        // db_version == latest: idempotent apply only
}

enum ExecutionStep {
    CreateEntity(String),           // entity name — new, needs CREATE
    MigrateEntity {                 // altered — run migration SQL, then re-apply
        entity_name: String,
        migration_sql_path: PathBuf,
        migration_version: u32,
    },
    ApplyEntity(String),            // unchanged — idempotent apply
    DropEntity {                    // removed — run DROP SQL
        entity_name: String,
        drop_sql_path: PathBuf,
        migration_version: u32,
    },
    RecordMigration {               // bookkeeping
        version: u32,
        checksum: String,
    },
    SetVersion(u32),                // update _dbd_meta
}
```

**Plan logic:**

1. If `db_version == 0` (fresh):
   - Steps = `ApplyEntity` for all entities + `SetVersion(latest_version)`
2. If `db_version == latest_version` (current):
   - Steps = `ApplyEntity` for all entities (idempotent)
3. If `db_version < latest_version` (behind):
   - Collect `added` entities from migration graphs → `CreateEntity` steps
   - Collect `altered` entities → `MigrateEntity` steps
   - Collect `dropped` entities → `DropEntity` steps
   - All other entities → `ApplyEntity` steps
   - Build dependency graph across all sets using `entity.refers`
   - Topological sort
   - Append `RecordMigration` + `SetVersion` steps

### I/O Boundary: `apply()`

```rust
pub async fn apply(&self, adapter, name, dry_run) -> Result<()> {
    // I/O: read state
    let db_version = adapter.get_db_version().await?;
    let latest_version = self.config.project.version.unwrap_or(0);
    let pending = snapshot::pending_migrations(db_version, &self.project_dir);

    // Pure: build plan
    let plan = build_execution_plan(&self.entities, db_version, latest_version, &pending);

    // I/O: execute plan
    if dry_run { return print_plan(&plan); }
    execute_plan(adapter, &plan).await?;
}
```

### Migrate Command

`dbd migrate [--status] [--apply] [--to N] [--dry-run]`

- `--status`: Print table showing current DB version, latest version, and pending migration list
- `--apply`: Run migration pass only (migration steps from plan, no entity apply)
- `--to N`: Stop at version N instead of latest
- `--dry-run`: Print SQL with `-- Version N` headers, highlight `DROP` statements

### Dry Run Output Format

```
-- Migration v1 -> v2
-- [ALTER] config.lookup_values
ALTER TABLE config.lookup_values ADD COLUMN notes TEXT;

-- Migration v2 -> v3
-- [DROP] staging.lookups
DROP TABLE staging.lookups CASCADE;

-- [ALTER] config.orders
ALTER TABLE config.orders ADD CONSTRAINT fk_orders_users
  FOREIGN KEY (user_id) REFERENCES config.users(id);
```

---

## Enum Handling

**Current scope:** ADD VALUE only.

- New enum values: `ALTER TYPE schema.name ADD VALUE 'val'`
- Dropped enum values: Warning printed, no SQL generated
- Renamed enum values: Warning printed, no SQL generated

**Future work:**
- Full enum recreation: create new type, ALTER columns from old to text, drop old type, create new type, ALTER columns from text to new type
- Data patches: mechanism for shipping data corrections in a migration (e.g., UPDATE statements to fix values before enum type change)

---

## Test Scenarios

All tests below should be written BEFORE implementation (TDD). Tests use fixture projects with known DDL files and mock/tempdir snapshots.

### Diff Engine Tests (`diff.rs`)

#### D1: Identical snapshots produce empty diff
```
Given: snapshot A and snapshot B with same tables/columns/enums
When:  diff(A, B)
Then:  Vec<MigrationDiff> is empty
```

#### D2: New table detected
```
Given: snapshot A has [users], snapshot B has [users, orders]
When:  diff(A, B)
Then:  one MigrationDiff { entity: "config.orders", action: Add }
```

#### D3: Dropped table detected
```
Given: snapshot A has [users, orders], snapshot B has [users]
When:  diff(A, B)
Then:  one MigrationDiff { entity: "config.orders", action: Drop }
```

#### D4: Column added to existing table
```
Given: snapshot A users has [id, name], snapshot B users has [id, name, email]
When:  diff(A, B)
Then:  MigrationDiff { entity: "config.users", action: Change([
         FieldChange { field: "email", type: Column, action: Add(ColumnDef{...}) }
       ])}
```

#### D5: Column dropped from existing table
```
Given: snapshot A users has [id, name, email], snapshot B users has [id, name]
When:  diff(A, B)
Then:  FieldChange { field: "email", type: Column, action: Drop }
```

#### D6: Column type changed
```
Given: snapshot A users.email is VARCHAR(100), snapshot B users.email is TEXT
When:  diff(A, B)
Then:  FieldChange { field: "email", type: Column, action: Alter { old: VARCHAR(100), new: TEXT } }
```

#### D7: Column nullable changed
```
Given: snapshot A users.email nullable=false, snapshot B nullable=true
When:  diff(A, B)
Then:  FieldChange { field: "email", type: Column, action: Alter { old.nullable=false, new.nullable=true } }
```

#### D8: Column default changed
```
Given: snapshot A users.status default=None, snapshot B default='active'
When:  diff(A, B)
Then:  FieldChange with Alter carrying old/new defaults
```

#### D9: Constraint added
```
Given: snapshot A users has no unique constraint, snapshot B adds unique on email
When:  diff(A, B)
Then:  FieldChange { field: "uq_email", type: Constraint, action: Add(...) }
```

#### D10: Constraint dropped
```
Given: snapshot A users has unique on email, snapshot B does not
When:  diff(A, B)
Then:  FieldChange { field: "uq_email", type: Constraint, action: Drop }
```

#### D11: Index added
```
Given: snapshot A has no index on users.email, snapshot B adds idx_email
When:  diff(A, B)
Then:  FieldChange { field: "idx_email", type: Index, action: Add(...) }
```

#### D12: Index dropped
```
Given: snapshot A has idx_email, snapshot B does not
When:  diff(A, B)
Then:  FieldChange { field: "idx_email", type: Index, action: Drop }
```

#### D13: Constraint changed (drop old, apply creates new)
```
Given: snapshot A has unique on [email], snapshot B has unique on [email, name] (same constraint name)
When:  diff(A, B)
Then:  FieldChange { field: "uq_email", type: Constraint, action: Drop }
       (new constraint created by regular apply, not in migration)
```

#### D14: Index changed (drop old, apply creates new)
```
Given: snapshot A has btree idx_email on [email], snapshot B has hash idx_email on [email]
When:  diff(A, B)
Then:  FieldChange { field: "idx_email", type: Index, action: Drop }
       (new index created by regular apply)
```

#### D15: Enum value added
```
Given: snapshot A gender_type has [male, female], snapshot B has [male, female, other]
When:  diff(A, B)
Then:  FieldChange { field: "other", type: EnumValue, action: Add(EnumValue("other")) }
```

#### D16: Enum value dropped (warning)
```
Given: snapshot A gender_type has [male, female, other], snapshot B has [male, female]
When:  diff(A, B)
Then:  FieldChange { field: "other", type: EnumValue, action: Drop }
       AND warning is included
```

#### D17: New enum detected
```
Given: snapshot A has no enums, snapshot B has gender_type
When:  diff(A, B)
Then:  MigrationDiff { entity: "public.gender_type", action: Add }
```

#### D18: Enum dropped (warning)
```
Given: snapshot A has gender_type, snapshot B has no enums
When:  diff(A, B)
Then:  MigrationDiff { entity: "public.gender_type", action: Drop }
       AND warning is included
```

#### D19: Multiple changes on same table
```
Given: snapshot B adds a column, drops a constraint, adds an index to users
When:  diff(A, B)
Then:  single MigrationDiff with Change containing 3 FieldChanges
```

#### D20: Multiple tables changed
```
Given: snapshot B modifies users and orders
When:  diff(A, B)
Then:  two MigrationDiff entries
```

#### D21: Mixed add/alter/drop across entities
```
Given: snapshot A has [users, orders, temp]
       snapshot B has [users(modified), orders, payments(new)] — temp dropped
When:  diff(A, B)
Then:  3 MigrationDiffs: users=Change, temp=Drop, payments=Add
```

### SQL Generation Tests (`diff.rs`)

#### S1: Column add generates ALTER TABLE ADD COLUMN
```
Given: FieldChange column add with type TEXT, nullable, no default
Then:  "ALTER TABLE config.users ADD COLUMN email TEXT"
```

#### S2: Column add with NOT NULL and DEFAULT
```
Given: FieldChange column add, nullable=false, default='active'
Then:  "ALTER TABLE ... ADD COLUMN status VARCHAR(20) NOT NULL DEFAULT 'active'"
```

#### S3: Column drop generates ALTER TABLE DROP COLUMN
```
Given: FieldChange column drop "email"
Then:  "ALTER TABLE config.users DROP COLUMN email"
```

#### S4: Column type change generates ALTER COLUMN TYPE
```
Given: FieldChange alter from VARCHAR(100) to TEXT
Then:  "ALTER TABLE config.users ALTER COLUMN email TYPE TEXT"
```

#### S5: Nullable change generates SET/DROP NOT NULL
```
Given: FieldChange alter nullable false->true
Then:  "ALTER TABLE config.users ALTER COLUMN email DROP NOT NULL"

Given: FieldChange alter nullable true->false
Then:  "ALTER TABLE config.users ALTER COLUMN email SET NOT NULL"
```

#### S6: Default change generates SET/DROP DEFAULT
```
Given: FieldChange alter default None->'active'
Then:  "ALTER TABLE ... ALTER COLUMN status SET DEFAULT 'active'"

Given: FieldChange alter default 'active'->None
Then:  "ALTER TABLE ... ALTER COLUMN status DROP DEFAULT"
```

#### S7: Constraint add generates ADD CONSTRAINT
```
Given: FieldChange constraint add (unique on [email])
Then:  "ALTER TABLE config.users ADD CONSTRAINT uq_email UNIQUE (email)"
```

#### S8: Constraint drop generates DROP CONSTRAINT
```
Given: FieldChange constraint drop "uq_email"
Then:  "ALTER TABLE config.users DROP CONSTRAINT uq_email"
```

#### S9: FK constraint add generates full FK syntax
```
Given: FieldChange constraint add FK (user_id -> config.users.id)
Then:  "ALTER TABLE config.orders ADD CONSTRAINT fk_orders_users
        FOREIGN KEY (user_id) REFERENCES config.users(id)"
```

#### S10: Index add generates CREATE INDEX
```
Given: FieldChange index add (btree on [email], unique)
Then:  "CREATE UNIQUE INDEX idx_email ON config.users (email)"
```

#### S11: Index drop generates DROP INDEX
```
Given: FieldChange index drop "idx_email"
Then:  "DROP INDEX idx_email"
```

#### S12: Enum value add generates ALTER TYPE ADD VALUE
```
Given: FieldChange enum value add "other"
Then:  "ALTER TYPE public.gender_type ADD VALUE 'other'"
```

#### S13: Table drop generates DROP TABLE CASCADE
```
Given: MigrationDiff action=Drop, entity="staging.lookups"
Then:  "DROP TABLE staging.lookups CASCADE"
```

#### S14: Multiple field changes produce multi-statement SQL
```
Given: MigrationDiff with 3 FieldChanges (add col, drop col, add index)
Then:  3 SQL statements joined by newlines
```

### Snapshot Create Tests (`snapshot.rs`)

#### SC1: First snapshot creates baseline (v1)
```
Given: project with 2 tables and 1 enum, no existing snapshots
When:  create_snapshot(design, "initial")
Then:  snapshots/001.json exists
       contains version=1, 2 tables, 1 enum
       no migrations/ folder created
       design.yaml version updated to 1
```

#### SC2: Second snapshot with changes creates v2 + migration
```
Given: snapshot v1 exists, DDL now has added column on users table
When:  create_snapshot(design, "add email")
Then:  snapshots/002.json exists with updated table structure
       migrations/002/graph.json exists with altered=["config.users"]
       migrations/002/config/users.sql contains ALTER TABLE ADD COLUMN
       design.yaml version updated to 2
```

#### SC3: Snapshot with no changes is skipped
```
Given: snapshot v1 exists, DDL files unchanged
When:  create_snapshot(design, "no change")
Then:  no snapshots/002.json created
       no migrations/002/ created
       design.yaml version unchanged
       return message: "No changes detected"
```

#### SC4: Snapshot with new table
```
Given: snapshot v1 has [users], DDL now has [users, orders]
When:  create_snapshot(design, "add orders")
Then:  graph.json has added=["config.orders"], altered=[], dropped=[]
       no migration SQL file for orders (new table uses regular apply)
```

#### SC5: Snapshot with dropped table
```
Given: snapshot v1 has [users, temp], DDL now has [users] only
When:  create_snapshot(design, "drop temp")
Then:  graph.json has dropped=["staging.temp"]
       migrations/002/staging/temp.sql contains DROP TABLE
```

#### SC6: Snapshot with mixed changes
```
Given: snapshot v1, DDL adds orders, modifies users, drops temp
When:  create_snapshot(design, "restructure")
Then:  graph.json has added=["config.orders"], altered=["config.users"], dropped=["staging.temp"]
       migration SQL files for users (ALTER) and temp (DROP) only
```

#### SC7: Entity to TableSnapshot conversion includes all fields
```
Given: entity with columns, constraints (PK, FK, unique, check), indexes, comments
When:  convert to TableSnapshot
Then:  all fields preserved in snapshot
```

#### SC8: Entity to EnumSnapshot conversion
```
Given: enum entity with values ["a", "b", "c"]
When:  convert to EnumSnapshot
Then:  name, schema, values all captured
```

#### SC9: Snapshot serialization round-trip
```
Given: Snapshot with tables and enums
When:  serialize to JSON, deserialize back
Then:  identical structure
```

#### SC10: Design.yaml version update
```
Given: design.yaml with version=2
When:  create_snapshot produces v3
Then:  design.yaml now has version=3
```

### Apply with Migrations Tests (integration, requires mock adapter)

#### A1: Fresh env — apply all, mark latest version
```
Given: empty DB (version=0), design.yaml version=3, snapshots v1-v3
When:  apply()
Then:  all entities applied via apply_entity()
       _dbd_meta version set to 3
       no migration SQL executed
```

#### A2: Current env — idempotent apply only
```
Given: DB version=3, design.yaml version=3
When:  apply()
Then:  all entities applied (idempotent)
       no migration SQL executed
       _dbd_meta unchanged
```

#### A3: Behind by one version — single migration
```
Given: DB version=1, design.yaml version=2
       migration v2 alters config.users (adds column)
When:  apply()
Then:  migration SQL for config.users runs
       then all entities applied
       _dbd_meta updated to 2
```

#### A4: Behind by multiple versions — sequential migrations
```
Given: DB version=1, design.yaml version=3
       migration v2 alters users, migration v3 alters orders
When:  apply()
Then:  v2 migration runs first, then v3
       all entities applied after
       _dbd_meta updated to 3
```

#### A5: Migration with new table dependency — interleaved
```
Given: DB version=1, design.yaml version=2
       migration v2: added=[config.users], altered=[config.orders adds FK to users]
When:  apply()
Then:  config.users CREATE runs before config.orders ALTER
       (dependency ordering respected)
```

#### A6: Migration with table drop
```
Given: DB version=1, migration v2 drops staging.temp
When:  apply()
Then:  DROP TABLE staging.temp runs
       staging.temp not in entity apply list
```

#### A7: Dry run prints SQL without executing
```
Given: DB version=1, pending migrations
When:  apply(dry_run=true)
Then:  SQL printed with version headers
       DROP statements highlighted
       no DB changes made
```

#### A8: Apply with --name filter still runs migrations
```
Given: DB version=1, pending migrations alter config.users
When:  apply(name="config.users")
Then:  migration for config.users runs
       only config.users entity applied
```

### Migrate Command Tests

#### M1: Status shows versions and pending list
```
Given: DB version=1, latest=3
When:  migrate --status
Then:  output shows "DB: v1, Latest: v3, Pending: v2, v3"
```

#### M2: Apply runs migrations only
```
Given: DB version=1, latest=3
When:  migrate --apply
Then:  migrations v2 and v3 run
       _dbd_meta updated to 3
       entity apply NOT run
```

#### M3: Apply with --to limits version
```
Given: DB version=1, latest=3
When:  migrate --apply --to 2
Then:  only migration v2 runs
       _dbd_meta updated to 2
```

#### M4: Dry run prints SQL
```
Given: DB version=1, pending migration v2
When:  migrate --apply --dry-run
Then:  SQL printed, no DB changes
```

#### M5: No pending migrations
```
Given: DB version=3, latest=3
When:  migrate --status
Then:  output: "Up to date at v3"
```

### Edge Cases

#### E1: Snapshot with self-referencing FK table
```
Given: table categories with parent_id -> categories.id
When:  create_snapshot + diff
Then:  self-ref handled correctly, no circular diff
```

#### E2: Constraint matching without name
```
Given: unnamed PK in v1, unnamed PK in v2 with same columns
When:  diff
Then:  matched as same constraint, no change
```

#### E3: Multiple column changes on same table
```
Given: add col A, drop col B, alter col C type on same table
When:  diff + generate SQL
Then:  single migration file with 3 ALTER statements
```

#### E4: Enum with no changes
```
Given: identical enum in both snapshots
When:  diff
Then:  not included in diff output
```

#### E5: Empty project (no tables, no enums)
```
Given: project with only schemas and extensions
When:  create_snapshot
Then:  snapshot created with empty tables/enums arrays
```

#### E6: Snapshot backwards compatibility (no enums field)
```
Given: old snapshot JSON without "enums" field
When:  read_snapshot
Then:  deserializes with enums defaulting to empty vec
```

---

## Files Modified/Created

| File | Action | Purpose |
|------|--------|---------|
| `crates/dbd-core/src/diff.rs` | Create | Diff engine: types, diff logic, SQL generation |
| `crates/dbd-core/src/snapshot.rs` | Modify | Add EnumSnapshot, create_snapshot(), update Snapshot/MigrationGraph |
| `crates/dbd-core/src/design.rs` | Modify | Updated apply() with interleaved migration logic |
| `crates/dbd-core/src/config.rs` | Modify | Add version to ProjectConfig |
| `crates/dbd-core/src/lib.rs` | Modify | Export diff module |
| `crates/dbd-cli/src/cli.rs` | Modify | Wire snapshot create, migrate subcommands |
| `crates/dbd-cli/src/commands.rs` | Modify | Implement cmd_snapshot_create, cmd_migrate_* |

## Future Work

- **Enum recreation:** DROP old type, CREATE new type, migrate column data (requires text intermediary)
- **Data patches:** Mechanism for shipping UPDATE/DELETE corrections in a migration folder (e.g., `migrations/NNN/patches/fix_status_values.sql`)
- **Column rename detection:** Heuristic matching (same type, adjacent position) to suggest RENAME instead of DROP+ADD
- **Migration rollback:** Reverse migration generation (undo ALTERs)
