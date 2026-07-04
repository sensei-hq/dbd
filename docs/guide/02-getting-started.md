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
dbd init --name myproject
```

This creates:

```
design.yaml
ddl/
  table/.gitkeep
  view/.gitkeep
  function/.gitkeep
  procedure/.gitkeep
  enum/.gitkeep
  table/public/example.ddl
import/.gitkeep
```

## DDL folder layout

dbd auto-discovers entities from the `ddl/` tree — you never list tables, views, functions, etc. in `design.yaml`. The path encodes the entity's **type** and **schema**:

```
ddl/<type>/<schema>/<name>.ddl     # schema-qualified types
ddl/role/<name>.ddl                # roles have no schema
```

`<type>` is one of `table`, `view`, `function`, `procedure`, `enum`, or `role`. The file name (minus extension) is the entity name and the parent folder is its schema:

```
ddl/
  table/config/lookups.ddl         →  table     config.lookups
  view/config/active_users.ddl     →  view      config.active_users
  function/billing/charge.ddl      →  function  billing.charge
  enum/config/status.ddl           →  enum      config.status
  role/admin.ddl                   →  role      admin
```

Files use the `.ddl` or `.sql` extension. Schemas are auto-added from these paths, so a `ddl/table/config/…` file implies the `config` schema even if it isn't listed under `schemas:`.

**Use the singular type folder** (`table`, `function`, …) — it's canonical. The plural form (`functions/`, `tables/`, …) is still accepted for backward compatibility, but `dbd doctor` flags it and `dbd doctor --fix` migrates plural folders to singular. If a singular folder already exists, the contents are merged in; on a same-name file collision the **newer** file (by modification time) is kept and the older one is saved alongside as `<name>.ddl.bkp` (reported so you can reconcile).

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
dbd deploy --source ./local/path -d $DATABASE_URL
dbd deploy --dry-run --source owner/repo/path
```

GitHub sources are downloaded once and cached under `~/.cache/dbd`
(`~/Library/Caches/dbd` on macOS). To force a fresh copy:

```sh
dbd deploy --no-cache --source owner/repo/path -d $DATABASE_URL     # re-download this source
dbd deploy --clear-cache --source owner/repo/path -d $DATABASE_URL  # wipe the whole cache first
```

Both flags are no-ops for local path sources (`--clear-cache` still clears the cache).
