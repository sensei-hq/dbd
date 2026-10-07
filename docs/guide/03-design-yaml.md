# design.yaml reference

The `design.yaml` file is the project manifest. It declares project metadata, targets, schemas, and data operations. DDL entities are auto-discovered from the `ddl/` folder — you don't list them here.

## Full example

```yaml
project:
  name: MyProject
  note: E-commerce database schema

source:
  dialect: postgresql

target:
  postgres:
    url: $DATABASE_URL
    extensions:
      - uuid-ossp
      - name: postgis
        schema: extensions
    roles:
      - name: advanced
        refers: [basic]
      - name: basic

schemas:
  - config
  - staging
  - extensions

external:
  - name: auth.users
    note: Supabase managed authentication table

apply:
  after:
    - sql/realtime.sql
    - script: sql/grants.sql
      writes: [config.lookups]

import:
  options:
    truncate: true
    null_value: ''
  tables:
    - staging.lookup_values:
        truncate: false
  after:
    - import/loader.sql

export:
  - config.lookups
  - config.lookup_values:
      format: jsonl

dbml:
  base:
    exclude:
      schemas: [staging, extensions]

ignore:
  - bfs
  - my_company.*

format:
  keyword_case: lower
```

## Sections

### `project`

| Field  | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes | Project display name (used in DBML, migration tracking, the viewer) |
| `note` | string | no | Project description — markdown; the viewer shows it on the overview |
| `version` | u32 | no | Schema version — written by `dbd snapshot`, `dbd release`, `dbd init --from-db` and `dbd merge` |
| `released` | bool | no | `true` once `dbd release` has cut v1. `dbd reconcile` refuses on a released project — changes go through `dbd snapshot` and `dbd apply` |

