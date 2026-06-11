# dbd-rs — Rust CLI Design Document

## Goal

Build a standalone Rust binary (`dbd`) that replicates the Node.js `dbd` CLI. Single binary, zero runtime dependencies. Cross-compilable for Linux, macOS, and Windows.

---

## Core principle: one parse, one representation

```mermaid
flowchart LR
    files["Files\n(DDL, YAML, CSV)"]
    parser["Parser\n(sqlparser-rs)"]
    ir["Internal Representation\n(Entity + TableDef)"]

    dbml["DBML Generator"] --> dbml_out[".dbml files"]
    graph["Dependency Graph"] --> graph_out["apply order\ngraph command"]
    snap["Snapshot Builder"] --> snap_out["snapshots/NNN.json"]
    diff["Migration Diff"] --> diff_out["ALTER SQL"]
    adapter["Adapter"] --> db["Database\n(apply, import, export)"]

    files --> parser --> ir
    ir --> dbml
    ir --> graph
    ir --> snap
    ir --> diff
    ir --> adapter
```

Every DDL file is parsed **once** into an `Entity` with an optional `TableDef`. That same structure drives every feature — DBML, graph, apply, snapshot, diff, migration. No re-parsing, no parallel type hierarchies, no lossy intermediate formats.

The Node.js version parses SQL multiple times for different consumers (entity extraction, snapshot building, DBML conversion via `@dbml/core`), each with its own partial representation. The Rust version eliminates this: `TableDef` captures the full column/constraint/index/FK structure including details the Node.js version drops (FK actions, CHECK constraints, identity columns, enum values).

### Parallel file parsing

The parse phase is embarrassingly parallel — each DDL file is independent until reference resolution. Use `rayon` (data parallelism) to parse all files concurrently:

```
scan ddl/                          ← sequential (fast, single walkdir traversal)
  → Vec<PathBuf>
  → rayon::par_iter()              ← parallel: read file + sqlparser::parse per file
    → Vec<Entity>                     (CPU-bound, scales with core count)
  → resolve references             ← sequential (needs all entities)
  → sort by dependencies           ← sequential
  → ready for all consumers
```

```rust
use rayon::prelude::*;

let entities: Vec<Entity> = ddl_files
    .par_iter()                           // parallel iterator
    .map(|path| {
        let sql = std::fs::read_to_string(path)?;
        parser.parse_entity(path, &sql)   // sqlparser is pure Rust, thread-safe
    })
    .collect::<Result<Vec<_>>>()?;
```

**Where parallelism helps:**

| Phase | Strategy | Rationale |
|---|---|---|
| File read + parse | `rayon::par_iter` | CPU-bound (parse + extract per file), no shared state |
| Snapshot table parsing | `rayon::par_iter` | Same: independent per table |
| Apply entities | **sequential** | Must respect dependency order, FK constraints |
| Import data | **sequential** | Procedures depend on prior imports |
| DBML emit | **sequential** | Fast string assembly, not worth parallelizing |

**Why `rayon` not `tokio::spawn`?** Parsing is CPU-bound, not IO-bound. `rayon` uses a work-stealing thread pool optimized for compute. `tokio` is for async IO. The adapter (database operations) uses `tokio`; the parser uses `rayon`. Both are pure Rust — no C FFI threading concerns.

---

## Scope — Phase 1

Phase 1 covers the core commands that work with a local or remote source:

| Command      | Priority | Notes                                |
| ------------ | -------- | ------------------------------------ |
| `inspect`    | P0       | Config parsing, DDL scanning, validation |
| `apply`      | P0       | Full entity apply with migrations    |
| `import`     | P0       | CSV/TSV/JSONL data loading           |
| `deploy`     | P0       | Fetch + apply + import               |
| `combine`    | P0       | Combine DDL into single file         |
| `graph`      | P1       | Dependency graph as JSON             |
| `dbml`       | P1       | DBML generation                      |
| `snapshot`   | P1       | Schema snapshot creation             |
| `migrate`    | P1       | Standalone migration apply           |
| `export`     | P1       | Data export                          |
| `doctor`     | P2       | Stale config detection               |
| `reset`      | P2       | Schema teardown                      |
| `grants`     | P2       | Supabase grants                      |
| `policies`   | P2       | RLS policy application               |
| `init`       | P2       | Project scaffolding                  |

---

## Architecture

### Crate structure — workspace with library core

The project is a Cargo workspace with a **library crate** (`dbd-core`) and a **binary crate** (`dbd-cli`). This enables three consumption modes:

1. **CLI binary** — `dbd` command-line tool (what users install)
2. **Library embed** — Rust apps `use dbd_core::Design` to run apply/migrate/import programmatically
3. **UI app** — a future desktop/web app imports `dbd-core` for orchestration + visualization data

```
dbd-rs/
├── Cargo.toml                  # Workspace root
├── crates/
│   ├── dbd-core/               # Library crate — all logic lives here
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs              # Public API re-exports
│   │       ├── config.rs           # design.yaml parsing and entity discovery
│   │       ├── entity.rs           # Entity types, creation, validation
│   │       ├── scanner.rs          # File system scanning (ddl/, import/, policies/)
│   │       ├── design.rs           # Design orchestrator — the main entry point
│   │       ├── dependency.rs       # Dependency graph, topological sort, cycle detection
│   │       ├── references.rs       # Reference matching and resolution
│   │       ├── snapshot.rs         # Snapshot creation and migration file I/O
│   │       ├── migration.rs        # Schema diff, migration SQL generation
│   │       ├── script.rs           # DDL generation, reset/grant script builders
│   │       ├── github.rs           # GitHub source download, caching
│   │       ├── dbml.rs             # DBML conversion
│   │       ├── error.rs            # Error types (thiserror)
│   │       │
│   │       ├── parser/             # Reads DDL files → internal representation
│   │       │   ├── mod.rs          # SqlParser trait + dialect dispatcher
│   │       │   ├── extractors.rs   # AST → TableDef/Entity extraction logic
│   │       │   ├── tables.rs       # Table column/constraint/FK extraction
│   │       │   ├── views.rs        # View dependency extraction
│   │       │   ├── procedures.rs   # Function/procedure reads/writes extraction
│   │       │   ├── indexes.rs      # Index extraction
│   │       │   └── triggers.rs     # Trigger extraction
│   │       │
│   │       └── adapter/            # Applies internal representation → target
│   │           ├── mod.rs          # DatabaseAdapter trait + factory
│   │           ├── postgres.rs     # PostgreSQL (sqlx, COPY streaming, catalog queries)
│   │           ├── supabase.rs     # Supabase (extends postgres, filters managed infra)
│   │           ├── sqlite.rs       # SQLite (rusqlite, subset features)
│   │           └── convex.rs       # Convex (generates TypeScript schema, no SQL)
│   │
│   └── dbd-cli/                # Binary crate — thin CLI shell
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs             # Entry point, tokio runtime bootstrap
│           ├── cli.rs              # clap argument structs
│           └── commands.rs         # Command handlers (maps CLI args → dbd_core calls)
│
└── tests/                      # Integration tests (against dbd-core)
    ├── config_test.rs
    ├── entity_test.rs
    ├── scanner_test.rs
    ├── dependency_test.rs
    ├── references_test.rs
    ├── snapshot_test.rs
    ├── migration_test.rs
    ├── github_test.rs
    ├── parser/
    │   ├── tables_test.rs
    │   ├── views_test.rs
    │   └── procedures_test.rs
    ├── adapter/
    │   ├── classify_test.rs    # Tests adapter.classify_reference()
    │   └── postgres_test.rs    # Adapter integration tests (requires DB)
    └── fixtures/               # Test DDL files, design.yaml samples
        ├── design.yaml
        ├── ddl/
        │   ├── table/config/lookups.ddl
        │   ├── table/config/lookup_values.ddl
        │   ├── view/config/genders.ddl
        │   └── procedure/staging/import_lookups.ddl
        └── import/
            └── staging/lookups.csv
```

### Parser vs adapter — independent concerns

The parser and adapter are separate because they serve different roles and don't have to match:

```
parser/                          adapter/
  Reads DDL files                  Writes to target
  Input: SQL text                  Input: Entity + TableDef
  Dialect: of the DDL files        Target: where we deploy
  No DB connection needed          Needs DB connection (usually)
  Pure, stateless                  Stateful (connection, catalog)
```

**The parser understands the source dialect.** DDL files are written in a specific SQL dialect (currently PostgreSQL). The parser uses `sqlparser-rs` configured for that dialect to produce `Entity` + `TableDef`.

**The adapter understands the target.** It takes the internal representation and applies it to the target system. The adapter also owns catalog knowledge (classify_reference, resolve_entity).

**They're independent.** Convex proves this: PostgreSQL DDL in (parser), TypeScript schema out (adapter). The parser doesn't need to know about Convex, and the Convex adapter doesn't need to parse SQL.

```
DDL files (PostgreSQL SQL)
  → parser (PostgreSQL dialect)
    → Entity + TableDef (internal representation)
      ├──→ postgres adapter   → executes SQL on PostgreSQL
      ├──→ supabase adapter   → executes SQL, filters managed infra
      ├──→ sqlite adapter     → translates to SQLite-compatible SQL
      └──→ convex adapter     → generates convex/schema.ts (no SQL)
```

**Not source vs target adapters** — there's only one adapter (the target). The source is always DDL files on disk, read by the parser. The adapter never reads DDL files directly.

**Convex is a transformer, not a database.** It's an adapter in the trait sense (implements `DatabaseAdapter`) but its `apply_entities()` generates files instead of executing SQL. `execute_script()` is a no-op. This is fine — the trait allows it via `prefers_batch_apply() → true`.

### Why a workspace?

