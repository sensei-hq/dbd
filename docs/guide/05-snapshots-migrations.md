# Snapshots and migrations

## How schema evolution works

DDL files are always the final desired state. Snapshots capture the diff between versions. Migrations are the ALTER/DROP scripts that bridge environments from one version to the next.

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

- `design.yaml` version field tracks the latest snapshot version
- The bookkeeping table tracks the applied version in the database — `dbd.meta` in a dedicated
  `dbd` schema on Postgres/Supabase, `_dbd_meta` on SQLite
- `dbd apply` compares these and runs pending migrations

## Apply scenarios

| DB state | Action |
|----------|--------|
| Fresh (no bookkeeping table) | Apply all DDL directly, mark latest version |
| Behind (version < latest) | Run pending migrations, then apply DDL |
| Current (version == latest) | Idempotent apply (CREATE IF NOT EXISTS) |

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

Shows current DB version, latest version, and pending migration list.
