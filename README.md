# dbd — Database Designer

A standalone CLI and embeddable library for managing database schemas as code.

Define your schema in SQL files, and dbd handles the rest: dependency ordering, migrations, data loading, documentation, and deployment.

## Quick start

```sh
# Install
cargo install dbd-cli

# Scaffold a new project
dbd init --name myproject

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
| `dbd init` | Scaffold a new project (postgres or supabase) |
| `dbd inspect` | Validate configuration, report errors and warnings |
| `dbd apply` | Apply schemas, entities, and pending migrations |
| `dbd import` | Load staging data from CSV/TSV/JSONL files |
| `dbd export` | Export table data to csv/tsv/jsonl files |
| `dbd deploy` | Fetch from GitHub or local path + apply + import |
| `dbd combine` | Combine all DDL into a single SQL file |
| `dbd graph` | Output dependency graph as JSON |
| `dbd dbml` | Generate DBML documentation |
| `dbd snapshot` | Capture schema state, generate migration SQL |
| `dbd migrate --status` | Show migration version status |
| `dbd doctor` | Audit design.yaml for stale entries |
| `dbd reset` | Drop project schemas (with safety guards) |

## Targets

| Target | Status |
|--------|--------|
| PostgreSQL | Working (sqlx, PG17+) |
| Supabase | Working (grants, protected reset, external entities) |
| SQLite | Planned |
| Convex | Planned |

## Schema evolution

```sh
# 1. Edit DDL files
# 2. Create a snapshot — auto-generates migration SQL
dbd snapshot --name "add email to users"

# 3. Review the migration
cat migrations/002/config/users.sql

# 4. Apply — runs pending migrations automatically
dbd apply
```

Smart multi-snapshot: complex changes (column rename, type change, enum value removal) are automatically split into multiple safe migration stages.

## Deploy from GitHub

```sh
dbd deploy --source sensei-hq/daemon/database -d $DATABASE_URL
dbd deploy --source sensei-hq/daemon/database@v2.1 -d $DATABASE_URL
dbd deploy --source ./local/path -d $DATABASE_URL
```

## Use as a library

```toml
# Cargo.toml
[dependencies]
dbd-core = { git = "https://github.com/sensei-hq/dbd" }
```

```rust
use dbd_core::Design;
use dbd_core::adapter::postgres::PostgresAdapter;
use std::path::Path;

async fn run_migrations(database_url: &str) -> anyhow::Result<()> {
    let design = Design::from_config(
        Path::new("database/design.yaml"),
        "prod",
    )?;

    let adapter = PostgresAdapter::new(database_url, &design.config().project.name).await?;
    design.apply(&adapter, None, false).await?;
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
