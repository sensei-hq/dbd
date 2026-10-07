# What is dbd?

dbd (Database Designer) is a tool for managing database schemas as code. You write SQL DDL files, organize them in folders, and dbd handles:

- **Dependency ordering** — tables with foreign keys are applied after the tables they reference
- **Schema migrations** — versioned snapshots with auto-generated ALTER scripts
- **Data loading** — CSV/TSV/JSONL files loaded into staging tables with automatic procedure calls
- **Documentation** — DBML generation for dbdocs.io / dbdiagram.io, and a hosted schema viewer (`dbd diagram`)
- **Multi-target deployment** — PostgreSQL, Supabase, SQLite, and Convex (TypeScript codegen)
- **Scoped deployments** — deploy a subset of the schema (a named "scope") to different databases from one design, with dependency-gap checking
- **Access control** — schema grants and row-level-security policies, kept as code

## Core concepts

### DDL files are the source of truth

Your schema is defined in standard SQL files under `ddl/`. Each file contains one entity — a table, view, materialized view, function, procedure, enum, sequence or role. The file path determines the entity name:

```
ddl/table/config/lookups.ddl                   → config.lookups (table)
ddl/view/config/genders.ddl                    → config.genders (view)
ddl/materialized_view/analytics/daily.ddl      → analytics.daily (materialized view)
ddl/procedure/staging/import.ddl               → staging.import (procedure)
ddl/enum/config/status.sql                     → config.status (enum)
ddl/sequence/config/ticket_no.ddl              → config.ticket_no (sequence)
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
  options:
    truncate: true
```

Everything else is auto-discovered from the folder structure.

### Two ways to change a schema

Before the first release, `dbd reconcile` diffs the live database against the
DDL and alters it in place — no migration files while the design is still
churning. Once you cut v1 with `dbd release`, every change is a versioned
snapshot. See [recommended workflows](06-recommended-workflows.md).

### Snapshots track schema evolution

After release, when you change a DDL file, create a snapshot to capture the diff:

```sh
dbd snapshot --name "add notes column"
```

This generates:
- `snapshots/002.json` — full schema state at this version
- `migrations/002/config/lookup_values.sql` — the ALTER TABLE statement

Next time you run `dbd apply`, the migration runs automatically.

### Adapters handle the target

The same parsed schema can deploy to different targets. The connection URL
picks the adapter:

- **PostgreSQL** (`postgres://`) — executes SQL directly via `sqlx`, one
  transaction per apply
- **Supabase** — the same PostgreSQL adapter; naming the target `supabase` adds
  Supabase's rules: `public` exposed by default, its platform schemas protected
  from prune and reset, and PostgREST access added to configured grants
- **SQLite** (`sqlite://`, `sqlite::memory:`, `file:`) — bare-name catalog,
  batched multi-row `INSERT` import (≤500 rows per batch); enums / roles /
  sequences / procedures / functions / extensions / materialized views error
  out, schemas are silently no-op; the DDL formatter keeps
  `CREATE TRIGGER … BEGIN … END;` atomic
- **Convex** (`convex:`, optional `?deploy=true`) — codegen target that
  writes `convex/schema.ts` with `v.*` validators, enum `export const … =
  v.union(v.literal(…))`, and `v.id("table")` for single-column FKs; names
  flatten from `schema.entity` to `schema_entity`; migration state lives
  in `.dbd_state.json`; `dbd import` shells out to `npx convex import`
  (with `import.options.truncate: false`)

dbd also *reads* T-SQL and MySQL DDL, and `dbd emit` writes the schema out for
MySQL, T-SQL or SQLite — but there is no adapter that applies to MySQL or SQL
Server. See [targets and adapters](08-targets.md) and
[other dialects](13-other-dialects.md).

## Who is this for?

- **Application developers** who want schema-as-code without an ORM
- **DevOps teams** automating database deployments from Git
- **Rust developers** who want to embed schema management in their apps
- **Teams using multiple databases** from the same SQL source
