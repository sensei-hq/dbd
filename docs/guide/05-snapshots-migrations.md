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
  edits it in place, keeping the file's comments and layout; in a workspace,
  each module's (see [below](#per-module-history))
- The bookkeeping table tracks the applied version in the database — `dbd.meta` in a dedicated
  `dbd` schema on Postgres/Supabase, `_dbd_meta` on SQLite
- `dbd apply` compares these and runs pending migrations

## Apply scenarios

| DB state | Action |
|----------|--------|
| Fresh (no bookkeeping table) | Apply all DDL directly, mark latest version — `Fresh install at v2` |
| Built but unversioned (no version recorded, but dbd applied it before, or the design's objects are already there) | Apply all DDL over what exists, mark latest version — `v2 recorded on a database that was already built but had no version` |
| Behind (version < latest) | Run the pending migrations version by version, then re-apply the DDL |
| Current (version == latest) | Idempotent apply (CREATE IF NOT EXISTS) |
| Ahead (version > the project's, or the project has none) | Refused before anything runs — `version guard: the database is at v3, but this project is at v2` |

A database ahead of the project was built from a newer revision — typically a
branch checked out from before the latest snapshot. Applying the older design
would re-apply its DDL over the newer schema, which nothing short of a newer
revision makes right, so `apply`, `deploy` and their dry runs refuse it and
there is no flag to pass it. Before 0.25 the run said `Already up to date (v3)`
and re-applied the older DDL. (`reconcile`, a pre-release command, is not
covered: it records the project's version even over a newer one.)

No recorded version does not mean an empty database: one built before the
project had a version, or by hand, records none. `apply` tells the two apart and
says which it found, rather than calling the second a fresh install.

On PostgreSQL every scenario runs in one transaction (unless `DBD_NO_TX` is
set), so a migration that fails part-way leaves the database at the version it
started from — with one exception, a version that adds enum values (see
[how a run is ordered](#how-a-migration-run-is-ordered)).

To see what a run will do first, `dbd apply --dry-run` with a database
configured lists the pending migrations it would execute before the entities —
from the version the database records, under the same scope, in the order the
run takes them — with each statement that destroys something marked `⚠`, and a
drop of a table the database does not have marked as skipped. It refuses where
the run would: a database ahead of the project, a pending migration with an
unresolved `data.sql` TODO, or a named apply over pending migrations. Without a
database it cannot know the recorded version, so it says the migrations are not
listed.

**A named apply is refused while migrations are pending.** `dbd apply -n
app.people` builds one entity; it cannot run a version's changes to the other
tables, and recording the version without them would leave the database short
of the design while saying it is current. So it stops before any write, naming
the pending versions: run `dbd apply` first, then `dbd apply -n`. With nothing
pending, a named apply works as ever.

Snapshots record tables and enums, so a released project never drops a view,
function or sequence removed from the design: drop it by hand. A migration under `--scope`
runs only the steps for the scope's entities while the database's version still
advances, and its dry run says so when nothing in the pending versions runs in
the scope; the drop of a table the design removed runs under a scope that
occupies its schema, and only where the table still exists — see
[scopes](07-scopes.md#migrations-under-a-scope).

## How a migration run is ordered

A run from v1 to v4 executes each pending version whole before the next starts,
as upgrading one release at a time would:

1. the enum values the version adds (below);
2. its creates and schema changes (`<table>.sql`), in dependency order;
3. the data scripts (`<table>.data.sql`) of the tables it alters or adds, in
   dependency order — after every schema change of the version, since one
   table's script may read a column another table gains in the same version;
4. its drops;
5. the record that it ran.

After the last version, every entity's DDL is re-applied on the migrated tables,
and the latest version is recorded. Before 0.25 the run went entity by entity —
every pending version of one table, then the next table — so a jump across
versions could run a later version's change before an earlier one it depended
on, and fail where a stepwise upgrade succeeded.

**Enum values commit first.** PostgreSQL will not use an enum value inside the
transaction that added it (`unsafe use of new value`), and the rest of a
version may — a default, a CHECK, a data script. So each version's
`ALTER TYPE … ADD VALUE` statements, found in the parse tree, run at its head as
`ADD VALUE IF NOT EXISTS`, and the run commits there: everything up to and
including those additions, earlier versions recorded, is committed before the
rest of the version runs. If the rest then fails, the values stay and the
database records the version before it; fix the migration and re-run, and the
additions run again as no-ops. A run whose pending versions add no enum value is
one transaction from start to end.

**Scripts run on the path their entity's CREATE runs on.** A migration file that
states no `SET search_path` runs on the path its table's CREATE runs on — the
last `SET search_path` the DDL file states before the CREATE, else
`source.search_path` — so a `data.sql` can name its table bare, as the DDL does;
the drop of a table the design no longer has runs on `source.search_path`. With
neither, it runs on the session default (`RESET search_path`).

One case a jump across versions still gets wrong: a table an earlier pending
version adds is created from its *current* DDL, so a later pending version's
generated `ALTER TABLE … ADD COLUMN` for it fails with `already exists`. Upgrade
one release at a time there, or edit that migration to `ADD COLUMN IF NOT
EXISTS`.

## A key a foreign key holds

PostgreSQL will not drop a primary key, a unique key, or a column of one, while a foreign key
references it, and a version's migrations run table by table in dependency order — the
referenced table's first. So the migration of a table whose key changes drops the foreign keys
holding that key first, and adds back, after its new key, the ones the design keeps; one the
design replaces, its own table's migration adds. A version runs in one transaction (above), so
nothing is left half-changed. Before 0.27.1 the key's drop failed while the foreign key held it.

A key the design leaves unnamed has the name PostgreSQL gave it, which no snapshot records. When
the migrations will run on PostgreSQL — the database `-d`/`DATABASE_URL` or the target's `url:`
names is one, or, with neither to go by, the target is called `postgres` or `supabase` — the
migration drops it by the name the catalog has for it: a `DO` block looking up the table's
primary key, or the unique key or foreign key over exactly those columns. Otherwise it is left,
as it was everywhere before 0.27.1, as a comment saying to drop it by hand. An unnamed CHECK or EXCLUDE is always left to you: only its expression
identifies it. A primary key declared on its column (`id uuid primary key`) is not migrated when
it changes — the migration says so; declare a key that will change as `primary key (…)`.

A function whose return type changes is dropped and created again when `apply` re-applies it, as
`reconcile` does — see [recommended workflows](06-recommended-workflows.md).

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

A change Postgres can make in one step is one version; one it cannot is split
into stages:

### Column rename (1 snapshot)
v(N): `ALTER TABLE … RENAME COLUMN old TO new;`, ahead of the table's other changes

### Column type change (1 snapshot, or 2 when dbd cannot cast it)
v(N): `ALTER TABLE … ALTER COLUMN c TYPE t USING c::t;` — in place, as `reconcile` does

A rename and a type change are made in place, so Postgres keeps everything on
the column: its values, NOT NULL, default, identity (the sequence going on past
the values it handed out), generated expression, keys, the foreign keys on
either side of it, CHECKs, EXCLUDE constraints, indexes and comment — and
refuses a value the new type cannot hold, failing the version, rather than
losing it. Narrowing into a bounded character type (`text` → `varchar(3)`) is
an assignment, without the `USING` cast that would cut a value to fit.

A rename is read from the diff: a table's one column dropped and one added, of
one type. The snapshot says when it took a change for one; if the added column
is a new column, cut the drop and the add in separate versions.

A type change dbd cannot cast (`jsonb` → `integer`) takes two versions, both on
the column itself:

v(N): data.sql — a TODO: make every value of the column castable to the new type, in place
v(N+1): `ALTER TABLE … ALTER COLUMN c TYPE t USING c::t;` — edit the `USING` for a conversion a cast cannot make

Before 0.27.2 a rename and a type change were a column swap over two versions:
`<column>_new` added and filled, then the column dropped and `<column>_new`
renamed into its place. A type change's `data.sql` filled the old column,
which the second version then dropped — every value lost, NULL in its place —
and the column came back with its type alone. A database migrated through one
has lost those values; `dbd diff` lists what else it is missing. A pending
version cut that way is refused by `apply` and `deploy`, naming its `data.sql`
and the column: delete the unapplied versions and cut them again.

### Enum value removal (2 snapshots)
v(N): data.sql (TODO: map removed values to remaining)
v(N+1): the type swap — the old type renamed aside and the enum recreated with
the declared values, its `COMMENT ON TYPE` carried over; for each column of it,
the views and materialized views over it, the indexes that read it and its
default dropped, the column moved across with `USING col::text::type`, and all
of them restored; the old type dropped once no column uses it

Before 0.25 the change took three stages through a `TEXT` column, and failed on
any view over the column and on an enum default.

Three cases the generated migration does not handle, so check them by hand: a
column that is an *array* of the enum is not seen; when the same snapshot also
drops a table using the enum, the renamed old type is left behind; and a view
over the column in a scope the database does not build is dropped and not
recreated.

Each version's `migrations/NNN/graph.json` records its place in the run as `"stage": {"index": k, "of": n}` — `1/1` for an ordinary version — so tools can group the stages back into the one change they make up; the viewer's changelog does exactly that. Graphs written before the marker existed have no `stage`, and are read by their snapshot's `(stage k/n)` description instead.

## Data corrections (*.data.sql)

Migration folders can contain data correction scripts:

```
migrations/002/
  graph.json
  config/users.sql           # ALTER TABLE (schema change)
  config/users.data.sql      # UPDATE (data correction)
```

Within a version, every `.sql` runs before any `.data.sql`, and the data
scripts run before the version's drops — see
[how a run is ordered](#how-a-migration-run-is-ordered). A version runs the
`data.sql` of a table it alters or adds — one seeding a table the version
creates runs after the create — and the previews list it; a `data.sql` for a
table the version neither alters nor adds does not run.

A type change dbd can cast needs no data.sql — it is made in place. Non-derivable corrections (a type change dbd cannot cast, an enum value mapping) generate TODO comments that the developer must fill in before committing. These are enforced: `dbd inspect` surfaces any unresolved `-- TODO:` in migration files, and `dbd apply` and `dbd deploy` refuse to run while pending TODOs remain — their dry runs too, with a database configured to say which migrations are pending — so an unfinished migration can't reach the database.

## Listing snapshots

```sh
dbd snapshot --list
```

## Per-module history

In a workspace — a `design.yaml` with [`modules:`](03-design-yaml.md#modules) —
each module has a history of its own: its own `snapshots/` and `migrations/` in
its folder, its own version, and its own release. A module releases when it is
ready, and the others keep changing by `dbd reconcile` until they are.

```yaml
modules:
  shared:
    version: 3
    released: true       # changes by snapshot and apply
  core:
    depends_on: [shared]
    version: 1           # not released: still reconciled
```

- **`dbd snapshot`** snapshots every module, each in its folder at its own
  version — `shared/snapshots/004.json` while core's is `core/snapshots/002.json`
  — or with `--module core`, that module alone. It never snapshots a scope: a
  snapshot records the design. A module with no changes says so and is skipped.
- **`dbd release`** releases every module not released yet, each at its own
  version — a module already released is said and passed over, and only a
  workspace with every module released is refused — or with `--module core`,
  that module alone, refused if it is released. Every module is checked before
  any is written, so a release refused for one leaves `design.yaml` as it was.
  A module's dependencies are not released with it.
- **The version and release state are written in the module's block** —
  `modules.<name>.version` and `modules.<name>.released`, never `project.*`,
  which a workspace refuses. As for a project, the edit is in place: the file's
  comments and layout stay, a module written `core: {}` becomes a block, and an
  edit that cannot be made in place rewrites the file whole rather than change
  anything else.
- **`dbd apply`** migrates each module on its own version: core can be at v2
  while shared stays at v1. Each module's record in the database is its own
  (`dbd.meta` under its owner, `shop/core`), so `migrate --status` and the
  version guard answer for each module.
- **`dbd reconcile`** refuses a released module asked for — with `--module`, or
  as the project — and leaves out, saying so, a released module the run would
  otherwise take in, a dependency included:

  ```
  module shared is released — it changes by `dbd snapshot` and `dbd apply`; reconcile leaves it out.
  ```

  The manifest's `apply:` hooks still run around the modules it reconciles.

`snapshot --list`, `format` and `doctor` go over each module's folder too — or
`--module`'s, or the folder dbd runs in.

A project splits into modules before its first release: `dbd split
--from-scopes` refuses a project with `snapshots/` or `migrations/`, since a
migration is SQL over several tables and does not divide by module (see
[scopes](07-scopes.md#from-scopes-to-modules-dbd-split)).

## Migration status

```sh
dbd migrate --status
```

Shows current DB version, latest version, and pending migration list. A
database with no recorded version shows `Database version: none recorded`, and
`Database is up to date.` appears only when the two versions match — with none
pending but the versions apart, it says why instead (the next apply will record
the version, no migration bridges the gap, or the database is ahead of the
latest snapshot). A database pinned to a scope is noted: it was built with that
scope's entities, not the whole design.
