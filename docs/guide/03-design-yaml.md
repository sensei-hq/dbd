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

import:
  staging: [staging]
  options:
    truncate: true
    null_value: ''
    format: csv
  tables:
    - staging.lookups
    - staging.lookup_values:
        truncate: false

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
```

## Sections

### `project`

| Field  | Type   | Required | Description |
|--------|--------|----------|-------------|
| `name` | string | yes | Project display name (used in DBML, migration tracking) |
| `note` | string | no | Project description |
| `version` | u32 | no | Schema version (managed by `dbd snapshot`) |

### `source`

| Field     | Type   | Default      | Description |
|-----------|--------|--------------|-------------|
| `dialect` | string | `postgresql` | SQL dialect of the DDL files |

### `target`

Each key is a target name. **The first listed target is used** — it supplies the platform config (extensions, roles, grants, `skip_schemas`) and its `url`, unless the URL is overridden per run by `-d`/`--database` or `$DATABASE_URL`. Additional entries are not selectable at run time (there is no target-name selector), so a second target only documents intent. To deploy one design to several databases, keep a single target and pair `-d`/`--database` with [`scopes`](#scopes):

```sh
dbd deploy --scope hive -d $HIVE_URL    # the adapter is chosen by the URL scheme
dbd deploy -d $MAIN_URL
```

The adapter (PostgreSQL/SQLite/Convex) is selected from the **URL scheme** (`postgres://`, `sqlite://`/`file:`, `convex:`), not the target name — so the target key is just a label.

**PostgreSQL / Supabase:**

| Field        | Type   | Description |
|--------------|--------|-------------|
| `url`        | string | Connection URL (env var references like `$DATABASE_URL` are expanded) |
| `extensions` | list   | Extensions to install (string or `{name, schema}` object) |
| `roles`      | list   | Roles to create (`{name, refers}` objects) |
| `grants`     | object | Schema grants — Supabase only (`{schema: {role: [perms]}}`) |
| `schemas`    | list   | PostgREST-exposed schemas — Supabase only |
| `skip_schemas` | list | Schemas to exclude from entity scanning |

**Grants example (Supabase):**

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

URL forms: `sqlite://relative/path.db`, `sqlite::memory:`, or `file:/abs/path.db`. The file is created on first connect. SQLite has no schemas — top-level `schemas:` entries still validate the design, but no `CREATE SCHEMA` is emitted. Enum/role/procedure/function/extension entities are rejected at apply time.

**Convex target:**

```yaml
target:
  convex:
    url: convex:                                # writes to ./convex/schema.ts
    # url: convex://./generated                # custom output directory
    # url: convex://./generated?deploy=true    # also runs `npx convex deploy` after each apply
```

The Convex adapter is a codegen target — it does not connect to a server. `dbd apply` parses each table's `TableDef`, maps SQL types to `v.*` validators (`int*`/`numeric` → `v.number()`, text-like → `v.string()`, `jsonb` → `v.any()`, `bytea` → `v.bytes()`, arrays → `v.array(...)`, nullable columns → `v.optional(...)`), and writes one `defineSchema({ ... })` file. Table names flatten from `schema.entity` to `schema_entity` because Convex forbids `.` in table names. Indexes render as `.index("name", ["col"])`.

**Enums and foreign keys.** `Entity::Enum` (DDL `CREATE TYPE … AS ENUM (…)`) emits `export const <name> = v.union(v.literal("…"), …);` above `defineSchema`; columns whose type matches an enum reference the const. Single-column foreign keys (inline or table-level) emit `v.id("target_table")` instead of the scalar validator.

