# Targets and adapters

dbd reads one design and writes it to PostgreSQL, Supabase, SQLite or Convex.
Three things decide how a run talks to a database: which `target:` entry is
used, which URL it connects to, and which adapter that URL selects.

## One target per design

```yaml
target:
  supabase:
    url: $DATABASE_URL
    extensions: [pgcrypto]
    grants:
      app:
        authenticated: [usage, select, insert, update, delete]
```

**Only the first `target:` entry is used** — the first key as written in the
file. It supplies the platform configuration (extensions, roles, grants,
`skip_schemas`) and a default URL. There is no flag that selects a different
entry, so a second entry is never read; don't write one.

The connection comes from, in order: `-d`/`--database`, then `$DATABASE_URL`,
then the target's `url`. A `url` written as a whole `$VAR` is expanded from the
environment; if the variable is unset, the literal string is used and the
connection fails. Interpolation inside a longer string is not supported.

To deploy one design to several databases, keep the one target and change the
URL per run — pairing it with a [scope](07-scopes.md) when each database needs a
different subset.

## The URL picks the adapter

| URL | Adapter |
|---|---|
| `postgres://…`, `postgresql://…` (and anything not listed below) | PostgreSQL, via sqlx |
| `sqlite://path.db`, `sqlite::memory:`, `file:/abs/path.db` | SQLite, via sqlx — the file is created on first connect |
| `convex:`, `convex:./dir`, `convex://./dir`, `…?deploy=true` | Convex — a code generator, not a connection |
| `mysql://`, `mariadb://`, `sqlserver://`, `mssql://`, `jdbc:sqlserver://` | **none** — refused by name |

The last row is deliberate. dbd *reads* T-SQL and MySQL DDL, and `dbd emit`
*writes* it, but there is no adapter that applies to either engine, so a URL
naming one is refused rather than handed to the PostgreSQL driver:

```
Error: Failed to connect to database: Config error: no adapter for MySQL: dbd can READ MySQL DDL (source.dialect) but cannot connect to one, so `apply`, `deploy`, `diff` and `reconcile` are not available. `parse_sql_as`, `project::survey` and `dbd inspect` work offline.
```

See [Other dialects](13-other-dialects.md) for what a T-SQL or MySQL project can
do.

## What the `supabase` key changes

The adapter comes from the URL, so a target named `supabase` uses the same
PostgreSQL adapter as one named `postgres`. The **key** is still not just a
label. When the first target is named `supabase`, four things change:

