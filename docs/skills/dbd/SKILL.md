---
name: dbd
description: >-
  Use when working in a project whose database schema is managed with dbd —
  SQL DDL files under ddl/<type>/<schema>/<name>, a design.yaml, and dbd
  commands (init, inspect, apply, deploy, migrate, snapshot, reconcile, diff,
  release, dbml, diagram, emit, import/export, policies, reset, format,
  install). Also use when embedding the
  dbd-core Rust crate. Covers the file-as-source-of-truth model, the full CLI
  surface, scopes, targets (PostgreSQL/Supabase/SQLite/Convex), and the library
  API. Load before editing DDL, changing design.yaml, or running any dbd command.
---

# dbd — database schema as code

dbd parses SQL DDL files, resolves dependencies, generates migrations, imports
data, and deploys to **PostgreSQL, Supabase, SQLite, or Convex** — one binary,
zero runtime deps. Same engine is embeddable as the `dbd-core` Rust crate.

Full reference: **https://dbd.sensei-hq.com/llms-full.txt** — every command has a
section there (a test checks that); guide pages per topic are listed in
https://dbd.sensei-hq.com/llms.txt. When the two disagree with the binary, run
`dbd <command> --help`.

## Mental model — files are the source of truth

Path = `ddl/<type>/<schema>/<name>.<ext>` (roles have no schema).
`<type>` ∈ `table|view|materialized_view|function|procedure|enum|sequence|role`; ext
`.ddl` or `.sql`. Folder = type, parent dir = schema, filename = entity name.
**Singular** type folder is canonical (`dbd doctor --fix` migrates plural →
singular).

- `ddl/table/config/lookups.ddl` → entity `config.lookups` (table)
- `ddl/view/public/genders.ddl` → entity `public.genders` (view)
- `ddl/materialized_view/analytics/daily_sales.ddl` → `analytics.daily_sales` (matview)
- `ddl/procedure/staging/import_lookups.ddl` → `staging.import_lookups`

Each file is parsed once into `Entity` + `TableDef`; that structure drives every
feature (dependency graph, apply, migrations, DBML, diagram, import).

`design.yaml` holds:

- `project` — `name`, `note`, `version`, `released` (set only by `dbd release`).
- `source` — `dialect` (`postgresql` default; `tsql`, `mysql`, `sqlite`),
  `parser` (override: `pg_query|tsql|mysql|verbatim`), `search_path`.
- `target.<name>` — **only the first entry is read**. `url` (use `$ENV_VAR` —
  never hardcode secrets; `-d`/`$DATABASE_URL` override it), `extensions`,
  `roles`, `grants` (`{schema: {role: [perms]}}`, any target key),
  `skip_schemas` (entities in these schemas are dropped from the design).
  The URL scheme picks the adapter.
- `schemas` — names, or `{name: {grants: …, exposed: bool}}`.
- `external` — objects managed elsewhere (only `name` is read); never a gap.
- `apply` / `import.after` — lifecycle hooks; `import.staging` — the schemas an
  import may load; `import.options`/`tables` tune the load (`tables` does not
  filter which files load); `export` — tables, views or matviews to export
  (empty ⇒ all tables).
- `materialized_views`, `dbml`, `scopes`, `ignore` (exact names or `prefix.*`;
  matching references are not resolved), `format` (formatter options).

Parsed but not read (reserved): `import.options.format`, `import.tables[].env`,
`target.schemas`, `target.path`, `external[].columns`. **`import.staging` is
enforced** (0.25+): when it lists schemas, a data file for any other schema is
refused — all of them named — before anything is truncated, on `import`,
`import -f`, `deploy` and their dry runs.