**Migration + data state.** Versioning lives in `<output>/.dbd_state.json` (no `_dbd_meta` table because Convex isn't a SQL store). `dbd import` shells out to `npx convex import --table <flat_name> --replace -y <file>` per import entity. Whole-deployment export remains the Convex CLI's job (`npx convex export`) — the CLI doesn't expose per-table dumps.

Append `?deploy=true` to the URL to run `npx convex deploy` automatically after each apply. The CLI must be on PATH (or accessible via `npx`); set `with_cli_dry_run(true)` programmatically (or use `dbd apply --dry-run`) to log commands without spawning.

### `schemas`

List of schema names. dbd runs `CREATE SCHEMA IF NOT EXISTS` for each. Schemas referenced in entity file paths are auto-added.

### `external`

FK stubs for tables managed outside the project (e.g., Supabase `auth.users`):

```yaml
external:
  - name: auth.users
    note: Supabase managed authentication table
```

### `scopes`

Named subsets of entities, so one design can deploy to multiple databases (e.g. a full primary DB and a smaller embedded-postgres "hub").

```yaml
scopes:
  hub:
    includes: [config, app.users, app.sessions]   # whole schema or specific entity
    deps: report                                    # report (default) | include
  reporting:
    excludes: [staging.*, app.audit_log]           # wildcard drops a schema's entities
```

Each entry in `includes`/`excludes` is one of three forms:

- **`schema`** — a bare schema token: every entity in the schema **plus the `CREATE SCHEMA` entity itself**.
- **`schema.entity`** — one qualified entity (e.g. `app.users`).
- **`schema.*`** — a wildcard matching every entity under the schema, using the same `prefix.*` syntax as the [`ignore`](#ignore) list. Unlike the bare schema token it does **not** carry the schema entity, so `excludes: [staging.*]` drops staging's tables while keeping the `staging` schema, whereas `excludes: [staging]` drops the schema too. For `includes`, `schema.*` and `schema` are equivalent (the schema entity is re-added because its entities are present).

`includes` omitted means start from the full set; `excludes` removes from it. An unknown name or wildcard prefix that matches nothing is an error (typo protection).

| Field        | Type   | Default  | Description |
|--------------|--------|----------|-------------|
| `includes`   | list   | (all)    | Schema names, qualified entities, or `schema.*` wildcards to include |
| `excludes`   | list   | (none)   | Schema names, qualified entities, or `schema.*` wildcards to remove from the set |
| `deps`       | string | `report` | `report`: error on dependency gaps; `include`: auto-expand to transitive closure |
| `extensions` | list   | (all)    | Per-scope extension allowlist. Omitted ⇒ all target extensions apply; a list ⇒ only those apply; `[]` ⇒ none |

**`extensions`** restricts which of the `target.extensions` apply under this scope. Omit it for today's behavior (every target extension is installed). Set it to deploy a scope to a database that lacks one — e.g. a hub on embedded Postgres without `pgvector`:

```yaml
scopes:
  hub:
    includes: [config]
    extensions: []          # install no extensions on the hub DB
  search:
    includes: [docs]
    extensions: [vector]    # only pgvector, not the others
```

List each extension by its **bare name** (`vector`, `postgis`, `uuid-ossp`) — the same name dbd uses for the entity, even for extensions declared with a `schema:`. The allowlist is honored everywhere the scope is (apply/deploy and the read commands); roles and externals remain always-on regardless.

**`deps: report`** — `dbd inspect --scope X` lists every in-scope entity that references a managed entity outside the scope (with dependency chain) and exits non-zero. `apply`/`import`/`deploy` refuse to proceed until gaps are resolved — including their `--dry-run` modes, so a dry-run surfaces the same error a real run would.

**`deps: include`** — the working set silently expands to the full transitive dependency closure before the command runs. This applies to every scope-aware command (`apply`, `import`, `deploy`, `combine`, `dbml`, `graph`, `export`), not just `deploy`.

`external:` entries are never considered gaps regardless of `deps` setting.

Omit the `scopes:` block (or don't define `default:`) to deploy the full entity set. Define `default:` only if a bare `dbd deploy` should deploy a subset.

#### Worked example: a schema as an optional add-on

A common pattern is an optional schema (say `hive`) that ships to a dedicated database, while the main database deploys everything *except* it. Define one scope that includes the add-on and a `default` that excludes it:

```yaml
scopes:
  hive:
    includes: [hive]      # only the hive schema (its entities + the CREATE SCHEMA)
    deps: include         # auto-pull any shared tables hive references
  default:
    excludes: [hive]      # the full set minus hive (the schema isn't even created)
```

```sh
dbd deploy --scope hive -d $HIVE_URL   # the hive database: only hive
dbd deploy -d $MAIN_URL                # no --scope ⇒ the `default` scope ⇒ everything except hive
dbd deploy --scope all -d $MAIN_URL    # the true full set, including hive (bypasses `default`)
```

Two choices worth understanding:

- **`excludes: [hive]` vs `excludes: [hive.*]`.** The bare token drops hive's entities *and* the `CREATE SCHEMA hive` statement, so the main DB never creates the schema — usually what you want. Use `hive.*` instead if the empty schema should still exist on the main DB.
- **`deps: include` on `hive`.** If hive tables reference shared tables in other schemas, a `report`-policy `--scope hive` would fail with a dependency gap. `deps: include` expands the working set to pull those dependencies in automatically; alternatively list them in `includes` or declare them under [`external`](#external). Run `dbd inspect --scope hive` to see any gaps before deploying.

### `import`

| Field     | Type   | Description |
|-----------|--------|-------------|
| `staging` | list   | Schemas allowed for import (import fails for other schemas) |
| `options` | object | Default options: `truncate`, `null_value`, `format` |
| `tables`  | list   | Explicit table list (string or `{name: options}`); per-table `options` may include `env` (load only under matching `-e`) |
| `after`   | list   | SQL scripts (project-relative paths) run after data load — e.g. `import/loader.sql` |

### `export`

List of tables to export (string or `{name: options}`). Writes to `export/<schema>/<name>.<format>`.

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

List of reference names or patterns to ignore during validation. Useful for functions from extensions or shared schemas that aren't part of the project.

## What you don't put in design.yaml

- Individual tables, views, functions, procedures — auto-discovered from `ddl/`
- Column definitions — live in DDL files
- Migration scripts — auto-generated by `dbd snapshot`
