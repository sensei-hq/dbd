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
  table/public/example.ddl
  view/.gitkeep
  materialized_view/.gitkeep
  function/.gitkeep
  procedure/.gitkeep
  enum/.gitkeep
import/.gitkeep
```

`dbd init --target supabase` writes the same tree, with a `supabase` target, a
`config` schema, the Supabase tables you will reference (`auth.users`,
`storage.objects`, …) declared as `external:`, and `ignore:` patterns for the
platform schemas. It writes no grants — add them yourself (see
[access control](09-access-control.md)).

Already have a database? `dbd init --from-db <url>` generates the project from
it instead — see [recommended workflows](06-recommended-workflows.md#adopt-dbd-for-an-existing-database).

## DDL folder layout

dbd auto-discovers entities from the `ddl/` tree — you never list tables, views, functions, etc. in `design.yaml`. The path encodes the entity's **type** and **schema**:

```
ddl/<type>/<schema>/<name>.ddl     # schema-qualified types
ddl/role/<name>.ddl                # roles have no schema
```

`<type>` is one of `table`, `view`, `materialized_view`, `function`, `procedure`, `enum`, `sequence`, or `role`. The file name (minus extension) is the entity name and the parent folder is its schema:

```
ddl/
  table/config/lookups.ddl         →  table     config.lookups
  view/config/active_users.ddl     →  view      config.active_users
  materialized_view/app/daily.ddl  →  materialized view  app.daily
  function/billing/charge.ddl      →  function  billing.charge
  enum/config/status.ddl           →  enum      config.status
  sequence/billing/invoice_no.ddl  →  sequence  billing.invoice_no
  role/admin.ddl                   →  role      admin
```

Files use the `.ddl` or `.sql` extension. Schemas are auto-added from these paths, so a `ddl/table/config/…` file implies the `config` schema even if it isn't listed under `schemas:`.

There is no `ddl/extension/` or `ddl/schema/` folder: extensions are declared in the target's `extensions:` list and schemas under `schemas:` in `design.yaml`, because dbd generates both statements itself. `dbd inspect` reports a file in either folder as an error, and `dbd doctor` names the `design.yaml` entry to write instead.

**Use the singular type folder** (`table`, `function`, …) — it's canonical. The plural form (`functions/`, `tables/`, …) is still accepted for backward compatibility (and `matview/` for `materialized_view/`), but `dbd doctor` flags it and `dbd doctor --fix` migrates plural folders to singular. If a singular folder already exists, the contents are merged in; on a same-name file collision the **newer** file (by modification time) is kept and the older one is saved alongside as `<name>.ddl.bkp` (reported so you can reconcile).

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
  options:
    truncate: true
```

Every key is described in the [design.yaml reference](03-design-yaml.md).

## Set the database URL

```sh
export DATABASE_URL=postgres://user:pass@localhost:5432/mydb
```

## Validate

```sh
dbd inspect
```

Output:

```
Everything looks ok

5 entities — no issues
```

or the errors and warnings, listed per entity. `inspect` exits 1 when there is
an error that would stop `apply`, so it works as a CI gate. It needs no
database.

## Apply schema

```sh
dbd apply
```

Applies every entity in dependency order. Each type has its place —
schemas, extensions, roles, sequences, enums, tables, views, materialized
views, then functions and procedures — and a real dependency moves an entity
later when it needs to: a view that calls a function is applied after the
function. On PostgreSQL the whole run is one transaction.

The database records which environment it was built for, and a run without `-e`
is `-e prod`. Build a development database with `-e dev` from its first run:
one first built without it is marked prod, and from then on a `-e dev` run
against it is refused, so that dev seed data never reaches production (see
[global options](04-commands.md#global-options)).

```sh
dbd apply -e dev
```

Preview the order — offline, or, when a database is configured, checked against
it the way the real run checks it, with its pending migrations listed:

```sh
dbd apply --dry-run
```

## Load data

```sh
dbd import
```

Loads CSV, TSV, JSON and JSONL files from `import/` into staging tables, then calls the procedures that read them. See [import and export](10-data.md).

## Evolve the schema

How you change the schema depends on whether the project has been released.

**Before release — reconcile.** While the design is still churning, edit the
DDL and let `dbd reconcile` alter the database in place. No snapshot, no
migration file:

```sh
# 1. Edit DDL files (add a column, a new table, …)

# 2. Preview, then apply the difference
dbd reconcile --dry-run
dbd reconcile
```

**Release v1.** When the schema settles, cut the first version. This writes a
baseline snapshot, sets `project.released: true`, and turns `reconcile` off:

```sh
dbd release --name "v1"
```

**After release — snapshot and apply.** Every change is now a versioned
migration:

```sh
# 1. Edit DDL files

# 2. Validate
dbd inspect

# 3. Create a snapshot — captures the diff, generates migration SQL
dbd snapshot --name "add notes column"

# 4. Review the migration
cat migrations/002/config/lookup_values.sql

# 5. Apply — pending migrations run alongside the DDL
dbd apply
```

See [recommended workflows](06-recommended-workflows.md) and
[snapshots and migrations](05-snapshots-migrations.md).

## Generate documentation

```sh
dbd dbml
```

Produces `design.dbml` for use with [dbdocs.io](https://dbdocs.io) or [dbdiagram.io](https://dbdiagram.io).

Or open the schema in the hosted viewer — an ER diagram, a page per entity and
a changelog from your snapshots, carried privately in the link:

```sh
dbd diagram
```

See [the schema viewer](11-schema-viewer.md).

## Deploy from GitHub

`dbd deploy` brings a database fully up from source in one step — apply,
grants, import, then RLS policies — and is the one command that can fetch the
source from GitHub:

```sh
dbd deploy --source sensei-hq/daemon/database -d $DATABASE_URL
dbd deploy --source sensei-hq/daemon/database@v2.1 -d $DATABASE_URL
dbd deploy --source ./local/path -d $DATABASE_URL
dbd deploy --dry-run --source owner/repo/path
```

For a private repository, set `GITHUB_TOKEN`; dbd sends it to `api.github.com`
only. Other commands take a local path — clone the repository and pass its
directory.

GitHub sources are downloaded once and cached under `~/.cache/dbd`
(`~/Library/Caches/dbd` on macOS). To force a fresh copy:

```sh
dbd deploy --no-cache --source owner/repo/path -d $DATABASE_URL     # re-download this source
dbd deploy --clear-cache --source owner/repo/path -d $DATABASE_URL  # wipe the whole cache first
```

Both flags are no-ops for local path sources (`--clear-cache` still clears the cache).

## Teach your AI assistant

```sh
dbd install              # into ~/.claude (or $CLAUDE_CONFIG_DIR)
dbd install --project    # into ./.claude, to share with the team by committing it
```

This installs dbd's Claude Code skill and its `dbd-pattern-verifier` agent, so
an assistant working in the project follows the conventions on this page —
which workflow fits the release state, idempotent DDL, secrets kept out of
`design.yaml`. Both are embedded in the binary; re-run after upgrading dbd.
