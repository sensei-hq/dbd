# Getting started

## Install

```sh
cargo install dbd-cli
```

Or build from source:

```sh
git clone https://github.com/sensei-hq/dbd.git
cd dbd
cargo build --release
```

## Create a project

```sh
dbd init -p myproject
cd myproject
```

This creates:

```
myproject/
  design.yaml              # Project configuration
  ddl/                     # DDL files
    table/config/
      lookups.ddl
      lookup_values.ddl
    view/config/
      genders.ddl
    procedure/staging/
      import_lookups.ddl
  import/                  # Staging data
    staging/
      lookups.csv
      lookup_values.csv
```

## Configure

Edit `design.yaml`:

```yaml
project:
  name: myproject

source:
  dialect: postgresql

target:
  postgres:
    url: $DATABASE_URL
    extensions:
      - uuid-ossp

schemas:
  - config
  - staging

import:
  staging: [staging]
  options:
    truncate: true
```

## Set the database URL

```sh
export DATABASE_URL=postgres://user:pass@localhost:5432/mydb
```

## Validate

```sh
dbd inspect
```

Output:
- `Everything looks ok` — ready to apply
- Errors and warnings listed per entity

## Apply schema

```sh
dbd apply
```

Applies all entities in dependency order: schemas, extensions, roles, enums, tables, views, functions, procedures.

## Load data

```sh
dbd import
```

Loads CSV/TSV/JSONL files from `import/` into staging tables, then calls import procedures automatically.

## Evolve the schema

```sh
# 1. Edit DDL files (add column, new table, etc.)

# 2. Validate
dbd inspect

# 3. Create a snapshot — captures diff, generates migration SQL
dbd snapshot --name "add notes column"

# 4. Review the migration
cat migrations/002/config/lookup_values.sql

# 5. Apply — migrations run automatically alongside DDL
dbd apply
```

## Generate documentation

```sh
dbd dbml
```

Produces `design.dbml` for use with [dbdocs.io](https://dbdocs.io) or [dbdiagram.io](https://dbdiagram.io).

## Deploy from GitHub

For automated deployments:

```sh
dbd deploy --source sensei-hq/daemon/database -d $DATABASE_URL
```

This fetches the repository, applies the schema, imports data, and cleans up.
