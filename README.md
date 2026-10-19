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
    materialized_view/config/
      genders_mv.ddl
    procedure/staging/
      import_lookups.ddl
  import/              # Staging data (CSV, TSV, JSON, JSONL)
    staging/
      lookups.csv
  snapshots/           # Versioned schema captures (auto-generated)
  migrations/          # ALTER scripts (auto-generated from snapshot diffs)
```

**File path is the entity name.** `ddl/table/config/lookups.ddl` becomes entity `config.lookups` of type `table`. No configuration needed — dbd discovers entities from the folder structure.

## Commands

| Command | Purpose |
|---------|---------|
| `dbd init` | Scaffold a new project (postgres or supabase), or reverse-engineer one from a live DB with `--from-db` (a Supabase database gets a `supabase` target) or a DBML file with `--from-dbml` |
| `dbd merge` | Sync an existing database into the current project (reverse-engineer + reconcile) |
| `dbd split` | Divide a project into a workspace of modules, one per region of its scopes (`--from-scopes`; `--write` to move the files; `--undo` to put back an interrupted `--write`) |
| `dbd inspect` | Validate configuration, report errors and warnings |
| `dbd apply` | Apply schemas, entities, and pending migrations |
| `dbd import` | Load staging data from CSV, TSV, JSON and JSONL files, then call the procedures that read it |
| `dbd export` | Export table data — and the views `export:` lists — to csv/tsv/jsonl files |
| `dbd refresh` | Refresh materialized views now (`REFRESH MATERIALIZED VIEW [CONCURRENTLY]`); `--name <entity>` or `<schema>.*` to target a subset. Scheduled refresh is managed via pg_cron |
| `dbd deploy` | Fetch from GitHub or local path + apply + grants + import + RLS policies |
| `dbd combine` | Combine all DDL into a single SQL file |
| `dbd emit` | Translate the schema into another engine's DDL (MySQL, T-SQL, SQLite), reporting what each target cannot take — a type past its limits included |
| `dbd graph` | Output dependency graph as JSON |
| `dbd diagram` | Open the schema in the hosted interactive viewer (`--print-url` to print the link, `--json` for the raw model) |
| `dbd dbml` | Generate DBML documentation |
| `dbd reconcile` | Pre-release: diff the live DB against the design and apply ALTER/CREATE in place (no snapshots), with the design's apply hooks and grants |
| `dbd diff` | Read-only: the complete difference between the live DB and the design — columns, keys, constraints, indexes, column comments, enums, matview drift. Works after release; `--exit-code` for CI |
| `dbd release` (alias `baseline`) | Write a baseline snapshot — or, when snapshots exist, release at the latest — and lock in the snapshot/migration workflow (disables `reconcile`) |
| `dbd snapshot` | Capture schema state, generate migration SQL |
| `dbd migrate --status` | Show migration version status |
| `dbd format` | Format DDL files (river-style; layout and keyword case only — string literals, `$$` bodies, quoted names and comments stay byte for byte, and formatting twice changes nothing; `--check` for CI/pre-commit) |
| `dbd policies` | Apply RLS policies from `policies/` |
| `dbd doctor` | Audit/migrate design.yaml + DDL layout |
| `dbd reset` | Drop the project's own objects (with safety guards; PostgreSQL, Supabase and SQLite) and say what it dropped |
| `dbd install` | Install dbd's Claude Code skill + agent into `~/.claude` (or `./.claude` with `--project`) |

## Reading vs. connecting

dbd does two separable things, and it supports a different set of databases for
each. **Reading** a dialect means parsing its DDL into entities; **connecting**
means having an adapter that can apply to a live server.

| Dialect | Read its DDL | Connect and apply |
|---|---|---|
| PostgreSQL / Supabase | yes — full structure (columns, constraints, indexes) | yes |
| SQLite | yes — verbatim, the file *is* the model | yes |
| T-SQL (SQL Server) | yes — identity and references, **no columns** | **no adapter** |
| MySQL / MariaDB | yes — identity and references, **no columns** | **no adapter** |
| Convex | — (a codegen target, not a source) | yes, writes `schema.ts` |

So a T-SQL or MySQL project works with `dbd inspect`, `dbd graph`,
`dbd combine`, `parse_sql_as` and `project::survey`, and `apply`/`deploy` have
nothing to connect to — a `mysql://` or `sqlserver://` URL is refused by name
rather than attempted. `dbd emit` goes the other way, writing a PostgreSQL
schema out as MySQL, T-SQL or SQLite DDL.