| Concern                          | Single crate                     | Workspace (chosen)                        |
| -------------------------------- | -------------------------------- | ----------------------------------------- |
| Embed in other Rust apps         | Must depend on the binary crate  | `dbd-core` is a clean library dependency  |
| CLI-specific deps (clap)         | Pulled into library consumers    | Isolated to `dbd-cli` only                |
| Future UI app                    | Extracts core later (refactor)   | `dbd-core` is ready to import from day 1  |
| Compile times                    | Monolithic rebuild               | Only changed crate recompiles             |
| Publish to crates.io             | One package, mixed concerns      | `dbd-core` and `dbd-cli` publish separately |
| Testability                      | Integration tests need the binary| Library tests are direct function calls   |

### Public API surface (`dbd-core`)

The library exposes a layered API — consumers pick the level they need:

```rust
// High-level: one-call operations (deploy, apply, inspect)
use dbd_core::Design;

let design = Design::from_config(Path::new("design.yaml"), Some(db_url), "prod").await?;
design.apply(None, false).await?;          // Apply all entities + migrations
design.import_data(None, false).await?;    // Import staging data
let report = design.report(None);          // Inspect: get errors/warnings

// Mid-level: individual subsystems
use dbd_core::config;
use dbd_core::dependency;
use dbd_core::snapshot;

let config = config::read(Path::new("design.yaml"))?;
let sorted = dependency::sort_by_dependencies(&entities);
let pending = snapshot::pending_migrations(db_version, Path::new("."))?;

// Low-level: parser, adapter trait, entity types
use dbd_core::parser::PostgresParser;
use dbd_core::adapter::DatabaseAdapter;
use dbd_core::entity::{Entity, EntityType};
```

### Embedding example — auto-migration in a Rust web app

```rust
// In a Rust web server's startup routine
use dbd_core::Design;

async fn run_migrations(database_url: &str) -> anyhow::Result<()> {
    let design = Design::from_config(
        Path::new("database/design.yaml"),
        Some(database_url),
        "prod",
    ).await?;
    design.apply(None, false).await?;
    design.import_data(None, false).await?;
    Ok(())
}
```

### UI app consumption — graph + report data

The library returns structured data that a UI can render:

```rust
use dbd_core::Design;
use dbd_core::dependency::GraphResult;

// Get dependency graph for visualization
let graph: GraphResult = design.graph(None);
// graph.nodes: Vec<{name, type, schema}>
// graph.edges: Vec<{from, to}>
// graph.layers: Vec<Vec<String>>

// Get validation report for dashboard
let report = design.report(None);
// report.issues: Vec<Entity>   (entities with errors)
// report.warnings: Vec<Entity> (entities with warnings)

// Get snapshot diff for migration preview
use dbd_core::snapshot;
let pending = snapshot::pending_migrations(db_version, project_dir)?;
// Each migration: { from_version, to_version, altered, dropped }
```

---

## Key Types

### Entity

The central data structure. All DDL objects flow through this type.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_type: EntityType,
    pub name: String,           // Fully qualified: "schema.name"
    pub schema: Option<String>,
    pub file: Option<PathBuf>,
    pub format: Option<String>, // "ddl" or "sql"
    pub refers: Vec<String>,    // Declared dependencies (from design.yaml)
    pub references: Vec<Reference>,  // Parsed from SQL
    pub search_paths: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reads: Vec<String>,     // Tables read (procedures only)
    pub writes: Vec<String>,    // Tables written (procedures only)
    pub table_def: Option<TableDef>,  // Parsed table structure (tables only)
    pub enum_values: Vec<EnumValue>,  // Enum variants (enums only)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityType {
    Schema,
    Extension,
    Role,
    Enum,
    Table,
    View,
    Function,
    Procedure,
    External,
    Import,
    Export,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub ref_type: Option<String>,
}
```

### Parsed table structure (TableDef)

The parser populates `TableDef` with the full column/constraint/index detail needed for both DBML generation and snapshot diffing. The Node.js version stores this information spread across extractors and loses FK actions — the Rust version captures everything in one structure.

```rust
/// Full parsed table definition — populated by the parser, consumed by
/// DBML generator, snapshot builder, and migration diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDef {
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub indexes: Vec<IndexDef>,
    pub comments: TableComments,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_pk: bool,
    pub is_unique: bool,
    pub is_identity: bool,         // SERIAL / GENERATED ALWAYS AS IDENTITY
    pub comment: Option<String>,   // COMMENT ON COLUMN
    pub inline_fk: Option<ForeignKey>,  // Inline REFERENCES (single-column FK)
}

/// Foreign key — captures the full detail the Node.js version drops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: Option<String>,          // Constraint name (if named)
    pub columns: Vec<String>,          // FK columns on this table
    pub ref_schema: Option<String>,    // Referenced table schema
    pub ref_table: String,             // Referenced table name
    pub ref_columns: Vec<String>,      // Referenced columns
    pub on_delete: Option<FkAction>,   // ON DELETE action
    pub on_update: Option<FkAction>,   // ON UPDATE action
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FkAction {
    Cascade,
    Restrict,
    SetNull,
    SetDefault,
    NoAction,
}

/// Table-level constraints (PRIMARY KEY, UNIQUE, FOREIGN KEY, CHECK)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TableConstraint {
    PrimaryKey {
        name: Option<String>,
        columns: Vec<String>,
    },
    Unique {
        name: Option<String>,
        columns: Vec<String>,
    },
    ForeignKey(ForeignKey),
    Check {
        name: Option<String>,
        expression: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: Option<String>,
    pub columns: Vec<IndexColumn>,
    pub unique: bool,
    pub index_type: Option<IndexType>,   // btree (default) or hash
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexColumn {
    pub name: String,
    pub order: Option<SortOrder>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum IndexType { Btree, Hash }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SortOrder { Asc, Desc }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TableComments {
    pub table: Option<String>,           // COMMENT ON TABLE
    pub columns: HashMap<String, String>, // COMMENT ON COLUMN
}

/// Enum variant with optional note
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    pub name: String,
    pub note: Option<String>,
}
```

#### What this fixes vs Node.js

| Data point | Node.js parser | Rust parser |
|---|---|---|
| FK on_delete / on_update | **not captured** | `FkAction` enum on every FK |
| FK constraint name | captured on table-level, lost on inline | always captured |
| Column comments | extracted separately in comment pass | inline on `ColumnDef.comment` |
| Table comments | extracted separately | inline on `TableComments.table` |
| CHECK constraints | not captured | `TableConstraint::Check` |
| SERIAL / identity | not detected | `ColumnDef.is_identity` |
| Enum values | not on Entity | `Entity.enum_values` |
| Index type (hash/btree) | captured in snapshot only | on `IndexDef` from parse |

This structure serves three consumers:
1. **DBML generator** — `table_def` has everything needed to emit Tables, Columns, Refs, Indexes, Enums
2. **Snapshot builder** — `table_def` maps directly to `TableSnapshot` (no second parse pass)
3. **Migration diff** — column/constraint/index comparison uses the same types

### Config (design.yaml) — restructured

The Node.js version conflates source dialect, target platform, and project metadata into one `project.database` field. Supabase and Convex config are scattered as top-level keys. The Rust version separates these cleanly.

#### New design.yaml structure

```yaml
# ── Project identity ──────────────────────────────────
project:
  name: MyProject
  note: E-commerce database schema    # Optional, used in DBML

# ── Source: what dialect are the DDL files written in ──
source:
  dialect: postgresql                  # Parser dialect (default: postgresql)

# ── Target: where to deploy ───────────────────────────
# The first target listed is used (platform config + default url).
# Adapter is chosen by the url scheme; override the url per run with -d.
target:
  postgres:
    url: $DATABASE_URL
    extensions:                        # Target-specific: which extensions to install
      - uuid-ossp
      - name: postgis
        schema: extensions
    roles:                             # Target-specific: database roles
      - name: advanced
        refers: [basic]
      - name: basic

  # supabase:
  #   url: $SUPABASE_DB_URL
  #   schemas: [public, config]        # PostgREST-exposed schemas
  #   extensions:                      # Only non-managed extensions
  #     - postgis
  #   roles:
  #     - name: app_user
  #   grants:
  #     config:
  #       anon: [usage, select]
  #       service_role: [usage, all]

  # convex:
  #   skip_schemas: [public]
  #   # No extensions, roles, or grants — not applicable

  # sqlite:
  #   path: ./local.db
  #   # No extensions, roles, or grants — not applicable

# ── Schema declarations (universal) ──────────────────
schemas:
  - config
  - staging
  - extensions

# ── External entities (FK stubs) ──────────────────────
external:
  - name: auth.users
    note: Supabase managed authentication table
    columns:
      - id: uuid

# ── Data import ───────────────────────────────────────
import:
  staging: [staging]                   # Schemas allowed for import
  options:
    truncate: true
    null_value: ''
    format: csv
  tables:
    - staging.lookups
    - staging.lookup_values:
        truncate: false
  after:
    - import/loader.sql

# ── Data export ───────────────────────────────────────
export:
  - config.lookups
  - config.lookup_values:
      format: jsonl

# ── DBML generation ───────────────────────────────────
dbml:
  base:
    exclude:
      schemas: [staging, extensions]
  core:
    include:
      schemas: [config]

# ── Scopes: named entity subsets (universal) ──────────
# Deploy one design to multiple databases. Orthogonal to `target` (DB
# platform) and the connection — pair at run time: `--scope hub -d $URL`.
scopes:
  hub:
    includes: [config, app.users]   # schema (all its entities) or specific entity
    deps: report                    # report (default) | include
  reporting:
    excludes: [staging]

# ── Ignore list (reference classification) ────────────
ignore:
  - bfs
  - my_company.*
```

#### What changed vs Node.js

| Concern | Node.js (current) | Rust (new) |
|---|---|---|
| Database dialect | `project.database: PostgreSQL` (conflated) | `source.dialect: postgresql` |
| Target platform | `project.database` again | `target.postgres:` / `target.supabase:` / etc. |
| Connection URL | CLI flag only | `target.<name>.url` (with CLI override) |
| Extensions | `extensions:` top-level (all targets) | `target.<name>.extensions:` (per target) |
| Roles | `roles:` top-level (all targets) | `target.<name>.roles:` (per target) |
| Grants | `schemas:` with grants + `supabase:` | `target.supabase.grants:` |
| Supabase schemas | `supabase:` top-level | `target.supabase.schemas:` |
| Convex options | `convex:` top-level | `target.convex:` |
| Staging schemas | `project.staging` | `import.staging` (where it belongs) |
| DBML config | `project.dbdocs` | `dbml:` top-level (own concern) |
| Ignore list | `project.ignore` (undocumented) | `ignore:` top-level |
| Null value key | `nullValue` (camelCase) | `null_value` (snake_case, Rust convention) |

**What's universal vs target-specific:**

| Universal (top-level) | Target-specific |
|---|---|
| `schemas` — logical groupings, always needed | `extensions` — Postgres/Supabase only |
| `external` — FK stubs for any target | `roles` — Postgres/Supabase only |
| `import` / `export` — data operations | `grants` — Supabase only |
| `dbml` — documentation generation | `url` / `path` — connection config |
| `scopes` — named entity subsets for deploy | `skip_schemas` — per-target entity filtering |
| `ignore` — classification overrides | — |

#### Key design decisions

**`source.dialect` vs `target`** — the source is the DDL language. The target is where you deploy. They're independent: PostgreSQL DDL can deploy to Convex (TypeScript generation). Default dialect is `postgresql` since that's what existing projects use.

**Multiple targets** — the config schema allows several `target.<name>` entries, but **the first one listed is the one used** — there is no run-time target-name selector. The adapter is chosen by the connection URL scheme (`postgres://` / `sqlite://` / `convex:`), and the URL is overridable per run with `--database`/`$DATABASE_URL`. To deploy one design to several databases, pair `--database` with `scopes` rather than relying on multiple target blocks. (A `--target` selector could be wired through `Config::get_target(Some(name))`, which already exists, if per-name selection is wanted later.)

**Connection URL in config** — optionally declare the URL per target. Environment variable references (`$DATABASE_URL`) are expanded at runtime. CLI `--database` flag overrides. This means `dbd apply` can work without any CLI flags if the config has the URL.

**Backward compatibility** — the Rust parser can accept both the old and new config format by detecting `project.database` (old) vs `source.dialect` (new). Warn on old format, migrate with `dbd doctor --fix`.

**No schema_prefix** — an earlier spike added a `schema_prefix` config option intended for multi-tenant deployments (turning `app` into `tenant_42_app`). It was removed for two reasons: first, `design.yaml` lives in source control and is shared across all tenants, so a static prefix in the file cannot work — every tenant would overwrite each other's value. Second, the correct multi-tenant pattern is a separate database (or connection string) per tenant, not shared schemas with a prefix. A per-run CLI flag (`--schema-prefix`) was also considered but discarded: with 100 tenants, running `dbd apply --schema-prefix tenant_42` for each tenant in sequence is operationally tedious and error-prone. The right solution is per-tenant connection strings and a single `design.yaml` with no tenant-specific knobs.

#### Rust types

```rust
#[derive(Debug, Deserialize)]
pub struct DesignConfig {
    pub project: ProjectConfig,
    pub source: SourceConfig,
    pub target: IndexMap<String, TargetConfig>,  // Ordered — first is default
    pub schemas: Vec<SchemaEntry>,
    pub external: Vec<ExternalEntry>,
    pub import: ImportConfig,
    pub export: Vec<ExportEntry>,
    pub dbml: Option<HashMap<String, DbmlDocConfig>>,
    pub ignore: Vec<String>,
    // Note: extensions, roles, grants live under target — not here
}

#[derive(Debug, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourceConfig {
    #[serde(default = "default_dialect")]
    pub dialect: String,  // "postgresql", "sqlite", etc.
}

#[derive(Debug, Deserialize)]
pub struct TargetConfig {
    // Connection
    pub url: Option<String>,               // Postgres/Supabase (env var refs expanded)
    pub path: Option<PathBuf>,             // SQLite

    // Postgres / Supabase
    pub extensions: Vec<ExtensionEntry>,
    pub roles: Vec<RoleEntry>,

    // Supabase-specific
    pub schemas: Option<Vec<String>>,      // PostgREST-exposed schemas
    pub grants: Option<HashMap<String, GrantConfig>>,  // schema → role → perms

    pub skip_schemas: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ImportConfig {
    pub staging: Vec<String>,          // Schemas allowed for import
    pub options: ImportOptions,
    pub tables: Vec<ImportTableEntry>,
    pub after: Vec<String>,
}

/// Schema entry: plain string or object with grants
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SchemaEntry {
    Name(String),
    WithGrants {
        // First key is schema name, value has grants
    },
}

/// Extension: string or object with schema
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ExtensionEntry {
    Name(String),
    WithSchema { name: String, schema: String },
}
```

### Snapshot & Migration

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub description: String,
    pub timestamp: String,
    pub tables: Vec<TableSnapshot>,
}

