# Snapshots and migrations

## How schema evolution works

DDL files are always the final desired state. Snapshots capture the diff between versions. Migrations are the ALTER/DROP scripts that bridge environments from one version to the next.

This is the workflow **after release**. Before `dbd release`, a project changes
its database with `dbd reconcile` instead and writes no snapshots — see
[recommended workflows](06-recommended-workflows.md).

## Creating a snapshot

After changing DDL files:

```sh
dbd snapshot --name "add email to users"
```

This:
1. Parses current DDL files into a schema snapshot
2. Compares against the previous snapshot
3. Generates migration SQL (ALTER TABLE, DROP, etc.)
4. Writes snapshots/NNN.json and migrations/NNN/

## Version tracking

- `design.yaml` version field tracks the latest snapshot version — `dbd snapshot`
  edits it in place, keeping the file's comments and layout
- The bookkeeping table tracks the applied version in the database — `dbd.meta` in a dedicated
  `dbd` schema on Postgres/Supabase, `_dbd_meta` on SQLite
- `dbd apply` compares these and runs pending migrations

## Apply scenarios

| DB state | Action |
|----------|--------|
| Fresh (no bookkeeping table) | Apply all DDL directly, mark latest version — `Fresh install at v2` |
| Built but unversioned (no version recorded, but dbd applied it before, or the design's objects are already there) | Apply all DDL over what exists, mark latest version — `v2 recorded on a database that was already built but had no version` |
| Behind (version < latest) | Run pending migrations, then apply DDL |
| Current (version == latest) | Idempotent apply (CREATE IF NOT EXISTS) |

No recorded version does not mean an empty database: one built before the
project had a version, or by hand, records none. `apply` tells the two apart and
says which it found, rather than calling the second a fresh install.

On PostgreSQL every scenario runs in one transaction (unless `DBD_NO_TX` is
set), so a migration that fails part-way leaves the database at the version it
started from.

Snapshots record tables and enums. A migration under `--scope` runs only the
steps for the scope's entities while the database's version still advances; the
drop of a table the design removed runs under a scope that occupies its schema,
and only where the table still exists — see
[scopes](07-scopes.md#migrations-under-a-scope).

## Dropping a table

Delete a table's DDL file and `dbd snapshot` writes its drop where it writes
every per-table migration, `migrations/<NNN>/<schema>/<table>.sql`:

```
migrations/002/
  graph.json
  app/b.sql                  # DROP TABLE app.b CASCADE;
```

The next `dbd apply` runs it — `Migrated v1 → v2 — … 1 dropped.` A hand-written
`<table>.drop.sql` in the same folder takes precedence over the generated file.
A drop runs only where the table still exists, and the summary counts only the
drops that ran. Before 0.25 apply looked only for `<table>.drop.sql`, so no
generated drop ever ran, while the migration was recorded as applied and its
summary counted the drop; a database upgraded through such a migration still has
the table, and needs it dropped by hand.

## Smart multi-snapshot

Complex changes are automatically split into multiple snapshots:

### Column rename (2 snapshots)
v(N): ADD new column + data.sql (UPDATE SET new = old)
v(N+1): DROP old column

### Column type change (2 snapshots)
v(N): ADD new column with new type + data.sql (UPDATE SET new = old::type)
v(N+1): DROP old column

### Enum value removal (3 snapshots)
v(N): data.sql (TODO: map removed values to remaining)
v(N+1): ALTER columns to TEXT + DROP old enum
v(N+2): CREATE new enum + ALTER columns back

Each version's `migrations/NNN/graph.json` records its place in the run as `"stage": {"index": k, "of": n}` — `1/1` for an ordinary version — so tools can group the stages back into the one change they make up; the viewer's changelog does exactly that. Graphs written before the marker existed have no `stage`, and are read by their snapshot's `(stage k/n)` description instead.

## Data corrections (*.data.sql)

Migration folders can contain data correction scripts:

```
migrations/002/
  graph.json
  config/users.sql           # ALTER TABLE (schema change)
  config/users.data.sql      # UPDATE (data correction)
```

The apply engine runs *.sql first, then *.data.sql for the same entity.

Auto-generated data.sql uses Postgres CAST for type conversions. Non-derivable corrections (enum value mapping) generate TODO comments that the developer must fill in before committing. These are enforced: `dbd inspect` surfaces any unresolved `-- TODO:` in migration files, and `dbd apply` refuses to run while pending TODOs remain — so an unfinished migration can't reach the database.

## Listing snapshots

```sh
dbd snapshot --list
```

## Migration status

```sh
dbd migrate --status
```

Shows current DB version, latest version, and pending migration list. A
database with no recorded version shows `Database version: none recorded`, and
`Database is up to date.` appears only when the two versions match — with none
pending but the versions apart, it says why instead (the next apply will record
the version, no migration bridges the gap, or the database is ahead of the
latest snapshot).