Only the PostgreSQL reader produces a structured model, so `diff`, `reconcile`,
`emit` and snapshots need it: `diff`, `reconcile` and `emit` refuse the others
rather than compare nothing and report "in sync". See
[other dialects](docs/guide/13-other-dialects.md).

## Targets

Where `apply` can write. See the table above for what dbd can *read*.

| Target | Status | URL form |
|--------|--------|----------|
| PostgreSQL | Working (sqlx, PG17+) | `postgres://user:pass@host:5432/db` |
| Supabase | Working — the PostgreSQL adapter; a target named `supabase` exposes `public`, protects Supabase's schemas from prune and reset, and adds PostgREST grants | `postgres://...` (with `target: supabase`) |
| SQLite | Working (sqlx-sqlite, batched multi-row CSV/TSV/JSON/JSONL import, trigger-aware splitter) | `sqlite://./app.db`, `sqlite::memory:`, `file:/abs/path.db` |
| Convex | Working (codegen `schema.ts` with enums + FK `v.id`, sidecar state, `?deploy=true` auto-deploy, per-table `npx convex import --replace`) | `convex:` (default `./convex`), `convex://./out`, `convex://./out?deploy=true` |

### Adapter notes

- **SQLite** has no schemas, enums, roles, extensions, or stored procedures,
  so `dbd reset` there drops the project's tables and views, in one
  transaction.
  `Schema` entities are a no-op; `Enum` / `Function` / `Procedure` / `Role` /
  `Extension` / `Sequence` / `MaterializedView` entities error on apply. Entity names like `auth.users` are
  resolved as the bare table `users`. Imports use multi-row `INSERT … VALUES (?,?), …`
  batches inside one transaction (≤500 rows or 32k binds per batch); SQLite has
  no `TRUNCATE`, so the import's truncate runs `DELETE FROM`. The DDL
  formatter knows about SQLite `CREATE TRIGGER … BEGIN … END;` blocks and
  keeps them whole. Offline `inspect` classifies ~150 built-in functions /
  types plus the `sqlite_*` prefix as Internal without touching the DB.
- **Exposed schemas.** `schemas: [{ app: { exposed: true } }]` marks a schema
  as served from outside the database (PostgREST on Supabase). `dbd inspect`
  then reports tables in it with no RLS policy. Internal by default, except
  `public` on a `supabase` target. A different axis from the protected set
  above: ownership decides what `reconcile --prune` may drop, exposure decides
  what the internet can read. See [access control](docs/guide/09-access-control.md).
- **Convex** is a codegen target — `apply` writes `convex/schema.ts` from
  parsed `TableDef`s and tracks migrations in a sidecar `.dbd_state.json`.
  Names are flattened (`config.users` → `config_users`). SQL types map to
  `v.*` validators (`int*`/`numeric` → `v.number()`, `text`/`uuid` →
  `v.string()`, `jsonb` → `v.any()`, `bytea` → `v.bytes()`, arrays →
  `v.array(...)`, nullable → `v.optional(...)`). `Entity::Enum` emits
  `export const <name> = v.union(v.literal(…))` above `defineSchema`, and
  columns whose type names match are routed to the const. Foreign keys
  (inline or single-column table-level) emit `v.id("target_table")`.
  Append `?deploy=true` to the URL (or call `with_auto_deploy(true)`) to
  run `npx convex deploy` automatically after each apply; `dbd import`
  shells out to `npx convex import --table <flat_name> --replace -y <file>`,
  which replaces the table, so `import.options.truncate: false` has no effect
  there; a `.json` data file must be a JSON array.
  Whole-deployment export remains the Convex CLI's job (`npx convex export`)
  because the CLI doesn't expose per-table dumps.

## Sharing a database

Several projects can deploy into one database. Each entity has one owner — the
project whose design declares it — and no run changes or drops another
project's: declaring one is refused, `reconcile --prune` leaves it alone, and a
drop another project depends on is refused, naming the use. Move an entity with
`--adopt-from <owner>`, and see what shares a database with
`dbd inspect --shared`. See [sharing a database](docs/guide/14-sharing-a-database.md).

## Offline reference cache