/// TableSnapshot is built from TableDef — same column/constraint/index types,
/// plus the table identity (name, schema). No duplicate type hierarchies.
#[derive(Debug, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub name: String,
    pub schema: String,
    pub columns: Vec<ColumnDef>,          // Reuses ColumnDef from parser
    pub indexes: Vec<IndexDef>,           // Reuses IndexDef from parser
    pub table_constraints: Vec<TableConstraint>, // Reuses TableConstraint from parser
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SchemaDiff {
    pub from_version: u32,
    pub to_version: u32,
    pub added_tables: Vec<TableSnapshot>,
    pub dropped_tables: Vec<TableSnapshot>,
    pub altered_tables: Vec<AlteredTable>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MigrationGraph {
    pub from_version: u32,
    pub to_version: u32,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
}
```

Note: `TableSnapshot` reuses `ColumnDef`, `IndexDef`, and `TableConstraint` — no parallel type hierarchies. The parser populates `Entity.table_def`, the snapshot builder copies it into `TableSnapshot`, and the diff compares using the same types.

### Adapter trait

```rust
#[async_trait]
pub trait DatabaseAdapter: Send + Sync {
    // Lifecycle
    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn test_connection(&self) -> Result<bool>;

    // DDL execution
    async fn execute_script(&self, sql: &str) -> Result<()>;
    async fn execute_file(&self, path: &Path) -> Result<()>;
    async fn apply_entity(&self, entity: &Entity) -> Result<()>;
    async fn apply_entities(&self, entities: &[Entity]) -> Result<()>;
    fn prefers_batch_apply(&self) -> bool { false }

    // Data operations (COPY via sqlx streaming — no psql dependency)
    async fn import_data(&self, entity: &Entity, dry_run: bool) -> Result<()>;
    async fn export_data(&self, entity: &Entity) -> Result<()>;
    async fn batch_export(&self, entities: &[Entity]) -> Result<()>;

    // Catalog — adapter-owned knowledge of its target environment
    async fn load_catalog(&mut self) -> Result<()>;
    fn classify_reference(&self, name: &str, installed_extensions: &[String]) -> ReferenceClass;
    async fn resolve_entity(&self, name: &str) -> Result<Option<CatalogEntry>>;

    // Migration tracking
    async fn ensure_migrations_table(&self) -> Result<()>;
    async fn get_db_version(&self) -> Result<u32>;
    async fn apply_migration(&self, version: u32, sql: &str, desc: &str, checksum: &str) -> Result<()>;
    async fn clear_project_migrations(&self) -> Result<()>;
}
```

### Parser trait

```rust
pub trait SqlParser: Send + Sync {
    /// Parse a SQL script and extract entity identity, references, and table structure
    fn parse_entity(&self, path: &Path, sql: &str) -> Result<Entity>;

    /// Parse a table DDL into a snapshot structure (reuses TableDef from Entity)
    fn parse_table_snapshot(&self, entity: &Entity) -> Result<TableSnapshot>;

    /// Parse a view DDL and extract column names
    fn parse_view_columns(&self, entity: &Entity) -> Result<Vec<String>>;
}

// Note: classify_reference() is on DatabaseAdapter, not SqlParser.
// The adapter knows what's native to its target environment.
```

---

## Dependencies (Cargo.toml)

### Workspace root

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT"
repository = "https://github.com/jerrythomas/dbd"

[workspace.dependencies]
# Shared across crates
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
tokio = { version = "1", features = ["full"] }
thiserror = "2"
anyhow = "1"
```

### `dbd-core` (library)

```toml
[package]
name = "dbd-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
serde_yaml.workspace = true
tokio.workspace = true
thiserror.workspace = true

# PostgreSQL
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres"] }

# SQL Parsing
sqlparser = { version = "0.61", features = ["visitor"] }  # Pure Rust, multi-dialect

# File system & parallelism
walkdir = "2"
rayon = "1"              # Parallel file parsing (work-stealing thread pool)

# HTTP (GitHub source)
reqwest = { version = "0.12", features = ["json", "stream"] }
flate2 = "1"
tar = "0.4"

# Utilities
async-trait = "0.1"
sha2 = "0.10"            # SHA-256 for migration checksums
tempfile = "3"
dirs = "5"               # XDG cache directory
chrono = "0.4"

[dev-dependencies]
insta = "1"              # Snapshot testing
tempfile = "3"
tokio = { version = "1", features = ["full", "test-util"] }
```

### `dbd-cli` (binary)

```toml
[package]
name = "dbd-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "dbd"
path = "src/main.rs"

[dependencies]
dbd-core = { path = "../dbd-core" }
clap = { version = "4", features = ["derive", "env"] }
tokio.workspace = true
anyhow.workspace = true

[dev-dependencies]
assert_cmd = "2"         # CLI integration tests
predicates = "3"
tempfile = "3"
```

Note: `clap`, `anyhow`, and CLI-specific deps live only in `dbd-cli`. Library consumers don't pull them in.

### Key dependency choices

| Need                | Node.js             | Rust                 | Rationale                                     |
| ------------------- | ------------------- | -------------------- | --------------------------------------------- |
| CLI parsing         | sade                | clap (derive)        | Industry standard, derive macros              |
| YAML parsing        | js-yaml             | serde_yaml           | Serde ecosystem, zero-copy                    |
| SQL parsing         | pgsql-parser (WASM) | sqlparser-rs         | Pure Rust, typed AST, multi-dialect, no C dep |
| PostgreSQL          | postgres (npm)      | sqlx                 | Compile-time safety, async, connection pool   |
| FP utilities        | ramda               | Iterator chains      | Rust iterators are the idiomatic equivalent   |
| Parallel parsing    | —  (single-threaded) | rayon                | Work-stealing thread pool for CPU-bound parse |
| HTTP                | curl (child proc)   | reqwest              | Pure Rust, async, no external dependency      |
| Tarball extraction  | tar (child proc)    | flate2 + tar         | Pure Rust, no curl/tar binaries needed        |
| Hashing             | crypto.createHash   | sha2                 | Pure Rust SHA-256                             |
| Error handling      | throw/catch         | thiserror + anyhow   | Typed errors + ergonomic propagation          |

---

## Reference classification — adapter-owned

The Node.js version uses a standalone classifier with hardcoded lists of PostgreSQL builtins and extension functions. This breaks when:
- A function isn't in the list (e.g., `bfs`, `width_bucket`)
- An extension adds functions unknown to the classifier (PostGIS has 500+ functions)
- A different database is used (Oracle's `decode` vs PostgreSQL's `coalesce`)

### The fix: the adapter owns classification

Each adapter knows what's native to its target. The classifier isn't a standalone module — it's part of the `DatabaseAdapter` trait.

```rust
#[async_trait]
pub trait DatabaseAdapter: Send + Sync {
    // ... existing methods ...

    /// Load the catalog of built-in functions, types, and extension objects.
    /// Called once after connect. Results cached for the session.
    async fn load_catalog(&mut self) -> Result<()>;

    /// Classify a reference as internal, extension, or user-defined.
    /// Uses the loaded catalog (if available) with static fallback.
    fn classify_reference(&self, name: &str, installed_extensions: &[String]) -> ReferenceClass;
}
```

### What the catalog provides

Each adapter queries its target's system catalog for **functions**, **data types**, and **operators** — the three things that appear as references in DDL.

**PostgreSQL / Supabase:**

```sql
-- Built-in functions (coalesce, now, gen_random_uuid, ...)
SELECT proname FROM pg_proc p
JOIN pg_namespace n ON p.pronamespace = n.oid
WHERE n.nspname = 'pg_catalog';

-- Extension functions (st_distance, uuid_generate_v4, crypt, ...)
SELECT p.proname, e.extname
FROM pg_proc p
JOIN pg_depend d ON d.objid = p.oid AND d.deptype = 'e'
JOIN pg_extension e ON e.oid = d.refobjid;

-- Built-in types (varchar, jsonb, timestamptz, ...)
SELECT typname FROM pg_type
WHERE typnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'pg_catalog');

-- Extension types (vector, geometry, ...)
SELECT t.typname, e.extname
FROM pg_type t
JOIN pg_depend d ON d.objid = t.oid AND d.deptype = 'e'
JOIN pg_extension e ON e.oid = d.refobjid;
```

**SQLite:**
- Built-in functions: fixed list (SQLite has ~50 built-in functions, stable across versions)
- No extensions, no custom types — everything not in the project is internal

**Convex:**
- No SQL functions — classification always returns `Internal` (no SQL to parse)

**Future Oracle adapter (example):**
```sql
-- Oracle built-in functions (decode, nvl, sysdate, ...)
SELECT object_name FROM all_procedures
WHERE owner = 'SYS' AND object_type = 'FUNCTION';

-- Oracle types
SELECT type_name FROM all_types WHERE owner = 'SYS';
```

### Three-tier resolution

```
1. User overrides (design.yaml ignore list)     ← highest priority
2. Adapter catalog (loaded from target DB)       ← authoritative
3. Static patterns (offline fallback)            ← works without DB connection
```

```rust
pub enum ReferenceClass {
    Internal,          // Built-in function, type, or operator
    Extension(String), // From a specific extension (name included)
    Ignored,           // User-configured ignore
    UserDefined,       // Project entity — treated as dependency
}
```

#### Adapter implementation (PostgreSQL)

```rust
pub struct PostgresAdapter {
    pool: PgPool,
    catalog: Option<AdapterCatalog>,  // Loaded lazily on first classify
    // ...
}

pub struct AdapterCatalog {
    builtin_functions: HashSet<String>,      // pg_catalog functions
    builtin_types: HashSet<String>,          // pg_catalog types
    extension_objects: HashMap<String, String>, // name → extension
}

impl DatabaseAdapter for PostgresAdapter {
    async fn load_catalog(&mut self) -> Result<()> {
        // Query pg_proc, pg_type, pg_extension
        // Populate self.catalog
    }

    fn classify_reference(&self, name: &str, installed: &[String]) -> ReferenceClass {
        let lower = name.to_lowercase();

        // Catalog lookup (authoritative)
        if let Some(catalog) = &self.catalog {
            if catalog.builtin_functions.contains(&lower)
                || catalog.builtin_types.contains(&lower) {
                return ReferenceClass::Internal;
            }
            if let Some(ext) = catalog.extension_objects.get(&lower) {
                return ReferenceClass::Extension(ext.clone());
            }
        }

        // Static pattern fallback (no DB connection)
        if Self::matches_static_pattern(&lower) {
            return ReferenceClass::Internal;
        }

        ReferenceClass::UserDefined
    }
}
```

#### Static patterns (offline fallback)

Each adapter provides its own static patterns. These handle the common cases when no DB connection is available:

```rust
impl PostgresAdapter {
    const INTERNAL_PATTERNS: &[&str] = &[
        r"^pg_", r"^information_schema\.", r"^array_", r"^json_",
        r"^jsonb_", r"^regexp_", r"^current_", r"^gen_random_",
        r"^to_", r"^date_", r"^time_", r"^string_to_",
    ];

    fn matches_static_pattern(name: &str) -> bool {
        Self::INTERNAL_PATTERNS.iter().any(|p| Regex::new(p).unwrap().is_match(name))
    }
}

impl SqliteAdapter {
    // SQLite's function list is small and stable — just enumerate them
    const BUILTINS: &[&str] = &[
        "abs", "changes", "coalesce", "glob", "hex", "ifnull",
        "iif", "instr", "json", "length", "like", "lower",
        "ltrim", "max", "min", "nullif", "printf", "quote",
        "random", "replace", "round", "rtrim", "substr",
        "total", "trim", "typeof", "unicode", "upper", "zeroblob",
    ];
}
```

#### User overrides (design.yaml)

Applied **before** adapter classification — highest priority:

```yaml
ignore:
  - bfs                # ltree graph traversal function
  - my_company.*       # Shared schema functions (pattern)
  - decode             # Oracle compat function in a custom extension
```

#### Catalog cache

Results cached per connection to avoid re-querying on every `inspect`:

- **Location:** `~/.cache/dbd/{connection_hash}.json`
- **Key:** SHA-256 of connection URL (without password)
- **TTL:** 24 hours, or invalidated when `design.yaml` extensions list changes
- **Fallback:** When no DB connection, uses static patterns only (more warnings, no errors)

#### Cross-dialect examples

| SQL construct | PostgreSQL | Oracle | SQLite |
|---|---|---|---|
| Conditional | `coalesce(a, b)` | `nvl(a, b)` or `decode(...)` | `coalesce(a, b)` or `ifnull(a, b)` |
| UUID generation | `gen_random_uuid()` | `sys_guid()` | N/A |
| Current time | `now()` | `sysdate` | `datetime('now')` |
| String concat | `\|\|` or `concat()` | `\|\|` or `concat()` | `\|\|` |
| JSON access | `jsonb_extract_path()` | `json_value()` | `json_extract()` |

Each adapter's catalog knows its own dialect's builtins. The classifier never needs to know about other databases — it asks the connected adapter "is this yours?"

---

## Patterns

### Error handling

Two-tier error strategy:

```rust
// Typed errors for library-level code (adapter, parser, config)
#[derive(Debug, thiserror::Error)]
pub enum DbdError {
    #[error("Config error: {0}")]
    Config(String),

    #[error("Parse error in {file}: {message}")]
    Parse { file: PathBuf, message: String },

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Entity validation failed: {name} — {errors:?}")]
    Validation { name: String, errors: Vec<String> },

    #[error("GitHub source error: {0}")]
    GitHubSource(String),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("Safety guard: {0}")]
    SafetyGuard(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// anyhow::Result in main.rs and CLI glue for ergonomic .context()
```

### Builder pattern for Design

The `Design` struct mirrors the Node.js `Design` class but uses Rust builder conventions:

```rust
impl Design {
    pub async fn from_config(
        config_path: &Path,
        database_url: Option<&str>,
        env: &str,
    ) -> Result<Self> { ... }

    pub fn validate(&mut self) -> &mut Self { ... }
    pub fn report(&self, name: Option<&str>) -> Report { ... }
    pub async fn apply(&self, name: Option<&str>, dry_run: bool) -> Result<()> { ... }
    pub async fn import_data(&self, name: Option<&str>, dry_run: bool) -> Result<()> { ... }
    pub fn combine(&self, file: &Path) -> Result<()> { ... }
    pub fn graph(&self, name: Option<&str>) -> GraphResult { ... }
}
```

### Collect-errors-don't-panic

Match the Node.js pattern: entities accumulate `errors` and `warnings` vectors. Functions return partial results. Only truly unrecoverable errors (IO, connection failure) use `Result::Err`. Validation errors are collected and reported.

```rust
// Good: collect and continue
entity.errors.push(format!("File not found: {}", path.display()));

// Only for truly fatal:
Err(DbdError::Database(e))
```

### Dependency resolution — iterative grouping

Port the existing algorithm directly:

```rust
pub fn sort_by_dependencies(entities: &[Entity]) -> Vec<Entity> {
    // 1. Build adjacency: name → Set<dependency names>
    // 2. Iteratively extract entities with no in-group dependencies
    // 3. Mark remaining as cyclic
}
```

### Import plan — call-graph + DDL-graph sort

Import ordering is more complex than entity ordering. It combines two dependency graphs:

```
1. For each staging import table:
   - Find target config table (match by base name across schemas)
   - Find import procedure (procedure that reads this staging table)
   - Identify config tables the procedure writes to

2. Build call-graph dependencies:
   - If procedure A writes to config.X, and procedure B reads config.X,
     then B depends on A (A must import first)

3. Topological sort with DDL-order tiebreaker:
   - Pick ready entries (no pending deps)
   - Tiebreaker: entity position in DDL dependency graph
   - Cycles: append remaining sorted by DDL order (with warning)
```

```rust
pub struct ImportPlanEntry {
    pub table: Entity,               // Staging table being imported
    pub target: Option<Entity>,      // Config table it maps to
    pub procedure: Option<Entity>,   // Import procedure that processes it
    pub targets: Vec<String>,        // Config tables written by procedure
    pub warnings: Vec<String>,
}

pub fn build_import_plan(
    import_tables: &[Entity],
    entities: &[Entity],
) -> Vec<ImportPlanEntry> { ... }
```

### SQL parsing — `sqlparser-rs` (pure Rust, no regex fallback)

**`sqlparser-rs`** (Apache DataFusion) is the parser. It replaces both `pg_query` (C FFI) and the regex fallbacks from the Node.js version.

| Concern | `pg_query` (C FFI) | `sqlparser-rs` (chosen) |
|---|---|---|
| Language | 235K lines of C (PostgreSQL parser) | Pure Rust |
| Cross-compilation | Hard (C toolchain per target) | Trivial |
| WASM target | Heavy | Easy |
| Compile time | Slow (C compilation) | Fast |
| DDL coverage | 100% PostgreSQL | Full PostgreSQL DDL (CREATE TABLE, FUNCTION, ENUM, VIEW, INDEX, TRIGGER) |
| PL/pgSQL body | Parsed as string blob | Parsed as dollar-quoted string (same — body is opaque) |
| Dollar-quoting | Yes | Yes (`DollarQuotedString` token) |
| Multi-dialect | No (PG only) | Yes (20+ dialects — future Oracle/MySQL adapters) |
| AST types | Protobuf (harder to match) | Native Rust enums (ergonomic pattern matching) |
| Deparse | Yes | Yes |
| Error recovery | No | No |

**Why not `pg_query`?** The C dependency defeats the "zero runtime deps, easy cross-compilation" goal. `sqlparser-rs` handles every DDL type we need, and its pure Rust AST is far more ergonomic to work with.

**Why not tree-sitter?** It's an editor tool (syntax highlighting, CST), not a semantic SQL parser. No typed DDL nodes.

**No regex fallback needed.** The Node.js version falls back to regex because `pgsql-parser` (WASM) fails on some function/procedure/enum DDL. `sqlparser-rs` handles all of these natively:

```rust
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::ast::Statement;

let dialect = PostgreSqlDialect {};
let statements = Parser::parse_sql(&dialect, sql)?;

for stmt in statements {
    match stmt {
        Statement::CreateTable { name, columns, constraints, indexes, .. } => {
            // Full table definition with columns, FKs, constraints
        }
        Statement::CreateFunction { name, params, return_type, function_body, language, .. } => {
            // Function/procedure — body is a DollarQuotedString (opaque, not parsed)
            // Extract reads/writes from body string using simple pattern matching
        }
        Statement::CreateType { name, representation: UserDefinedTypeRepresentation::Enum { labels }, .. } => {
            // Enum with all variant labels
        }
        Statement::CreateView { name, query, columns, .. } => {
            // View definition with column list and SELECT query
        }
        Statement::CreateIndex { name, table_name, columns, unique, using, .. } => {
            // Index with method, columns, uniqueness
        }
        Statement::SetVariable { variable, value, .. } => {
            // SET search_path = ...
        }
        _ => {}
    }
}
```

**PL/pgSQL body analysis:** Function bodies are captured as opaque strings (both `pg_query` and `sqlparser-rs` do this). For extracting `reads`/`writes` (which tables a procedure SELECTs FROM or INSERTs INTO), we do simple pattern matching on the body string — same as the Node.js version. This is not regex fallback for parsing; it's intentional analysis of a code block.

**Multi-dialect future:** `sqlparser-rs` supports MySQL, SQLite, Oracle, and others. When we add adapters for those databases, the same parser crate handles their SQL dialects — we just switch the dialect struct.

### GitHub source — pure Rust (no curl/tar binaries)

Replace `execFileSync('curl', ...)` and `execFileSync('tar', ...)` with `reqwest` + `flate2` + `tar`:

```rust
pub fn download_github_source(source: &str) -> Result<GitHubDownload> {
    let parsed = parse_github_source(source)?;
    let tmp_dir = tempfile::tempdir()?;
    let url = format!(
        "https://api.github.com/repos/{}/{}/tarball/{}",
        parsed.owner, parsed.repo, parsed.git_ref
    );

    let response = reqwest::blocking::Client::new()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "dbd-cli")
        .send()?;

    let decoder = flate2::read::GzDecoder::new(response);
    tar::Archive::new(decoder).unpack(tmp_dir.path())?;
    // ... resolve work_dir from extracted contents
}
```

---

## CLI Structure (clap)

```rust
#[derive(Parser)]
#[command(name = "dbd", version, about = "Database design tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to config file
    #[arg(short, long, default_value = "design.yaml", global = true)]
    config: PathBuf,

    /// Database connection URL
    #[arg(short, long, env = "DATABASE_URL", global = true)]
    database: Option<String>,

    /// Environment (dev or prod)
    #[arg(short, long, default_value = "prod", global = true)]
    environment: String,

    /// Source directory or GitHub repo (owner/repo/path)
    #[arg(short, long, default_value = ".", global = true)]
    source: String,
    // … plus --scope / --deps / --verbose (all global = true)
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a starter project
    Init {
        #[arg(short, long, default_value = "database")]
        project: String,
        #[arg(long)]
        target: Option<String>,
    },
    /// Validate the project configuration
    Inspect {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long)]
        verbose: bool,
        #[arg(long)]
        no_cache: bool,
    },
    /// Apply DDL scripts to database
    Apply {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Combine all DDL into one file
    Combine {
        #[arg(short, long, default_value = "init.sql")]
        file: PathBuf,
    },
    /// Load data files into database
    Import {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Export table data to files
    Export {
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Generate DBML documentation
    Dbml {
        #[arg(short, long, default_value = "design.dbml")]
        file: PathBuf,
    },
    /// Output dependency graph as JSON
    Graph {
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Drop all schemas (bare state)
    Reset {
        #[arg(long, default_value = "supabase")]
        target: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply RLS policies
    Policies {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Apply schema grants
    Grants {
        #[arg(long, default_value = "supabase")]
        target: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a versioned schema snapshot
    Snapshot {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long)]
        list: bool,
    },
    /// Apply pending migrations
    Migrate {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        status: bool,
        #[arg(long)]
        to: Option<u32>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Deploy from source: fetch + apply + import
    Deploy {
        #[arg(long)]
        dry_run: bool,
    },
    /// Audit design.yaml for stale entries
    Doctor {
        #[arg(long)]
        fix: bool,
    },
}
```

---

## Test Strategy

Tests are organized by what they exercise and what infrastructure they need.

### Unit tests — `dbd-core` (per module, `#[cfg(test)]`)

Each `dbd-core` module has inline tests for pure functions. No database or external resources needed.

**Core modules:**

| Module            | What to test                                                     |
| ----------------- | ---------------------------------------------------------------- |
| `config.rs`       | YAML parsing, schema normalization, env normalization            |
| `entity.rs`       | Entity creation from file paths, validation rules                |
| `scanner.rs`      | File discovery in ddl/, import/, policies/ folders               |
| `dependency.rs`   | Topological sort, cycle detection, graph building                |
| `references.rs`   | Reference matching, external entity handling                     |
| `snapshot.rs`     | Snapshot serialization, version numbering, pending detection     |
| `migration.rs`    | Schema diff, SQL generation from diffs                           |
| `script.rs`       | Reset script, grants script, DDL generation                      |
| `github.rs`       | Source string parsing, validation, cache path resolution         |
| `dbml.rs`         | DBML text generation from TableDef/Entity structures             |

**Parser modules** (DDL → internal representation, no DB needed):

| Module               | What to test                                                  |
| -------------------- | ------------------------------------------------------------- |
| `parser/tables.rs`   | Column extraction, FK with on_delete/on_update, constraints   |
| `parser/views.rs`    | View dependency extraction, column list                       |
| `parser/procedures.rs` | Function/procedure body analysis, reads/writes detection    |
| `parser/indexes.rs`  | Index extraction: unique, type, composite, expression         |
| `parser/triggers.rs` | Trigger event, timing, function reference                     |
| `parser/extractors.rs` | Entity identification from multi-statement DDL files        |

**Adapter modules** (classification logic, testable without DB):

| Module                  | What to test                                               |
| ----------------------- | ---------------------------------------------------------- |
| `adapter/postgres.rs`   | Static pattern matching, Supabase managed infra lists      |
| `adapter/convex.rs`     | SQL type → Convex validator mapping                        |
| `adapter/sqlite.rs`     | SQLite builtin function list, unsupported entity handling  |

### Parser integration tests (`tests/parser/`)

Test the full parse pipeline using real DDL fixture files. Verifies that `sqlparser-rs` produces the correct `Entity` + `TableDef` from actual DDL.

```rust
// tests/parser/tables_test.rs
#[test]
fn parses_table_with_fk_and_actions() {
    let sql = include_str!("../fixtures/ddl/table/config/lookup_values.ddl");
    let parser = PostgresParser::new();
    let entity = parser.parse_entity(Path::new("ddl/table/config/lookup_values.ddl"), sql).unwrap();

    assert_eq!(entity.name, "config.lookup_values");
    assert_eq!(entity.entity_type, EntityType::Table);

    let table_def = entity.table_def.unwrap();
    assert!(table_def.columns.iter().any(|c| c.name == "lookup_id"));

    // FK detail — including actions (not captured by Node.js version)
    let fk = table_def.constraints.iter()
        .find_map(|c| match c {
            TableConstraint::ForeignKey(fk) if fk.ref_table == "lookups" => Some(fk),
            _ => None,
        })
        .expect("FK to lookups should exist");
    assert_eq!(fk.columns, vec!["lookup_id"]);
    assert_eq!(fk.ref_columns, vec!["id"]);
}

#[test]
fn parses_enum_with_values() {
    let sql = include_str!("../fixtures/ddl/enum/config/status.sql");
    let parser = PostgresParser::new();
    let entity = parser.parse_entity(Path::new("ddl/enum/config/status.sql"), sql).unwrap();

    assert_eq!(entity.entity_type, EntityType::Enum);
    assert!(entity.enum_values.iter().any(|v| v.name == "active"));
}

#[test]
fn parses_procedure_reads_and_writes() {
    let sql = include_str!("../fixtures/ddl/procedure/staging/import_lookups.ddl");
    let parser = PostgresParser::new();
    let entity = parser.parse_entity(
        Path::new("ddl/procedure/staging/import_lookups.ddl"), sql
    ).unwrap();

    assert!(entity.reads.contains(&"staging.lookups".to_string()));
    assert!(entity.writes.contains(&"config.lookups".to_string()));
}
```

### Adapter classification tests (`tests/adapter/`)

Test `classify_reference()` for each adapter — both static fallback and catalog-based.

```rust
// tests/adapter/classify_test.rs

#[test]
fn postgres_classifies_builtins_statically() {
    let adapter = PostgresAdapter::new_disconnected(); // No DB, static patterns only
    assert_eq!(
        adapter.classify_reference("pg_catalog.now", &[]),
        ReferenceClass::Internal
    );
    assert_eq!(
        adapter.classify_reference("jsonb_build_object", &[]),
        ReferenceClass::Internal
    );
    assert_eq!(
        adapter.classify_reference("config.lookups", &[]),
        ReferenceClass::UserDefined
    );
}

#[tokio::test]
#[cfg(feature = "test-db")]
async fn postgres_classifies_from_catalog() {
    let mut adapter = PostgresAdapter::connect(test_db_url()).await.unwrap();
    adapter.load_catalog().await.unwrap();

    // coalesce is a pg_catalog builtin — resolved from actual catalog
    assert_eq!(
        adapter.classify_reference("coalesce", &[]),
        ReferenceClass::Internal
    );
    // width_bucket — another builtin that static patterns miss
    assert_eq!(
        adapter.classify_reference("width_bucket", &[]),
        ReferenceClass::Internal
    );
}

#[tokio::test]
#[cfg(feature = "test-db")]
async fn postgres_classifies_extension_functions() {
    let mut adapter = PostgresAdapter::connect(test_db_url()).await.unwrap();
    // Assumes uuid-ossp extension is installed in test DB
    adapter.load_catalog().await.unwrap();

    assert_eq!(
        adapter.classify_reference("uuid_generate_v4", &["uuid-ossp"]),
        ReferenceClass::Extension("uuid-ossp".into())
    );
}

#[test]
fn sqlite_classifies_builtins() {
    let adapter = SqliteAdapter::new_disconnected();
    assert_eq!(
        adapter.classify_reference("coalesce", &[]),
        ReferenceClass::Internal
    );
    assert_eq!(
        adapter.classify_reference("ifnull", &[]),
        ReferenceClass::Internal  // SQLite-specific, not in PostgreSQL
    );
}

#[test]
fn convex_classifies_everything_as_internal() {
    let adapter = ConvexAdapter::new();
    // Convex has no SQL — nothing is a user-defined reference
    assert_eq!(
        adapter.classify_reference("anything", &[]),
        ReferenceClass::Internal
    );
}
```

### DBML generation tests (`tests/dbml/`)

Test the full pipeline: parsed entities → DBML text. Use insta snapshots for complex output.

```rust
#[test]
fn generates_dbml_for_table_with_fks() {
    let entities = parse_fixture_entities("tests/fixtures");
    let dbml = dbml::generate_dbml(&DbmlParams {
        entities,
        project: test_project_config(),
        filter: None,
    });
    insta::assert_snapshot!(dbml[0].content.unwrap());
}

#[test]
fn dbml_includes_fk_actions() {
    let entity = make_table_entity("config.orders", vec![
        ForeignKey {
            columns: vec!["user_id".into()],
            ref_table: "users".into(),
            on_delete: Some(FkAction::Cascade),
            on_update: Some(FkAction::NoAction),
            ..Default::default()
        }
    ]);
    let dbml = dbml::emit_refs(&[entity]);
    assert!(dbml.contains("[delete: cascade, update: no action]"));
}

#[test]
fn dbml_includes_enums() {
    let entity = make_enum_entity("config.status", &["active", "inactive"]);
    let dbml = dbml::emit_enum(&entity);
    insta::assert_snapshot!(dbml);
}
```

### Snapshot / migration tests (insta)

```rust
#[test]
fn generates_migration_sql_for_added_column() {
    let diff = SchemaDiff { /* ... */ };
    let sql = migration::generate_sql(&diff);
    insta::assert_snapshot!(sql);
}

#[test]
fn generates_reset_script() {
    let script = script::build_reset(&["config", "staging"], &[], "postgres");
    insta::assert_snapshot!(script);
}
```

### CLI integration tests — `dbd-cli` (assert_cmd)

Test the binary end-to-end. Lives in `crates/dbd-cli/tests/`.

```rust
#[test]
fn inspect_example_project() {
    Command::cargo_bin("dbd").unwrap()
        .arg("inspect")
        .arg("--source").arg("tests/fixtures")
        .assert()
        .success();
}

#[test]
fn combine_writes_file() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("init.sql");
    Command::cargo_bin("dbd").unwrap()
        .arg("combine")
        .arg("-f").arg(&out)
        .arg("--source").arg("tests/fixtures")
        .assert()
        .success();
    assert!(out.exists());
}
```

### Test fixtures — reuse from Node.js

Symlink or copy the existing DDL fixtures from the Node.js `example/` directory into `tests/fixtures/`. Parser and integration tests should produce equivalent results to the Node.js version.

### Scenario tests (requires PostgreSQL)

End-to-end tests against a real database. These verify the full lifecycle — not just individual functions but the correct sequencing of operations across commands. Run with `cargo test --features test-db` or in CI with a Postgres container.

Each scenario uses a fresh database (CREATE DATABASE per test, DROP after).

#### Scenario 1: Initial deploy (fresh database)

Empty database, no tables, no `_dbd_migrations`.

```
Given: empty database
When:  design.apply()
Then:
  - schemas created (config, staging)
  - extensions installed (uuid-ossp)
  - roles created in dependency order
  - tables created in dependency order (FKs resolve)
  - views, functions, procedures created
  - _dbd_migrations has one row at latest snapshot version (or no row if no snapshots)
  - design.report() shows zero errors
```

#### Scenario 2: Initial deploy with data seeding

Fresh database, apply + import.

```
Given: empty database, import/ folder with CSV files
When:  design.apply() then design.import_data()
Then:
  - all entities created (same as Scenario 1)
  - staging tables truncated before load (truncate: true default)
  - CSV data loaded into staging tables via COPY FROM STDIN
  - import procedures called automatically (staging.import_<name>())
  - import.after SQL files executed
  - config tables populated (via procedures moving data from staging → config)
```

#### Scenario 3: Incremental update (schema migration)

Database at v1, DDL files changed, snapshot v2 exists with migrations.

```
Given: database at v1 (tables exist, _dbd_migrations.version = 1)
       snapshots/002.json exists with column additions
       migrations/002/ has ALTER SQL files
When:  design.apply()
Then:
  - pending migration v1→v2 detected
  - ALTER TABLE runs before each affected table's CREATE OR REPLACE
  - unaffected tables re-applied via CREATE OR REPLACE (idempotent)
  - views/functions/procedures re-applied (pick up column changes)
  - _dbd_migrations records version 2
  - no data loss in existing tables
```

#### Scenario 4: Incremental update with data re-seeding

Database has existing data, apply migrations + reload staging.

```
Given: database at v1 with data in config tables
       new snapshot v2, import/ has updated CSV files
When:  design.apply() then design.import_data()
Then:
  - migrations applied (same as Scenario 3)
  - staging tables TRUNCATED before re-import (truncate: true)
  - fresh CSV data loaded
  - import procedures re-run (moves staging → config)
  - existing config data updated/replaced by procedure logic
  - import.after SQL files re-executed
```

#### Scenario 5: Incremental seeding only (no schema change)

Database schema is current, just reload staging data.

```
Given: database at latest version, config tables have stale data
When:  design.import_data()
Then:
  - no schema changes (apply not called)
  - staging tables truncated
  - CSV data loaded
  - import procedures called
  - config tables refreshed
```

#### Scenario 6: Append-only import (truncate: false)

Import with truncate disabled — data accumulates.

```
Given: database with existing staging data
       design.yaml has import.options.truncate: false
When:  design.import_data()
Then:
  - staging tables NOT truncated
  - new rows appended via COPY FROM STDIN
  - import procedures called (procedure decides how to merge)
  - pre-existing rows preserved
```

#### Scenario 7: Staging table dropped and recreated during apply

Staging tables may have stale columns from a prior version. Apply drops and recreates them via CREATE OR REPLACE.

```
Given: database at v1, staging.lookups has columns (id, name)
       DDL changed: staging.lookups now has (id, name, category)
       snapshot v2 has migration for staging.lookups
When:  design.apply()
Then:
  - migration ALTER adds "category" column to staging.lookups
  - CREATE OR REPLACE re-applies DDL (idempotent, column already exists)
  - staging.lookups now has 3 columns
  - import files with "category" column can now be loaded
```

#### Scenario 8: Reset and rebuild

Full teardown and fresh start. Requires `--force` once the database has graduated past v0 or is marked prod.

```
Given: database at v3 with data, _dbd_meta.env = "dev", version = 3
When:  design.reset(force=true) then design.apply()
Then:
  - all schemas dropped (CASCADE)
  - _dbd_meta and _dbd_migrations cleared for this project
  - full rebuild from DDL (no ALTER scripts)
  - _dbd_meta re-created with version at latest snapshot
  - tables exist but are empty (no data — import not called)
```

#### Scenario 9: Multi-version catch-up

Database is several versions behind.

```
Given: database at v1, snapshots up to v4
       migrations/002/, 003/, 004/ all exist
When:  design.apply()
Then:
  - migrations applied in order: v1→v2, v2→v3, v3→v4
  - each migration's ALTER runs before its table's CREATE OR REPLACE
  - dropped tables (if any) removed after all entities applied
  - _dbd_migrations has entries for v2, v3, v4
```

#### Scenario 10: Deploy from GitHub source

End-to-end deploy from a remote source.

```
Given: empty database, GitHub repo with design.yaml + ddl/ + import/
When:  deploy(source="owner/repo/database", database_url=url)
Then:
  - source downloaded to temp directory
  - apply runs (same as Scenario 1)
  - import runs (same as Scenario 2)
  - temp directory cleaned up
  - database fully populated
```

#### Scenario 11: Environment-specific import

Dev vs prod data loading.

```
Given: database with schema applied
       import/dev/staging/fixtures.csv exists
       import/prod/staging/seeds.csv exists
       import/staging/lookups.csv exists (shared)
When:  design.import_data() with env="dev"
Then:
  - shared files loaded (import/staging/lookups.csv)
  - dev files loaded (import/dev/staging/fixtures.csv)
  - prod files NOT loaded
  - import procedures called for loaded tables only
```

#### Scenario 12: Dry-run produces no side effects

Verify preview mode.

```
Given: empty database
When:  design.apply(dry_run=true) then design.import_data(dry_run=true)
Then:
  - no tables created
  - no data loaded
  - stdout lists entities that would be applied
  - stdout lists tables that would be imported
  - database still empty
```

#### Scenario 13: Dev free-reset before v1

During initial development, reset is unrestricted.

```
Given: _dbd_meta has env = "dev", version = 0
When:  design.reset()
Then:
  - reset proceeds (dev, pre-v1 — free reset mode)
  - all schemas dropped
```

#### Scenario 14: Dev reset blocked after v1

Once a snapshot is applied, dev databases graduate.

```
Given: _dbd_meta has env = "dev", version = 1
When:  design.reset()
Then:
  - ERROR: "reset is blocked — database has applied migrations. Use --force to override."
  - database unchanged
```

```
Given: _dbd_meta has env = "dev", version = 1
When:  design.reset(force=true)
Then:
  - reset proceeds (explicit override)
  - all schemas dropped
```

#### Scenario 15: Prod always blocked

Production databases are always protected, even at version 0.

```
Given: _dbd_meta has env = "prod", version = 0
When:  design.reset()
Then:
  - ERROR: "reset is blocked — database is marked as prod. Use --force to override."
  - database unchanged
```

#### Scenario 16: First apply records environment

```
Given: empty database, no _dbd_meta table
When:  design.apply() with env = "dev"
Then:
  - _dbd_meta table created
  - row inserted: { project: "MyProject", env: "dev", version: 0 }
  - reset is now allowed (dev, pre-v1)
```

```
Given: empty database, no _dbd_meta table
When:  design.apply() with env = "prod"
Then:
  - _dbd_meta row: { project: "MyProject", env: "prod", version: 0 }
  - reset is blocked from this point
```

#### Scenario 17: Environment mismatch warning

```
Given: _dbd_meta has env = "prod"
       caller passes --environment dev
When:  design.apply()
Then:
  - WARNING: "database is marked as prod but command was called with env=dev"
  - apply proceeds (not destructive, just a warning)
  - _dbd_meta.env NOT overwritten (database's recorded env is authoritative)
```

#### Scenario 18: Reset guard works for embedded consumers

```
Given: Rust web app calls design.reset() programmatically
       _dbd_meta has env = "prod"
When:  design.reset("supabase", false)
Then:
  - returns Err(DbdError::SafetyGuard(...))
  - database unchanged
```

### Reset safety model

`reset` drops schemas with CASCADE — it's irreversible. The guard is **database-side**: the `_dbd_meta` table records the environment, and reset checks it before proceeding.

#### `_dbd_meta` table

Created alongside `_dbd_migrations` on first `apply`. Records project-level metadata.

```sql
CREATE TABLE IF NOT EXISTS _dbd_meta (
  project     varchar NOT NULL PRIMARY KEY,
  env         varchar NOT NULL DEFAULT 'dev',
  version     integer NOT NULL DEFAULT 0,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);
```

- `env` — set explicitly via `dbd apply -e dev` or `dbd apply -e prod`. Records what the database is.
- `version` — mirrors the latest applied version from `_dbd_migrations`. Updated by `apply` and `migrate`.

Populated on first apply:

```sql
INSERT INTO _dbd_meta (project, env, version)
VALUES ('MyProject', 'dev', 0)
ON CONFLICT (project) DO UPDATE
  SET env = EXCLUDED.env, version = EXCLUDED.version, updated_at = now();
```

After a snapshot is applied, version is updated:

```sql
UPDATE _dbd_meta SET version = 1, updated_at = now() WHERE project = 'MyProject';
```

#### Guard logic

```
reset called
  → query _dbd_meta for current project
  → if no row (fresh db)              → ALLOW (nothing to protect)
  → if env = "dev" and version < 1    → ALLOW (pre-v1 free reset)
  → if env = "dev" and version >= 1   → BLOCK (dev has graduated)
  → if env = "prod"                   → BLOCK (always protected)
  → if --force                        → ALLOW (explicit override)
```

**Why database-side, not CLI-side?**

- The **database knows what it is**. A prod database is prod regardless of what flags the caller passes.
- CLI flags can be wrong — a script might default to `dev` while pointing at a prod connection string.
- The guard survives across tools: whether reset is called from the CLI, an embedded Rust app, or a CI script, the same check runs.
- The `_dbd_meta` table is lightweight (one row per project) and created automatically.

| `_dbd_meta.env` | version | `reset`       | `reset --force` |
| ---------------- | ------- | ------------- | --------------- |
| no table (fresh) | —       | allowed       | allowed         |
| `dev`            | 0       | allowed       | allowed         |
| `dev`            | >= 1    | **blocked**   | allowed         |
| `prod`           | any     | **blocked**   | allowed         |

**Dev free-reset mode:** During initial development (before v1 snapshot), the schema is in flux. `dbd apply -e dev` sets `env = "dev"` in `_dbd_meta`, and reset is unrestricted. Once the first snapshot is taken and applied (version becomes >= 1), the dev database has graduated — reset requires `--force` from that point on.

**Prod is always protected:** Even at version 0, a prod database may have data loaded outside of dbd. Reset always requires `--force`.

**Library API:**

```rust
impl Design {
    pub async fn reset(&self, target: &str, force: bool) -> Result<()> {
        if !force {
            let adapter = self.get_adapter().await?;
            if let Some(meta) = adapter.get_project_meta().await? {
                if meta.env == "prod" {
                    return Err(DbdError::SafetyGuard(
                        "reset is blocked — database is marked as prod. \
                         Use --force to override.".into()
                    ));
                }
                if meta.version >= 1 {
                    return Err(DbdError::SafetyGuard(
                        "reset is blocked — database has applied migrations. \
                         Use --force to override.".into()
                    ));
                }
            }
        }
        // ... proceed with reset
    }
}
```

**Adapter trait additions:**

```rust
#[async_trait]
pub trait DatabaseAdapter: Send + Sync {
    // ... existing methods ...

    // Meta tracking
    async fn ensure_meta_table(&self) -> Result<()>;
    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>>;
    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()>;
}

pub struct ProjectMeta {
    pub project: String,
    pub env: String,
    pub version: u32,
}
```

**CLI integration:**

```rust
/// Drop all schemas (bare state)
Reset {
    #[arg(long, default_value = "supabase")]
    target: String,
    #[arg(long)]
    dry_run: bool,
    /// Override database environment safety guard
    #[arg(long)]
    force: bool,
},
```

---

## Build order

Implement bottom-up within `dbd-core`, then wire the CLI.

### Phase A — `dbd-core` library (steps 1–12)

| Step | Module(s)                                | Depends on       | Deliverable                          |
| ---- | ---------------------------------------- | ---------------- | ------------------------------------ |
| 1    | Workspace scaffold, `error.rs`           | —                | Cargo workspace, error types         |
| 2    | `entity.rs`                              | error            | Entity types, creation, validation   |
| 3    | `scanner.rs`                             | entity           | File discovery                       |
| 4    | `config.rs`                              | entity, scanner  | design.yaml parsing                  |
| 5    | `dependency.rs`                          | entity           | Topological sort, cycle detection    |
| 6    | `parser/*.rs`                            | entity           | SQL parsing + extraction             |
| 7    | `references.rs`                          | entity, parser   | Reference resolution                 |
| 8    | `script.rs`                              | entity           | DDL generation                       |
| 9    | `adapter/mod.rs`, `adapter/postgres.rs`  | entity, script   | sqlx adapter + catalog classifier    |
| 10   | `snapshot.rs`, `migration.rs`            | entity, parser   | Snapshot + diff + SQL gen            |
| 11   | `design.rs`, `lib.rs`                    | all above        | Design orchestrator, public API      |

At this point `dbd-core` is usable as a library. Verify with integration tests.

### Phase B — `dbd-cli` binary (steps 13–14)

| Step | Module(s)                                | Depends on       | Deliverable                          |
| ---- | ---------------------------------------- | ---------------- | ------------------------------------ |
| 13   | `cli.rs`, `commands.rs`, `main.rs`       | dbd-core         | CLI wiring (all commands)            |
| 14   | CLI integration tests                    | dbd-cli          | assert_cmd end-to-end tests          |

### Phase C — Extended features (steps 15–16)

| Step | Module(s)                                | Depends on       | Deliverable                          |
| ---- | ---------------------------------------- | ---------------- | ------------------------------------ |
| 15   | `github.rs`                              | —                | GitHub source support                |
| 16   | `dbml.rs`                                | entity, parser   | DBML conversion                      |

---

## Multi-adapter support

The Node.js version supports 5 adapters. The Rust version must support them as feature-gated modules.

### Adapter capabilities matrix

| Capability | Postgres | Supabase | SQLite | Convex |
|---|---|---|---|---|
| Schemas | yes | yes (filtered) | no | no |
| Extensions | yes | yes (filtered) | no | no |
| Enums | yes | yes | no | no |
| Roles | yes | yes | no | no |
| Functions/procedures | yes | yes | no | no |
| SQL execution | yes | yes | yes | no |
| Batch apply | no | no | no | **yes** |
| COPY import | yes | yes | no | npx |
| Migrations table | yes | yes | yes | no |
| Catalog queries | yes | yes | no | no |
| Snapshots | yes | yes | tbd | no |

### Supabase adapter

Extends Postgres — filters out DDL for managed infrastructure:

- **Managed schemas** (9): auth, storage, realtime, graphql_public, supabase_functions, extensions, pgbouncer, pgsodium, vault
- **Pre-installed extensions** (10): plpgsql, uuid-ossp, pgcrypto, pgjwt, pg_graphql, pgsodium, supabase_vault, pg_stat_statements, pgaudit, pg_tle
- Overrides `apply_entity()` to skip CREATE SCHEMA and CREATE EXTENSION for these

```rust
pub struct SupabaseAdapter {
    inner: PostgresAdapter,
    managed_schemas: HashSet<String>,
    managed_extensions: HashSet<String>,
}

impl DatabaseAdapter for SupabaseAdapter {
    async fn apply_entity(&self, entity: &Entity) -> Result<()> {
        match entity.entity_type {
            EntityType::Schema if self.managed_schemas.contains(&entity.name) => Ok(()),
            EntityType::Extension if self.managed_extensions.contains(&entity.name) => Ok(()),
            _ => self.inner.apply_entity(entity).await,
        }
    }
}
```

### Convex adapter

Generates TypeScript schema — no SQL execution at all:

- `prefers_batch_apply()` returns `true`
- `apply_entities()` generates `convex/schema.ts` from all table entities
- SQL types mapped to Convex validators (`v.string()`, `v.number()`, etc.)
- Import via `npx convex import`

### SQLite adapter

Subset of features — no schemas, extensions, enums, roles, or stored procedures.

### Feature gates (Cargo features)

```toml
[features]
default = ["postgres"]
postgres = ["sqlx"]
supabase = ["postgres"]
sqlite = ["dep:rusqlite"]
convex = []               # No DB driver needed — generates files
```

Consumers choose which adapters to compile in. The CLI binary enables all by default.

---

## Differences from Node.js version

| Aspect              | Node.js                              | Rust                                    |
| ------------------- | ------------------------------------ | --------------------------------------- |
| Runtime             | bun/node                             | Native binary, zero runtime deps        |
| SQL parser          | pgsql-parser (WASM, C via FFI)       | sqlparser-rs (pure Rust, multi-dialect) |
| PostgreSQL driver   | postgres npm + psql fallback         | sqlx (sole driver, no psql needed)      |
| Data import/export  | psql \copy (shell out)               | sqlx COPY FROM STDIN / COPY TO STDOUT   |
| Concurrency         | Single-threaded async                | Tokio multi-threaded async              |
| FP style            | Ramda (pick, omit, etc.)             | Iterator chains, struct methods         |
| Error handling      | try/catch, collect errors            | Result + collect errors on Entity       |
| GitHub download     | curl + tar (child processes)         | reqwest + flate2 + tar (pure Rust)      |
| DBML conversion     | @dbml/core npm                       | Port DDL cleanup, or shell to dbml-cli  |
| Config format       | YAML (js-yaml)                       | YAML (serde_yaml) — identical format    |

### No psql dependency

The Node.js version shells out to `psql` for data import/export (`\copy`). The Rust version uses `sqlx` natively:

- **Import:** `COPY table FROM STDIN WITH (FORMAT csv, HEADER true)` — stream file contents directly via `sqlx::raw_sql` + `PgCopyIn`
- **Export:** `COPY (SELECT * FROM table) TO STDOUT WITH (FORMAT csv, HEADER true)` — stream rows to file via `PgCopyOut`

This eliminates the `psql` runtime dependency entirely. The binary is fully standalone.

### DBML generation — native, no library dependency

The Node.js version cleans SQL DDL, feeds it to `@dbml/core` (JS library), then post-processes the output. No equivalent Rust library exists.

The Rust version **generates DBML directly** from the parsed table structures — no intermediate SQL-to-DBML conversion. This is simpler and more reliable: we already have the full table/column/FK/index graph from the parser.

#### Approach

```
Parsed entities (tables, enums, FKs, indexes)
  → dbml::generate() 
  → DBML text output
```

No DDL cleanup step needed — we're generating from structured data, not from raw SQL.

#### DBML format reference

The generator must produce valid DBML compatible with dbdocs.io / dbdiagram.io:

```dbml
Project "MyProject" {
  database_type: 'PostgreSQL'
  Note: 'Generated by dbd'
}

Enum config.status_type {
  active [note: 'Currently active']
  inactive
  archived
}

Table "config"."lookups" {
  "id" uuid [pk, not null, default: `uuid_generate_v4()`]
  "name" varchar(100) [not null, unique]
  "display_order" int [default: 0]
  "created_at" timestamptz [default: `now()`]

  indexes {
    name [unique, name: 'idx_lookups_name']
  }

  Note: 'Reference data lookup categories'
}

Table "config"."lookup_values" {
  "id" uuid [pk, not null, default: `uuid_generate_v4()`]
  "lookup_id" uuid [not null]
  "value" varchar(255) [not null]
  "sequence" int [default: 0]
  "is_active" boolean [default: true]

  indexes {
    (lookup_id, value) [unique]
  }
}

Ref: "config"."lookup_values"."lookup_id" > "config"."lookups"."id"
```

#### DBML syntax rules for the generator

**Strings and quoting:**
- Identifiers with special chars or reserved words: double quotes (`"column"`)
- String values: single quotes (`'text'`)
- SQL expressions: backticks (`` `now()` ``)
- Multi-line notes: triple single quotes (`'''...'''`)
- Types with spaces: double quotes (`"double precision"`)

**Column settings** (comma-separated in `[]`):

| Setting | When to emit |
|---------|-------------|
| `pk` | Column is in PRIMARY KEY |
| `not null` | Column has NOT NULL constraint |
| `unique` | Column has UNIQUE constraint (and not part of composite unique) |
| `increment` | Column uses SERIAL / GENERATED ALWAYS AS IDENTITY |
| `default: val` | Column has a DEFAULT — numbers/bools unquoted, strings in `'`, expressions in `` ` `` |
| `note: 'text'` | Column has a COMMENT ON COLUMN |

**Refs** — standalone format with delete/update actions:

```dbml
// Simple FK
Ref: schema.child.col > schema.parent.col [delete: cascade]

// Composite FK
Ref: schema.child.(col_a, col_b) > schema.parent.(col_a, col_b)
```

Ref operator mapping from SQL:
- FK on child table → `>` (many-to-one: child references parent)
- FK with UNIQUE on child column → `-` (one-to-one)

**Indexes:**

```dbml
indexes {
  column_name [unique]
  (col_a, col_b) [pk]
  column_name [type: hash, name: 'idx_name']
}
```

**Enums:**

```dbml
Enum schema.enum_name {
  value1
  value2 [note: 'Description']
}
```

Referenced in column type: `status schema.enum_name`

#### Generator module structure (`dbml.rs`)

```rust
pub fn generate_dbml(params: &DbmlParams) -> Vec<DbmlDocument> { ... }

struct DbmlParams {
    entities: Vec<Entity>,       // All project entities
    project: ProjectConfig,
    filter: Option<DbdocsFilter>,
}

struct DbmlDocument {
    file_name: String,
    content: Result<String, DbdError>,
}
```

Internal functions:

```rust
fn emit_project_block(name: &str, db_type: &str, note: Option<&str>) -> String;
fn emit_enum(name: &str, schema: &str, values: &[EnumValue]) -> String;
fn emit_table(table: &TableSnapshot, comments: &TableComments) -> String;
fn emit_column(col: &ColumnSnapshot, pk_columns: &HashSet<String>) -> String;
fn emit_indexes(indexes: &[IndexSnapshot], pk_columns: &HashSet<String>) -> String;
fn emit_refs(entities: &[Entity]) -> String;  // Standalone Ref blocks from FK constraints
fn quote_default(value: &str) -> String;      // Classify as number/bool/string/expression
fn quote_ident(name: &str) -> String;         // Double-quote if needed
```

#### What we skip (vs Node.js approach)

| Node.js step | Rust equivalent | Needed? |
|---|---|---|
| `removeCommentBlocks()` | — | No — generating from data, not SQL |
| `removeIndexCreationStatements()` | — | No |
| `removeNonSchemaStatements()` | — | No |
| `normalizeComments()` | — | No |
| `qualifyTableNames()` | — | No — names already schema-qualified |
| `removeRedundantInlineRefs()` | — | No — we emit standalone Refs only |
| `convertToDBML()` (@dbml/core) | `emit_table()` + `emit_refs()` | Replaced — native generation |
| `applyTableReplacements()` | — | No — names correct from the start |

The entire DDL cleanup pipeline disappears. We generate clean DBML directly from the parsed entity graph.