1. **`public` is exposed by default.** `dbd inspect` reports tables in exposed
   schemas that have no RLS policy, and on a `supabase` target `public` counts
   as exposed (when it is listed under `schemas:`) unless it says
   `exposed: false`. See [Access control](09-access-control.md#exposed-schemas).
2. **`reconcile --prune` leaves Supabase's own schemas alone.** Drops in `auth`,
   `storage`, `realtime`, `graphql_public`, `supabase_functions`, `pgbouncer`,
   `pgsodium`, `vault`, `extensions` and `supabase_migrations` are filtered out of
   the plan. (`public` is not in this list — it is yours.)
3. **Configured grants get PostgREST access.** When the design declares any
   grants, each schema listed under `schemas:` also gets `GRANT USAGE` for
   `anon`, `authenticated` and `service_role`. See
   [Access control](09-access-control.md#postgrest-on-supabase).
4. **`dbd reset` protects the Supabase schemas.** `reset --schemas` never drops
   `public`, `auth`, `storage`, `realtime`, `graphql_public`,
   `supabase_functions`, `pgbouncer`, `pgsodium`, `vault`, `extensions` or
   `supabase_migrations` (see below).

On every target, `pg_catalog`, `information_schema`, `pg_toast` and dbd's own
`dbd` schema are never dropped or pruned.

`source.dialect: supabase` is something else: a spelling of `postgresql` for
the reader. It changes nothing about the target.

`dbd init --from-db` names the target it writes `supabase` when the database
has both an `auth` and a `storage` schema — Supabase's own — and `postgres` (or
`sqlite`) otherwise. `dbd init --from-dbml --target supabase` does the same for a
DBML source, which names no platform.

### `reset --target`

`dbd reset` decides what is protected by a target name: the design's target key
by default — the first `target:` entry, so a Supabase project's schemas are
protected without passing anything — or the `--target` flag when given.

```sh
dbd reset --schemas --dry-run                    # protected set from the design's target key
dbd reset --schemas --target postgres --dry-run  # override: drop public too
```

With `postgres`, `public` **is** dropped by `--schemas`, and every role the design
declares (`ddl/role/` and `target.roles`) is dropped on a full (unscoped) reset;
with `supabase`, neither is.

## PostgreSQL

The reference target: every feature works here.

- **One transaction per apply.** `dbd apply` (and the apply phase of
  `dbd deploy`) runs every entity, migration and version stamp in a single
  transaction, so an interrupted upgrade rolls back to the schema it started
  from instead of leaving objects half-applied. `apply.before` runs before it
  and outside it, so its effects stay if the batch rolls back; matview
  stamping, pg_cron sync and `apply.after` run after it commits.
  DDL that cannot run inside a transaction — `CREATE INDEX CONCURRENTLY`, for
  one — fails there; set `DBD_NO_TX` (to any value) to apply without the
  transaction.
- **Bookkeeping** lives in a `dbd` schema: `dbd.meta` (project, environment,
  version, pinned scope), `dbd.migrations`, and `dbd.matview_stamps` (each
  materialized view's drift stamp — kept out of the view's `COMMENT`, which is
  the author's). The schema is not in PostgREST's default `db-schemas`, is never
  reverse-engineered, and is never dropped.
- **Legacy bookkeeping heals itself.** Databases written by older dbd versions
  kept `_dbd_meta`/`_dbd_migrations` in `public` (or, after a scoped apply, in
  other schemas). The next `apply`, `deploy`, `reconcile` or `reset` folds them
  into `dbd.meta`/`dbd.migrations` in one transaction and drops the originals.
  Read-only commands work on an unhealed database.
- **Import** streams CSV and TSV through `COPY`. JSON and JSON Lines records are
  staged as bound `jsonb` values and moved into the table by a procedure the
  load installs as `staging.import_jsonb_to_table` (creating the `staging`
  schema if needed). The truncate before a load is `TRUNCATE`.

## SQLite

```yaml
source:
  dialect: sqlite
target:
  sqlite:
    url: sqlite://./app.db
```

- **The file is the model.** A SQLite project is read by the `verbatim` reader:
  each file is applied exactly as written. `dbd init --from-db sqlite://…`
  writes `dialect: sqlite` for you.
- **No schemas.** Schema entities are a no-op, and `auth.users` resolves to the
  bare table `users`.
- **Rejected at apply:** extensions, roles, sequences, enums, functions,
  procedures and materialized views —
  `SQLite adapter does not support Enum entities (status)`.
- **No transaction around the apply**, unlike PostgreSQL.
- **Grants are skipped** with `Skipping schema grants (target has no grant
  model).` **Policies** are PostgreSQL SQL, so each policy file fails.
- **Import** writes multi-row `INSERT … VALUES (?,?),(?,?), …` batches of at most
  500 rows (or 32,000 bound values) in one transaction, matching CSV columns to
  the header's names and JSON records to their keys. SQLite has no `TRUNCATE`,
  so the default truncate runs `DELETE FROM` instead; with `truncate: false`,
  rows are appended.
- **Not available:** `diff`, `reconcile` and `emit` refuse a SQLite project,
  because the `verbatim` reader builds no structured model to compare or
  translate; `refresh` has no materialized views to refresh.
- Bookkeeping is `_dbd_meta`/`_dbd_migrations` — with no schemas, the prefix is
  the namespace.

## Convex

```yaml
target:
  convex:
    url: "convex:"                            # writes ./convex/schema.ts (quoted: a bare trailing colon is not valid YAML)
    # url: convex://./generated               # another output directory
    # url: convex://./generated?deploy=true   # also run `npx convex deploy`
```

Convex is a code generator. `dbd apply` writes one `defineSchema({ … })` file
from the design's tables and enums:

- SQL types map to `v.*` validators — `int*`/`numeric` → `v.number()`, text-like
  and `uuid` → `v.string()`, `boolean` → `v.boolean()`, timestamps →
  `v.number()`, `json`/`jsonb` → `v.any()`, `bytea` → `v.bytes()`, `T[]` →
  `v.array(T)`, a nullable column → `v.optional(T)`.
- An enum becomes `export const <name> = v.union(v.literal("…"), …)`, and
  columns of that type reference it. A single-column foreign key becomes
  `v.id("<table>")`.
- Names flatten from `schema.table` to `schema_table` — Convex forbids `.`.
- **Only tables and enums are written.** Every other entity type is skipped
  without a message.
- State lives in `<output>/.dbd_state.json`, not a table.
- `dbd import` runs `npx convex import --table <name> --replace -y <file>` per
  data file. `--replace` empties the table first, so the truncate step sends
  nothing and `truncate: false` has no effect — a Convex import always
  replaces. A `.json` file must be a JSON array: Convex's importer reads no JSON
  Lines from one. An import *procedure* fails, since Convex runs no SQL
  (`Convex adapter does not execute SQL scripts`). `dbd export` is refused: the
  Convex CLI exports whole deployments only (`npx convex export`).
- Convex runs no SQL, so `reset`, `policies`, grants, `diff` and `reconcile` are
  not available.

## What works where

| | PostgreSQL / Supabase | SQLite | Convex | MySQL / SQL Server |
|---|---|---|---|---|
| `apply`, `deploy` | yes, one transaction | yes | writes `schema.ts` | no adapter |
| `import` | `COPY`, JSON via a procedure | batched `INSERT`; truncate is `DELETE FROM` | `npx convex import --replace` | no adapter |
| `export` | yes | yes | refused | no adapter |
| `diff`, `reconcile` | yes | no (no structured model) | no | no |
| grants, `policies` | yes | grants skipped; policies fail | no | no |
| `refresh`, materialized views | yes | no | no | no |
| `dbd emit` can write its DDL | — | `--dialect sqlite` | — | `--dialect mysql`, `--dialect tsql` |
| Bookkeeping | `dbd.meta`, `dbd.migrations`, `dbd.matview_stamps` | `_dbd_meta`, `_dbd_migrations` | `.dbd_state.json` | — |
