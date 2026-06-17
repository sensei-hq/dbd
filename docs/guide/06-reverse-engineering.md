# Reverse-engineering & workflows

dbd supports two ways of starting a schema. **Greenfield** — author the DDL from scratch.
**Brownfield** — generate a project from an existing database or a DBML diagram. Either way you
converge on the same `design.yaml` + `ddl/` tree, snapshots, and `dbd apply`, so day-to-day work
is identical regardless of where the project came from.

Step-by-step flows for each command are in the command reference:
[`dbd init`](04-commands.md#dbd-init) and [`dbd merge`](04-commands.md#dbd-merge).

## Recommended workflows

### Greenfield — a brand-new schema

No database yet. `dbd init` scaffolds `design.yaml`, the `ddl/` tree, and a sample table; edit the
DDL, `dbd apply`, and `dbd snapshot` each change (see
[snapshots & migrations](05-snapshots-migrations.md)).

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
**refuses** if the database is already dbd-managed (it finds a `_dbd_meta` table) — use `dbd merge`
for that. From here it's an ordinary project: edit DDL → snapshot → apply.

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

`merge` never edits `design.yaml` and never deletes files (orphans are reported, not removed), and
it **refuses** if the source database is behind your project version (see Version safety below).

### Start from a dbdiagram.io / dbdocs design

Designed the schema visually first? Reverse-engineer the DBML export — no connection needed:

```sh
dbd init --from-dbml schema.dbml         # schemas + enums + tables + foreign keys
```

### Keep a project in sync with a drifting database

Someone changed the database out-of-band. Re-`merge` to pull the drift back as a new snapshot
version, then apply it everywhere:

```sh
dbd merge -d $DATABASE_URL --dry-run     # preview create/skip/conflict + the snapshot version
dbd merge -d $DATABASE_URL               # overwrite DDL + auto-snapshot the diff (guarded)
```

## What each source captures

| Source | Captured |
|--------|----------|
| **Postgres / Supabase** (`--from-db postgres://…`) | schemas, extensions, enums, tables (columns, defaults, identity, PK/FK/unique/check, indexes incl. `gin/gist/brin/hash`, comments, `serial`/identity columns), views, functions & procedures (verbatim), standalone sequences, and roles (opt-in `--roles`). **Not** captured: partial/expression indexes, triggers. |
| **SQLite** (`--from-db sqlite://…`) | tables, user indexes, and views — captured **verbatim** from `sqlite_master` (CHECK constraints, type affinity, `AUTOINCREMENT`, `WITHOUT ROWID` all survive). No schemas/enums/functions/sequences/roles; triggers skipped. Files land flat (`ddl/table/<name>.ddl`); `design.yaml` carries `schemas: []`. |
| **DBML** (`--from-dbml <file>`) | schemas + enums + tables + foreign keys only — DBML can't express functions, procedures, views, standalone sequences, roles, or check constraints. `serial`/identity survive (dbd's DBML carries `bigserial`/`[increment]`). |

The generated `design.yaml` target dialect comes from the connection URL scheme (`postgres://` →
`postgres`, `sqlite://`/`file:` → `sqlite`). Secrets never land in the repo — the target `url` is
written as the literal `$DATABASE_URL` reference, not the connection string you passed.

## Version safety

`dbd merge` against a **dbd-managed** database compares the database's applied version **D** against
the project's `project.version` **Y**:

- **D < Y → refuse.** The project is ahead of a stale database; overwriting project DDL from it
  would discard newer work. Bring the database up to date with `dbd apply`, or revert the project
  to version `D` via version control if you really mean to discard it. There is no override flag.
- **D ≥ Y, or any source with no `_dbd_meta` (a foreign DB, a DBML file, an unmanaged SQLite DB) →
  proceed.** dbd overwrites the introspected DDL and auto-snapshots the diff.