**The `supabase` target key** is the same PostgreSQL adapter plus four rules:
`public` is exposed by default; Supabase's schemas (`auth`, `storage`,
`realtime`, `extensions`, …) are never pruned by `reconcile --prune`; configured
grants also give `anon`/`authenticated`/`service_role` `USAGE` on every listed
schema (PostgREST); and `dbd reset` protects them and `public` (its `--target`
defaults to the design's key). `init --from-db` writes the key `supabase` when
the database has both an `auth` and a `storage` schema, and
`init --from-dbml --target supabase` does for a DBML source.

**Grants** (`schemas[].grants` + `target.grants`, target lists win per role) are
applied by `dbd apply`, `dbd deploy` and `dbd reconcile` after the schema and `apply.after`,
before import and policies; not on `--dry-run`; under `--scope` only on the
schemas the scope builds; skipped on SQLite/Convex. `exposed:` grants nothing — it drives `inspect`'s advisory "No RLS
policy on N table(s) in exposed schema(s)" report.

## CLI command map

| Command | What it does |
|---|---|
| `dbd init` | Scaffold a project (`--target postgres\|supabase`); or reverse-engineer one with `--from-db <conn>` (a Supabase DB gets a `supabase` target, and only there are its platform schemas left out; extensions go in the target's `extensions:`, FK targets outside the project in `external:`; every schema left out is named, and a schema nothing was captured in is left out of `schemas:` unless `--schema` named it; re-runnable DDL — an enum inside `DO … EXCEPTION WHEN duplicate_object` — so it applies back to its own DB, and to a fresh one, with an empty `diff` — for what dbd models: a domain or composite type is neither captured nor named, so a column typed with one fails on a fresh DB; identity `START`/`INCREMENT`, storage parameters, column `STORAGE`, RLS and its policies, and comments on constraints, indexes and functions are dropped without a word; a default holding a backslash reads as drift). It captures a table's triggers into its file (`CREATE OR REPLACE TRIGGER`; a constraint trigger after `DROP TRIGGER IF EXISTS`) and a view's `INSTEAD OF` triggers into the view's, EXCLUDE constraints, column collations, and a FK's `DEFERRABLE`/`INITIALLY DEFERRED`/`MATCH FULL`; leaves out every object an extension owns; folds a partitioned table's partitions into it as one plain table; and names, as a warning, what it cannot carry — `NOT VALID`/`NOT ENFORCED`/`NO INHERIT`, a PK's or UNIQUE's deferral, `UNLOGGED`, inheritance, partitioning, a disabled trigger. `--schema X` declares the extensions the captured objects use (in `public` or a captured schema) and prints a `needs:` line, naming the users, for every other type, function or table they need from outside — the empty diff then holds on a database that has those. `--from-dbml <file>` reads DBML as @dbml/core does (aliases, inline `ref:`, multi-line notes, `Note { }`, one-line tables @dbml/core accepts — anything after a column, an enum value or a header on its line is refused, naming it — a bare `pk`/`unique` after a type, a `pk` index entry, `Ref name:`, index `type:`), every name read back as itself; a one-to-one `Ref: a - b` keys `b`, an inline `[ref: - t.c]` its own column; a `<>` Ref or a `TablePartial` is refused; a `dbd dbml` stub comes back as `external:`; refuses over a dbd-managed DB — use `merge` |
| `dbd merge` | Sync a live DB / DBML back into the project (reverse + reconcile; edits only `project.version` in `design.yaml`). Never captures a table `design.yaml` declares `external:` (names it, left out), never tells you to declare external a table the project builds, keeps a table file's triggers (a SQLite table's too; `merge --from-dbml`, whose document holds none, keeps the triggers written in each table or view file it rewrites, with their `SET search_path`), warns about an undeclared extension or external table for you to add, and names only the schemas it wrote files into. `merge --schema` prints no `needs:` lines |
| `dbd inspect` | **Validate**: report entity errors/warnings + scope gaps. **Exits 1 on blocking errors** (since v0.12.0 — was always 0). Under `--scope`, splits errors (and warnings) into the scope's vs out-of-scope and runs its advisory checks over the scope; `-n` counts and advises on that entity alone (an `external:` name is refused); `--fix --scope` formats only the scope's files; a `ddl/extension/` or unlisted `ddl/schema/` file is an error (declare it in design.yaml). `--from-db` resolves refs against the live catalog and caches to `.dbd/refcache.json`. **Offline & no SQL execution** |
| `dbd apply` | Apply schemas + entities + pending migrations (blocks on migration TODOs) — one dependency sort, one transaction on PostgreSQL (`DBD_NO_TX` opts out) except where a version adds enum values (see below) — then `apply.after`, then grants. `--with-policies` also applies RLS. Names each DDL file that states no `SET search_path` before it runs. **Refuses** a `-e dev` run on a DB marked prod, a project older than the DB (`version guard: the database is at v3, but this project is at v2` — no override), and `-n <entity>` while migrations are pending (it would record versions it never ran — run `dbd apply` first). `--dry-run` runs apply's own gates (a missing hook file and a migration TODO included), refuses where apply would, and names the hooks a scope would skip; given a database it also checks the pin, the env and version guards and the `-n` refusal, and lists pending migrations in run order (`⚠` on drops) — a SQLite file that does not exist yet is not created, and reads as no database. Scope-guarded — `--allow-scope-change` to re-pin |
| `dbd deploy --source owner/repo/path` | Fetch → apply → **grants** → import → **apply RLS policies**. The only command that takes a GitHub source (`GITHUB_TOKEN` for private repos, sent to `api.github.com` only); others refuse one. `--no-cache` / `--clear-cache`. What the import phase would refuse (`import.staging`, a loader tie, a loader the scope leaves out) is refused before anything is applied, as are apply's guards (`-e dev` on a prod DB, an older project). Names each DDL file that states no `SET search_path`, as `apply` does. `--dry-run` gates like the real run and reports after scope filtering — given a database, the pin, the env and version guards and pending migrations too. Scope-guarded — `--allow-scope-change` to re-pin |
| `dbd reconcile` | Pre-release declarative diff+`CREATE`/`ALTER` in place (no snapshots): columns, keys, FKs, CHECKs, indexes, column comments, enum values. Writes in `apply`'s dependency order and is **not one transaction** — a failing step stops the run with earlier steps applied. Runs the `apply.before`/`apply.after` hooks and the grants, scoped as `apply` does, so it builds what `apply` builds — `--dry-run` lists them after the schema changes; refuses `-e dev` on a prod DB (dry run too); records the version even over a newer DB's. A destructive plan is refused listing each drop by entity unless `--allow-destructive`; an enum value removal that a live matview reading the column blocks is refused before any write (dry run too, non-zero exit), naming the `DROP MATERIALIZED VIEW` to run by hand before re-running. `--prune`, `--allow-scope-change`. Disabled when `project.released: true` |
| `dbd diff` | Read-only live↔design difference (same model reconcile converges, plus matview drift; column comments, not table/view/enum ones); `--json`, `--exit-code`; works after release; gap-gated under `--scope`; notes — never refuses — a DB pinned to another scope, whose unbuilt entities then read as missing |
| `dbd release` (`baseline`) | Cut v1: baseline snapshot + `project.released: true` (locks in snapshot/migration flow). A project that already has snapshots is released at its latest snapshot's version — refused while the design has unsnapshotted changes or `project.version` disagrees |
| `dbd snapshot` | Diff vs latest snapshot → migration SQL (smart multi-stage for renames/type changes; an enum value removal is 2 stages — the data mapping, then the type swap). A removed table's drop goes to `migrations/<NNN>/<schema>/<table>.sql`, and `apply` runs it |
| `dbd migrate --status` | Show migration status (read-only; `apply` runs them). `none recorded` for an unversioned DB; "up to date" only when the versions match; notes a DB pinned to a scope (built with that scope's entities, not the whole design) |
| `dbd import` / `export` | Load / dump table data (CSV/TSV/JSON/JSONL; a `.json` file is a JSON array of records or JSON Lines) via the `import/` & `export/` conventions. Env-scoped: `import/<env>/<schema>/<file>` — and a file directly in `import/dev/` or `import/prod/` — loads only when `-e <env>` (`dev`/`prod`) matches; `import/<schema>/<file>` loads always; a file in a deeper folder (`import/staging/sub/x.csv`) never loads and is named. A `-e dev` import into a DB marked prod is refused, an ad-hoc `import -n -f` and `--dry-run` (given a database) included. JSON keys name columns (an unknown key is refused); a file setting a generated column is refused; ids load as given and identity/serial sequences move past them. CSV/TSV are RFC 4180 and load **by header name** on both engines (a header column the table lacks is refused). `import` truncates (one `TRUNCATE` of every staging table it reloads, a referenced one loaded first — a table outside the import holding a key to one fails it, named, with nothing emptied; `DELETE FROM` on SQLite, referencing table first, a SQLite import ordered by the foreign keys the database reports; Convex always replaces), refusing first a data file whose target is a view, matview or sequence, then loads, `CALL`s each staging table's one loader — `import_<table>` first, procedures only, never functions — and runs `import.after`. `export` writes every table, or what `export:` lists — views and matviews included — counting tables and views apart, to the project's `export/<schema>/<table>.<fmt>` (`-o <dir>` = same layout under `<dir>`, so `-o import` round-trips — except into a schema folder of the project's `import/` tree, `import/<schema>` or `import/<env>/<schema>`, where each file is `<table>.<fmt>`, as that import loads it, and two tables sharing a file name are refused — export there, with `-n`, the one whose rows the file should hold; generated columns left out); its CSV round-trips (NULL vs `""` kept). JSONL writes every column's key: `null` is SQL NULL for every type, and a json/jsonb column holding the JSON value `null` is `{"$json": null}` (one level unwrapped on import, Postgres and SQLite); a column whose key no record carries takes its DEFAULT (on Postgres a key missing from only some records is NULL there; SQLite gives each such record the DEFAULT). A SQLite BLOB is written `\x…` hex, as Postgres writes a `bytea`, and read back into a BLOB column. Both refuse a DB pinned to another scope (no bypass), and a scoped `import` refuses a loader the scope leaves out. Never skips silently — see the seed-data gotcha below |
| `dbd refresh` | `REFRESH MATERIALIZED VIEW [CONCURRENTLY]` now (`-n <entity>`/`<schema>.*` for a subset). Scope-filtered — a matview the scope excludes was never built on that plane — and scope-guarded, with no bypass flag. Scheduled refresh is managed via pg_cron under `materialized_views:` in design.yaml |
| `dbd dbml` / `graph` / `diagram` / `combine` | DBML docs / dependency JSON / hosted interactive viewer (model in the URL fragment, never uploaded) / one SQL file. `dbml` and `diagram` name each file they leave out because it does not parse, and still write the rest; `graph` names it on stderr, keeping stdout JSON, and leaves it out of nodes, layers and edges (`graph -n <external>` is refused); `combine` leaves such a file out silently (run `inspect` first), puts `source.search_path` ahead of a file with none and, after each top-level `SET search_path` whose path lacks `public`, a statement of its own adding it — never editing the file's text — as apply does; on T-SQL every entity's DDL ends its own `GO` batch. `dbml` writes an index's method (`type:`), its `INCLUDE`/opclasses/storage parameters (in the index note), a named PK (`pk` index entry with `name:`) and a named FK (`Ref "name":`), leaving out Postgres's default names, and every name in DBML's quoting, read back as itself whatever it holds (an enum column's type as `"app"."mo\"od"` when it needs quotes); a FK's deferral and `MATCH FULL`, EXCLUDE and collations have no DBML form, an expression default or CHECK holding a backtick gives a document `init --from-dbml` refuses, and a line break in a CHECK's string literal comes back as a space |
| `dbd emit --dialect mysql\|tsql\|sqlite` | Translate a PostgreSQL schema into another engine's DDL; what a target cannot take is reported (`--report` JSON) — a type past the target's limits is switched or clamped (`nvarchar(max)`, `MEDIUMTEXT`, `DECIMAL(60,30)`), MySQL gets `LONGTEXT`/`LONGBLOB`/`DATETIME(6)`. Keeps PK, UNIQUE, FK and CHECK names (MySQL calls every PK `PRIMARY` — reported), and escapes the target's quote character inside a name (SQLite doubles `"`, MySQL a backtick, SQL Server `]`); a `time` clock default is the time of day on every target; a FK's `DEFERRABLE INITIALLY DEFERRED` reaches SQLite; a lost `NULLS NOT DISTINCT` (MySQL, SQLite), EXCLUDE, collation, `MATCH FULL` and deferral elsewhere are reported. Not reported: a `timestamptz`'s zone on MySQL/SQLite, SQL Server's one-NULL UNIQUE, index-key byte limits, identity `START`/`INCREMENT`; a FK named on its column loses the name. On SQLite, view queries and CHECKs are written again from the parse tree, or left out when SQLite cannot run them; extensions, roles, grants and policies are reported as not emitted. T-SQL output ends every statement with `GO` — split on it before sending it through a driver. `-f`/`--report` stay inside the project. Refuses non-PostgreSQL projects |
| `dbd policies` | Apply RLS from `policies/<schema>/<table>.sql` (idempotent, fail-forward). Honors `--scope`: a policy whose table the scope excludes is skipped and reported. Scope-guarded, with no bypass flag — `--dry-run` too, given a database |
| `dbd doctor [--fix]` | Audit/repair config + layout: old design.yaml, stale files, plural dirs (`--fix` migrates these). Also flags **misfiled view/matview DDL** (a `CREATE MATERIALIZED VIEW` under `ddl/view/`, or a plain view under `ddl/materialized_view/`) — reported with a move hint, **not** auto-fixed — and `ddl/extension/`/`ddl/schema/` files, with the design.yaml entry each becomes. Run it to verify layout before `reset`/`apply` |
| `dbd reset` | Drop the project's own objects (guarded by the bookkeeping env check — `dbd.meta` on Postgres/Supabase, `_dbd_meta` on SQLite; blocked when the DB is marked prod — `production` rows included — or versioned, `versioned at vN`, "a released project's database" only when `project.released` is set). `--target postgres\|supabase\|sqlite` (default: the design's target key) picks the reset and its protected-schema set, and one that is not the kind of DB the connection reaches is refused before connecting; on SQLite it drops tables and views in one transaction. A full reset on `postgres` drops every role the design declares (`ddl/role/` + `target.roles`), first revoking the grants `apply` made them — keeping, and naming, a role another database on the server holds — never `DROP OWNED BY`; `--extensions` under a scope drops only its extensions. A **scoped reset never cascades**: all or nothing, it refuses naming what outside the scope depends on it. A reset under the all-scope, or under the scope the DB is pinned to (a bare reset in a design whose `default:` scope built it), also forgets the DB's bookkeeping row — env, version and pin; a narrower scope keeps it. Prints what it dropped. Also scope-guarded — `--force` or `--allow-scope-change` bypasses. `--dry-run` checks the same guards whenever a database is configured |
| `dbd format [--check]` | DDL formatter (river-style SELECT bodies; `--check` for pre-commit); options under `format:`. Changes layout and keyword case only: statements are split by libpg_query, and a statement is laid out only if it parses to the same tree with every string literal, `$$`/`$tag$` body, quoted name and comment byte-for-byte the same — else only recased, else left as written; a file PostgreSQL's scanner cannot read is left unchanged. A keyword is recased where the grammar reads one (a column named `name` or `level` keeps its case; a word on the formatter's own list — `data`, `type`, `key`, `comment` — is recased even as a column name). Formatting twice changes nothing, and a comment beside or above a statement stays there. A table is laid out only if sqlparser 0.62 reads it — `UNLOGGED`, `LIKE`, `EXCLUDE`, a key's `INCLUDE`, column `STORAGE` and the like leave it recased, not laid out. Writes a file back in its own encoding (UTF-16 stays UTF-16) |
| `dbd install [--project]` | Install this skill and the `dbd-pattern-verifier` agent into `~/.claude` (or `$CLAUDE_CONFIG_DIR`, or `./.claude`) |

`-e/--environment` takes `dev` or `prod` (`development`/`production` are
aliases; anything else is an error; default `prod`). It picks `import/<env>/`
folders and is recorded in bookkeeping — a DB applied without `-e dev` is prod.
**Environment guard:** a `-e dev` `apply`/`deploy`/`reconcile`/`import` (an
ad-hoc `import -n -f` too) against a DB marked prod is refused before any write,
and so is its dry run, given a database — it would lift reset's prod guard and
load `import/dev/` data into production. The mark moves only dev → prod, and a run
that promotes a dev DB says so (`This database was marked dev and is now marked
prod`); there is no override. So build a dev DB with `-e dev` from its first run;
one built without it is rebuilt with `dbd reset --force` under the all-scope or
the scope the DB is pinned to (either forgets the DB's record), then `-e dev`.

**The record is keyed on `project.name`.** A project with no record of its own,
on a DB that holds its tables, views, matviews, enums or sequences, is held to
the other projects' records there — the mark a renamed `project.name` left — by
every guard (env, version, scope pin, reset), and the refusal names the old
project: set `project.name` back. Two projects sharing a DB each read their own
record. A renamed project that trips no guard is recorded beside the old name,
and one ahead of the old record's version is planned as a fresh install, so its
pending migrations do not run — don't rename a deployed project.

A PostgreSQL URL that cannot be reached fails within 5 seconds, naming its
`host:port` (never the URL, which may hold a password).

**Global scope flags**: `--scope <name>` selects a `scopes:` subset; `--deps
report|include` overrides its gap policy. **Gap-gated** (refuse on a dependency
gap under `report`): `inspect`/`apply`/`import`/`deploy`/`reconcile`/`diff`.
**Filter-only**: `dbml`/`combine`/`emit`/`graph`/`diagram`/`export`/`reset`/
`refresh`/`policies`. **Ignored with a warning** (`--scope` or `--deps` alike):
`snapshot`/`release`/`migrate`/`doctor`/`init`/`merge`/`format`/`install`. `all`
and `default` are reserved scope names (`--scope all` = full set; `default` =
what a run without `--scope` builds). One design → many DBs:
`dbd deploy --scope hub --database $HUB_URL`. Every command that takes a scope
except `graph` prints `scope 'X': N of M entities` — the working set (closure
included) out of every entity, externals left out (`diff --json` on stderr;
`graph` puts the scope in its JSON). A scope keeps the **sequences** of its
schemas and the schema its extensions install into — one that excludes that
schema while keeping the extension is refused; a table depends on its column's
enum, its default's sequence and its trigger's function (gaps reported);
**grants** follow the scope; a **hook** is skipped only when it touches a table
the design manages outside the scope (an `external:` table never causes a skip),
and an `apply.*` skip is named before the run (`apply`, `deploy`, `reconcile`)
and in the dry runs, an `import.after` skip in `import`/`deploy --dry-run`; a
scoped migration installs the roles and extensions it adds; a dry run filters as
its real run does; a `-n` outside the scope is an error
(`X is outside scope 'S'`). Not narrowed: roles, and pg_cron refresh jobs — a
scope whose `extensions:` leaves out `pg_cron` fails if a matview outside it has
a schedule.

**A scope narrows what is applied, not what the project owns.** From 0.24.1,
`reconcile --prune --scope` never drops a table the design declares outside the
scope. **Before 0.24.1 it did** (#40, `DROP TABLE … CASCADE` on the project's
own data) — check `dbd --version` and upgrade before running a scoped prune.
**Before 0.25**, a design with a role or an extension without `schema:` made
`public` a managed schema, and any `reconcile --prune` dropped every table in
`public` the design did not declare — upgrade before pruning such a design.

**`snapshot`, `release` and `migrate` deliberately ignore `--scope`** and warn
when given it (`⚠ --scope X ignored: this command operates on the whole
design, not a scope`). A snapshot records the **design**, not one database: a
scoped baseline would omit entities, and every later `dbd snapshot` would diff
against an incomplete record and emit a spurious `CREATE` for them on every
plane. Scope decides which migration steps *run*, not which ones exist — so
`dbd deploy --scope daemon` on a released project applies only the in-scope
migrations while the database's version still advances.

**Scope guard**: the bookkeeping table (`dbd.meta` on Postgres/Supabase, `_dbd_meta` on
SQLite) records the scope a database was built with (nullable `scope` column; `NULL` =
unpinned). `apply`/`deploy`/`reconcile`/`reset`/`import`/`export`/`refresh`/`policies`
refuse to run under a different scope than the one the database is pinned to —
an exact match (so do `reconcile --dry-run`, and the `apply`/`deploy`/`import`/
`policies`/`reset` dry runs given a database); `diff` and `migrate --status`,
which write nothing, note the pin instead:
`scope guard: this database is pinned to scope 'X', but you requested 'Y'.
Applying a different scope would build a divergent schema.` Pass `--scope X` to
match the pin, or `--allow-scope-change` to override — a successful write then
re-pins the database to the new scope (`reset --force` also bypasses the check;
a reset does not re-pin; `import`, `export`, `refresh` and `policies` have no
bypass — none re-pins, so run them under the pinned scope or re-point the database
with `apply` first).
Databases created before this feature are unpinned and aren't blocked until their
next write pins them; a missing `--scope` resolves to the `default` scope (or
`all`) and pins on first write. To legitimately host multiple modules in one
database, define a named scope in `design.yaml` that includes them, rather than
re-pinning back and forth.

## Critical gotcha — inspect ≠ apply

`inspect` validates the **parsed model**, and only at the **entity level**: it
checks that referenced *tables/views/types* exist (`resolve_references`). It does
**not** validate *column-level* references (index columns, `CHECK`, generated
columns, FK column lists) and does **not** execute SQL. `apply` for a table ships
the **raw .ddl text** straight to the database. So a missing/renamed **column**
referenced by an index or constraint passes `inspect` cleanly and only fails at
`apply`, as a database error. Column-level correctness is the database's job, not
`inspect`'s.

## Critical gotcha — schema applied, rows missing

DDL applying is not evidence that seed data loaded. `dbd deploy` runs
**apply → import → policies**; the import phase can legitimately load nothing,
and the usual cause is layout, not failure:

- **Wrong env.** `import/<env>/<schema>/<file>` (or a file directly in
  `import/<env>/`) loads only when `-e <env>` matches. A `-e prod` deploy walks
  straight past `import/dev/…`.
- **Outside the convention.** Only `import/**` with a `.csv`/`.tsv`/`.json`/`.jsonl`
  extension is scanned — `data/` and `seeds/` are invisible — and only
  `import/[<env>/][<schema>/]<file>`: a file one folder deeper never loads (dbd
  names it; before 0.25 `import/dev/staging/sub/y.csv` loaded into `staging.y`).
- **Cut by `--scope`.** An entry is dropped when a table its procedure writes, or
  its declared staging table, falls outside the scope; the warning names which.
- **A loader written as a function.** Only procedures are run; a function that
  reads a staging table no procedure loads is never called, and is warned about.
  Each staging table gets **one** loader — `import_<table>` if a reader has that
  name, else the reader that touches no other staging table — and a procedure
  that reads it without being its loader is warned about, not run.
- **Unparseable staging file.**

dbd reports every one of these: the summary always states the table count —
including `0` — followed by the reason. **Do not treat a silent-looking success
as proof**; read the count. Verify against the database (`select count(*)`),
not the exit code.

Some causes are refused outright, before anything is truncated (0.25+): a `-n`
that selects no data file (`X has no data file under import/`, or the file is
under another env's folder), a data file whose schema a non-empty
`import.staging` leaves out, two procedures with an equal claim to one staging
table (name one `import_<table>`), and a CSV/TSV header naming a column the
table does not have — the header names the columns, so it must match them
exactly or in lower case, with nothing trimmed (`id, name` fails). On PostgreSQL
a CSV whose header held labels rather than column names loaded by position
before 0.25, and is refused now. So are a JSON key naming no column, a file that
sets a generated column, a data file whose target is a view, matview or
sequence, and — under `--scope` — a staging table whose loader procedure the
scope leaves out. The wrong database is refused too: a `-e dev` import into one
marked prod, and one pinned to another scope. A table outside the import that
holds a foreign key to a staging table fails the truncate, named, before
anything is emptied.

Policies are the same shape: `deploy` applies them unconditionally, but a failed
policy file is **non-fatal** — warned and counted, exit still 0. "The deploy
succeeded" does not mean RLS is in place. `dbd apply` skips policies entirely
unless given `--with-policies`.

Under `--scope`, a policy whose table is outside the working set is **skipped,
not run** — its target is read from the path (`policies/<schema>/<table>.sql`).
Before this, a policy for a schema a given plane lacks ran anyway and reported
`FAILED … schema X does not exist` on every deploy: an expected condition
dressed as an error, which is how real failures stop being read. Skips are
reported per file and counted in the deploy summary.

## Lifecycle script hooks — `apply.before` / `apply.after` / `import.after`

For SQL dbd does not model: attaching a table to a publication (Supabase
realtime), `grant`s, and other post-DDL setup.

```yaml
apply:
  before:
    - sql/pre_ddl.sql
  after:
    - sql/realtime.sql               # deps derived by parsing the SQL
    - script: sql/grants.sql         # deps declared explicitly
      writes: [app.messages]
import:
  after:
    - import/loader.sql
```

| Hook | Runs | Position |
|---|---|---|
| `apply.before` | `apply`, `deploy` **and** `reconcile` | after design + scope checks pass, before the first entity |
| `apply.after` | `apply`, `deploy` **and** `reconcile` | after every entity applied and matview stamped, **before** the configured grants and `policies/` |
| `import.after` | `import` **and** `deploy` | after data load and the import procedures |

**The trap: `import.after` never runs on `dbd apply`.** Only `dbd import` and
`dbd deploy` reach the import phase. Post-DDL SQL parked in `import.after` —
because it was the only hook that existed — silently does nothing on
`dbd apply`. If a hook's job is "run once every entity exists", it belongs in
`apply.after`.

**Migrating a realtime/publication hook** (the common case):

```yaml
# before — runs on import and deploy, silently skipped on apply
import:
  after: [import/after/realtime_publication.sql]

# after — runs on both, and scope-aware
apply:
  after:
    - script: import/after/realtime_publication.sql
      writes: [app.relay_sessions, app.relay_segments, app.relay_inbox]
```

`writes:` is **required** for this shape, not optional. dbd derives a script's
tables by parsing it, which only works when they appear as SQL identifiers. A
realtime hook names its tables as *data* — inside `array['a','b']` and
`format('alter publication … %I', t)` — where `select_tables()`/`dml_tables()`
return empty. No parser can recover those. Declare them.

**Scope filtering.** A hook is skipped when it depends on a table the design
manages outside the working set; a table the design does not manage (an
`external:` one, or one the hook creates) never causes a skip. A skip is
reported — an `apply.*` skip before the run starts and in the `apply`/`deploy`/
`reconcile` dry runs (`Design::hook_skips`), an `import.after` skip in
`import`/`deploy --dry-run` — and the run continues. A script whose deps
cannot be derived and that declares no `writes:` **always runs** — the safe
direction. A hook stating no `SET search_path` runs on `source.search_path`
(else the session default, `RESET search_path`), and its bare table names are
judged as Postgres resolves them: statement by statement, along the path in
effect there (`public` after it), to the first schema where the design has that
table or view. Scope filtering is a
correctness aid, **not a security boundary**; never use a scope to prevent a
hook running.

Scripts run in list order. A path dbd cannot find is an **error, not a skip**.
Paths must stay in the project — `..` and absolute paths are refused.

## Workflow — pre-release vs upgrades (read this before changing a schema)

dbd has **two** schema-change workflows and using the wrong one corrupts the
project's history. Decide which you're in **first**:

**Which am I in?** Read `project.released` and `snapshots/`:

- `project.released: true` → **released**. `dbd reconcile` refuses.
- no `released`, no `snapshots/` → **pre-release**.
- no `released`, but `snapshots/` exists → **snapshots but not released** — the
  state `dbd init --from-db` and `dbd merge` leave. It is **not** released: dbd
  checks only the flag, so `reconcile` still runs here. It is on the migration
  track, so **run `dbd release`**: it releases the project at the version its
  latest snapshot reached, without a second baseline. It refuses while the
  design has changes no snapshot records (`dbd snapshot` them first) or when
  `project.version` disagrees with the latest snapshot. Until it is released,
  change it with snapshot → apply, never reconcile.

| | Pre-release (still iterating, pre-v1) | Released (upgrades) |
|---|---|---|
| Signal | no `project.released`, no snapshots | `project.released: true` (snapshots without the flag: run `dbd release` first) |
| Change a schema | edit DDL, then **`dbd reconcile`** (diffs live↔design, applies `CREATE`/`ALTER` in place — no snapshot, no version bump) | edit DDL, then **`dbd snapshot`** (writes the migration) → **`dbd apply`** (runs it) |
| Fresh DB | `dbd apply` | `dbd apply` (built from the DDL at the latest version; migrations run only on a DB at an earlier one) |
| `dbd reconcile` | ✅ the whole point | ❌ **disabled** — do not use |
| Drops | `reconcile --allow-destructive` (columns, constraints incl. replacing a PK in place, and removing an enum **value** — the type is recreated; only the managed views over a table with a column of the enum are dropped and re-applied, never tables or routines; a live matview reading the column refuses the removal, naming the `DROP MATERIALIZED VIEW` to run by hand — dbd never drops one; the type's comment is kept) / `--prune` (orphan tables) | expressed as migrations via `dbd snapshot` (a removed table's drop in `migrations/<NNN>/<schema>/<table>.sql`) |

- **Pre-release example:** you renamed a column in `ddl/table/app/orders.ddl`; run
  `dbd reconcile -d $DATABASE_URL` to converge the dev DB. No snapshot is written.
- **Released example:** same rename after `dbd release`; run `dbd snapshot --name "rename …"`
  (dbd generates the migration, splitting a rename into safe stages) then `dbd apply`.
- **Never** hand-write SQL against a released database, and never run `reconcile` on one —
  `dbd release` disables it precisely to force changes through the migration trail.

**How `apply` runs pending migrations** (0.25): version by version, each whole
before the next, as upgrading one release at a time would — a version's creates
and `.sql` alters in dependency order, then the `.data.sql` scripts of the
tables it alters or adds (one seeding a table it adds runs after the create),
then its drops, then the record that it ran; after the last version every entity's
DDL re-applies. A version's `ALTER TYPE … ADD VALUE` statements run first, as
`IF NOT EXISTS`, and **commit** (Postgres will not use a value inside the
transaction that added it): the run is one transaction except that everything
up to each such version's additions commits, earlier versions recorded — if the
rest fails, the values stay and the DB records the version before; re-running
after the fix adds them again as no-ops. Known gaps: on a jump, a table an
earlier pending version adds is created from its current DDL, so a later
version's generated `ADD COLUMN` on it fails (`already exists`) — upgrade a
release at a time or make it `IF NOT EXISTS`; a released project never drops a
view, function or sequence (snapshots hold tables and enums only).

## Self-check before you touch a dbd project

- **Right workflow for the release state** (above) — the #1 mistake is `reconcile` on a released project or hand-migrations on a pre-release one.
- **Singular type folder**: `ddl/table/…` not `ddl/tables/…` (`dbd doctor --fix` migrates plural).
- **Idempotent DDL** so re-`apply` is safe: `create table if not exists`, `create or replace view`, `create materialized view if not exists` (+ `create [unique] index if not exists`). Postgres has no `create or replace materialized view`.
- **Secrets via `$ENV_VAR`** in `design.yaml` target URLs — never a literal connection string.
- **String-set `CHECK` → consider an enum**: `check (status in ('a','b'))` is better modeled as a Postgres `enum` (`ddl/enum/…`) for type safety + introspection. `dbd inspect` suggests these.
- **`inspect` doesn't check column-level refs** (index/CHECK/FK column lists) — those fail at `apply` as DB errors, not at `inspect`.
- **Seed data lands where the env says it does**: `import/<env>/…` only loads under `-e <env>`. Build a dev database with `-e dev` from its first run — one built without it is marked prod, and a later `-e dev` run is refused (rebuild with `dbd reset --force` — under the all-scope or the DB's pinned scope, which forgets its record — then `-e dev`). If a deploy must seed rows, check the reported import count (never assume) and confirm `policies/` is applied by `deploy`, not by a bare `apply`.
- **Import loaders are procedures, one per staging table.** dbd `CALL`s the procedure that reads a staging table; a function is never run, so a loader written as one leaves its target empty (dbd warns). Name each loader `import_<table>` — it settles which procedure loads a table when several read it; two equal claims are refused.
- **CSV headers name columns.** A CSV/TSV loads by its header on both engines; a header naming a column the table lacks (or with a space after a comma) is refused. With `import.staging` set, every data file must sit under one of its schemas.
- **Post-DDL SQL belongs in `apply.after`, not `import.after`** — `import.after` runs only with the import phase (`dbd import`, `dbd deploy`), so on `dbd apply` it silently does nothing. Move any "run once every entity exists" hook (realtime publications, grants) to `apply.after`, and declare `writes:` when the tables are `format()`/`array[…]` data rather than SQL identifiers.
- **`dbd inspect` exits 1 on blocking errors** (v0.12.0+). Safe to use as a CI gate; a pipeline that relied on it always exiting 0 will now fail on a broken design.
- **Don't rename `project.name` on a deployed project.** dbd keys its record of a database on it: the guards hold a renamed project to the old record, but a run that trips none records a second row, and a design ahead of the old version is planned as a fresh install — its pending migrations never run.
- **Scoped prune needs dbd ≥ 0.24.1.** Never run `reconcile --prune --scope …` with an older binary — it dropped the design's own out-of-scope tables (#40).
- **Run `dbd doctor` to verify layout** before `reset`/`apply`. Beyond old-format config, stale files, and plural folders (all `--fix`-able), it flags **misfiled view/matview DDL**: dbd types an entity by its folder, so a `CREATE MATERIALIZED VIEW` sitting under `ddl/view/` is treated as a plain view and `dbd reset` emits `DROP VIEW` on it → `"… is not a view"`. doctor prints the file and a move hint (`→ ddl/materialized_view/<schema>/…`); move the file to fix (not auto-fixed — folder = type is the source of truth). It also names the design.yaml entry for each `ddl/extension/` or `ddl/schema/` file, which dbd never reads.

## Materialized views

`ddl/materialized_view/<schema>/<name>.ddl` holds `create materialized view if
not exists … with data;` + trailing `create [unique] index if not exists …`.
Scheduled refresh is declared under `materialized_views:` in design.yaml
(shared `options` + per-view `overrides`) and runs in-database via **pg_cron**
(synced on `apply`/`deploy`; needs the `pg_cron` extension; `concurrently`
needs a unique index). `dbd refresh` refreshes on demand. **Reconcile only
*warns* on a drifted matview definition — it never auto-drops one** (a recreate
is `DROP … CASCADE`, losing data/dependents); to apply a changed definition,
drop it manually, then `apply`/reconcile recreates it. `dbd diff` reports matview
drift (`missing`/`drifted`/`unstamped`/`orphan`). The drift stamp lives in
`dbd.matview_stamps`, so a matview's own `COMMENT ON MATERIALIZED VIEW` is the
author's; a stamp a dbd before 0.25 kept as the comment is copied there on
upgrade, before the file's `COMMENT ON` can erase it, and `reset` deletes the
stamps of the matviews it drops. PostgreSQL/Supabase only.

**The folder is the type.** A matview MUST live under `ddl/materialized_view/<schema>/`
— one placed under `ddl/view/` is classified as a plain view, so `dbd reset` runs
`DROP VIEW` on it and fails (`"… is not a view"`). `dbd doctor` flags this mismatch
with a move hint; see the self-check above.

## Library usage (dbd-core)

```toml
# Cargo.toml
dbd-core = { git = "https://github.com/sensei-hq/dbd" }
```

```rust
use dbd_core::{connect, ApplyComplete, Design};
use dbd_core::design::Progress;
use std::path::Path;

// 1. Load the declarative design for an environment (sync; scans ddl/ next to the config).
let mut design = Design::from_config(Path::new("design.yaml"), "prod")?;

// 2. Validate offline — no DB needed.
let report = design.report(None, None);
//    report.issues              → errors blocking the active scope (what apply refuses on)
//    report.out_of_scope_issues → errored entities the scope excludes (non-blocking)
//    report.warnings            → entities with warnings the scope builds (e.g. unresolved entity refs)
//    report.out_of_scope_warnings → entities with warnings the scope excludes

// 3. Connect an adapter — Postgres/SQLite/Convex chosen from the URL scheme.
let adapter = connect("postgres://localhost/mydb", &design.config().project.name).await?;

// 4. Apply schema + entities + pending migrations.
//    The three callbacks travel together in ONE `Progress` value — they are not
//    three arguments. `Progress::none()` for a silent run.
let scope = design.resolve_scope(None, None)?; // None,None ⇒ full design, default deps policy
design
    .apply(&*adapter, None, /*dry_run*/ false, Some(&scope), Progress {
        on_start: |desc: &str| println!("→ {desc}"),
        on_done: |desc: &str, err: Option<&str>| if let Some(e) = err { eprintln!("✗ {desc}: {e}") },
        on_complete: |s: ApplyComplete| println!("applied {} entities", s.applied),
    })
    .await?;

// Or apply + grants + import + policies in one call — the same pipeline `dbd deploy` runs:
design.deploy(&*adapter, false, Some(&scope), |s| {
    println!("deployed to v{}", s.apply.to_version);
    println!("{} table(s) imported", s.import.tables);
    // Non-fatal diagnostics: skipped imports and failed policy files.
    for w in s.warnings() { eprintln!("warning: {w}"); }
}).await?;
```

`deploy` applies RLS policies from `policies/` unconditionally, so an embedder
gets the same end state as the CLI. A failing policy file does not fail the
deploy — it lands in `summary.policies.failed` and is surfaced by
`summary.warnings()`. A policy the scope excludes lands in
`summary.policies.skipped` and is surfaced the same way. Use
`deploy_with_progress` for per-step callbacks. `deploy` applies the configured
grants between the schema and the data (`summary.grants`, a `GrantsOutcome`);
`Design::apply_grants(&*adapter, Some(&scope))` applies them alone — scoped, as
`dbd apply` runs it after `apply`.

Key public types (re-exported at the crate root): `Design`, `DatabaseAdapter`,
`Entity` / `EntityType`, `SchemaModel`, `ResolvedScope`, `ApplyComplete` /
`DeployComplete` / `ImportComplete`, `DbdError` / `Result`. The free fn
`dbd_core::design::apply_policies` remains available to apply policies alone.
`Design::scoped_import_plan(name, scope)` replaces the deprecated
`design::import_entry_in_scope` and says why each entry was left out.
`dbml::DbmlMultiParams` gained a required `design_entities` (the whole design, for
typing stubs).

**Breaking in 0.25.** `Design::check_scope_guard`, public since 0.24.1, takes
`&DatabaseMarks` instead of `Option<&ProjectMeta>` — read them with
`design.database_marks(&*adapter).await?` (this project's record, or the one a
renamed `project.name` left), or wrap a row you hold with
`DatabaseMarks::of_own`. Exhaustive matches need new arms: `RefKind::Uses`,
`ExecutionStep::AddEnumValues`/`MigrateData`, `TableConstraint::Exclude`.
Struct literals need new fields: `Entity.triggers`, `ColumnDef.collation`,
`ForeignKey.match_full`/`deferrable`/`initially_deferred`,
`ReconcilePlan.destructive_changes`/`enum_swaps`. A key named on its column, or
an inline `UNIQUE NULLS NOT DISTINCT`, now parses to the table-level
`TableConstraint` it amounts to (a UNIQUE leaves the column's `is_unique` false).
`DatabaseAdapter` gained a required `forget_project_meta` (a reset under the
all-scope or the pinned scope calls it).

**New in 0.25.** The guards, each taking `&DatabaseMarks`:
`Design::check_env_guard(marks, env)`, `check_version_guard(marks, version)`,
the method `design.check_reset_guard(&marks)` (it calls the DB a released
project's only when `project.released` is set), plus `check_apply_guards(marks)` (env + version —
what `Design::apply` runs before its first write, dry run included),
`conflicting_pin(marks, scope)`, `guard_env(adapter)` and `env()`; `deploy`,
`reconcile` and `import_data` check the env guard. `dbd_core::connect_existing`
(and `SqliteAdapter::open_existing`) connects without creating a database —
`None` for a missing SQLite file — which is how a dry run asks. Also:
`Design::check_import_plan(name)` (every import refusal, offline — `deploy` runs
it before applying), `hook_skips(scope)`, `apply_hooks_to_run(scope)` and
`grants_to_apply(scope)` (what `reconcile --dry-run` lists), `unparsed(&scope)`,
`search_path_warnings(name, scope)` (the files `apply` names for stating no
path), `pending_migration_preview` (`MigrationPreview` with `recorded`;
`MigrationAction::Create`/`EnumValues`/`Migrate`/`Data`/`Drop`),
`DeployComplete::policy_warnings()`, `ApplyComplete::previously_built`,
`Report::out_of_scope_warnings`, `DesignConfig::effective_schema_grants()`,
`source_text::read_with_encoding`, `Design::reset` returning `ResetComplete`
(`dropped`, `count(kind)`, `roles_kept: Vec<KeptRole>`) with
`roles_kept_by_reset`/`reset_script_keeping` for a preview,
`ReconcileComplete::before_scripts`/`after_scripts` (`reconcile` runs the apply
hooks, not the grants — call `apply_grants` after it), `script::runnable_ddl`,
`runnable_script`, `enum_value_additions`, `idempotent_enum_value_additions`,
`Design::live_import_plan(adapter, name, scope)` (the import plan ordered by
the database's foreign keys too), and the `reverse::Declared`/`design_yaml_with`/
`left_out_schemas`/`outside_needs`/`keep_authored_triggers`/
`SchemaSelect.supabase` pieces. `DatabaseAdapter` gained, with
defaults: `stamp_matview`, `forget_matview_stamps`, `truncate_table`,
`truncate_tables` (Postgres: one `TRUNCATE`), `existing_objects`,
`roles_held_elsewhere`, `live_matviews`, `matviews_reading_columns`,
`other_projects_meta` (the rename guard's read), `execute_project_script` (a
migration or hook on a search path), `dependencies` (`adapter::Dependency`,
what `init --from-db --schema` reads for `needs:`) and `foreign_keys` (the
keys between the database's tables, which order a SQLite import).
`Design::reconcile` does **not** check `project.released`, and
`Design::apply`/`deploy`/`reconcile`/`reset`/`import_data` and `design::apply_policies` do not
run the scope guard — the CLI does both, so an embedder must
(`Design::check_scope_guard(&design.database_marks(&*adapter).await?, &scope.name, false)`). The guide page https://dbd.sensei-hq.com/guide/embedding covers
the whole API with compiled examples.

### The schema model as a wire format

`dbd diagram --json` writes a `SchemaModel` — what `dbd_core::schema_model::build(&design, scope)` returns as `Result<SchemaModel>`: it fails on a `deps: include` scope whose closure needs something the scope excludes, as every command does. It is read by dbd's own viewer, by the shared viewer package, and by external indexers, so it is a cross-repo contract rather than an internal shape, and it states its own `version` (currently `3`).

| Field | What it holds |
| --- | --- |
| `tables` | Tables (`kind: table`), with columns (`pk`/`nn`/`en`/`fk`/`uq`, type, default, comment, `identity` `always`\|`by default`, `generated` expression), indexes (`def`, `unique`, `name`, `where` for a partial index) and `checks` (`{name?, expression}`) |
| `refs` | Every foreign key — the ER edges, column-level, as `{s,t,c}` — landing on a table or a stub |
| `stubs` | Table-shaped nodes for the tables `refs` land on that `tables` does not carry: only the referenced columns, `kind` `external` \| `out_of_scope` \| `unresolved`; omitted when empty |
| `entities` | Views, materialized views, functions, procedures, triggers and sequences (`schema`, `name`, `kind`, `note`, `noteMd`) |
| `deps` | `{from, to, kind}` where `kind` is `reads` \| `writes` \| `calls` \| `member` \| `uses` (a column's enum or its default's sequence) — the call/reference graph |
| `schemas` / `project` | Every schema the model draws something in, with table and enum counts, and the project's name, dialect and note |
| `history` | What each snapshot changed, oldest first (v3; omitted when there are none) — `{version, through?, description, timestamp, baseline?, changes}` |
| `enums` | Enum types (v3; omitted when there are none) — `{schema, name, values, note?, noteMd?}`, values in declaration order, named by file stem like every entity |

`entities` and `deps` are separate arrays rather than folded into `tables`/`refs` under a `kind` discriminator. Folding reads tidier and would silently change what every existing consumer of `tables` receives; the version field is there so the next extension is not a guess downstream.

`deps` is a projection of `Entity::refs`, already resolved and deduplicated — no second parse. An edge whose target is not a project entity (a Postgres built-in like `now()`, or a genuine dangling reference) carries `"unresolved": true` instead of being dropped: the edge is real, only the endpoint is unplaceable, so a renderer should dim it rather than pretend the call does not happen.

Comments reach every kind. A table's live on `TableDef::comments`; a view, materialized view, function, procedure or sequence has no `TableDef`, so its own `COMMENT ON` lands on `Entity::comment`. Both project to `note` (first line — the one-line summary an entity table shows) and `noteMd` (the full text). The fields added within v3 — `stubs`, sequences, `checks`, `where`, `identity`, `generated` — are optional, so an earlier v3 payload still reads; the viewer refuses a model whose version is newer than it reads.

`history` is not filled by `build`, which reads the design and nothing else; `dbd diagram` attaches `dbd_core::history::load(project_dir)` — or `load_scoped(project_dir, keep)` under `--scope`, where `keep: impl Fn(&str, &str) -> bool` is asked `(schema, name)` for each table and enum, and filters before diffing so the baseline counts the scope. The `keep` `dbd diagram` passes is `scope::admits(&scopes, &resolved, &working_set, &existing, qualified)`: the scope's working set — the `deps: include` closure too — for names the design still has, the scope's own `includes`/`excludes` for names it no longer has. It reuses `diff::diff` and `classify_changes`, so the changelog describes a step the way the migration did: each `changes[]` item is `{kind: table|enum, schema, name, op: added|removed|modified, fields}` and each field `{kind: column|constraint|index|value, name, op: added|removed|modified|renamed, from?, to?, note?}`. The first snapshot is a `baseline` of counts; a multi-stage change is one entry spanning `version`..`through`; types are canonicalised first; output is sorted. An unreadable snapshot is an error from `load` — `dbd diagram` warns and omits the history rather than ship one with a hole.

### Parsing SQL without a dbd project

An embedder with its own scanner — a code indexer, say — wants entities and
their relationships, not a deploy. Use `parse_sql`, not `parse_entity`:

```rust
let parsed = dbd_core::parser::parse_sql(sql)?;   // ParsedFile
for e in &parsed.entities {
    // e.entity_type / e.schema / e.name  — read off the CREATE statement
    // e.refs                             — every reference, each with its kind
    // e.reads() / e.writes() / e.calls() — filtered views of e.refs
    // e.comment                          — its own COMMENT ON (non-tables)
}
```

`parse_entity` takes identity from the path (`ddl/<type>/<schema>/<name>.ddl`),
which is right inside a dbd project and silently wrong outside one: it falls
back to `EntityType::Table` and names the entity after a directory, so a stored
procedure reads as a table with only `entity.errors` to hint at it. `parse_sql`
asks the SQL what it declares. Several declarations in one file become several
entities; `CREATE INDEX`, `COMMENT ON` and `ALTER TABLE … ADD CONSTRAINT` fold
into the entity they *name*.

Reference resolution is a separate phase: each entity carries its references as
written, provisionally qualified against `search_paths[0]`, and
`references::resolve_references(&mut entities, &externals, &ignore)` re-resolves
them once every file is read, warning on what it cannot place. That split is
what makes a scan parallelisable — the per-file parse touches no shared state,
so a bare `t` that could be `a.t` or `b.t` stays undecided until the whole set
is known.

### Multiple dialects

`parse_sql_as(dialect, sql)` reads a file in a stated dialect; `Dialect::detect`
reads it out of the SQL when nothing states one.

```rust
use dbd_core::parser::{Dialect, parse_sql_as};

let parsed = parse_sql_as(Dialect::detect(sql), sql)?;
println!("{:?} file, read as {:?}", parsed.kind, parsed.dialect);
```

| dialect | reader | structured? |
|---|---|---|
| `PostgreSql` | libpg_query | yes — columns, constraints, indexes |
| `TSql` | statement-head lexer | no — identity and edges only |
| `MySql` | statement-head lexer | no — identity and edges only |
| `Sqlite` | verbatim | no — the file is the model |
| `Unstated` | libpg_query (fallback) | — |

Detection **fails closed**: `CREATE TABLE t (id int)` is valid in every dialect
and says nothing, so it is `Unstated` rather than a guess. `ParsedFile::dialect`
reports `Unstated` honestly in that case rather than claiming the fallback
reader's dialect.

`ParsedFile::kind` says what the file is *for* — `Declaration`, `Migration`,
`Data`, `Mixed`, `Empty`. A corpus is mostly not declarations, and a change
script that minted an identity for the table it alters would give you two nodes
for one table.

### References a file makes outside any declaration

Most SQL is not inside a declaration. A data script, a migration, an ad-hoc
report — these declare nothing and are nothing *but* references, so there is no
entity to hang them on. `ParsedFile::references` carries them as the **file's**,
which is what they are; nothing is attached to an entity that did not make it.

```rust
use dbd_core::parser::{Dialect, parse_sql_as};

let parsed = parse_sql_as(Dialect::TSql, sql)?;
for e in &parsed.entities {
    // owned by a declaration
    for r in e.reads() {
        println!("{} reads {}", e.name, r.name);
    }
}
println!("the file itself reads {:?}", parsed.references.reads);
println!("        writes {:?}", parsed.references.writes);
println!("        calls  {:?}", parsed.references.calls);
```

### One list of references

`Entity::refs` is every reference the entity makes, each carrying three facts:

```rust
use dbd_core::entity::RefKind;
use dbd_core::parser::parse_sql;

for r in &parse_sql(sql)?.entities[0].refs {
    println!("{:?} {} (schema {:?}, unresolved {})",
        r.kind, r.name, r.schema_source, r.unresolved);
}
```

| | |
|---|---|
| `kind` | `Reads` / `Writes` / `Calls` / `Member` / `Uses`. `Uses` is a column's type or the sequence its default draws from — what makes a table depend on its enum and sequence. `Calls` and `Uses` are **soft** — an unresolved one is a built-in, not a broken edge, and is dropped |
| `schema_source` | whether the schema is the source's word or dbd's guess |
| `unresolved` | set by `resolve_references` when nothing matched |

`reads()`, `writes()`, `calls()` and `refs_of(kind)` filter it; `refers()`
yields every name once. **`refers()` omits unresolved refs and the others do
not** — a dependency graph must never wait on an entity that does not exist,
while `reads()` is the file's own account, which is what the import plan
matches a staging table against.

This replaced four parallel fields (`refers`, `references`, `reads`, `writes`)
that could not answer "which of my reads has a guessed schema?" without a join
on a key that was not unique.

### The body is not a reference

`Entity::body` holds the verbatim DDL for the types whose `CREATE` is
reconstructed from one — a view's or matview's `SELECT`, a sequence's whole
`CREATE`, one string per routine overload. It used to share the `writes` field
with table references, which meant two different things depending on whether a
parser or the introspector filled it.

Filled in by the statement-head readers (`TSql`, `MySql`). The PostgreSQL reader
leaves it empty and that is not an omission — libpg_query hands back a statement
list where a function carries its body as one node, so a reference cannot float
outside the declaration that made it.

Worth having: over a 2,154-file T-SQL corpus this is 4,059 references, and **817
of those files reported nothing at all before it existed**.

### Exposed schemas and unprotected tables

```rust
use dbd_core::Design;
use std::path::Path;

let design = Design::from_config(Path::new("design.yaml"), "prod")?;
for t in design.unprotected_exposed_tables() {
    println!("{t} is reachable from outside with no RLS policy");
}
```

`exposed_schemas()` is what something outside the database serves — PostgREST
on Supabase. Internal unless `exposed: true`, except `public` on a `supabase`
target. `unprotected_exposed_tables()` crosses that with
`policies/<schema>/<table>.sql` to find tables readable by `anon` with no
policy declared.

Distinct from `script::SUPABASE_INFRASTRUCTURE`, which is about who *owns* a
schema: ownership decides what `reconcile --prune` may drop, exposure decides
what is readable from outside.

### Translating to another engine

`dbd emit --dialect mysql|tsql|sqlite` rewrites a **PostgreSQL** project's
schema as another engine's DDL. Distinct from `combine`, which consolidates
this project's own DDL into one script.

Only PostgreSQL can be the source: it is the one dialect dbd reads into columns
and constraints (`ParserChoice::produces_structure`). Tables carry their keys,
foreign keys and `CHECK` constraints with their names (MySQL names every primary
key `PRIMARY`, which is reported), indexes, identity/`serial` columns (numbered
by the target), generated columns, defaults and comments where the target keeps
them; views and materialized views become `CREATE VIEW` (a matview becomes a
plain view, reported, and its indexes are left out). On MySQL and SQL Server a
view's query and a `CHECK` pass through untranslated and are reported; on SQLite
(3.39+) both are written again from the parse tree — flattened table names,
`instr()` for `POSITION`, NULL order and `LIKE` escape spelled out — or left out
and reported when SQLite cannot run them with the same meaning, so the script
applies. SQL Server also gets `CREATE SCHEMA` and `CREATE SEQUENCE`, and its
script ends every statement with `GO`; MySQL and SQLite report the sequences
they cannot hold. Routine and trigger bodies (a trigger in a table's file too)
do not translate and are skipped and reported; extensions, roles, grants and RLS
policies are reported as not emitted; a file that does not parse is reported as
missing. A generated expression or an index predicate passes through
untranslated and is reported. `-f` and `--report` must stay inside the project.

Anything the target cannot express is **downgraded to the nearest equivalent**,
and each real loss is reported: as a comment at the site in the emitted file, in
the run summary, and as JSON with `--report`. A faithful mapping (`integer` →
`INT`) is not reported, so the report stays worth reading. An enum column becomes
plain text on every target (reported — the values are no longer enforced). A type
past the target's documented limits is switched or clamped where the target would
refuse it, and reported — `nvarchar(max)` past 4,000 on SQL Server, `MEDIUMTEXT`/
`LONGTEXT` past 16,383 on MySQL, `numeric` clamped to 65,30 or 38 digits (SQLite reports one it may round), a
row too wide giving up its widest non-key columns (TEXT on MySQL, `nchar` →
`nvarchar` on SQL Server); MySQL gets `LONGTEXT`/`LONGBLOB` for `text`/`bytea`
and `DATETIME(6)`/`TIME(6)`. A `time` column's clock default is the time of day
(`CURRENT_TIME`, `(CURRENT_TIME(p))`, `CONVERT(time, SYSDATETIME())`). A lost
`NULLS NOT DISTINCT` (MySQL, SQLite), an EXCLUDE constraint, a column collation,
a `MATCH FULL` foreign key and — outside SQLite, which keeps it — a deferred one
are reported. Not yet reported: an identity column's `START`/`INCREMENT`
options, a `timestamptz`'s zone on MySQL (`DATETIME`) and SQLite (`TEXT`), SQL
Server's UNIQUE admitting one NULL, index keys past MySQL's 3,072 or SQL
Server's 900 bytes (not checked), the name of a foreign key named on its column,
and a `CREATE VIEW v (a, b)` column list (lost on every target). The MySQL and
SQL Server output is written from documented syntax, not run against a server.

```rust
use dbd_core::Design;
use dbd_core::emit_dialect::emit_schema;
use dbd_core::parser::Dialect;
use std::path::Path;

let design = Design::from_config(Path::new("design.yaml"), "prod")?;
let (sql, downgrades) = emit_schema(&design, Dialect::MySql, None)?;
std::fs::write("schema.mysql.sql", sql)?;
for d in &downgrades {
    println!("{} {:?}: {} -> {}", d.entity, d.column, d.from, d.to);
}
```

### The namespace context a file established

Every entity carries `schema_path` — where unqualified names in its file
resolve. PostgreSQL states it with `SET search_path TO a, b`; T-SQL and MySQL
state the *database* with `USE db`, which lands on `Entity::catalog`.

```rust
use dbd_core::parser::parse_sql;

let parsed = parse_sql(sql)?;
for e in &parsed.entities {
    for schema in e.schema_path.schemas() {
        println!("{} resolves bare names against {schema}", e.name);
    }
    if !e.schema_path.stated() {
        println!("  ...supplied by {:?}, not by the file", e.schema_path.source);
    }
}
```

`stated` is the part worth checking. A file that says nothing is not a file
that says `public`: Postgres's real default is `"$user", public`, so with a
schema named after the connecting role a bare `lookup` resolves to
`<role>.lookup`. `ALTER ROLE … SET search_path` and `ALTER DATABASE … SET
search_path` move it too, so it is not knowable from the file.

`schemas()` yields only entries that name a schema — `"$user"` is a
placeholder, kept in `entries` for a caller that has a connection, and never
used to qualify a name.

`source` names the author: `File` (the DDL said `SET search_path`), `Project`
(design.yaml's `source.search_path` is the path the file runs on), or
`SessionDefault` (nobody did, so the connection decides).

```yaml
source:
  dialect: postgresql
  search_path: [app, shared]   # for a file that states none of its own
```

Every dbd DDL file is expected to open with `SET search_path TO <schema>;` —
no emitter writes it, so it is a convention the author keeps. `search_path`
is the project's answer for the file that forgets, replacing a `public` that
was compiled into dbd and had nothing to do with any project. It is the path
such a file **runs** on, not only the one it is parsed against: `apply`,
`deploy`, `reconcile` and `combine` put `SET search_path TO <list>, public`
ahead of it (`script::runnable_ddl`), so a table lands where the parser said and
`diff` agrees. Without the key the file runs as written, on the session's path —
typically creating its table in `public`. A migration script stating no path
runs on the path its entity's CREATE runs on (the last `SET search_path` its
file states before it, else `source.search_path`), a hook on
`source.search_path`;
with neither, on the session default (`RESET search_path`) —
`script::runnable_script`, through `DatabaseAdapter::execute_project_script`.

A file that **does** state its path keeps it, and its text is never edited:
after each top-level `SET search_path` (or `SET LOCAL`) whose path lacks
`public`, `apply` and `combine` add a statement of their own setting the same
path with `public` added, so an extension installed there stays reachable. The
statements are found in libpg_query's parse tree — a function's `SET
search_path` clause, a body, a literal and a comment are never one (a regex
before 0.25 rewrote such text, breaking `SET search_path TO ''` functions).

**It is never silent.** Loading a project records a warning naming every DDL
file that stated no path and what its names were resolved against instead;
`dbd inspect` names and counts them, and `apply` and `deploy` print them before
they run (`Design::search_path_warnings`). A forgotten
`SET search_path` is an authoring mistake, and resolving it quietly is how a
reference ends up aimed at a schema nobody chose.

### Whether a schema was written or guessed

The PostgreSQL reader qualifies a bare `REFERENCES parent` with the first entry
on the entity's `search_path`, so it becomes `app.parent` — indistinguishable
from a source that wrote `app.parent`. `Reference::schema_source` and
`ForeignKey::ref_schema_source` say which it was:

| `SchemaSource` | meaning |
|---|---|
| `Stated` | the source wrote the schema, or there is none to doubt |
| `Inferred` | dbd supplied it from `search_path[0]`. **It can be wrong** |
| `Resolved` | inferred, then confirmed against the full entity set |

`is_guess()` is the usual question — true only for `Inferred`.

This matters most to a per-file consumer. `references::resolve_references`
corrects a bad guess, but it needs every entity in the scan, so a caller
reading one file at a time cannot run it and would otherwise record a guessed
schema as a confident edge. T-SQL and MySQL never infer: an unqualified name is
reported unqualified rather than guessed at.

Provenance is metadata, not schema. It is excluded from `ForeignKey`'s
`PartialEq` and never serialized with one — otherwise an inferred FK would read
as drift against an introspected one forever, and every snapshot would change
on regeneration.

Only a structured reader can be diffed: `diff`, `reconcile` and `emit` refuse
on `tsql`, `mysql` and `verbatim` projects, naming the reason. There is no MySQL
or SQL Server adapter — `mysql://`/`sqlserver://` URLs are refused by name.

### Encoding

`source_text::read_to_string` decodes before parsing — SSMS writes UTF-16LE by
default, and 16.2% of one measured SQL Server corpus was invisible to
`std::fs::read_to_string`. Use it, not the stdlib, for any file that might hold
SQL somebody else's tooling wrote. A BOM-less UTF-16 file reads as binary. Every
DDL reader in dbd uses it — the scan, `apply`, `deploy`, `combine`, `format`,
`doctor`, `merge` and the migration readers — so a UTF-16 DDL file applies like
any other. `source_text::read_with_encoding` also returns the file's `Encoding`,
and `Encoding::encode` writes text back in it — how `format` and `inspect --fix`
leave a UTF-16 file UTF-16. Import data files are read as UTF-8 only.
