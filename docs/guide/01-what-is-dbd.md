# What is dbd?

dbd (Database Designer) is a tool for managing database schemas as code. You write SQL DDL files, organize them in folders, and dbd handles:

- **Dependency ordering** — tables with foreign keys are applied after the tables they reference
- **Schema migrations** — versioned snapshots with auto-generated ALTER scripts
- **Data loading** — CSV/TSV/JSONL files loaded into staging tables with automatic procedure calls
- **Documentation** — DBML generation for dbdocs.io / dbdiagram.io
- **Multi-target deployment** — PostgreSQL, Supabase, SQLite, and Convex (TypeScript codegen)

## Core concepts

### DDL files are the source of truth

Your schema is defined in standard SQL files under `ddl/`. Each file contains one entity (table, view, function, procedure, enum). The file path determines the entity name:

```
ddl/table/config/lookups.ddl     → config.lookups (table)
ddl/view/config/genders.ddl      → config.genders (view)
ddl/procedure/staging/import.ddl → staging.import (procedure)
ddl/enum/config/status.sql       → config.status (enum)
```

No DSL, no migration files to write by hand, no ORM. Just SQL.

### design.yaml is the project manifest

One YAML file declares project metadata, target databases, schemas, and data operations:

```yaml
project:
  name: MyProject

source:
  dialect: postgresql

target:
  postgres:
    url: $DATABASE_URL
    extensions: [uuid-ossp]

schemas: [config, staging]

import:
  staging: [staging]
  options:
    truncate: true
```

Everything else is auto-discovered from the folder structure.

### Snapshots track schema evolution

When you change a DDL file, create a snapshot to capture the diff:

```sh
dbd snapshot --name "add notes column"
```

This generates:
- `snapshots/002.json` — full schema state at this version
- `migrations/002/config/lookup_values.sql` — the ALTER TABLE statement

Next time you run `dbd apply`, the migration runs automatically.

### Adapters handle the target

The same parsed schema can deploy to different targets:

- **PostgreSQL** (`postgres://`) — executes SQL directly via `sqlx`
- **Supabase** — PostgreSQL with managed-infrastructure filtering and grants
- **SQLite** (`sqlite://`, `sqlite::memory:`, `file:`) — bare-name catalog,
  batched multi-row `INSERT` import (≤500 rows per batch); enums / roles /
  procedures / functions / extensions error out, schemas are silently
  no-op; the DDL formatter keeps `CREATE TRIGGER … BEGIN … END;` atomic
- **Convex** (`convex:`, optional `?deploy=true`) — codegen target that
  writes `convex/schema.ts` with `v.*` validators, enum `export const … =
  v.union(v.literal(…))`, and `v.id("table")` for single-column FKs; names
  flatten from `schema.entity` to `schema_entity`; migration state lives
  in `.dbd_state.json`; `dbd import` shells out to `npx convex import`

## Who is this for?

- **Application developers** who want schema-as-code without an ORM
- **DevOps teams** automating database deployments from Git
- **Rust developers** who want to embed schema management in their apps
- **Teams using multiple databases** from the same SQL source
