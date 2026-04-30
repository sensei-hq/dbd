# dbd — Database Designer

A standalone CLI and embeddable library for managing database schemas as code.

Define your schema in SQL files, and dbd handles the rest: dependency ordering, migrations, data loading, documentation, and deployment — across PostgreSQL, Supabase, SQLite, and Convex targets.

## Vision

Database schema management shouldn't require a runtime, a migration framework, or a DSL. Write standard SQL, organize it in folders, and let the tool figure out the dependency graph, generate migrations, and deploy to any target.

dbd is:

- **A single binary** — zero runtime dependencies, instant startup
- **An embeddable library** — Rust apps import `dbd-core` to run apply/migrate/import programmatically
- **Multi-target** — same SQL source deploys to PostgreSQL, Supabase, SQLite, or Convex
- **Migration-aware** — versioned snapshots with auto-generated ALTER scripts
- **Safe** — database-side environment guards prevent accidental resets in production

## Quick start

```sh
# Install
cargo install dbd-cli

# Scaffold a new project
dbd init -p myproject
cd myproject

# Validate the design
dbd inspect

# Apply to a database
export DATABASE_URL=postgres://user:pass@localhost:5432/mydb
dbd apply

# Load staging data
dbd import

# Generate documentation
dbd dbml
```

## How it works

```
myproject/
  design.yaml          # Project config: schemas, targets, import/export
  ddl/                 # SQL files (auto-discovered)
    table/config/
      lookups.ddl
      lookup_values.ddl
    view/config/
      genders.ddl
    procedure/staging/
      import_lookups.ddl
  import/              # Staging data (CSV, TSV, JSONL)
    staging/
      lookups.csv
  snapshots/           # Versioned schema captures (auto-generated)
  migrations/          # ALTER scripts (auto-generated from snapshot diffs)
```

**File path is the entity name.** `ddl/table/config/lookups.ddl` becomes entity `config.lookups` of type `table`. No configuration needed — dbd discovers entities from the folder structure.

## Commands

| Command | Purpose |
|---------|---------|
| `dbd init` | Scaffold a new project |
| `dbd inspect` | Validate configuration, report errors and warnings |
| `dbd apply` | Apply schemas, entities, and pending migrations |
| `dbd import` | Load staging data from CSV/TSV/JSONL files |
| `dbd export` | Export table data to files |
| `dbd deploy` | One-shot: fetch from GitHub + apply + import |
| `dbd combine` | Combine all DDL into a single SQL file |
| `dbd graph` | Output dependency graph as JSON |
| `dbd dbml` | Generate DBML documentation |
| `dbd snapshot` | Capture schema state, generate migration SQL |
| `dbd migrate` | Apply pending migrations independently |
| `dbd doctor` | Audit design.yaml for stale entries |
| `dbd reset` | Drop all schemas (with safety guards) |

## Targets

| Target | Adapter | Status |
|--------|---------|--------|
| PostgreSQL | `sqlx` | Working — executes SQL, COPY streaming for data |
| Supabase | `sqlx` | Planned — PostgreSQL + managed infrastructure filtering |
| SQLite | `rusqlite` | Planned — subset features |
| Convex | File generation | Planned — generates TypeScript schema |

## Deploy from GitHub

```sh
# One-shot deployment from a remote repository
dbd deploy --source sensei-hq/daemon/database -d $DATABASE_URL

# Pin to a release tag
dbd deploy --source sensei-hq/daemon/database@v2.1 -d $DATABASE_URL
```

## Use as a library

```rust
use dbd_core::Design;

// Embed in a Rust web app for auto-migration at startup
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

## Documentation

- [What is dbd?](docs/guide/01-what-is-dbd.md)
- [Getting started](docs/guide/02-getting-started.md)
- [design.yaml reference](docs/guide/03-design-yaml.md)
- [Commands reference](docs/guide/04-commands.md)
- [Snapshots and migrations](docs/guide/05-snapshots-migrations.md)
- [Design document](docs/design/architecture.md)

## License

MIT
