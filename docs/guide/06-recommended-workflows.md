# Recommended workflows

dbd supports two ways of starting a schema. **Greenfield** — author the DDL from scratch.
**Brownfield** — generate a project from an existing database or a DBML diagram. Either way you
converge on the same `design.yaml` + `ddl/` tree, snapshots, and `dbd apply`, so day-to-day work
is identical regardless of where the project came from.

Step-by-step flows for each command are in the command reference:
[`dbd init`](04-commands.md#dbd-init) and [`dbd merge`](04-commands.md#dbd-merge).

## Recommended workflows

### Greenfield — a brand-new schema

No database yet. `dbd init` scaffolds `design.yaml`, the `ddl/` tree, and a sample table.

**Before v1 — iterate fast with `reconcile`.** While the schema is still churning, cutting a
snapshot for every column tweak is tedious. `dbd reconcile` diffs the live database against the
design and applies the difference in place — `CREATE` for new tables, `ALTER` for changed ones — with
no snapshot files and no version bump:

```sh
dbd reconcile --dry-run -d $DATABASE_URL   # preview the plan
dbd reconcile -d $DATABASE_URL             # apply it
dbd reconcile --allow-destructive -d $DATABASE_URL   # also drop columns/constraints
dbd reconcile --prune -d $DATABASE_URL     # also drop orphaned tables (removed from the design)
```

Reconcile only diffs within the schemas the design declares, so it never touches tables in other
schemas. Two kinds of destruction each need their own opt-in:

- **`--allow-destructive`** — drop a *column* or constraint from a managed table, or remove a
  *value* from an enum (the type is recreated; the managed views over a table with a column of
  the enum are dropped and re-applied, and nothing else is dropped). dbd never drops a
  materialized view, so a live one reading such a column blocks the removal: reconcile refuses
  before it writes anything, `--dry-run` included, naming the matview and the way through —
  `DROP MATERIALIZED VIEW` it by hand, then re-run, which re-creates the matviews the design
  declares. Without the flag, the refusal lists each drop by the entity it belongs to.
- **`--prune`** — drop a whole *table* that is still in a managed schema but no longer in the
  design (an orphan). Without `--prune`, orphans are reported and left in place. Under
  `--scope`, a table the design declares outside the scope is not an orphan — a scope narrows
  what is applied, not what the project owns. **Upgrade to 0.24.1 before a scoped prune:**
  earlier releases dropped those tables ([#40](https://github.com/sensei-hq/dbd/issues/40)).
  `public` is managed only when the design puts something there — a role or an extension with
  no `schema:` does not; **before 0.25 either did**, and prune dropped the undeclared tables in
  `public`.

Orphaned *enums* are only warned about, never auto-dropped (columns may still reference them).

Reconcile runs the design's `apply.before` and `apply.after` hooks and its `grants:` too, scoped
as `apply` scopes them, so a dev loop on reconcile builds the database `apply` would;
`reconcile --dry-run` lists them after the schema changes.

Reconcile is not one transaction: it writes in the dependency order `apply` uses, each step
committing as it runs, so a step that fails stops the run with the earlier steps applied. Fix the
cause and run it again — what is already in place is no longer drift.

The exception is a key a foreign key holds. PostgreSQL will not drop a primary key, a unique key
or index, or a column of one, while a foreign key references it, so reconcile drops those
foreign keys first, in the step of the table whose key changes. One the design keeps comes back
in that same step, once the new key is there; one the design replaces comes back in its own
table's step, and the steps from the key change to that one run in one transaction — a failure in
any of them leaves the key as it was. A step in that transaction waits at most 10 seconds for a
lock another session holds; then the run fails and rolls it back. A foreign key on a table the run
does not reconcile — another project's or module's, one the scope leaves out, one no design
declares, one the role has no privilege on — is never dropped: the key change is refused before
anything is written, `--dry-run` included, naming it. Before 0.27.1 the key's drop failed with
`cannot drop constraint … because other objects depend on it`.

A function or procedure is re-applied with its file's `CREATE OR REPLACE`, which PostgreSQL
refuses when the routine changes what it returns, renames a parameter, loses a parameter's default
or turns from a function into a procedure. `reconcile` and `apply` then drop it and create it
again, in one transaction — when it is the only routine of its name and nothing depends on it.
Otherwise the run fails naming the routines of that name, or what depends on the routine (a view,
a trigger, a column default): drop that, and the run creates again what the design declares. A
grant or comment the file does not make goes with the dropped routine.

**Cut v1 with `dbd release`.** When the schema stabilizes, `dbd release` (alias `dbd baseline`)
writes a baseline snapshot at the current version and flips `project.released: true`:

```sh
dbd release --name "v1 GA"
```

After release, `reconcile` is disabled and the project is on the migration track: edit the DDL,
`dbd snapshot` each change, and `dbd apply` (see
[snapshots & migrations](05-snapshots-migrations.md)). This mirrors the guarantee you want in
production — every change is a versioned, reviewable migration.

### Adopt dbd for an existing database

You already run a database and want to manage it as code. Baseline the project straight from the
live database, review it, commit, and build from there:

```sh
dbd init --from-db postgres://user:pass@host/db   # or sqlite://./app.db
dbd inspect        # validate
dbd format         # normalize to your conventions (introspected column order is physical)
# commit the generated design.yaml + ddl/ — this is your v1 baseline
```

`init --from-db` writes a baseline snapshot, so the project is version-tracked from day one. It
**refuses** if the database already records this project (in `dbd.meta`, or a legacy `_dbd_meta`) —
use `dbd merge` for that. Another project's record is no bar: a second project onboards beside the
first, leaving out what that one owns (see [sharing a database](14-sharing-a-database.md)). From here it's an ordinary project: edit DDL → snapshot → apply.

The reversed project applies cleanly back to the database it came from — the DDL is re-runnable
(`IF NOT EXISTS`, `OR REPLACE`), so the first `dbd apply` records the version and changes nothing,
and `dbd diff` afterwards is empty — and to a fresh one. Its extensions are in the target's
`extensions:`, its schemas under `schemas:` (only those something was captured in, or that
`--schema` named), and a table a foreign key reaches outside what was captured under `external:`;
every schema left out is named as it is left out. The DDL is fully qualified and states no
`SET search_path`, so `dbd inspect`, `apply` and `deploy` note that on every file; nothing in the
files depends on the path, so the note is harmless.

Much of what dbd's model cannot hold is named as it is captured, rather than written weaker in
silence: a partitioned table becomes one plain table holding every row, its partitions folded into
it; an `UNLOGGED` table, inheritance, a primary key's or UNIQUE's deferral, and a constraint's
`NOT VALID`, `NOT ENFORCED` or `NO INHERIT` are each named for the object that has them. What an
extension owns — its tables, views, functions, types — is left out entirely, as it is by `diff`
and `reconcile`. Some is still dropped without a word: a domain or composite type is neither
captured nor named, so a project with a column typed by one fails on a fresh database; an identity
column's `START`/`INCREMENT`, storage parameters (`fillfactor`), a column's `STORAGE`, row-level
security and its policies, and comments on constraints, indexes and functions are lost; and a
default holding a backslash reads as drift in `diff` against the database it came from. Check for
those before you rely on the empty diff.

A capture narrowed by `--schema` declares the extensions its objects use, when they are installed
in `public` or a captured schema, and prints a `needs:` line for each type, function or table the
captured objects use from a schema it left out, naming who uses it. A new database needs those
first, so the project applies with an empty `diff` to a database that has what the `needs:`
lines name.

Adopting a Supabase database? `init --from-db` recognises it by Supabase's `auth` and `storage`
schemas, names the target `supabase`, so Supabase's own schemas are protected (see
[targets and adapters](08-targets.md#what-the-supabase-key-changes)), and leaves its platform
schemas out of the project. Only a Supabase database loses them: on any other PostgreSQL, an `auth`
or `extensions` schema is the project's own and is captured.

#### Snapshots, but not released

`init --from-db` and `dbd merge` write snapshots, but only `dbd release` sets
`project.released: true`. A project built this way is in between:

- it **has snapshots**, so it is on the migration track — `dbd snapshot` then `dbd apply` is
  the way to change it, as for a released project;
- it is **not released**, so `dbd reconcile` still runs — reconcile checks the flag, nothing
  else.

Treat it as released, and make it so with `dbd release`: on a project with snapshots it writes
no second baseline — it releases at the version the latest snapshot reached and sets the flag,
after which `reconcile` refuses. It refuses while the design has changes no snapshot records
(run `dbd snapshot` first), and when `project.version` disagrees with the latest snapshot.

### Add a new database to an existing project

You have a dbd project and are bringing in another database — say a new service's schema. `dbd
merge` reverse-engineers its objects and snapshots the diff into your project. If that content is a
**separate module** that deploys to its own database, give it a
[scope](03-design-yaml.md#scopes) so it can be applied independently:

```sh
dbd merge postgres://host/newdb          # pull the new tables in (+ snapshot the diff)
# then, in design.yaml, scope the new module so it deploys on its own:
#   scopes:
#     billing: { includes: [billing], deps: include }
dbd apply --scope billing -d $BILLING_URL   # deploy just that module to its own database
```

`merge` edits nothing in `design.yaml` but `project.version`, and never deletes files (orphans are
reported, not removed; an extension, an external table or a schema the config should declare is
warned about, for you to add — only a schema it wrote files into). A table the design declares
`external:` is never captured, only named, and a table the project builds is never called
external. It **refuses** if the source database is behind your project version (see Version
safety below).

A project that keeps growing this way — several parts, each deploying to its own databases — is
better as a **workspace** of modules, each with its own folder, version and history; see the next
workflow.

### Divide a scoped project into a workspace

A project whose scopes already say which parts deploy where — `default` to the main database,
`dojo` to another — becomes a **workspace** with `dbd split --from-scopes`. Each region of the
scopes' Venn diagram, taken over each scope's entities with their dependencies, becomes a module:
the region every scope holds is `shared`, a region one scope alone holds is named for it (`default`
as `core`), one no scope holds is `unscoped`. Each module depends on the modules its entities
reference, and each scope is rewritten as the modules it holds.

```sh
git status                                 # start clean: the split moves files with git mv
dbd split --from-scopes                    # the plan — modules, their scopes, entity counts, depends_on, the new scopes
dbd split --from-scopes -v                 # … with each module's entities and each file's move
dbd split --from-scopes --rename shared=common   # name a module of the plan otherwise
dbd split --from-scopes --write            # move the files, write modules: and scopes: into design.yaml
dbd inspect                                # the workspace loads, module by module
git diff --stat --cached                   # the renames, for review and commit
```

`--write` is whole or not at all. It first builds the workspace in a scratch folder and refuses
unless it loads and deploys, under each scope and with none, exactly what the project deploys now.
A move that fails puts back every move before it; a split interrupted part-way leaves a journal
(`.dbd-split`), every command that loads the project refuses it until `dbd split --undo` puts it
back, and the split can then run again. It refuses, before moving anything, a project with `snapshots/` or
`migrations/` — a migration is SQL over several tables and does not divide by module, so **split
before the first release** — and a project it cannot divide as it stands, naming each reason: a
file that does not parse, a scope that leaves out what its entities need, a file that maps to no
module or to two.

Nothing changes on the databases until you deploy. Each module's first run on a database takes over
what it declares from the project as it was — the transfer rule — and says so; once the modules hold
everything the old project held there, its record is retired:

```sh
dbd deploy --dry-run                       # [dry-run] would adopt relation app.lookups from "sensei" …
dbd deploy                                 # each module takes its entities over; project "sensei" is retired
dbd deploy --scope dojo -d $DOJO_URL       # the same on each database a scope deploys to
```

The same entities deploy to each database as before the split, with their rows. From then on each
module changes on its own: move a file between modules and the module it moves to takes it over
(nothing is dropped); `--module core` runs one module with what it depends on; each module is
released on its own (`dbd release --module core`). See [design.yaml](03-design-yaml.md) for the
`modules:` block and [sharing a database](14-sharing-a-database.md) for how modules own what they
declare.

### Start from a dbdiagram.io / dbdocs design

Designed the schema visually first? Reverse-engineer the DBML export — no connection needed:

```sh
dbd init --from-dbml schema.dbml         # schemas, enums, tables (CHECKs, indexes) and foreign keys
dbd init --from-dbml schema.dbml --target supabase   # the same, for a Supabase database
```

### Keep a project in sync with a drifting database

Someone changed the database out-of-band. Re-`merge` to pull the drift back as a new snapshot
version, then apply it everywhere:

```sh
dbd merge -d $DATABASE_URL --dry-run     # preview created/unchanged/conflict + the snapshot version
dbd merge -d $DATABASE_URL               # overwrite DDL + auto-snapshot the diff (guarded)
```

## What each source captures

| Source | Captured |
|--------|----------|
| **Postgres / Supabase** (`--from-db postgres://…`) | schemas (under `schemas:`), extensions (in the target's `extensions:`), enums with their comments, tables (columns with their collations, defaults, identity, PK/FK/unique/check/EXCLUDE with their names, a foreign key's `DEFERRABLE`, `INITIALLY DEFERRED` and `MATCH FULL`, indexes incl. `gin/gist/brin/hash`, partial and expression indexes, comments, `serial`/identity columns, and triggers, in the table's file), views and materialized views with their comments (definition + indexes; a view's `INSTEAD OF` triggers), functions & procedures (verbatim), standalone sequences, and roles (opt-in `--roles`); a table a foreign key reaches outside the project is declared `external:`. **Not** captured, and named: partitions (folded into their table), `UNLOGGED`, inheritance, PK/UNIQUE deferral, `NOT VALID`/`NOT ENFORCED`/`NO INHERIT`. **Not** captured, and **not** named: domains, composite types, identity `START`/`INCREMENT`, storage parameters, column `STORAGE`, row-level security and policies, and comments on constraints, indexes and functions. Left out: whatever an extension owns. A database with Supabase's `auth` and `storage` schemas gets a `supabase` target, and only there are its platform schemas left out. |
| **SQLite** (`--from-db sqlite://…`) | tables, user indexes, and views — captured **verbatim** from `sqlite_master` (CHECK constraints, type affinity, `AUTOINCREMENT`, `WITHOUT ROWID` all survive). A table's and a view's triggers are written into its file. No schemas/enums/functions/sequences/roles. Files land flat (`ddl/table/<name>.ddl`); `design.yaml` carries `schemas: []`. |
| **DBML** (`--from-dbml <file>`) | schemas, enums, tables and foreign keys — DBML can't express functions, procedures, views, standalone sequences or roles. From dbd's own DBML, CHECKs, identity (`ALWAYS` kept apart from `BY DEFAULT`), `serial`, generated columns, column `UNIQUE`, partial and expression indexes, key order, an index's method, `INCLUDE` columns, operator classes and storage parameters, and primary-key and foreign-key names other than PostgreSQL's defaults survive; a table-level UNIQUE comes back as its named constraint, and a stub table as an `external:` entry; enum comments, EXCLUDE constraints, collations and a foreign key's deferral and `MATCH FULL` are lost. A hand-written document is read as @dbml/core reads it — aliases, inline `ref:`, multi-line notes, a `pk` index, one-line tables; a one-to-one `-` Ref keys its right-hand side, and a many-to-many `<>` Ref, a `TablePartial` or anything after a column on its line is refused, saying why. Every name is read back as itself, whatever it holds. |

The generated `design.yaml` target comes from the source: the connection URL scheme
(`postgres://` → `postgres`, `sqlite://`/`file:` → `sqlite`), `supabase` for a PostgreSQL database
with Supabase's `auth` and `storage` schemas, and `--target` for a DBML file. Secrets never land in the repo — the target `url` is
written as the literal `$DATABASE_URL` reference, not the connection string you passed.

## Version safety

`dbd merge` against a **dbd-managed** database compares the database's applied version **D** against
the project's `project.version` **Y**:

- **D < Y → refuse.** The project is ahead of a stale database; overwriting project DDL from it
  would discard newer work. Bring the database up to date with `dbd apply`, or revert the project
  to version `D` via version control if you really mean to discard it. There is no override flag.
- **D ≥ Y, or any source with no `dbd.meta`/`_dbd_meta` (a foreign DB, a DBML file, an unmanaged
  SQLite DB) → proceed.** dbd overwrites the introspected DDL and auto-snapshots the diff.