`released` is set by `dbd release` and by nothing else. `dbd init --from-db`
and `dbd merge` write snapshots without setting it, which leaves a project with
snapshots that is not released: `reconcile` still runs, and `dbd release`
refuses because snapshots exist. See
[recommended workflows](06-recommended-workflows.md#snapshots-but-not-released).

### `source`

| Field     | Type   | Default      | Description |
|-----------|--------|--------------|-------------|
| `dialect` | string | `postgresql` | SQL dialect of the DDL files — see below |
| `parser`  | string | (from `dialect`) | Override the reader: `pg_query`, `tsql`, `mysql` or `verbatim` |
| `search_path` | list | (session default) | Where unqualified names resolve in a file that states no `SET search_path` |

#### `search_path`

Every dbd DDL file is expected to open with `SET search_path TO <schema>;`.
Nothing generates that line — it is a convention you keep — so a file can
forget it, and then its unqualified names resolve against whatever the
connection happens to have.

`search_path` is the project's answer for that case:

```yaml
source:
  dialect: postgresql
  search_path: [app, shared]
```

A file that states its own path still wins; this only fills the gap. Omit the
key and PostgreSQL's own default applies (`"$user", public`), which depends on
the connecting role and can be moved by `ALTER ROLE`/`ALTER DATABASE … SET
search_path` — so dbd cannot know it from your files.

**Either way it is reported, not silent.** Every DDL file that states no path
is named in a warning saying what its names were resolved against instead.
`dbd inspect` counts them; `apply` and `deploy` print them. An empty list is
refused rather than treated as "no schemas".

`dialect` picks the reader; `parser` overrides that choice and you should not
normally need it.

| `dialect` | reader | structured? |
|---|---|---|
| `postgresql` / `postgres` / `supabase` | `pg_query` | yes |
| `tsql` / `mssql` / `sqlserver` | `tsql` | no |
| `mysql` / `mariadb` | `mysql` | no |
| `sqlite` | `verbatim` | no |
| anything else | `pg_query` | yes |

- **`pg_query`** (libpg_query, PostgreSQL's own grammar) builds a structured
  model: columns, constraints, indexes.
- **`tsql`** reads SQL Server statement *heads* — what each file declares and
  what it references — with a lexer rather than a grammar. Measured over 2,154
  real T-SQL files, `sqlparser`'s `MsSqlDialect` loses 99% of `CREATE
  PROCEDURE` and 95% of `CREATE TABLE`; Microsoft's own complete parser is
  .NET. It reads no column lists.
- **`mysql`** reads MySQL statement heads, the same way. It differs from
  `tsql` in three places that would be wrong the other way round: `ALTER` never
  carries a body (MySQL needs `DROP` + `CREATE` for that), `db.table` names a
  **database** rather than a schema because MySQL has none, and backticks quote
  while `#` comments. **Fixture-verified only** — no MySQL corpus was available,
  unlike the 2,154 files behind `tsql`.
- **`verbatim`** takes the file as written and applies it unchanged. This is
  what `dialect: sqlite` selects, and it is not a weaker fallback — it mirrors
  how SQLite is read on the other side, where `sqlite_master.sql` *is* the
  schema. A structured PostgreSQL parse would reject `AUTOINCREMENT`,
  `WITHOUT ROWID` and `STRICT` outright.

**Only a structured reader can diff.** `diff` and `reconcile` compare columns
and constraints, and `emit` translates them, so all three work on `pg_query`
projects and refuse on `tsql`, `mysql` and `verbatim` ones, naming the reason.
`apply`, `deploy`, `import` and `export` need an adapter for the database:
PostgreSQL, SQLite and Convex have one; MySQL and SQL Server do not, so a T-SQL
or MySQL project is read — `inspect`, `graph`, `combine` — and not applied. See
[other dialects](13-other-dialects.md).

`dbd init --from-db sqlite://…` writes `dialect: sqlite` for you.

`parser: sqlparser` was a second PostgreSQL parser kept during the libpg_query
migration and **was removed in 0.14.0** — a project still naming it fails to
load with a message saying so.

### `target`

Each key is a target name. **Only the first listed target is used** — it supplies the platform config (extensions, roles, grants, `skip_schemas`) and its `url`, unless the URL is overridden per run by `-d`/`--database` or `$DATABASE_URL`. Further entries are never read (there is no target-name selector), so declare one. To deploy one design to several databases, keep a single target and pair `-d`/`--database` with [`scopes`](#scopes):

```sh
dbd deploy --scope hive -d $HIVE_URL    # the adapter is chosen by the URL scheme
dbd deploy -d $MAIN_URL
```

The adapter (PostgreSQL/SQLite/Convex) is selected from the **URL scheme** (`postgres://`, `sqlite://`/`file:`, `convex:`), not the target name. The key still matters in one case: a target named **`supabase`** uses the same PostgreSQL adapter, but exposes `public` by default, keeps Supabase's own schemas out of `reconcile --prune` and `dbd reset`, and adds PostgREST `USAGE` to configured grants. See [targets and adapters](08-targets.md#what-the-supabase-key-changes).

A `url` written as a whole `$VAR` is read from the environment. A variable that
is not set is used literally, and the connection fails.

**PostgreSQL / Supabase:**

| Field        | Type   | Description |
|--------------|--------|-------------|
| `url`        | string | Connection URL (a whole-value `$VAR` is expanded) |
| `extensions` | list   | Extensions to install (string or `{name, schema}` object) |
| `roles`      | list   | Roles to create (`{name, refers}` objects) |
| `grants`     | object | Schema grants, `{schema: {role: [perms]}}` — for any target key; see [`schemas`](#schemas) |
| `skip_schemas` | list | Schemas whose entities are dropped from the design — the target owns them. The schema itself is still created if listed under `schemas:` |
| `schemas`    | list   | Reserved — parsed, not used (`dbd doctor --fix` writes it when migrating an old config). Use [`exposed`](#exposed) |
| `path`       | string | Reserved — parsed, not used |

**Grants example:**

```yaml
target:
  supabase:
    url: $DATABASE_URL
    grants:
      config:
        anon: [usage, select]
        authenticated: [usage, select, insert, update, delete]
```

**SQLite target:**

```yaml
target:
  sqlite:
    url: sqlite://./app.db
```

URL forms: `sqlite://relative/path.db`, `sqlite::memory:`, or `file:/abs/path.db`. The file is created on first connect. SQLite has no schemas — top-level `schemas:` entries still validate the design, but no `CREATE SCHEMA` is emitted. Enum, role, sequence, procedure, function, extension and materialized-view entities are rejected at apply time.

**Convex target:**

```yaml
target:
  convex:
    url: "convex:"                              # writes to ./convex/schema.ts (quote it: YAML)
    # url: convex://./generated                # custom output directory
    # url: convex://./generated?deploy=true    # also runs `npx convex deploy` after each apply
```

The Convex adapter is a codegen target — it does not connect to a server. It writes tables and enums only; other entity types are skipped. `dbd apply` parses each table's `TableDef`, maps SQL types to `v.*` validators (`int*`/`numeric` → `v.number()`, text-like → `v.string()`, `jsonb` → `v.any()`, `bytea` → `v.bytes()`, arrays → `v.array(...)`, nullable columns → `v.optional(...)`), and writes one `defineSchema({ ... })` file. Table names flatten from `schema.entity` to `schema_entity` because Convex forbids `.` in table names. Indexes render as `.index("name", ["col"])`.

**Enums and foreign keys.** `Entity::Enum` (DDL `CREATE TYPE … AS ENUM (…)`) emits `export const <name> = v.union(v.literal("…"), …);` above `defineSchema`; columns whose type matches an enum reference the const. Single-column foreign keys (inline or table-level) emit `v.id("target_table")` instead of the scalar validator.

**Migration + data state.** Versioning lives in `<output>/.dbd_state.json` (no `_dbd_meta` table because Convex isn't a SQL store). `dbd import` shells out to `npx convex import --table <flat_name> --replace -y <file>` per import entity. `--replace` empties the table first, so the truncate step sends nothing and `import.options.truncate: false` has no effect on Convex; a `.json` file must be a JSON array, since Convex's importer reads no JSON Lines from one. Whole-deployment export remains the Convex CLI's job (`npx convex export`) — the CLI doesn't expose per-table dumps.

Append `?deploy=true` to the URL to run `npx convex deploy` automatically after each apply. The CLI must be on PATH (or accessible via `npx`); set `with_cli_dry_run(true)` programmatically (or use `dbd apply --dry-run`) to log commands without spawning.

### `schemas`

List of schema names. dbd runs `CREATE SCHEMA IF NOT EXISTS` for each. Schemas referenced in entity file paths are auto-added.

A schema entry may also be an object carrying `grants:` (and [`exposed`](#exposed)). Schema grants and the first target's `grants:` both apply, whatever the target is named:

```yaml
schemas:
  - config
  - staging:
      grants:
        app_user: [usage, select]
        app_admin: [usage, select, insert, update, delete]
```

This emits `GRANT USAGE ON SCHEMA` (for `usage`, written in lower case), `GRANT <perms> ON ALL TABLES IN SCHEMA`, and matching `ALTER DEFAULT PRIVILEGES` for each role, then `NOTIFY pgrst, 'reload config'`. If the same schema also has a `target.grants` entry, the target's per-role permissions merge in on top (adding new roles, replacing the list for roles present in both). On a `supabase` target, configured grants also give `anon`, `authenticated` and `service_role` `USAGE` on every listed schema, for PostgREST.

`dbd apply` and `dbd deploy` apply grants once the schema is in place — after
the entities and the `apply.after` hooks, before import and policies. They are
not applied by `--dry-run` and not filtered by `--scope`. Targets without a SQL
grant model (SQLite, Convex) skip them with a note rather than failing. See
[access control](09-access-control.md).

#### `exposed`

Whether something **outside** the database serves this schema — PostgREST on
Supabase. A table in an exposed schema is reachable by `anon` over HTTP; a
table anywhere else is not.

```yaml
schemas:
  - app:
      exposed: true
  - internal          # private: the default
```

Internal unless declared, with one exception: on a `supabase` target `public`
is exposed by default, mirroring Supabase itself. An explicit `exposed: false`
overrides that — a bare schema name does not, because saying nothing is not
the same as saying no. Nothing is exposed on a plain `postgres` target, where
nothing serves HTTP.

**What it buys you.** `dbd inspect` reports every table in an exposed schema
with no RLS policy declared:

```
⚠ No RLS policy on 1 table(s) in exposed schema(s) app:
  app.orders
```

Advisory only — it never changes the exit code, because dbd cannot know the
author did not mean it. A policy is a file at `policies/<schema>/<table>.sql`;
dbd checks that one exists rather than parsing it.

This is a **different axis** from whether Supabase owns a schema. `extensions`
is Supabase's and not exposed; your own `app` schema can be exposed without
being Supabase's. Platform ownership decides what `reconcile --prune` may
touch; exposure decides what the internet can read.

### `external`

Tables and functions managed outside the project (e.g., Supabase `auth.users`), so references to them resolve:

```yaml
external:
  - name: auth.users
    note: Supabase managed authentication table
```

A reference to an external entity is never an "Unresolved reference" warning
and never a [scope](#scopes) dependency gap; `dbd dbml` draws it as a stub
table. Only `name` is read — `note` documents it for people, and a `columns:`
list is parsed but not used.

### `scopes`

Named subsets of entities, so one design can build several databases. The full
guide is [scopes and multi-database deploys](07-scopes.md); this is the
reference.

```yaml
scopes:
  hub:
    includes: [config, app.users, app.sessions]   # whole schema or specific entity
    deps: report                                    # report (default) | include
    extensions: []                                  # install no target extensions
  reporting:
    excludes: [staging.*, app.audit_log]           # wildcard drops a schema's entities
  everything: all                                   # the full set under another name
  default:
    excludes: [hive]                                # what a run without --scope builds
```

| Field        | Type   | Default  | Description |
|--------------|--------|----------|-------------|
| `includes`   | list   | (all)    | Schema names, qualified entities, or `schema.*` wildcards to include |
| `excludes`   | list   | (none)   | Schema names, qualified entities, or `schema.*` wildcards to remove from the set |
| `deps`       | string | `report` | `report`: error on dependency gaps; `include`: auto-expand to transitive closure |
| `extensions` | list   | (all)    | Per-scope extension allowlist. Omitted ⇒ all target extensions apply; a list ⇒ only those apply; `[]` ⇒ none |

- A bare `schema` selects its entities **and** its `CREATE SCHEMA`; `schema.*`
  selects the entities only; `schema.entity` selects one entity. A name or
  wildcard that matches nothing is an error. Standalone sequences are not
  selectable in 0.24.1, so a named scope builds none — see
  [scopes](07-scopes.md#defining-a-scope).
- A scope's value may be the bare string `all` — the full set. Any other string
  is refused.
- **`all`** and **`default`** are reserved: `--scope all` is always the full
  design (so a scope you define named `all` is never used), and `default`, when
  defined, is what a run without `--scope` builds.
- An [`external`](#external) entry is never a dependency gap.

### `apply`

Lifecycle hooks around the DDL phase, for SQL dbd does not model — attaching a table to a publication, granting on objects.

| Field    | Type | Description |
|----------|------|-------------|
| `before` | list | Scripts run after the design and scope checks pass, before the first entity is applied |
| `after`  | list | Scripts run once every entity is applied and every matview stamped — and **before** `policies/` |

`apply.after` runs before `policies/` because publications and grants attach to objects that must already exist, while RLS policies are independent of them. A hook that needs a policy to exist will not find one.

These run on `dbd apply` and `dbd deploy`. `import.after` runs with the import
phase instead — on `dbd import` and `dbd deploy`, never on `dbd apply`. Post-DDL
SQL that lives in `import.after` today (publications, grants) belongs in
`apply.after`: on a plain `dbd apply` an `import.after` hook does nothing.

The `grants:` declared in `design.yaml` are applied after `apply.after`, so a
hook cannot rely on them yet; grants written *in* a hook script are the hook's
own.

See [Hook scripts](#hook-scripts) for the entry format and how scopes filter them.

### `import`

Data files are found by convention — every `import/<schema>/<table>.<csv|tsv|json|jsonl>`
loads into `<schema>.<table>` — so this section only tunes the load. The full
guide is [import and export](10-data.md).

| Field     | Type   | Description |
|-----------|--------|-------------|
| `options` | object | `truncate` (default `true`: empty each table before loading it — `TRUNCATE` on PostgreSQL, `DELETE FROM` on SQLite; a Convex import always replaces) and `null_value` (default `''`) |
| `tables`  | list   | Per-table overrides of `truncate`, `format` and `null_value` (`{name: options}`). Not a list of what to load — every data file loads |
| `after`   | list   | SQL scripts run after the data is loaded and the import procedures have run — on `dbd import` and `dbd deploy`. See [Hook scripts](#hook-scripts) |
| `staging` | list   | Reserved — parsed, not used |

`options.format` and a per-table `env` are parsed but not used either: the
format comes from the file's extension (or a per-table `format`), and the
environment from the folder.

`null_value` is the sentinel string that means SQL NULL in a CSV/TSV data file. It defaults to `''` (empty string), so by default an empty cell loads as NULL — this is unchanged unless you configure a sentinel. Set a non-empty sentinel (e.g. `\N`, Postgres's own COPY default) when your data files need to distinguish a genuine empty string from NULL: once configured, only a cell matching the sentinel exactly becomes NULL, and a plain empty cell loads as a literal empty string. A per-table `null_value` under `tables:` overrides the global `options.null_value` for that table only:

```yaml
import:
  options:
    null_value: ''       # default: empty cell → NULL
  tables:
    - staging.legacy_export:
        null_value: '\N' # this table's data files use \N for NULL; empty cells are literal ''
```

> **Environment-specific data** is by folder, not config: files under `import/<env>/<schema>/<file>` load only when `-e <env>` matches; files directly under `import/<schema>/` load in every environment (see [Commands → import](04-commands.md#dbd-import)).

### Hook scripts

`apply.before`, `apply.after` and `import.after` all take the same entry format — a project-relative path, or an object that declares the tables the script touches:

```yaml
apply:
  after:
    - sql/realtime.sql                 # dependencies derived from the SQL
    - script: sql/grants.sql           # dependencies declared explicitly
      writes: [app.messages]
```

Scripts run in the order written. A path dbd cannot find is an error, not a skip — a hook you declared and dbd cannot locate is a misconfiguration, and continuing past it is how a declared hook comes to do nothing without saying so. Paths must stay inside the project: `..` and absolute paths are refused.

**Scope filtering.** Under `--scope`, a hook runs only when every table it depends on is in the working set — the same rule that already governs staging entries. A hook whose table the scope excludes is skipped and reported:

```
⚠ after-script import/loader.sql skipped — needs staging.values, which is outside scope 'partial'
```

The run continues; a scoped run is a normal workflow, not an error. This exists so a loader doing `INSERT INTO target SELECT … FROM staging_a JOIN staging_b` does not run against half-loaded data when `staging_b` was scoped out.

**Where `writes:` is needed.** dbd derives a script's tables by parsing it, which works when they appear as SQL identifiers. It cannot work when they are *data* — inside a `format()` string or an `array[…]` literal, as a realtime-publication hook typically is. No parser can recover those, so declare them with `writes:`.

> **Scope filtering is a correctness aid, not a security boundary.** A script whose dependencies cannot be derived and that declares no `writes:` always runs — the safe direction, since silently skipping a hook because analysis came up short would hide, say, a realtime hook quietly not firing. Never use a scope to *prevent* a hook from running.

### `export`

What `dbd export` writes, and in what format:

```yaml
export:
  - config.lookups
  - config.lookup_values:
      format: jsonl        # always JSON Lines, whatever -f says
```

- **Omitted or empty** — `dbd export` writes every table in the design (within
  the scope).
- **Listed** — only these. `dbd export -n <name>` then has to name one of them,
  or the export refuses.
- An entry may be a **table, view or materialized view**; a view exports the
  rows its query returns. An entry that names none of those in the design is
  reported, and the rest still export.
- A per-entry `format` (`csv`, `tsv` or `jsonl`) wins over `-f`.

Files are written to `export/<schema>/<name>.<format>`, or flat into the
directory `-o` names. See [import and export](10-data.md#export).

### `materialized_views`

Refresh scheduling for materialized views (entities under `ddl/materialized_view/`). A shared `options` block applies to every matview; `overrides` tweaks individual ones by qualified name. dbd compiles this into **pg_cron** jobs (named `dbd:refresh:<schema>.<name>`) synced on `apply`/`deploy`.

```yaml
materialized_views:
  options:
    refresh: "0 2 * * *"       # shared cron schedule for every matview (omit ⇒ no scheduled refresh)
    concurrently: true         # shared default; REFRESH … CONCURRENTLY needs a unique index
  overrides:
    analytics.top_products:
      refresh: "*/30 * * * *"  # override just this matview's schedule
    analytics.realtime:
      concurrently: false      # override just the concurrently flag (schedule inherited)
```

| Field | Type | Effect |
|-------|------|--------|
| `options.refresh` | cron string | Shared 5-field cron schedule; a matview with no resolved schedule gets no pg_cron job (refresh it on demand with `dbd refresh`) |
| `options.concurrently` | bool | Shared default for `REFRESH … CONCURRENTLY` (requires a unique index on the matview) |
| `overrides.<schema>.<name>` | `{refresh?, concurrently?}` | Per-matview overlay onto `options` (each field falls back to the shared default) |

**Requirements & validation.** Scheduling requires the `pg_cron` extension in `target.postgres.extensions`; `concurrently: true` requires the matview to declare a unique index. `dbd inspect` reports both, offline. PostgreSQL/Supabase only.

**Reconcile & diff.** `dbd reconcile` creates an absent matview (stamping a `dbd:hash` sentinel) but only **warns** when a deployed matview's definition drifts — it never auto-drops one (a recreate is `DROP … CASCADE`). To apply a changed definition, drop it manually, then `apply`/reconcile recreates it. `reconcile --dry-run` previews these creates and drift warnings, and read-only `dbd diff` reports each matview as `missing`/`drifted`/`unstamped`/`orphan` (in `--json` and `--exit-code`).

### `dbml`

Each key becomes a separate DBML output file. `dbd dbml` writes them all into the parent directory of the `-f` argument.

| Field | Type | Effect |
|-------|------|--------|
| `include.schemas` / `include.tables` | list | Only matching entities appear |
| `exclude.schemas` / `exclude.tables` | list | Removes matching entities after `include` is applied |
| `output` | string | Filename for this document (default: `<key>.dbml`) |
| `auto_group_by_schema` | bool | Emit `TableGroup <schema>` for each schema present in the filtered set |
| `groups` | list of `{name, tables}` | Explicit `TableGroup` blocks; name takes precedence over `auto_group_by_schema` for that schema |

```yaml
dbml:
  base:
    exclude: { schemas: [staging] }
    auto_group_by_schema: true
  core:
    include: { schemas: [config] }
    output: core.dbml
    groups:
      - name: lookups
        tables: [config.lookups, config.lookup_values]
```

Composite foreign keys (multi-column constraints) emit DBML tuple syntax automatically — `Ref: "shop"."orders".("user_id", "tenant_id") > "auth"."memberships".("user_id", "tenant_id")`.

### `ignore`

References dbd should not try to resolve — functions from extensions, objects in
schemas the project does not manage:

```yaml
ignore:
  - bfs              # an exact name
  - my_company.*     # every name in a schema
```

Before references are resolved, any reference matching an entry is dropped:
it raises no "Unresolved reference" warning, and it creates no dependency edge,
so it plays no part in apply order or [scope](#scopes) gaps. Matching is
case-insensitive, and there are two forms only — an exact name, and `prefix.*`,
where the prefix is matched exactly (`supabase_*.*` matches nothing). `ignore`
does not exclude files or entities; for that, use `target.skip_schemas`.

### `format`

Options for `dbd format` (and `dbd inspect --fix`). Every field is optional.

```yaml
format:
  keyword_case: lower        # lower | upper | preserve
  comma_style: leading       # leading | trailing
  type_alignment: 27         # column where a column's type starts; 0 = off
  indent: 2                  # spaces per level
  query_style: river         # river | none — align SELECT bodies
  gutter: 10                 # river: width of the right-aligned keyword column
```

| Field | Default | Values |
|---|---|---|
| `keyword_case` | `lower` | `lower`, `upper`, `preserve` |
| `comma_style` | `leading` | `leading`, `trailing` |
| `type_alignment` | `27` | a column number; `0` turns alignment off |
| `indent` | `2` | spaces |
| `query_style` | `river` | `river`, `none` |
| `gutter` | `10` | the river's keyword width — wide enough for `inner join` |

See [`dbd format`](04-commands.md#dbd-format) for what each style does.

## What you don't put in design.yaml

- Individual tables, views, functions, procedures — auto-discovered from `ddl/`
- Column definitions — live in DDL files
- Migration scripts — auto-generated by `dbd snapshot`