After `dbd inspect --from-db`, dbd writes a snapshot of all user-defined
tables, views, and enum types to `<project>/.dbd/refcache.json`. Subsequent
runs of `dbd inspect` consult that snapshot to silence "Unresolved
reference" warnings even when no `DATABASE_URL` is available — useful in
CI, on planes, or when the database is temporarily down. Commit the file
to share the snapshot with the team, or `.gitignore` it for purely local
use.

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

Smart multi-snapshot: a column rename and a type change are made in place — `RENAME COLUMN`, `ALTER COLUMN … TYPE … USING` — so the column keeps its values and everything on it; what cannot be done in one step (a type change dbd cannot cast, an enum value removal) is split into safe migration stages.

Pending migrations run version by version, each whole before the next — its creates and alters, then
its data scripts, then its drops — so a database several versions behind ends where upgrading one
release at a time would. The run is one transaction, except that a version adding enum values
commits up to and including those additions first, because Postgres will not use a value in the
transaction that added it.

`dbd apply --dry-run` with a database configured lists the pending migrations in the order the run
executes them, each destructive statement marked `⚠`. A project older than the database — a branch
checked out from before the latest snapshot — is refused by `apply` and `deploy` and their dry runs,
rather than re-applying its older DDL over the newer schema. `apply -n <entity>` is refused while
migrations are pending: it builds one entity and cannot run them, so run `dbd apply` first.

## Environments

