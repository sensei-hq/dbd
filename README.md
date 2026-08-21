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
| `dbd init` | Scaffold a new project (postgres or supabase), or reverse-engineer one from a live DB with `--from-db` |
| `dbd merge` | Sync an existing database into the current project (reverse-engineer + reconcile) |
| `dbd inspect` | Validate configuration, report errors and warnings |
| `dbd apply` | Apply schemas, entities, and pending migrations |
| `dbd import` | Load staging data from CSV/TSV/JSONL files |
| `dbd export` | Export table data to csv/tsv/jsonl files |
| `dbd refresh` | Refresh materialized views now (`REFRESH MATERIALIZED VIEW [CONCURRENTLY]`); `--name <entity>` or `<schema>.*` to target a subset. Scheduled refresh is managed via pg_cron |
| `dbd deploy` | Fetch from GitHub or local path + apply + import + RLS policies |
| `dbd combine` | Combine all DDL into a single SQL file |
| `dbd graph` | Output dependency graph as JSON |
| `dbd diagram` | Open the schema in the hosted interactive viewer (`--print-url` to print the link, `--json` for the raw model) |
| `dbd dbml` | Generate DBML documentation |
| `dbd reconcile` | Pre-release: diff the live DB against the design and apply ALTER/CREATE in place (no snapshots) |
| `dbd release` (alias `baseline`) | Write a baseline snapshot and lock in the snapshot/migration workflow (disables `reconcile`) |
| `dbd snapshot` | Capture schema state, generate migration SQL |
| `dbd migrate --status` | Show migration version status |
| `dbd format` | Format DDL files (river-style; `--check` for CI/pre-commit) |
| `dbd policies` | Apply RLS policies from `policies/` |
| `dbd doctor` | Audit/migrate design.yaml + DDL layout |
| `dbd reset` | Drop project schemas (with safety guards) |
| `dbd install` | Install dbd's Claude Code skill + agent into `~/.claude` (or `./.claude` with `--project`) |

## Targets

| Target | Status | URL form |
|--------|--------|----------|
| PostgreSQL | Working (sqlx, PG17+) | `postgres://user:pass@host:5432/db` |
| Supabase | Working (grants, protected reset, external entities) | `postgres://...` (with `target: supabase`) |
| SQLite | Working (sqlx-sqlite, batched multi-row CSV/TSV/JSONL import, trigger-aware splitter) | `sqlite://./app.db`, `sqlite::memory:`, `file:/abs/path.db` |
| Convex | Working (codegen `schema.ts` with enums + FK `v.id`, sidecar state, `?deploy=true` auto-deploy, per-table `npx convex import`) | `convex:` (default `./convex`), `convex://./out`, `convex://./out?deploy=true` |

### Adapter notes

- **SQLite** has no schemas, enums, roles, extensions, or stored procedures.
  `Schema` entities are a no-op; `Enum` / `Function` / `Procedure` / `Role` /
  `Extension` / `MaterializedView` entities error on apply. Entity names like `auth.users` are
  resolved as the bare table `users`. Imports use multi-row `INSERT … VALUES (?,?), …`
  batches inside one transaction (≤500 rows or 32k binds per batch). The DDL
  formatter knows about SQLite `CREATE TRIGGER … BEGIN … END;` blocks and
  keeps them whole. Offline `inspect` classifies ~150 built-in functions /
  types plus the `sqlite_*` prefix as Internal without touching the DB.
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
  shells out to `npx convex import --table <flat_name> --replace -y <file>`.
  Whole-deployment export remains the Convex CLI's job (`npx convex export`)
  because the CLI doesn't expose per-table dumps.

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

Smart multi-snapshot: complex changes (column rename, type change, enum value removal) are automatically split into multiple safe migration stages.

## Deploy from GitHub

```sh
dbd deploy --source sensei-hq/daemon/database -d $DATABASE_URL
dbd deploy --source sensei-hq/daemon/database@v2.1 -d $DATABASE_URL
dbd deploy --source ./local/path -d $DATABASE_URL
```

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

- Each entry in `includes`/`excludes` is either a schema name (selects the whole schema) or a qualified entity name like `app.users`.
- `includes` omitted ⇒ start from the full set; `excludes` removes from it.
- `deps: report` (default) — if a scoped entity references a managed entity that is **not** in the scope, `dbd inspect --scope X` reports the gaps with their dependency chain and exits non-zero; `apply`/`import`/`deploy` refuse to proceed (including `--dry-run`).
- `deps: include` — `deploy` auto-expands the scope to the transitive dependency closure instead of erroring.
- `--deps <report|include>` overrides a scope's own `deps` setting for one run.
- `external:` is the only sanctioned way to declare a dependency that lives outside the managed scope; references to `external:` entries are never counted as gaps.
- Omit `default` (or omit `scopes:` entirely) to deploy the full set. Define `default:` only if a bare `dbd deploy` should itself deploy a subset.
- **Scope guard.** The first `apply`/`deploy`/`reconcile` pins a database to its resolved scope (recorded in `_dbd_meta.scope`); a later `apply`/`deploy`/`reconcile`/`reset` under a *different* scope is refused unless you pass `--allow-scope-change` (which re-points the DB) — this stops a mistyped or forgotten `--scope` from building a divergent schema in the wrong database.

**Gap report example** (`deps: report`):

```
$ dbd inspect --scope hub
scope 'hub': 7 entities
✗ dependency gap: app.sessions requires app.tenants (out of scope)
    chain: app.sessions → app.tenants
Error: 1 dependency gap(s) in scope 'hub' — add them to the scope, or run with --deps include
```

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
reverse-engineered by `dbd merge` / `init --from-db` from `pg_matviews`.
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
`dbd:refresh:<schema>.<name>`, running `REFRESH MATERIALIZED VIEW
[CONCURRENTLY] …`. dbd only ever touches jobs with that reserved prefix —
hand-authored cron jobs are left alone. A matview with no resolved schedule
gets no job; refresh it on demand with `dbd refresh`.

`dbd inspect` validates the config offline: scheduling requires `pg_cron` in
`target.postgres.extensions`, and `concurrently: true` requires the matview
to declare a unique index (Postgres needs one for `REFRESH … CONCURRENTLY`).

### Reconcile

`dbd reconcile` **creates** a matview that's missing, and stamps a content
hash (`COMMENT ON MATERIALIZED VIEW … IS 'dbd:hash=…'`). When a matview's
definition later differs from the design, reconcile **warns** rather than
auto-recreating — dropping a matview means `DROP … CASCADE` (losing the
cached data, dependents, and grants), so applying a changed definition is
left to you (drop + recreate, or the snapshot/migrate workflow).

## Pre-commit integration

`dbd format --check` exits non-zero when any DDL file would be reformatted, so it drops into [pre-commit](https://pre-commit.com) directly. Add this to your `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/sensei-hq/dbd
  rev: v0.10.10
  hooks:
    - id: dbd-format
```

The `dbd-format` hook builds dbd from source via cargo on first install (slow once, cached after). For contributors who already have `dbd` on PATH (via `cargo install dbd-cli`, brew, or a release binary), use `dbd-format-system` instead — it skips the build and runs the installed binary.

Both hooks scan the project's `ddl/` tree themselves, so pre-commit invokes them with no positional args (`pass_filenames: false` in the shipped hook spec).

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

    // apply(adapter, name, dry_run, scope, on_start, on_done, on_complete)
    let scope = design.resolve_scope(None, None)?;
    design.apply(&adapter, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {}).await?;
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