`-e` is `dev` or `prod` (default `prod`). It picks which `import/<env>/` data loads, and the
database records it. A `-e dev` run against a database marked prod is refused — `apply`, `deploy`,
`reconcile` and `import` (an ad-hoc `import -n -f` too), and their dry runs, before anything is
written — because it would lift `reset`'s prod guard and load dev seed data into production. A dev
database may be promoted to prod, and the run that promotes it says so; nothing re-marks a prod
one, and there is no override. So build a development database with `-e dev` from its first run:
one built without it is marked prod, and the way back is `dbd reset --force` under the all-scope or
the scope the database is pinned to (which forgets the database's record of the project), then a
run with `-e dev`.

The record is keyed on `project.name`. After a rename, a project with no record of its own on a
database that holds its objects is held to the record the old name left, and a refusal names it —
set `project.name` back rather than lose the guards.

## Deploy from GitHub

```sh
dbd deploy --source sensei-hq/daemon/database -d $DATABASE_URL
dbd deploy --source sensei-hq/daemon/database@v2.1 -d $DATABASE_URL
dbd deploy --source ./local/path -d $DATABASE_URL
```

Set `GITHUB_TOKEN` for a private repository; it is sent to `api.github.com`
only. `deploy` is the one command that takes a GitHub source — the others take a
local path.

## Scopes

One `design.yaml` can deploy different subsets of entities to different databases — for example, a full primary database and a smaller embedded-postgres "hub" that only needs a subset of schemas.

```yaml
scopes:
  hub:
    includes: [config, app.users, app.sessions]   # whole schema or specific entity
    deps: report                                    # report (default) | include
  reporting:
    excludes: [staging, app.audit_log]
```

Pass a scope at deploy time via `--scope`:

```sh
dbd deploy --scope hub --database $HUB_URL
dbd inspect --scope hub
```

**Rules:**

- Each entry in `includes`/`excludes` is a schema name (the whole schema, `CREATE SCHEMA` included), a qualified entity name like `app.users`, or a `schema.*` wildcard (the schema's entities, not the schema).
- `includes` omitted ⇒ start from the full set; `excludes` removes from it.
- `deps: report` (default) — if a scoped entity references a managed entity that is **not** in the scope, `dbd inspect --scope X` reports the gaps with their dependency chain and exits non-zero; `apply`/`import`/`deploy` refuse to proceed (including `--dry-run`).
- `deps: include` — every scope-aware command auto-expands the scope to the transitive dependency closure instead of erroring.
- `--deps <report|include>` overrides a scope's own `deps` setting for one run.
- `external:` is the only sanctioned way to declare a dependency that lives outside the managed scope; references to `external:` entries are never counted as gaps.
- Omit `default` (or omit `scopes:` entirely) to deploy the full set. Define `default:` only if a bare `dbd deploy` should itself deploy a subset. `--scope all` is always the full set.
- **Scope guard.** The first `apply`/`deploy`/`reconcile` pins a database to its resolved scope (recorded in `dbd.meta.scope`, or `_dbd_meta.scope` on SQLite); a later `apply`/`deploy`/`reconcile`/`reset`/`import`/`export`/`refresh`/`policies` under a *different* scope is refused — and so are the `apply`, `deploy`, `import`, `policies` and `reset` dry runs, given a database — unless you pass `--allow-scope-change` on `apply`/`deploy`/`reconcile` (which re-points the DB; `reset` takes it to bypass the check; `import`, `export`, `refresh` and `policies` have no such flag and never re-pin) — this stops a mistyped or forgotten `--scope` from building a divergent schema in the wrong database, or loading data into one. `diff` and `migrate --status`, which write nothing, note the pin instead of refusing.
- **A scope keeps what its entities need**: sequences are selected like tables, a table depends on the enum and sequence its columns use, the schema an allowed extension installs into comes with it (a scope that excludes that schema while keeping the extension is refused), and grants are applied only on the schemas the scope builds. A hook is skipped only when it touches a table the design manages outside the scope.
- **Every scoped command names its scope** — `scope 'hub': 7 of 12 entities` — except `graph`, which puts it in its JSON.
- **A scope narrows what is applied, not what the project owns** — `reconcile --prune --scope` never drops a table the design declares outside the scope. Upgrade to 0.24.1 before running a scoped prune: earlier releases did ([#40](https://github.com/sensei-hq/dbd/issues/40)). And upgrade to 0.25 before any `reconcile --prune` in a design with a role or an extension that has no `schema:`: earlier releases treated `public` as managed and dropped every undeclared table there.

The full guide is [scopes and multi-database deploys](docs/guide/07-scopes.md).

**Gap report example** (`deps: report`):

```
$ dbd inspect --scope hub
scope 'hub': 7 of 12 entities
✗ dependency gap: app.sessions requires app.tenants (out of scope)
    chain: app.sessions → app.tenants
Error: 1 dependency gap(s) in scope 'hub' — add them to the scope, or run with --deps include
```

## Workspaces

A project that has outgrown one design becomes a **workspace**: a `modules:`
block divides it into modules, each a design of its own — its own folder,
owner (`<project>/<module>`), version and history — deployed in `depends_on`
order. A module references what its dependencies declare without `external:`
entries; a reference past `depends_on`, or two modules declaring one entity, is
refused before any database is touched.

```yaml
modules:
  shared: {}                 # shared/ddl/…, shared/import/…
  core:
    depends_on: [shared]
  dojo:
    depends_on: [shared]

scopes:
  dojo:
    modules: [dojo]          # dojo and what it depends on
```

Every command but `init`, `merge` and `split` runs the modules in order
(`reset` in reverse); `--module core`
runs one with its dependencies, and running inside a module's folder runs that
module. A scoped project becomes a workspace with `dbd split --from-scopes`
(`--write` to move the files, `--undo` to put an interrupted one back); on each database, the modules take over the
project's entities on their first deploy. See [design.yaml](docs/guide/03-design-yaml.md)
and [sharing a database](docs/guide/14-sharing-a-database.md).

## Materialized views

Materialized views are a first-class entity type, discovered from
`ddl/materialized_view/<schema>/<name>.ddl` just like tables and views:

```sql
-- ddl/materialized_view/analytics/daily_sales.ddl
create materialized view if not exists daily_sales as
select date_trunc('day', created_at) as day, sum(total) as revenue
from shop.orders
group by 1
with data;

-- indexes are declared as trailing statements, exactly like a table's
create unique index if not exists daily_sales_day_uidx on daily_sales(day);
```

They apply after views (a matview may read tables and views) and are
reverse-engineered by `dbd merge` / `init --from-db` from `pg_matviews`; `apply`
adopts such a matview, unstamped in the database, when its deployed definition
is the design's, so a brownfield project diffs clean.
**PostgreSQL and Supabase only** — SQLite and Convex error on apply.

### Scheduled refresh (pg_cron)

Refresh is handled in-database by [pg_cron](https://github.com/citusdata/pg_cron).
Declare a shared schedule and optional per-view overrides in `design.yaml`:

```yaml
materialized_views:
  options:
    refresh: "0 2 * * *"       # shared cron schedule for every matview
    concurrently: true         # shared default
  overrides:
    analytics.top_products:
      refresh: "*/30 * * * *"  # override just this one
    analytics.realtime:
      concurrently: false
```

On `apply`, dbd syncs one pg_cron job per scheduled matview, named
`dbd-refresh:<project>:<schema>.<name>`, running `REFRESH MATERIALIZED VIEW
[CONCURRENTLY] …` — at most 63 bytes, which pg_cron 1.3 and 1.4 keep, a hash
ending a longer one. dbd only ever touches the project's own jobs with that
reserved prefix — another project's sharing the database, and hand-authored
cron jobs, are left alone. A job an older dbd named
`dbd:refresh:<schema>.<name>` is replaced for a matview the project declares,
and unscheduled when the view it refreshes no longer exists. A matview with no resolved schedule
gets no job; refresh it on demand with `dbd refresh`.

`dbd inspect` validates the config offline: scheduling requires `pg_cron` in
`target.postgres.extensions`, and `concurrently: true` requires the matview
to declare a unique index (Postgres needs one for `REFRESH … CONCURRENTLY`).

### Reconcile

`dbd reconcile` **creates** a matview that's missing, and records a content
hash (`dbd:hash=…`) in `dbd.matview_stamps` — the matview's own `COMMENT` stays
yours. When a matview's
definition later differs from the design, reconcile **warns** rather than
auto-recreating — dropping a matview means `DROP … CASCADE` (losing the
cached data, dependents, and grants), so applying a changed definition is
left to you (drop + recreate, or the snapshot/migrate workflow).

## Pre-commit integration

`dbd format --check` exits non-zero when any DDL file would be reformatted, so it drops into [pre-commit](https://pre-commit.com) directly. Add this to your `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/sensei-hq/dbd
  rev: v0.27.5
  hooks:
    - id: dbd-format
```

The `dbd-format` hook builds dbd from source via cargo on first install (slow once, cached after). It builds from the tag you pin, so that tag needs an installable Cargo package at the repo root — **v0.12.3 and later**. On an earlier `rev` pre-commit aborts during environment setup with `found a virtual manifest instead of a package manifest`; use `dbd-format-system` with those.

For contributors who already have `dbd` on PATH (via `cargo install dbd-cli`), `dbd-format-system` is the faster choice at any `rev` — it skips the build and runs the installed binary.

Both hooks scan the project's `ddl/` tree themselves, so pre-commit invokes them with no positional args (`pass_filenames: false` in the shipped hook spec).

## Use as a library

```toml
# Cargo.toml
[dependencies]
dbd-core = "0.25"
```

```rust
use dbd_core::Design;
use dbd_core::adapter::postgres::PostgresAdapter;
use dbd_core::design::Progress;
use std::path::Path;

async fn run_migrations(database_url: &str) -> anyhow::Result<()> {
    let design = Design::from_config(
        Path::new("database/design.yaml"),
        "prod",
    )?;

    let adapter = PostgresAdapter::new(database_url, &design.config().project.name).await?;

    // apply(adapter, name, dry_run, scope, progress)
    let scope = design.resolve_scope(None, None)?;
    design.apply(&adapter, None, false, Some(&scope), Progress::none()).await?;
    Ok(())
}
```

See [Embedding dbd-core](docs/guide/12-embedding.md) for the whole API.

## Documentation

- [What is dbd?](docs/guide/01-what-is-dbd.md)
- [Getting started](docs/guide/02-getting-started.md)
- [design.yaml reference](docs/guide/03-design-yaml.md)
- [Commands reference](docs/guide/04-commands.md)
- [Snapshots and migrations](docs/guide/05-snapshots-migrations.md)
- [Recommended workflows](docs/guide/06-recommended-workflows.md)
- [Scopes and multi-database deploys](docs/guide/07-scopes.md)
- [Targets and adapters](docs/guide/08-targets.md)
- [Access control](docs/guide/09-access-control.md)
- [Data: import and export](docs/guide/10-data.md)
- [The schema viewer](docs/guide/11-schema-viewer.md)
- [Embedding dbd-core](docs/guide/12-embedding.md)
- [Other dialects](docs/guide/13-other-dialects.md)
- [Sharing a database](docs/guide/14-sharing-a-database.md)
- [Changelog](CHANGELOG.md)
- [Design document](docs/design/architecture.md)

## License

MIT
