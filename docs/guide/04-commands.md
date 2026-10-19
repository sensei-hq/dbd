# Commands reference

## Global options

All commands accept these options:

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--config` | `-c` | `design.yaml` | Config file name (inside the project directory) |
| `--database` | `-d` | `$DATABASE_URL` | Database connection URL |
| `--environment` | `-e` | `prod` | `dev` or `prod` (`development` and `production` are accepted and mean the same; anything else is an error) |
| `--source` | `-s` | `.` | Project directory (`dbd deploy` also takes a GitHub repo) |
| `--scope` | | (all) | Named scope from `scopes:` in design.yaml |
| `--deps` | | (scope default) | Override scope's `deps` setting: `report` or `include` |
| `--module` | | (every module) | In a [workspace](#workspaces), run this module and the modules it depends on — `reset`, `export`, `snapshot` and `release`, this module alone |
| `--verbose` | `-v` | | Show all details (entity list, full JSON) |
| `--help` | `-h` | | Print help |
| `--version` | `-V` | | Print version |

### Output modes

| Mode | Flag | What shows |
|------|------|-----------|
| Normal | (default) | Errors, warnings, progress, summary |
| Verbose | `-v` | Everything: entity details, apply order, full JSON |

Output piped into a command that stops reading early — `dbd graph | head` — ends
quietly with exit status 141, as a Unix tool does, rather than with a panic about
a closed pipe.

### `--source`

Defines the project root. Config file, DDL folder, import folder, snapshots — all relative to this.

```sh
dbd inspect                               # Current directory
dbd inspect -s ~/projects/mydb            # Explicit local path
dbd deploy -s sensei-hq/daemon/database   # GitHub repo — deploy only
```

A GitHub source — `owner/repo`, `owner/repo/path`, either with `@ref`, or a full
`https://github.com/…` URL — is accepted by **`dbd deploy` only** (see
[`dbd deploy`](#dbd-deploy)). Every other command refuses a GitHub-looking
`--source` with a message: clone the repository and pass its path.

### `--environment`

`-e` names the environment a run is for: `dev` or `prod`, with `development` and
`production` accepted as the same two. It decides which `import/<env>/` data
folders load (see [import and export](10-data.md#environment-folders)) and is
recorded in the database's bookkeeping row, where [`dbd reset`](#dbd-reset)
reads it back. The default is `prod`, so a database applied without `-e dev` is
marked prod.

**A dev run against a database marked prod is refused** — `apply`, `deploy`,
`reconcile` and `import` (`import -n X -f file` included), and their dry runs
whenever a database is configured, before anything is written. The dev run
would re-mark the database dev, which lifts reset's prod guard, and would load
`import/dev/` data into production:

```
Error: Safety guard: environment guard: this database is marked prod, but this run is -e dev.
A dev run would re-mark it dev, which lifts the prod guard on `dbd reset`, and would load import/dev/ data into it.
→ re-run with -e prod (the default). dbd never re-marks a prod database: if this is not production, a run without -e marked it prod — rebuild it: `dbd reset --force`, then run with -e dev.
```

The mark moves one way only: a dev database may be promoted to prod, which only
tightens what `reset` allows, and nothing re-marks a prod one. A run that
promotes one — any `apply`, `deploy` or `reconcile` without `-e` — says so, since
every later `-e dev` run will be refused:

```
⚠ This database was marked dev and is now marked prod: this run is -e prod, the default when -e is not given. dbd never re-marks a prod database dev, so later -e dev runs are refused — to keep it dev, rebuild it: `dbd reset --force`, then run with -e dev.
```

There is no override flag, because a flag that let a dev database through would
let a real production database through too. The case it bites is a dev database
first built without `-e`, and so marked prod; rebuild it with
`dbd reset --force`, then run with `-e dev`. The reset forgets the database's
record of the project — env, version and scope pin — when it covers everything
the database was built with: under the `all` scope, or under the scope the
database is pinned to. A bare `dbd reset --force` resolves to the design's
`default:` scope if it has one — the scope a bare apply pins — so on a database
built that way it forgets the record too; only a reset of a narrower scope
keeps it.

### Renamed projects

dbd keys its record of a database on `project.name`, and more than one project
may share a database. So a renamed `project.name` finds no record of its own.
When that happens and the database holds any of the design's tables, views,
materialized views, enums or sequences, the guards read the other projects'
records instead — the environment, version, scope and reset guards alike — and
a refusal names the project that set the mark:

```
Error: Safety guard: reset is blocked — project 'shop' marked this database prod.
This database holds this design's objects but no record of project 'shop2'; it records project 'shop'. dbd keys its record on project.name, so a renamed project.name is the likely cause — if this design was 'shop', set project.name back to 'shop'.
Use --force to override.
```

Before 0.25 a rename lifted every guard: `reset` with no `--force` dropped a
prod database's tables. Two projects sharing a database each read their own
record — except that any project's prod mark refuses an unforced `reset` — and a
project the database holds nothing of — a new one — is held to no other's.
Where the old name holds its entities in the ownership registry, the renamed
design is refused for them, the refusal naming the rename: set `project.name`
back, or take them over with `--adopt-from <old name>` (see
[sharing a database](14-sharing-a-database.md)). In a database from before the
registry, a run the old record does not refuse records the new name beside the
old one, which stays, and a renamed design ahead of the old record's version is
planned as a fresh install, so its pending migrations do not run. Set
`project.name` back before upgrading.

### Connecting

A PostgreSQL connection is tried once first, bounded at 5 seconds, so a server
that is down or a mistyped host fails fast, naming the `host:port` it tried and
never the URL, which may hold a password:

```
Error: Failed to connect to database: Config error: Database connection failed: no answer from db.example.com:5432 within 5s — is the server running, and are the host and port right?
```

A refused connection fails at once, with the server's reason after the
`host:port`. sqlx's pool used to retry for 30 seconds and then say only `pool
timed out while waiting for an open connection`.

A dry run opens only a database that exists: it reads a configured database to
check its run's guards, but a SQLite file that is not there yet is not created
— the dry run treats it as no database configured. `reconcile --dry-run`, which
needs a database to compare against, previews against an empty in-memory one
instead — what its run would create — and refuses where the run refuses.

### Scope guard

The first `apply`/`deploy`/`reconcile` **pins** a database to the scope it was built with — recorded in the `scope` column of the bookkeeping table (`dbd.meta` on Postgres/Supabase, `_dbd_meta` on SQLite). Any later `apply`, `deploy`, `reconcile`, `reset`, `import`, `export`, `refresh` or `policies` that requests a *different* scope is refused — the match is exact, so a database pinned to `all` refuses `export --scope hub`. So are `reconcile --dry-run`, and the `apply`, `deploy`, `import`, `policies` and `reset` dry runs whenever a database is configured, so a preview never shows a run that would be refused. A renamed project is held to the pin its old name left (see [renamed projects](#renamed-projects)):

```
Error: Safety guard: scope guard: this database is pinned to scope 'public', but you requested 'internal'.
Applying a different scope would build a divergent schema.
→ re-run with --scope public, or pass --allow-scope-change to re-point this database to 'internal'.
```

This stops a mistyped or forgotten `--scope` from building a half-formed, divergent schema in the wrong database — or from loading, reading, refreshing or securing tables that database was never given. To legitimately re-point a database to a new scope, pass **`--allow-scope-change`** on `apply`, `deploy` or `reconcile` — the run then re-pins the database to the new scope. On `reset`, the flag (like `--force`) only bypasses the check; a reset does not re-pin. `import`, `export`, `refresh` and `policies` have no bypass: none of them re-pins, so the way through is the pinned scope, or re-pointing the database with `dbd apply --scope <new> --allow-scope-change` first, and their message says so:

```
Error: scope guard: this database is pinned to scope 'hub', but you requested 'all'.
Importing under another scope would load tables this database was not built with.
→ re-run with --scope hub, or re-point the database with `dbd apply --scope all --allow-scope-change` first.
```

`diff` and `migrate --status` write nothing, so they note a pin rather than
refuse it — an unscoped `diff` of a database pinned to `hub` would otherwise
list everything `hub` does not build as missing, with no word of why:

```
note: this database is pinned to scope 'hub'; this diff compares scope 'all', so what 'hub' does not build reads as missing — `--scope hub` compares what it was built with.
```

To host multiple modules in one database, define a scope in `design.yaml` that includes them rather than switching back and forth. Databases created before this feature are unpinned and pin on their next write.

Which commands filter by `--scope`, which also refuse on a dependency gap, and
which ignore it is set out in [scopes](07-scopes.md#which-commands-honour-scope).

### Workspaces

A design.yaml that declares `modules:` is a **workspace**: each module is a design
of its own, in its own folder, recorded in the database as its own owner,
`<project>/<module>` (see [design.yaml](03-design-yaml.md) for the `modules:`
block, and [scopes](07-scopes.md) for scopes that select modules). Every command
runs one but `init`, `merge` and `split` (`install` reads no project).

**Module by module.** A run goes over the modules in `depends_on` order — of
those whose dependencies are in place, the one `modules:` lists first — each
connecting as its own owner, under a header naming it:

```
$ dbd inspect
module shared (shop/shared)
Everything looks ok

1 entities — no issues

module core (shop/core)
Everything looks ok

1 entities — no issues
```

Each module keeps its own record in the database — its env, version and scope
pin — and its own claims, and the guards read its own. A run over more than one
module checks every module's run as its dry run checks it — scope pin, guards,
claims, migrations, hook files, imports — before the first module writes, so a
refusal any module would meet comes first. A statement the database refuses
past those checks still stops the run with the modules before it applied, as
one project's run stops with its earlier statements committed. The manifest's
own `apply.before` runs once, before the first module's own hooks; its
`apply.after` and `import.after` run once, after the last module's.

**`--module <name>`** narrows the run to one module and the modules it depends
on — `reset`, `export`, `snapshot` and `release` take the module alone: a reset never drops a
dependency's entities, which are another owner's, and an export only reads. An
unknown name is refused naming the modules, and on a design without `modules:`
the flag is refused:

```
Error: Config error: no module "nope" — design.yaml's modules: are shared, core
Error: --module core: ./design.yaml declares no modules:
```

Under `--scope`, `--module` must be one of the scope's modules. With no
`--scope`, a module asked for — `--module`, or the folder dbd runs in — that the
`default` scope leaves out still runs, with its dependencies, each resolving its
own `default`, else `all`.

**`-n` in a workspace** runs the module declaring that entity, alone, with the
manifest's hooks around it. One `--module` leaves out is refused, naming its
module: `app.orders is module "core"'s, and --module shared does not select it`.

**Inside a module's folder.** Run in a module's folder — one with no
design.yaml of its own — dbd walks up, along the path the run was started in,
to the nearest design.yaml above it and, if that one declares `modules:`, runs
that module, as
`--module` would, unless `--module` says otherwise; `-s <module folder>` does
the same. Run in a folder of the workspace that no module holds, it runs the
workspace. `init` and `install` do not walk up, and a design without modules is
never looked for above.

**`init` and `merge`** onboard a project into one design, so they refuse a
workspace and say where one comes from:

```
Error: ./design.yaml declares modules: — init and merge onboard a project into one design; a project becomes a workspace with dbd split --from-scopes
```

A project becomes a workspace with [`dbd split`](#dbd-split); each database's
first deploy afterwards hands each module what it declares.

---

## `dbd inspect`

Validate project configuration and report errors/warnings.

```sh
dbd inspect                       # Validate all entities
dbd inspect -n config.lookups     # Inspect one entity
dbd inspect -v                    # Verbose: show entity JSON
dbd inspect --fix                 # Auto-fix DDL formatting (runs the formatter in place)
dbd inspect --scope hub           # Validate scope + report dependency gaps
dbd inspect --scope hub --fix     # Format only the files the 'hub' scope builds
dbd inspect --from-db -d $DATABASE_URL    # Resolve references against the live catalog
dbd inspect --shared -d $DATABASE_URL     # What shares the database, and this design's conflicts there
```

**`-n`** must name an entity the design has — `no entity named 'X'` otherwise — and, under
`--scope`, one the scope builds (`X is outside scope 'S'`). An `external:` name is refused too, as
`apply -n` refuses it: dbd has no DDL for it, so there is nothing to inspect. `apply`, `graph`,
`export` and `refresh` refuse a `-n` the same way, rather than succeed having selected nothing. The count
and the advisory checks then cover that entity alone (`1 entities — no issues`), not the
whole design.

**`--shared`** reports what shares the database: each project it records — env and version, what
it owns, the schemas it uses and what of other projects' it uses — which projects have not yet run a
dbd that records what they own, and what would refuse this design there: an entity another project
owns, or an owner name it reuses. It exits non-zero over any such conflict. It reports sharing
only: it does not validate the design, and reads no `--scope`. In a
[workspace](#workspaces) it lists what shares the database once, then each
module's conflicts under its header. See
[sharing a database](14-sharing-a-database.md#seeing-what-shares-a-database).

**`--from-db`** resolves "Unresolved reference" warnings against the live database catalog (tables, views, enums), using the `-d`/`--database` connection — useful when DDL references objects created outside the project. The resolved catalog is cached to `<project>/.dbd/refcache.json`, so subsequent **offline** `inspect` runs consult the cache and stay quiet without a connection.

**RLS report (advisory).** For every schema that is [exposed](03-design-yaml.md#exposed) — `exposed: true`, or `public` on a `supabase` target — `inspect` lists the tables with no `policies/<schema>/<table>.sql`. It never affects the exit code. See [access control](09-access-control.md#exposed-schemas).

**Suggestions (advisory).** On a Postgres/Supabase project, `inspect` prints a `Suggestions:` section when it finds a `CHECK` constraint that pins a column to a fixed set of string literals (`status IN ('active','inactive')`, `= ANY(ARRAY[…])`, or an `OR`-chain of `col = '…'`) — a Postgres `enum` (`ddl/enum/<schema>/<name>.ddl`) models that with type safety and cleaner introspection. Suggestions are **advisory only**: they never count as errors and never affect the exit code. Columns sharing a value set become one proposal. Where the plain column name would name two different files, the type is named `<table>_<column>` instead (and `_2`, `_3` if that still collides) — the footer says how many were renamed and why. An identifier that is not a single ordinary path segment gets no path at all, just the candidate; likewise values needing escaping are printed unquoted and flagged, since escaped text is not the literal it came from.

### Exit code

**Changed in v0.12.0.** `inspect` exits **1** when the design carries a blocking
error, and 0 otherwise. It previously always exited 0, which meant a CI step
running `dbd inspect` passed green on a file that `dbd apply` would refuse —
and apply's own error says *"run `dbd inspect` for the full report"*, pointing
at the command that had just passed.

If you have a pipeline that runs `dbd inspect` for its output only, it will now
fail on a broken design. That is the intent; it is a breaking change, hence the
minor bump.

Warnings and suggestions never affect the exit code.

### Parse errors say what they cost

A file dbd cannot parse is dropped from the desired set, so a run would build a
database missing that object. `apply`, `reconcile` and `deploy` refuse rather
than do that, and `inspect` now names the consequence with the error:

```
Errors:

./ddl/table/app/broken.ddl =>
  Parse error: Invalid statement: syntax error at or near ","

  → dbd apply / reconcile / deploy will refuse to run until these are fixed.
```

### Errors under `--scope`

A scoped run only builds the entities in its working set, so only those errors
block it. `inspect --scope` splits its report the same way, using the same rule
the run itself uses — so `inspect` and `apply` never disagree about what is
broken:

```
$ dbd inspect --scope daemon
scope 'daemon': 2 of 5 entities

Out of scope — not blocking 'daemon':

./ddl/table/svc/broken.ddl =>
  Parse error: Invalid statement: syntax error at or near ","

  → these files are not built by 'daemon', so it can run. They will block a run whose scope includes them.

2 entities — no issues
(1 error(s) outside scope 'daemon')
```

Warnings are split the same way: one about a file outside the scope is counted on
its own line, `(N warning(s) outside scope 'daemon')`, rather than listed as the
scope's to fix. A file that does not parse draws no `SET search_path` warning —
the line that failed may be that very statement — so its parse error is its only
finding.

Exit code is **0** here, matching `apply --scope daemon`, which applies cleanly.
Move the same broken file into a schema the scope *does* build and it becomes
`Errors (blocking scope 'daemon')` with exit 1.

Out-of-scope errors are reported, never suppressed: a file that no scope builds
is exactly how a broken file survives unnoticed. The entity count in the summary
is scope-aware — the first number of the `scope '…': N of M entities` line,
which counts the working set (the `deps: include` closure too) against every
entity the design declares, externals left out of both. Every command that
announces a scope counts it this way.

The advisory checks — materialized-view refresh config, enum suggestions, the
RLS report — run over the scope too, not the whole design. A `deps: include`
scope whose closure needs something the scope excludes is refused, as `apply`,
`dbml` and `diagram` refuse it.

---

## `dbd apply`

Apply DDL scripts to the database in dependency order.

```sh
dbd apply                          # Apply all entities
dbd apply -n config.lookups        # Apply one entity
dbd apply --dry-run                # Print apply order — offline, or checked against a configured database
dbd apply -v                       # Show entity list then execute
dbd apply -d postgres://...        # Explicit database URL
dbd apply --adopt-from billing     # Take over what the design declares that project 'billing' owns
dbd apply --allow-ownership-change # Run as this project's name although its entities here are all undeclared
```

**Beside other projects.** In a database other projects deploy into, `apply` claims every entity its
whole design declares, and is refused — before its first hook or statement, dry run included — for one
another project owns, or under an owner name whose entities it declares none of. `--adopt-from
<owner>` takes over what the design declares that `<owner>` holds, naming each; `--allow-ownership-change`
says this design is that owner now. A pending migration that drops what another project depends on
is refused, and an apply running migrations waits for the other projects' runs to finish. See
[sharing a database](14-sharing-a-database.md).

**Apply order** is one dependency sort across every entity type. Each type has
a band it is never placed before — schemas, extensions, roles, sequences,
enums, tables, views, materialized views, then functions and procedures — and
a real dependency can move an entity later than its band: a view that calls a
function is applied after the function, and a function whose body reads a view
after the view. Entities in a dependency cycle get an error and go last.

**`--dry-run`** prints the order, after the `scope 'X': N of M entities` line
under a scope. It runs the real apply's gates — a file that does not parse, a
dependency gap or closure conflict, a `-n` that names nothing, a hook file that
is missing — and refuses where `apply` would, and it lists the extensions the
scope's `extensions:` allowlist admits, not every one. Under a scope it also
names each hook the run would skip (see
[Hook scripts](03-design-yaml.md#hook-scripts)), with the same
`⚠ after-script … skipped — needs …` line the real run prints before it starts.

With a database configured — `-d`, `$DATABASE_URL`, or a `target.url` that
resolves — the dry run asks it the run's questions before it lists anything. It
refuses a database [pinned](#scope-guard) to another scope, a dev run on a
database marked prod ([`--environment`](#global-options)), a database newer than
the project (below), a named apply over pending migrations, and a pending
migration with an unresolved `data.sql` TODO. Then it lists the pending
migrations the run executes before the entities, in the order the run takes
them, each statement that destroys something marked `⚠`:

```
Pending migrations, v1 → v3 — the run executes these first:
  v2 migrate app.a
      ALTER TABLE app.a ADD COLUMN extra int
  v2 migrate data of app.a
      UPDATE app.a SET extra = 0
  v2 drop app.b
      ⚠ DROP TABLE app.b CASCADE
  v3 add values to app.mood (committed before the rest of the version)
      ALTER TYPE app.mood ADD VALUE IF NOT EXISTS 'archived'
  v3 migrate app.a
      ALTER TABLE app.a ALTER COLUMN mood SET DEFAULT 'archived'
```

A drop of a table this database does not have is listed as
`— not in this database, so skipped`, as the run skips it, and with nothing
pending the line is `No pending migration to run (database at v2, design at
v2).` A scoped run whose pending migrations touch nothing in its scope says the
run records them anyway — `nothing in them runs in this scope; the run records
them and moves the database to v7.` A configured database that cannot be
reached fails the preview rather than falling back to one; a SQLite file that
does not exist yet is not created. With none configured the dry run stays
offline, and in a project with migrations says that it cannot list them without
the version the database records.

**Pending migrations run version by version.** Each pending version runs whole
before the next — its enum value additions, its creates and schema changes, its
data scripts, its drops, then the record that it ran — and afterwards every
entity's DDL is re-applied; see
[how a migration run is ordered](05-snapshots-migrations.md#how-a-migration-run-is-ordered).

**A named apply is refused while migrations are pending.** `apply -n X` builds
one entity and cannot run the rest of a version, so with migrations pending it
stops before any write, `apply.before` included, and its dry run refuses the
same way:

```
Error: Config error: apply -n app.people refused: the database is at v7 and migration v8 is pending. A named apply builds one entity and cannot run it, and would record it as applied. Run `dbd apply` first to apply it, then `dbd apply -n app.people`.
```

Before 0.25 it ran the named table's pending changes and every version's drops,
skipped the rest, and recorded the versions as applied.

**Files with no `SET search_path` are named** before the run and in its
preview — each DDL file the run executes that states no path, with what its
names were resolved against (see
[`source.search_path`](03-design-yaml.md#search_path)).

**An older project is refused on a newer database.** A database records the
version it was built to, and a project behind it — a branch checked out from
before the latest snapshot — would re-apply its older DDL over the newer
schema, which nothing short of a newer revision makes right. `apply`, `deploy`
and their dry runs stop before anything runs, `apply.before` included. There is
no override — except for a design that `--allow-ownership-change` lets take over
a reused owner name, whose recorded version is the previous design's (see
[sharing a database](14-sharing-a-database.md#another-project-under-the-same-name)):

```
Error: Safety guard: version guard: the database is at v3, but this project is at v2.
Applying an older project would re-apply its DDL over a newer schema.
→ check out a newer revision of the project (at v3 or later), or point it at a database built from this one.
```

A project with no `project.version` counts as v0, so it is refused on any
versioned database (`… but this project records no version.`). Before 0.25 the
run said `Already up to date (v3)` and re-applied the older DDL. A dev run on a
database marked prod is refused too — see [`--environment`](#global-options).

**One transaction.** On PostgreSQL the entities, pending migrations and version
stamp run in a single transaction, so an interrupted upgrade rolls back to the
schema it started from. The exception is a version that adds enum values:
PostgreSQL will not use a value in the transaction that added it, so the run
commits after each such version's `ADD VALUE` statements and carries on in a new
transaction — a failure after that point keeps the values and the earlier
versions. `apply.before` runs before it — outside it, so a batch
that rolls back keeps whatever `apply.before` did; matview stamping, pg_cron
sync and `apply.after` run once it has committed. DDL that cannot run inside a
transaction (`CREATE INDEX CONCURRENTLY`) fails there — set `DBD_NO_TX` to any
value to apply without it. SQLite applies without a transaction.

**Grants.** After the schema — entities, migrations and `apply.after` — `dbd apply`
applies the `grants:` configured on schema entries and on the target (see
[access control](09-access-control.md#grants)); under `--scope`, only on the
schemas the scope builds. `--dry-run` applies no grants and runs no hook
scripts.

**Policies** are not applied unless you pass `--with-policies`; `dbd deploy`
applies them always.

**Materialized views.** After entities apply, `dbd apply` (and `dbd deploy`) records a `dbd:hash` drift stamp for any matview it just created — and for one the database already had with no stamp, when its deployed query and index keys are the design's, so a brownfield project (`init --from-db`) diffs clean; one whose definition differs stays unstamped, since a stamp would vouch for a definition the design does not have. The stamp lives in `dbd.matview_stamps`, not in the matview's comment, which stays the author's own `COMMENT ON MATERIALIZED VIEW` (dbd before 0.25 kept the stamp *as* the comment; on upgrade the first `apply`, `deploy`, `reconcile` or `reset` copies it into `dbd.matview_stamps` before the file's own `COMMENT ON` can erase it, and leaves the comment for you to replace). The run also **syncs pg_cron refresh jobs** to match the `materialized_views:` config (jobs named `dbd-refresh:<project>:<schema>.<name>`, the project's own only; requires the `pg_cron` extension when a schedule is set). Matview DDL should be idempotent (`create materialized view if not exists …` + `create [unique] index if not exists …`) so a re-`apply` is safe.

**Scope guard.** `apply` (like `deploy`/`reconcile`) refuses to run under a different scope than the database is pinned to, and pins the scope on every successful run. Pass `--allow-scope-change` to re-point the database — see [Scope guard](#scope-guard).

**In a workspace** each module applies in turn, as its own owner, every module's
run checked before the first writes (see [workspaces](#workspaces)), and
`apply -n` applies the module declaring the entity. A module's migrations are
its own: its `snapshots/` and `migrations/` are in its folder, and its version
is `modules.<name>.version`.

---

## `dbd refresh`

Refresh materialized views now — `REFRESH MATERIALIZED VIEW [CONCURRENTLY]`, honoring each matview's resolved `concurrently` setting.

```sh
dbd refresh                        # Refresh every materialized view
dbd refresh -n analytics.daily_sales   # Refresh one matview
dbd refresh -n analytics.*         # Refresh all matviews in the analytics schema
dbd refresh --scope hub            # Only matviews the 'hub' scope builds
```

Useful for the initial populate after a data load, or in CI — independent of the pg_cron schedule. Scheduled (recurring) refresh is managed in-database via pg_cron and configured under `materialized_views:` in design.yaml (see [design.yaml reference](03-design-yaml.md#materialized_views)). PostgreSQL/Supabase only.

**`--scope` (honored from v0.12.0).** A matview the scope excludes was never built on that plane, so refreshing it failed with `schema "…" does not exist`. `refresh` now filters to the scope's working set like every other entity-selecting command, and announces the narrowing (`scope 'hub': N of M entities`). Previously the flag was accepted and ignored.

A `-n` that selects nothing is an error: `no entity named 'X'`, `X is outside scope 'S'`, `X is not a materialized view`, or, for `<schema>.*`, no matview matching it or every one outside the scope.

`refresh` refuses a database [pinned](#scope-guard) to another scope — `Refreshing another scope's materialized views would read tables this database was not built with.` — and, since it never re-pins, takes no `--allow-scope-change`.

---

## `dbd import`

Load staging data from CSV, TSV, JSON and JSONL files.

```sh
dbd import                         # Import all tables + call procedures
dbd import -n staging.lookups      # Import one table (from the import/ folder)
dbd import -n activity.hook_events -f ~/data/events.jsonl   # load a specific file into a table
dbd import --dry-run               # Show import plan (offline, or checked against a configured database)
dbd import -e dev                  # Load dev-only data
```

**`-f`/`--file`** loads one explicit file into the table named by `-n` (required with `-f`),
instead of the `import/<schema>/<table>.<ext>` convention. The format is inferred from the file
extension — `.jsonl`, `.json` (a JSON array of records, or JSON Lines), `.tsv`, and CSV for
anything else. It loads that file and nothing else — no truncate, no procedures, no
`import.after`. Under `--scope`, a table the design declares must be one the scope builds
(`X is outside scope 'S'` otherwise); a table it does not declare loads, with a warning that the
scope was not applied to it. `import.staging` holds `-f` to its list like every other load. (On
`import`, `-f` is the *file*; on `export` it's the *format* — each is unambiguous within its
command.) It prints the scope line as every scoped run does, and refuses a database pinned to
another scope, or marked prod under `-e dev` (the [environment guard](#global-options)), like
the full import. Before 0.25 it skipped the environment guard, so `-e dev import -n X -f file`
loaded a dev file into production.

**`-n`** without `-f` must select a data file this run loads, or the import is refused before
anything runs — `no entity named 'X'`, `X has no data file under import/ — nothing to import`, or,
when the file is under another environment's folder, `no data file for X in this run:
import/dev/staging/x.csv belongs to another env, and this run's env is 'prod'`. It used to be a
warning, and the run went on to `import.after`.

**`import.staging`**, when it lists schemas, is the set an import may load — a load truncates its
table first. A data file whose target schema is not listed is refused, every such file named in
one error, before anything is truncated: on `import`, `import --dry-run`, `deploy` (before it
applies anything) and `deploy --dry-run`. An empty or absent list means no restriction.

**CSV and TSV** are read as RFC 4180, the way PostgreSQL's `COPY … (FORMAT csv)` reads them, on
PostgreSQL and SQLite alike: a quoted field may hold the delimiter, a line break and a doubled
quote; an unquoted field equal to `null_value` (by default, an empty one) is NULL, and a quoted
field never is, so `""` is the empty string; nothing is trimmed; CRLF line ends and a byte-order
mark are handled. The header row names the columns the values load into, matched as PostgreSQL
resolves names — exactly, or folded to lower case — so the file's column order need not be the
table's. A header naming a column the table does not have, or one column twice, is refused: that
file loads nothing and the import stops. Since nothing is trimmed, a header written `id, name`
names a column ` name` and is refused too.

**JSON and JSON Lines** records name their columns by key, so a key that names no column is
refused as a header is — it was dropped silently before 0.25. A file may not set a generated
column, by header or key: the table computes it, and `dbd export` never writes one. Ids load as
given, `GENERATED ALWAYS` identities included, and on PostgreSQL each identity or `serial`
sequence is then moved past the largest id loaded, so the next insert does not reuse one — a
`setval`, for which the importing role needs `UPDATE` on the sequence. See
[import and export](10-data.md#what-a-data-file-cannot-set).

In a JSON record, `null` is SQL NULL for every column type. A json/jsonb column holding the JSON
value `null` is written `{"$json": null}` (as `dbd export` writes it), and any one-key `{"$json":
…}` object is unwrapped one level, on PostgreSQL and SQLite alike. A column whose key no record
carries takes its DEFAULT; on PostgreSQL a key missing from only some records is NULL in those,
while SQLite gives each record that leaves a key out the DEFAULT.

**Truncation.** Before loading anything, the import empties every table it reloads (unless
`truncate: false`) — on PostgreSQL in one `TRUNCATE` of them all, since PostgreSQL empties tables
linked by a foreign key only together, and loads a referenced table before one that references
it. A table outside the import that holds a key to one of them fails the import, named, with
nothing emptied (`held by app.orders (orders_customer_fk, a key to staging.customers), outside the
tables this import reloads`). On SQLite, whose DDL dbd reads verbatim and so knows no references
from, the order comes from the foreign keys the database reports: a referenced table is loaded
first, and the tables are emptied one at a time, a referencing table first. `import --dry-run`
lists that order when the database exists; with no database to ask it lists the tables
alphabetically.

**`import.after`** scripts run after the procedures, on every `dbd import` (with or without
`-n`) and on `dbd deploy` — not on `dbd apply`, and not with `-f`. `--dry-run` lists them, and
under a scope names each one the run would skip, with the line the run prints.

The full guide is [import and export](10-data.md).

**Dry-run output shows the full plan:**
```
  import staging.lookups (csv) ← ./import/staging/lookups.csv

  call staging.import_lookups()
1 entities — no issues
```

**Which procedure loads a table.** Each staging table gets exactly one loader, chosen from the procedures whose body *reads* it — by reads/writes analysis, not by text matching. In order: the procedure named `import_<table>`; else the only one that reads no other staging table; else the only one no other table claimed, preferring one that writes a table of the same name. A procedure loads at most one table and is called once per import, however many data files that table has. The choice is made over every data file, so `import -n X` calls the loader a full run would. Two candidates still equal at the deciding rule is refused before anything runs, naming them — `cannot tell which procedure loads staging.x: … name it import_x, and call the others from it or from import.after`. Only a procedure is a loader — the step is `CALL`, which PostgreSQL refuses for a function — so a function is never run. A function that reads a staging table no procedure loads, and a procedure that reads a staging table without being its loader, are each named in a warning.

**How reads/writes are derived.** Function and procedure bodies are parsed, not text-matched. `LANGUAGE sql` bodies are parsed as an AST; PL/pgSQL bodies are parsed by libpg_query (PostgreSQL's own parser), which sees through `SELECT … INTO`, `PERFORM`, `FOR … IN … LOOP`, `RETURN QUERY`, and `IF` conditions. Either way, reads and writes are detected precisely through sub-selects, joins, set operations (`UNION`/`INTERSECT`/`EXCEPT`), and CTEs, and a write target (`INSERT`/`UPDATE`/`DELETE`) is distinguished from the tables it reads. A best-effort regex scan is used only if a body can't be parsed at all.

**Dynamic SQL is not tracked.** Statements built and run as strings (`EXECUTE format(...)`, `EXECUTE 'INSERT ...'`) are deliberately ignored. They create only a *runtime* dependency — they never determine whether the function can be created — so they are out of scope for dependency ordering. Tables referenced only inside dynamic SQL will not appear in the reads/writes analysis; reference them statically if you need them ordered.

**Environment-specific data.** Files placed under `import/<env>/<schema>/<file>` are loaded only when `-e <env>` matches; files directly under `import/<schema>/` load in every environment. So `import/dev/staging/seed.csv` loads on `dbd import -e dev` but not under `prod`, letting you ship dev seed data without it reaching production. `dev` and `prod` at the top of `import/` are always environment folders, so a file directly in `import/dev/` — where a SQLite project, with no schemas, keeps its dev data — loads only under `-e dev`, on every engine (before 0.25 SQLite loaded it in prod). And a dev import into a database marked prod is refused outright: see [`--environment`](#global-options).

**Nothing is skipped silently.** The summary always states the table count — tables, not data files, so a table with a shared file and an environment's own counts once — including `0`, and any file that did not load is reported with its reason:

| Reason | Warning |
| --- | --- |
| No `import/` directory | `no import/ directory — nothing to import` |
| File belongs to another env | `N data file(s) under import/<env>/ skipped — this run's env is '<env>'` |
| File in a folder no import reads (`import/staging/sub/x.csv`) | `import/staging/sub/x.csv not loaded — an import reads import/<schema>/<table>.<ext>, or import/<env>/<schema>/<table>.<ext> for env dev or prod, and no folder deeper` |
| Staging file failed to parse | `staging table X not imported — data file failed to parse` |
| Entry cut by `--scope` | `X not imported — T is outside scope '<name>'`, naming the staging table or target that is |
| A function reads a staging table no procedure loads | `X has no loader procedure — function F reads it but is not run` |
| A procedure reads a staging table it does not load | `P reads X but is not called — L is its loader` |

`import --dry-run` prints the same warnings. This is what makes the wrong-`-e` trap visible: a run that seeds no rows now says so instead of reporting a clean, empty success. (Before 0.25 a file in a deeper folder was reported as another env's, as though some `-e` could load it — or, under an env folder, loaded with its extra folder ignored: `import/dev/staging/sub/y.csv` into `staging.y`.) What the run cannot do safely is refused instead, before anything is truncated: a `-n` that selects no data file, a data file outside `import.staging`, a data file whose target the design declares as a view, materialized view or sequence, a staging table with two equal claims to its loader, and — under `--scope` — a staging table the scope keeps whose loader procedure it leaves out, since a database built with that scope has no such procedure:

```
import refused: scope 'imp' leaves out the loader of these staging tables, so a database built with it has no such procedure:
  staging.items → call staging.import_items()
Add each loader to the scope, or leave its staging table out of the scope too.
```

The real run also refuses the wrong database before it truncates: one marked prod under `-e dev`, and one [pinned](#scope-guard) to another scope. `--dry-run` checks both whenever a database is configured, and stays offline when none is.

---

## `dbd emit`

Translate the schema into another engine's DDL.

```sh
dbd emit --dialect mysql -f schema.mysql.sql
dbd emit --dialect tsql -f schema.tsql.sql --report downgrades.json
```

| Flag | Meaning |
|---|---|
| `--dialect` | Target engine: `mysql`, `tsql` or `sqlite` |
| `-f`, `--file` | Destination (default `schema.sql`) |
| `--report` | Also write every downgrade as JSON |

Both `-f` and `--report` must resolve inside the project, as `dbml` and `diagram`
outputs do; a bare file name resolves against the project root, wherever you
run from. A path outside is refused (`path traversal rejected: … is outside
project root`) rather than overwriting a file dbd does not own — write inside
the project and move the file.

**`emit` is not `combine`.** `combine` consolidates this project's own DDL into
one script for its own target; `emit` rewrites the schema for a *different*
engine.

The source must be a **PostgreSQL** project — it is the only dialect dbd reads
into columns and constraints. Tables come with their keys, foreign keys,
`CHECK` constraints, indexes, identity and `serial` columns (numbered by the
target), generated columns (computed by it), defaults and comments — each key
with its name (`CONSTRAINT [people_amt_ck] CHECK (…)`): a primary key on SQL
Server and SQLite (MySQL names every primary key `PRIMARY`, so the name is
reported), a UNIQUE, a foreign key and a `CHECK` — a key named on its column
(`id int constraint t_pk primary key`) included, though a column-level
`REFERENCES` loses its name; views and
materialized views as `CREATE VIEW` — a materialized view becomes a plain view,
reported. On MySQL and SQL Server a view's query and a `CHECK` expression are
passed through untranslated and reported. On SQLite both are written again from
PostgreSQL's parse tree — `core.cities` reads the flattened `core_cities`,
`POSITION('a' IN name)` becomes `instr(name, 'a')`, and PostgreSQL's column
names, `NULLS FIRST`/`LAST` order and `LIKE` escape are spelled out — and a
construct SQLite cannot run with the same meaning (an `array_agg()`, say) leaves
the view or `CHECK` out, reported, along with anything that reads it, so the
script still applies; that output needs SQLite 3.39 or later. T-SQL also gets
`CREATE SCHEMA` and `CREATE SEQUENCE`; MySQL and SQLite, which have neither,
report each sequence. Functions, procedures and triggers — a trigger written in
a table's file included — are skipped and reported, because their bodies do not
translate. Extensions, roles, grants (one entry per schema) and RLS policies
(one per policy file) are reported as not emitted. A generated column's
expression and an index predicate are passed through untranslated and reported;
an index the target would reject is left out and reported. A file dbd could not
parse is reported as missing from the schema. `--scope` limits the output to the
scope's entities.

### Downgrades

A construct the target cannot express is emitted as the nearest thing it can,
so the output is always a complete schema. Each loss marked ⚠ below is reported
— inline as a comment at the site, in the run summary, and as JSON with
`--report`:

| PostgreSQL | MySQL | T-SQL | SQLite |
|---|---|---|---|
| `text`, and `varchar` with no length | `LONGTEXT` | `nvarchar(max)` | `TEXT` |
| `varchar(n)` | `VARCHAR(n)`; past 16,383, `MEDIUMTEXT`/`LONGTEXT` ⚠ | `nvarchar(n)`; past 4,000, `nvarchar(max)` ⚠ | `TEXT` |
| `char(n)` | `CHAR(n)`; past 255, `VARCHAR(n)` ⚠; past 16,383, text ⚠ | `nchar(n)`; past 4,000, `nvarchar(max)` ⚠ | `TEXT` |
| `bytea` | `LONGBLOB` | `varbinary(max)` | `BLOB` |
| `numeric(p,s)` | `DECIMAL(p,s)`, at most 65 digits with 30 after the point ⚠ | `decimal(p,s)`, at most 38 digits ⚠ | `NUMERIC` (⚠ past 15 significant digits with a fraction, or 18 whole) |
| `numeric` with no precision | `DECIMAL(65,30)` ⚠ | `decimal(38,10)` ⚠ | `NUMERIC` ⚠ |
| `text[]` | `JSON` ⚠ | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| `jsonb` | `JSON` | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| `json` | `JSON` ⚠ (MySQL normalises the document) | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| `uuid` | `CHAR(36)` ⚠ | `uniqueidentifier` | `TEXT` ⚠ |
| `timestamptz` | `DATETIME(6)` | `datetimeoffset` | `TEXT` |
| `real` / `float4` | `FLOAT` | `real` | `REAL` |
| `time` | `TIME(6)` | `time` | `TEXT` |
| `timetz` | `TIME(6)` ⚠ | `time` ⚠ | `TEXT` |
| `interval` | `VARCHAR(255)` ⚠ | `nvarchar(255)` ⚠ | `TEXT` ⚠ |
| `inet`, `cidr` | `VARCHAR(49)` ⚠ | `nvarchar(49)` ⚠ | `TEXT` ⚠ |
| an enum type | `VARCHAR(255)` ⚠ | `nvarchar(255)` ⚠ | `TEXT` ⚠ |
| a view's query, a `CHECK` | untranslated ⚠ | untranslated ⚠ | rewritten from the parse tree ⚠, or left out ⚠ |
| schemas | folded into the name ⚠ | native, with `CREATE SCHEMA` | folded into the name ⚠ |
| identity, `serial` | `AUTO_INCREMENT` (⚠ for `ALWAYS`) | `IDENTITY(1,1)` (⚠ for `BY DEFAULT`, `serial`) | `INTEGER PRIMARY KEY AUTOINCREMENT` (⚠ for `ALWAYS`) |
| a sequence | nothing ⚠ | `CREATE SEQUENCE`, PostgreSQL's bounds | nothing ⚠ |
| `DEFAULT nextval('s')` | the table's counter on its key ⚠, else none ⚠ | `NEXT VALUE FOR` | the table's counter on its key ⚠, else none ⚠ |
| a generated column | stored, expression untranslated ⚠ | persisted, expression untranslated ⚠ | stored, expression untranslated ⚠ |
| a materialized view | a view ⚠, indexes left out ⚠ | a view ⚠, indexes left out ⚠ | a view ⚠, indexes left out ⚠ |
| comments | table and column `COMMENT`; a view's ⚠ | `MS_Description` | dropped ⚠ |
| `text` or unbounded `varchar` in a key | `VARCHAR(255)` ⚠ | `nvarchar(450)` ⚠ | `TEXT` |
| a row wider than the target stores | the widest non-key `VARCHAR`/`CHAR` → text ⚠ (65,535 bytes) | the widest `nchar` → `nvarchar` ⚠ (8,060 bytes) | — |
| defaults | literal without its `::cast`; `now()` → `CURRENT_TIMESTAMP(n)` at the column's precision, `current_date`; `gen_random_uuid()` → `(UUID())` ⚠; any other left out ⚠ | literal; `now()`, `current_date`, `gen_random_uuid()` → `NEWID()`; any other left out ⚠ | literal; `now()`, `current_date`, `gen_random_uuid()`; any other left out ⚠ |
| a clock default on a `time` column (`now()`, `current_time`, `localtime`) | `(CURRENT_TIME(n))` | `CONVERT(time, SYSDATETIME())` | `CURRENT_TIME` |
| a primary key's name | `PRIMARY` ⚠ | kept | kept |
| `UNIQUE NULLS NOT DISTINCT` | `UNIQUE` ⚠ | `UNIQUE` (SQL Server's treats NULLs as equal) | `UNIQUE` ⚠ |
| a foreign key's `DEFERRABLE INITIALLY DEFERRED` | checked at once ⚠ | checked at once ⚠ | kept |
| a foreign key's `MATCH FULL` | `MATCH SIMPLE` ⚠ | `MATCH SIMPLE` ⚠ | `MATCH SIMPLE` ⚠ |
| an `EXCLUDE` constraint | left out ⚠ | left out ⚠ | left out ⚠ |
| a column's `COLLATE` | the target's default ⚠ | the target's default ⚠ | the target's default ⚠ |
| a foreign key to an `external:` or out-of-scope table | left out ⚠ | left out ⚠ | left out ⚠ |

⚠ = a real loss of meaning, and therefore reported. A faithful mapping is not
reported — a report padded with `integer → INT` is one nobody reads. The limits
are each target's documented ones — MySQL's `VARCHAR` holds 16,383 four-byte
characters, SQL Server's `nvarchar(n)` 4,000, `DECIMAL` 65 digits on MySQL and
38 on SQL Server — and a type past one is switched or clamped, and reported,
where the target would otherwise refuse the statement. `text` and `bytea` are
`LONGTEXT` and `LONGBLOB` on MySQL because its `TEXT` and `BLOB` hold only
64 KB, and date-times keep fractional seconds — `DATETIME(6)`, `TIME(6)`, a
declared precision as written. An enum
column becomes plain text on every target, its values no longer enforced. A type
dbd has no mapping for (`point`, `money`) becomes text as well, reported as
unmapped; the enum reason is given only for a type the design declares as an
enum. Every note in the file is one line, even when what it quotes spans
several. Skipped routines, extensions, roles, grants and policies are noted in
the script too, each where it would have been; a schema folded into a name is
reported in the summary and `--report` only.

**T-SQL output ends every statement with `GO`**, the batch separator `sqlcmd`
and SSMS read — SQL Server needs `CREATE SCHEMA` and `CREATE VIEW` each alone in
a batch. `GO` is not T-SQL, so split the script on it before sending it through
a driver.

Still not reported: an identity column's `START`/`INCREMENT` options; a lost
time zone or length (`timestamptz` → `DATETIME(6)` on MySQL and `TEXT` on
SQLite, `varchar(n)` → `TEXT` on SQLite); an index key longer than MySQL's
3,072 bytes or SQL Server's 900, which the target refuses (a `UNIQUE` on a
`varchar(3000)` is 12,000 bytes on MySQL); a SQL Server `UNIQUE` over a
nullable column, which admits one NULL where PostgreSQL admits many; and a
view's column list — `CREATE VIEW v (a, b)` comes out with the query's own
column names on every target. On SQLite a generated column's expression and an
index predicate are still copied verbatim (reported). The MySQL and SQL Server output is written
from documented syntax, not run against a server. The full mapping and the report format are in
[other dialects](13-other-dialects.md#dbd-emit).

Downgrades do not fail the run; assert on the `--report` count instead.

**In a workspace** `emit` translates the modules a run selects — every one, or
`--module`'s and what it depends on, or a scope's — as one schema: a key into a
dependency's table is kept, the script creating that table too, and each
module's statements follow a comment naming it (`-- module shared
(shop/shared)`). A schema's grants, which every module has, are noted once.

## `dbd combine`

Combine all DDL into a single SQL file.

```sh
dbd combine                        # Writes init.sql
dbd combine -f bootstrap.sql       # Custom filename
dbd combine --scope hub -f hub.sql # Only the 'hub' scope's working set
dbd combine --scope hub --deps include  # Expand to the dependency closure first
```

**Scope-aware.** `--scope` filters the combined SQL to that scope's working set and prints `scope 'hub': N of M entities`; `--deps include` (or a scope with `deps: include`) first expands to the dependency closure so the script is self-contained. Filter-only — `combine` does not gate on dependency gaps (use `inspect --scope` to surface them). Roles are kept in every scope; extensions are too, unless the scope sets an `extensions` allowlist (then only the listed extensions — `[]` for none).

**In the project's dialect.** The files are concatenated as written, and the statements dbd generates around them follow `source.dialect`: `CREATE SCHEMA IF NOT EXISTS` for PostgreSQL; for T-SQL a guarded `IF SCHEMA_ID(N'x') IS NULL EXEC(N'CREATE SCHEMA [x]');`, with every entity's DDL then ending its own batch with `GO` — SQL Server needs `CREATE VIEW`, `CREATE PROCEDURE` and `CREATE TRIGGER` first in their batch, and a file that already ends in `GO` keeps its own; nothing for MySQL and SQLite, which have no schemas. Extensions and roles are PostgreSQL-only, so in a T-SQL, MySQL or SQLite script each becomes a comment saying it was not created. A file that does not parse is left out without a message — run `dbd inspect` first.

**The `search_path` apply runs.** On a PostgreSQL project, after each top-level `SET search_path` whose path leaves `public` out, the output gets a statement of its own setting the same path with `public` added, exactly as `apply` runs the file — extensions are often installed in `public` — and a file that states no path gets the [`source.search_path`](03-design-yaml.md#search_path) one ahead of it (`SET search_path TO "app", "public";`), so `psql -f init.sql` succeeds where `apply` does and creates each object where `apply` would. The file's own text is copied unchanged: the statements are found in the parse tree, so a function's `SET search_path TO ''` clause, a body or a string literal is never touched. `-f` must resolve inside the project, as for `emit`; a bare name resolves against the project root.

**In a workspace** `combine` writes one script over the modules a run selects,
in `depends_on` order, each module's statements after a comment naming it, each
entity once — the manifest's schemas included:

```sql
-- module shared (shop/shared)
CREATE SCHEMA IF NOT EXISTS "app";
set search_path to app;
SET search_path TO "app", "public";
create table if not exists currencies (code text primary key);


-- module core (shop/core)
set search_path to app;
SET search_path TO "app", "public";
create table if not exists orders (id integer primary key, currency text references currencies (code));
```

The `scope '…': N of M entities` line counts what the run writes, each entity
once.

---

## `dbd dbml`

Generate DBML documentation for dbdocs.io / dbdiagram.io.

```sh
dbd dbml                           # Writes design.dbml (when no dbml keys configured)
dbd dbml -f schema.dbml            # Custom filename
dbd dbml --scope hub -f hub.dbml   # Document only the entities in the 'hub' scope
dbd dbml --scope hub --deps include  # Expand to the dependency closure first
```

Generated natively — no `@dbml/core` dependency. Includes the Project block, enums, tables with columns, indexes, CHECKs and comments, standalone Ref blocks with FK actions (composite refs as `t.(c1, c2) > o.(c1, c2)`), `TableGroup` blocks, and a stub table for every table a Ref points at that the document does not define.

What DBML has no syntax for travels where DBML keeps it, so `init --from-dbml` can read it back:

- **CHECK constraints** go in the table's `checks { … }` block, each expression in backticks, with `[name: '…']` when the constraint is named.
- **Expression indexes** keep their key in backticks (`` `lower(email)` ``), rather than reading back as a column named `lower(email)`.
- **Table-level UNIQUE constraints** are written as unique indexes — DBML has no constraint syntax — with their name and the note `unique constraint` (`(code, region) [unique, name: 'people_code_uq', note: 'unique constraint']`), and `init --from-dbml` reads that mark back as the named constraint, `constraint "people_code_uq" unique (…)`.
- **Index notes** hold what an index's settings cannot, one fact per line: `where: <predicate>` for a partial index, `order: <entry>, …` when a key is not plain ascending (`asc` or `desc`, optionally `nulls first`/`nulls last`, one entry per key), and `nulls not distinct`. Only those lines are read back.
- **Generated columns and `ALWAYS` identities** ride in custom column properties: `generated always as: '<expression>'`, and `generated as identity: 'always'` beside `increment` — a bare `increment` means `BY DEFAULT`.
- Strings are escaped the way DBML's own lexer reads them, so a quote or backslash in a note or default survives, and a column comment keeps its line breaks (as DBML's `\n` escape). A note that spans lines is written as a triple-quoted string, which @dbml/core reads back exactly.
- **A SQL value keyword default** — `current_date`, `current_user`, … — is written as an expression (`` [default: `current_date`] ``), decided from its parse tree, so it does not come back as the string `'current_date'`.
- **An enum column's type** is written schema-qualified, as the table's `search_path` resolves it, in DBML's own spelling of an enum — `app.mood`, or `"app"."mo\"od"` for a name that needs quotes — so the rebuilt DDL names the type that exists.
- **Every name** — schema, table, column, enum and enum value, index, primary-key, foreign-key, UNIQUE and CHECK names — is written in DBML's quoting and read back as itself, whatever it holds: a quote, a backslash, a space, a dot, a `'` or non-ASCII text. (A schema, table or enum name also names a file, so a backslash in one is refused, and a dot in one is not supported by dbd.) The DDL `init --from-dbml` writes doubles a quote in every identifier, `CREATE SCHEMA` included.
- **A CHECK's name** comes back with it: `init --from-dbml` writes `CONSTRAINT "people_age_ck" CHECK (age >= 0)`.
- **An index's method** goes in its `type:` setting (`[type: gin]`), and its `INCLUDE` columns, operator classes and storage parameters in its note (`include: a, b`, `opclass: -, text_pattern_ops` — `-` for a key's default — and `with: fillfactor = 70`).
- **A named primary key** is a `pk` entry in the `indexes` block with its name (`id [pk, name: 'people_pk']`), and **a named foreign key** a named Ref (`Ref "people_org_fk": …`). PostgreSQL's default names — `<table>_pkey`, `<table>_<columns>_fkey` — are left out, so a key nobody named stays unnamed and the database names it again.

**Reading DBML.** `init --from-dbml` and `merge --from-dbml` read a document the way @dbml/core reads it — dbd's own output and the DBML a person writes: table aliases (`Table app.users as U`), an inline `[ref: > t.c]`, notes and settings that span lines, `Note { … }` blocks, one-line tables @dbml/core accepts, a bare `pk` or `unique` after a column's type, a `pk` index entry, `Ref name:` and `Ref name { … }`, an index's `type:`, and a table header's `[note: …]`. A one-to-one `Ref: a - b` puts the foreign key on `b`, as @dbml/core's SQL does; an inline `[ref: - t.c]` puts it on its own column. What dbd cannot build as written is refused, with why, rather than guessed: a many-to-many `<>` Ref, which has no foreign key (@dbml/core builds a junction table for it), and a table taking a `TablePartial`'s columns. Anything after a column, an enum value or a header on its line is refused, naming it, as @dbml/core refuses it (``table `a.p`: unexpected `name text` after the column `id` — each column needs a line of its own``). A column type in DBML's enum spelling (`"app"."mo\"od"`), or a quoted name the document declares as an enum, is that enum; any other quoted type is its own SQL text (`"timestamp with time zone"`). A note is dedented as @dbml/core dedents one — blank leading lines dropped, the indentation its lines share removed — except that the line a closing `'''` sits on is layout, not a trailing line break. A one-line note loses its leading whitespace.

DBML has no syntax for a foreign key's deferral or `MATCH FULL`, an `EXCLUDE` constraint or a column's collation, so `dbd dbml` leaves them out and they do not survive the round trip. An expression default or CHECK holding a backtick is written into a document `init --from-dbml` refuses, since DBML has no escape for one, and a line break inside a CHECK's or index predicate's string literal comes back as a space.

`checks` blocks need `@dbml/core` 5.0 or later and custom column properties 9.1 or later; tooling older than that cannot read a document that uses them.

**Stubs.** A Ref to a table the document does not define is a DBML error, so each such table gets a stub with only the referenced columns and a note saying why: declared `external:`, defined in this project but outside this document, or referenced and defined nowhere. The note is also how `init --from-dbml` knows a stub for what it is: it reads one back as an `external:` entry, never as a table to build. A stub column is typed like the foreign-key column that references it — else from the design, else `varchar` — and each column set a Ref lands on is declared unique in an `indexes { … [unique] }` block, so the SQL `dbml2sql` writes from the document applies (a foreign key needs a unique key to point at):

```
Table "auth"."users" {
  "id" uuid

  indexes {
    id [unique]
  }

  Note: 'External entity — managed outside this project'
}
```

**A file that does not parse** is named, and the document is still written (exit 0), with that entity missing: `⚠ ./ddl/table/app/broken.ddl could not be parsed, so app.broken is left out: Parse error: …`. `dbd diagram` does the same.

**Scope-aware.** `--scope` documents only the entities that deploy under that scope (its working set) and prints `scope 'hub': N of M entities`. `--deps include` (or a scope whose `deps: include`) first expands the selection to its dependency closure, so the diagram is self-contained. Unlike `apply`/`deploy`, `dbml` does not gate on dependency gaps — it is documentation, so it simply emits the filtered set, and an out-of-scope FK target appears as a stub typed from the design. (Before 0.25 a `--scope` document with such a Ref did not parse at all.) A `deps: include` scope whose closure needs something the scope excludes is refused.

**Multi-document.** If `design.yaml` declares multiple `dbml.<key>` entries (each with its own `include`/`exclude`, `output`, `auto_group_by_schema`, `groups`), `dbd dbml` writes one file per key into the parent directory of `-f`, using each key's `output` (default `<key>.dbml`). See [design.yaml reference](03-design-yaml.md#dbml) for the full schema.

**In a workspace** `dbml` documents the modules a run selects as one design:
each table is defined once, from the module declaring it, so a Ref into a
dependency's table lands on that table — and one a scope leaves out is a stub,
typed from its module. A document with no groups of its own — no `groups`, no
`auto_group_by_schema` — gets one `TableGroup` per module, each holding that
module's tables (DBML puts a table in one group):

```
TableGroup "shared" {
  "app"."currencies"
}

TableGroup "core" {
  "app"."orders"
}
```

---

## `dbd graph`

Output the dependency graph as JSON.

```sh
dbd graph                          # Full graph
dbd graph -n config.lookups        # Subgraph reachable from one entity
dbd graph --scope hub              # Only the 'hub' scope's working set
```

Output: `{ "nodes": [...], "edges": [...], "layers": [...], "scope": "hub" | null }`

`--scope`/`--deps` filter the graph to the scope's working set (closure under `include`). `-n` (entity subgraph) and `--scope` compose; a `-n` that names nothing, something outside the scope, or an `external:` table — dbd has no DDL for it, so it is no node of the graph — is an error rather than an empty graph.

Everything but the JSON goes to stderr, so `dbd graph | jq` — and `dbd graph -v | jq` — always parses: a file that does not parse is named there (`⚠ ./ddl/table/app/broken.ddl could not be parsed, so app.broken is left out: …`), as `dbml` and `diagram` name it, and so is `-v`'s count (`4 nodes, 2 edges, 3 layers`). That entity is left out of the nodes, layers and edges alike, as the warning says.

**`scope` (added in v0.12.0)** names the scope the graph was narrowed to, or is `null` for an unscoped run. It lives in the JSON rather than being printed beside it, because stdout here is a contract (`dbd graph | jq`) — and a graph silently missing a schema is indistinguishable from a complete one. Every other command that takes a scope — `inspect`, `apply`, `deploy`, `import`, `reconcile`, `diff`, `combine`, `emit`, `dbml`, `diagram`, `export`, `refresh`, `policies` and `reset`, and their dry runs — prints `scope '<name>': <kept> of <total> entities` instead (`diff --json` prints it on stderr, keeping stdout JSON); unscoped runs print nothing.

**In a workspace** the graph spans the modules a run selects, and each node
names the module declaring it:

```json
{ "module": "core", "name": "app.orders", "schema": "app", "type": "table" }
```

`-n` is the entity's subgraph across those modules — the entity and what it
needs, whichever module declares it. An entity of a module the run does not
select is refused naming it: `app.orders is module "core"'s, which the run does
not select`.

---

## `dbd diagram`

Open the schema in the **hosted interactive viewer** — a project overview, a pannable/zoomable ER diagram, a table list, a changelog built from your snapshots, and a sidebar of every entity (filterable by type) where each opens a page: a table's fields, references, CHECKs, indexes, dependencies, relationship diagram and own changelog; a view's, routine's or sequence's uses and users; an enum's values; and, for a table a foreign key lands on that the model does not carry, a stub page saying why. The model is gzip-compressed into the URL fragment (client-side only, never sent to a server), so the link is private and self-contained. The full guide is [the schema viewer](11-schema-viewer.md).

```sh
dbd diagram                        # build the model and open it in your browser
dbd diagram --print-url            # print the viewer URL instead of opening a browser
dbd diagram --site http://localhost:5173   # point at a different site (or set $DBD_DIAGRAM_URL)
dbd diagram --json -f schema.json  # write the raw SchemaModel JSON (drop it onto <site>/diagram)
dbd diagram --scope hub            # scope-aware (the scope's working set, its history too)
```

`dbd diagram` prints the URL and opens your default browser; on a headless machine use `--print-url`. The `--json` output is the dbd-native schema model; drag the file onto the site's `/diagram` page, or feed it to other tooling. A `deps: include` scope whose closure needs something the scope excludes is refused, as every command refuses it, rather than drawn as an empty model. A file that does not parse is named (`⚠ … could not be parsed, so <entity> is left out: …`) and the rest is drawn, as `dbd dbml` does.

**In a workspace** the model is the workspace's — the modules a run selects,
each entity drawn once, in the module declaring it — and it names the modules
and each entity's; the viewer groups its sidebar by module and tints the
diagram by module or by schema. Each module's changelog is its own. See
[the schema viewer](11-schema-viewer.md).

### The model JSON

The model carries a `version` (currently `3`), and being JSON rather than DBML it covers more than tables:

| Field | What it holds |
| --- | --- |
| `tables` | Tables, with columns (`pk`/`nn`/`en`/`fk`/`uq`, type, default, comment, and `identity` or `generated`), indexes (a partial index's predicate as `where`) and `checks` (`{name?, expression}`) |
| `refs` | Foreign keys — the ER edges, column-level, as `{s,t,c}` — every one, landing on a table or a stub |
| `stubs` | The tables `refs` land on that `tables` does not carry: table-shaped, only the referenced columns, `kind` `external`, `out_of_scope` or `unresolved` — omitted when there are none |
| `entities` | Views, materialized views, functions, procedures, triggers and sequences |
| `deps` | What reads, writes, calls or uses what — the call/reference graph (`uses` is a column's enum or sequence) |
| `schemas` / `project` | Every schema the model draws something in, with its table and enum counts; and the project's name, dialect and note |
| `history` | What each snapshot changed, oldest first — omitted when the project has none |
| `enums` | Enum types with their values in order, and their comment — omitted when there are none |

`entities` and `deps` are separate from `tables` and `refs` on purpose: an ER renderer wants foreign keys, a call-graph renderer wants dependencies, and folding them into one array would change what every existing consumer of `tables` receives.

A dependency edge whose target is not a project entity — a Postgres built-in like `now()`, or a genuine dangling reference — is marked `"unresolved": true` rather than dropped. The edge is real; only its endpoint cannot be placed on the diagram.

Comments reach the model for **every** kind, sequences included, not just tables: `note` is the first line (the one-line summary an entity table shows) and `noteMd` the full text.

`history` is read from `snapshots/` and is what the viewer's Changelog tab draws. The first snapshot is a **baseline** — counts of tables and enums, not every one listed as added; each later entry lists the tables and enums that version added, removed or modified, with their column, index, constraint and enum-value edits. A rename or type change that `dbd snapshot` cut in stages is one entry (`version` to `through`) and reads as the rename or type change, never as the intermediate column. A type spelled two ways (`varchar(32)`, `character varying(32)`) is not a change. Snapshots record tables and enums only, so views and routines have no history. A project with no snapshots has no history; `dbd release`, `dbd snapshot`, `dbd init --from-db` and `dbd merge` all write them. With `--scope`, the history covers the scope's working set — the `deps: include` closure too, so it matches what the diagram draws — and its baseline counts that, not the project.

---

## `dbd reset`

Drop the project's own managed objects so you can re-`apply` from scratch. Guarded by the bookkeeping environment check (`dbd.meta` on Postgres/Supabase, `_dbd_meta` on SQLite) **and** the [scope guard](#scope-guard) — a wrong-`--scope` reset (which would drop the wrong objects) is refused.

```sh
dbd reset                          # Blocked if the database is marked prod, or at version >= 1
dbd reset --force                  # Override safety guards (env + scope)
dbd reset --dry-run                # Show what would be dropped (refuses where reset would, given a database)
dbd reset --schemas                # Also DROP SCHEMA for managed schemas
dbd reset --extensions             # Also DROP EXTENSION for configured extensions
dbd reset --clean                  # Shorthand for --schemas --extensions
dbd reset --target postgres        # postgres, supabase or sqlite (default: the design's target key)
dbd reset --scope hub --dry-run    # Only the objects the 'hub' scope occupies
dbd reset --allow-ownership-change # Run as this project's name although its entities here are all undeclared
```

**The guards** read the database, not the flags of this run: a reset is refused
when the bookkeeping row says the database is `prod` (rows older versions wrote
as `production` included) — and any database applied without `-e dev` is — or
when it records version 1 or later, in any environment, whether or not a
migration has run on it. The refusal calls it a released project's database only
when `project.released` is set:

```
reset is blocked — database is marked as prod. Use --force to override.
reset is blocked — the database is versioned at v3. Use --force to override.
reset is blocked — the database is versioned at v3 (a released project's database). Use --force to override.
```

After a rename of `project.name`, the guards read the record the old name left,
and the refusal names that project (see
[renamed projects](#renamed-projects)).

**Beside other projects** a reset drops only what its design declares, and is refused — `--force` or
not — when another project depends on any of it. Another project's `prod` record — or one with an env
dbd does not know — refuses an unforced reset too; the `-e` guard still reads this project's own. `--schemas` keeps a
schema holding anything not this project's, `--extensions` an extension another project uses, and a
full reset a role another project uses, each named:

```
schema app kept: it holds table app.invoices ("billing").
```

See [sharing a database](14-sharing-a-database.md#reset).

**A reset of everything the database was built with forgets its record of the
project** — env, version and scope pin, the bookkeeping row the guards read —
because it leaves nothing of the project behind, and the next apply records the
database afresh. That is how a dev database first built without `-e` (and so
marked prod) is rebuilt as dev: `dbd reset --force`, then a run with `-e dev`,
which the [environment guard](#global-options) would otherwise refuse.
"Everything" is the `all` scope, or the scope the database is pinned to — so a
design with a `default:` scope, which builds and resets under it, forgets the
record on a bare `reset --force` too. A reset of a narrower scope keeps the
record, as it keeps the rest of the project.

**`--dry-run` checks the same guards** — prod, a versioned database, and the
[scope pin](#scope-guard) — whenever a database is configured: `-d`,
`$DATABASE_URL`, or a `target.url` that resolves (a `$VAR` that is unset does
not). `--force` and `--allow-scope-change` clear them as they clear the real
run's, and a configured database that cannot be reached fails to connect rather
than falling back to a preview. With no database configured, the dry run is an
offline preview of the script.

**Entity-level by default.** `reset` drops the project's own objects individually — functions/procedures (every overload, via a `DROP ROUTINE` block), materialized views (before the views/tables they read), views, tables, sequences and enums — in reverse dependency order, with `CASCADE` on a full reset: one under the `all` scope — `--scope all`, or no `--scope` when the design defines no `default:` scope. It does **not** `DROP SCHEMA` or `DROP EXTENSION`, so `public`, custom schemas, and installed extensions (e.g. `pgvector`) survive untouched; the next `dbd apply` repopulates the schemas. This avoids churning extensions and never aborts on `public`. The bookkeeping of what it dropped goes with it: the project's recorded migrations, the `dbd.matview_stamps` row of every materialized view the reset removed — by name, with its schema, or by `CASCADE` — so a later matview of the same name is not vouched for by a stale stamp, and on a reset of everything the database was built with the record of the project (above).

**It says what it dropped**, counted by kind, with roles named — or that it found
nothing — read from the database before and after the drop, since every drop is
`IF EXISTS` and the script alone cannot tell a reset that emptied the schema from
one that had nothing to drop:

```
Reset dropped 1 materialized view, 1 view, 2 tables, 1 enum, 1 role (writer).
Reset dropped nothing — none of the project's objects were in the database.
```

**Opt-in wider drops:**

- `--schemas` — also `DROP SCHEMA … CASCADE` for the project's managed schemas. Always skips the true system schemas (`pg_catalog`, `information_schema`, `pg_toast`) and dbd's own `dbd` bookkeeping schema; on a `supabase` target also skips the Supabase-managed set (`public`, `auth`, `storage`, `realtime`, `graphql_public`, `supabase_functions`, `pgbouncer`, `pgsodium`, `vault`, `extensions`, `supabase_migrations`). On a `postgres` target, `public` **is** dropped.
- `--extensions` — also `DROP EXTENSION … CASCADE` for each extension in the target's `extensions:` config — under `--scope`, only those the scope's `extensions:` allowlist installs.
- `--clean` — shorthand for `--schemas --extensions`.

**`--target`** names the target whose reset applies: `postgres`, `supabase` or
`sqlite`. It defaults to the design's target key — the first `target:` entry —
so a Supabase project's schemas are protected without passing anything; pass
`--target` to override. A design whose target has no reset (Convex) is refused
before anything connects, and so is a target that is not the kind of database
the connection reaches, since the target picks the dialect of the drops:

```
Error: reset: --target postgres does not match the database — the connection is SQLite. The target picks the dialect of the drops, so it must be the kind of database the connection reaches: pass --target sqlite, or drop --target to use the design's.
``` Every role the design declares — `ddl/role/` files as
well as the target's `roles:` — is dropped on a full reset, and only when the
target is `postgres`.

**On SQLite** the reset drops the project's tables and views in one transaction,
with foreign keys deferred to its commit (`PRAGMA defer_foreign_keys = ON`):
SQLite's DDL is read verbatim, so dbd has no references to order the drops by.
SQLite has no schemas or extensions, so `--schemas`, `--extensions` and
`--clean` change nothing there, and say so. The prod and versioned-database
guards apply as on PostgreSQL.

**Roles and the grants dbd made.** PostgreSQL refuses to drop a role that still
holds a privilege, so before the `DROP ROLE`s the reset revokes what `apply`
granted those roles — the `schemas:` and target `grants:` (`USAGE`, the table
privileges, and the `ALTER DEFAULT PRIVILEGES` entry set by the user running the
reset) — on every schema that survives the reset; a schema it drops takes its
grants with it.

Roles are server-wide, so two databases built from one design share them. A role
that holds anything in **another database** on the server is kept, and named —
dropping it would break that database:

```
Role reader kept: it holds privileges or objects in database other — roles are server-wide, and dropping it would break that database.
```

Its grants here are still revoked: left in place, they would make this database
the one that holds it when the other database is reset. A role that holds anything else **here** — a grant dbd did not make, a
default-privileges entry another user set, an object it owns — fails the reset,
which rolls back whole, with an error naming the role and what it holds:

```
reset cannot drop role "writer" — it still holds privileges or owns objects reset does not revoke (it revokes only the schema grants design.yaml declares): privileges for table extra. Revoke or reassign them, then re-run reset.
```

dbd never runs `DROP OWNED BY`, which would drop objects the project does not
own. With a database configured, `--dry-run` asks the server which roles it
would keep, leaves their `DROP ROLE` out of the script it prints, and names them
the same way (`[dry-run] Role reader kept: …`).

**Scope-aware.** `--scope` restricts the reset to the objects (and, with `--schemas`, the schemas) the scope's working set occupies. Roles are dropped only on a full reset; a subset scope leaves shared roles intact. A reset whose `--scope` differs from the database's pinned scope is refused unless you pass `--allow-scope-change` (or `--force`) — see [Scope guard](#scope-guard).

**A scoped reset never cascades.** `CASCADE` reaches past the scope — an in-scope
enum dropped with it took an out-of-scope table's column along — so a scoped
reset drops without it, all or nothing. When something outside the scope depends
on what it would drop, it drops nothing and names the dependent:

```
scoped reset refused, nothing was dropped — cannot drop type app.mood because other objects depend on it: column mood of table other.notes depends on type app.mood. A scoped reset drops only what its scope holds, and never cascades: add the dependents to the scope, or reset without --scope.
```

A table's policies, and a table's or a view's triggers, go with it, so they are dropped first: one
calling a function the scope also drops — a policy from `policies/`, or a trigger written in its
function's file — does not refuse the reset. One on a table outside the scope still does.

**In a workspace** reset drops each module before the modules it depends on —
reverse `depends_on` order — each as its own owner, and `--module` resets that
module alone, never its dependencies, which are other owners. Every module's
reset is previewed first, each beside the modules reset before it — their uses
and holdings do not hold it back, their prod marks still do — and the run is
refused whole or run whole; its `--dry-run` passes where it passes. A schema
that still holds a table another module now declares — moved there, not yet
taken over — is kept, naming it.

---

## `dbd snapshot`

Create versioned schema snapshots and generate migration scripts.

```sh
dbd snapshot --name "add notes column"    # Create a snapshot
dbd snapshot --list                        # List existing snapshots
```

Smart multi-snapshot: When complex changes are detected (column rename, type change, enum value removal) dbd automatically generates multiple snapshots with correct intermediate states.

- Column rename: 1 snapshot — `RENAME COLUMN`, in place
- Column type change: 1 snapshot — `ALTER COLUMN … TYPE … USING`, in place, as `reconcile` does; 2 when dbd cannot cast it (a data.sql TODO readies the column's values, then the in-place alter)
- In place, the column keeps its values and everything on it — NOT NULL, default, identity, generated expression, keys, the foreign keys on either side, CHECKs, EXCLUDEs, indexes, comment — and a value the new type cannot hold fails the version. Before 0.27.2 both were a column swap that lost the values of a type-changed column and the definition of either; a pending version cut that way is refused (see [Smart multi-snapshot](05-snapshots-migrations.md#smart-multi-snapshot))
- Enum value removal: 2 snapshots (the data mapping, then the type swap — the type renamed aside and recreated, each column moved across with its views, materialized views, indexes and default dropped and restored around it)

Data corrections (*.data.sql) are generated where a change needs one: a type change dbd cannot cast and an enum value mapping each generate TODO comments to fill in.

A table removed from the design is written as its `DROP TABLE … CASCADE` in `migrations/<NNN>/<schema>/<table>.sql`, the file every per-table migration uses, and the next `dbd apply` runs it. A hand-written `<table>.drop.sql` there takes precedence. The drop runs only where the table still exists, and the summary counts only the drops that ran. (Before 0.25 apply looked only for `<table>.drop.sql`, so a generated drop never ran while the migration was recorded as applied.)

`dbd snapshot` sets `project.version` by editing `design.yaml` in place: its comments, blank lines and flow-style lists stay as you wrote them.

**In a workspace** each module is snapshotted on its own, in its folder, at its
own version — every module, or the one `--module` names, never a scope: a
snapshot records the design — and its version is written under
`modules.<name>.version`, in place. `--list` lists each module's snapshots
under its header.

---

## `dbd migrate`

Show migration status (read-only).

```sh
dbd migrate --status     # Show DB version vs latest, list pending
```

```
Database version: 001
Latest snapshot:  002
1 pending migration(s):
  v001 -> v002  (0 added, 0 altered, 1 dropped)
```

A database with no recorded version shows `Database version: none recorded`.
`Database is up to date.` is printed only when the two versions match; with no
migration pending but the versions apart, the status says why instead — the
next `dbd apply` records the latest version on an unversioned database; it
leaves the recorded version where it is when no migration under `snapshots/`
bridges the gap; or the database is ahead of the latest snapshot, applied from
a newer design.

A database [pinned](#scope-guard) to a scope is noted — `note: this database is pinned to scope
'hub' — it was built with that scope's entities, not the whole design.` — since the versions are
the whole project's.

Use `dbd apply` to execute pending migrations. There is no separate migrate --apply.

**In a workspace** each module's status is shown under its header: each module
has its own version.

---

## `dbd deploy`

Deploy from a local path or GitHub source. Runs apply, grants, import and
policies in one step — `deploy` means bringing the database fully up from
source:

1. **Apply** — exactly what `dbd apply` does, `apply.before` and `apply.after`
   included, behind the same guards: a database [pinned](#scope-guard) to another
   scope, one marked prod under `-e dev` ([`--environment`](#global-options)) and
   one newer than the project ([the version guard](#dbd-apply)) are refused before
   anything runs.
2. **Grants** — the `grants:` configured on schema entries and on the target,
   the same as `dbd apply` applies (with the PostgREST `USAGE` grants on a
   `supabase` target).
3. **Import** — the data under `import/`, the import procedures and
   `import.after`, as `dbd import` runs them.
4. **Policies** — every RLS policy under `policies/`, unconditionally (unlike
   `apply`, where they are opt-in behind `--with-policies`).

`--adopt-from <owner>` and `--allow-ownership-change` work as on [`dbd apply`](#dbd-apply), and each
phase waits for the database's dbd write lock as `apply` does (see
[sharing a database](14-sharing-a-database.md#runs-wait-for-each-other)).

`-c`/`--config` works as on every other command. `dbd deploy` is a thin wrapper
over the library's `Design::deploy`, so an embedding application gets the
identical pipeline.

Every phase reports its count, including zero, and its warnings, in the order
the phases ran. If nothing was imported, `deploy` says why — no `import/`
directory, data files belonging to a different `--env`, a staging file that
failed to parse, entries cut by the active `--scope`, or a function reading a
staging table that no procedure loads. Grants, like the rest, are narrowed to
the schemas the scope builds. What the import phase would refuse — a data file
outside `import.staging`, a staging table whose loader is ambiguous, one whose
loader the scope leaves out — is refused before the apply phase starts, so a
refusal never leaves half a deploy behind.
A policy file that fails is non-fatal: it is reported as a warning and counted
in the summary, and the deploy still succeeds.

```sh
dbd deploy --source owner/repo/path -d $DATABASE_URL    # From GitHub
dbd deploy --source ./local/project                       # From local path
dbd deploy --dry-run --source owner/repo/path             # Preview
dbd deploy --scope hub --database $HUB_URL               # Deploy a named scope
dbd deploy --scope hub --deps include -d $HUB_URL        # Auto-expand dependencies
dbd deploy --no-cache --source owner/repo/path -d $URL     # Re-download this source
dbd deploy --clear-cache --source owner/repo/path -d $URL  # Wipe the whole cache first
```

**GitHub sources** — `owner/repo`, `owner/repo/path`, either with `@ref` (a branch,
tag or commit; default `HEAD`), or a full `https://github.com/owner/repo[/tree/<ref>/<path>]`
URL — are downloaded from the GitHub API. For a private repository, set
`GITHUB_TOKEN`: dbd sends it as a bearer token to `api.github.com` and nowhere
else. `deploy` is the only command that accepts a GitHub source.

Downloads are cached in `~/.cache/dbd/` (macOS: `~/Library/Caches/dbd`), keyed by
`owner-repo-ref`. `--no-cache` ignores the cached copy of the current source and re-downloads it;
`--clear-cache` removes the entire cache directory before deploying. Both are no-ops for local path
sources (`--clear-cache` still clears the cache).

`--dry-run` runs the real deploy's gates — it refuses a file that does not
parse, a dependency gap or a closure conflict, and what the import phase would
refuse — and then reports what the run would do, after scope filtering: the
`scope 'X': N of M entities` line, each `deps: include` gap as a
`+ auto-included:` line rather than a failure, the entities that would be
applied, the data files that would be imported with the reason for every one
left out and each `import.after` hook the scope skips, and the policy files that
would be applied and skipped:

```
$ dbd deploy --dry-run --scope hub        # hub: { includes: [app], deps: include }
Deploying from source: .
scope 'hub': 4 of 7 entities
+ auto-included: config.lookups (required by app.orders)
    chain: app.orders → config.lookups
4 entities would be applied.
0 data file(s) would be imported.
⚠ staging.lookups not imported — staging.lookups is outside scope 'hub'
⚠ after-script sql/after.sql skipped — needs staging.lookups, which is outside scope 'hub'
1 policy file(s) would be applied, 1 skipped (out of scope).
[dry-run] No changes applied.
```

With a database configured, the dry run also asks it what `apply --dry-run`
asks — the scope pin, the environment guard, the version guard and the pending
migrations' unresolved TODOs — and lists the pending migrations after the entity
count, in the order the run takes them (`No pending migration to run (…)` when
there are none). An unreachable configured database fails the preview, and a
SQLite file that does not exist yet is not created; with none configured it
stays offline. Like `apply`, the run and its preview name each DDL file that
states no `SET search_path`, and a run without `-e` that promotes a dev
database to prod says so.

**In a workspace** a deploy goes over the modules twice — every module's schema
(its apply and grants), then every module's data (its import and policies) — so
no module migrates after the import has emptied the tables it reloads:

```
Deploying from source: .
module shared (shop/shared)
Applying schema...
1 entities applied (no project.version — the database is not versioned).

module core (shop/core)
Applying schema...
1 entities applied (no project.version — the database is not versioned).

Loading data...
module shared (shop/shared)
Import complete — 1 table(s) loaded, 0 procedure(s) called, 0 after script(s) run. 0 policy file(s) applied.

module core (shop/core)
Import complete — 1 table(s) loaded, 0 procedure(s) called, 0 after script(s) run. 0 policy file(s) applied.
```

The import reloads as one project's does: the first module's import empties,
together, every table the run reloads that exists by then, in load order by the
database's keys, and the others empty none — so `shared`'s reference data
reloads with the `core` tables that hold keys to it. `deploy --module shared`
alone cannot empty `shared`'s tables under `core`'s keys, and fails before its
import empties anything — on PostgreSQL naming the keys that hold them. An import that fails
partway names what it emptied and did not reload: `the import emptied … to
reload them and stopped before it did — run it again once this is fixed`.

---

## `dbd reconcile`

Pre-release (pre-v1) **declarative** apply: diff the live database against the design and apply the
difference in place — `CREATE` for new tables, `ALTER` for changed ones — with **no snapshot files
and no version bump**. Ideal while the schema is still churning and cutting a snapshot per change is
overkill.

```sh
dbd reconcile --dry-run -d $DATABASE_URL              # Preview the plan (entity summary)
dbd reconcile --dry-run -v -d $DATABASE_URL           # Preview + show the column-level ALTER SQL
dbd reconcile -d $DATABASE_URL                        # Apply (create + alter)
dbd reconcile --allow-destructive -d $DATABASE_URL    # Also drop columns/constraints
dbd reconcile --prune -d $DATABASE_URL                # Also drop orphaned tables
dbd reconcile --prune-unowned -d $DATABASE_URL        # …and those nobody owns that another project might
dbd reconcile --adopt-from billing -d $DATABASE_URL   # Take over what the design declares that 'billing' owns
```

The plan lists one line per changed entity (`+ create`, `~ alter`, `- prune`). Materialized views
appear too: `+ create materialized view <name>` for one reconcile would create, and a `⚠` warning
for a matview whose deployed definition has drifted (dbd never auto-recreates a matview). Add `-v`
to a `--dry-run` to print the generated `ALTER` SQL beneath each altered entity, so you can review
the exact column-level change before applying.

**Grants and hooks.** A reconciled database is the one an apply builds, so reconcile runs the
design's `apply.before` and `apply.after` hooks and then its `grants:`, scoped as `apply` scopes
them. `apply.before` runs after every gate — the `--allow-destructive` refusal included, so a
refused run fires no hook — and before the first write; `apply.after` after the writes and the
pg_cron sync; the grants last. A hook the scope skips is named before the run, and `--dry-run`
names it too, refuses a hook file that is missing, and lists after the plan the hooks and grants
the run goes on to (`  run apply.after sql/realtime.sql`, `  grant on app: app_reader (usage,
select)`). The summary counts the hooks that ran:

```
Reconciled — 2 created, 0 altered, 3 re-applied, 0 re-stamped, 0 pruned, 3 hook script(s) run.
Applied grants.
```

**Order, and no single transaction.** Reconcile writes in one pass over the design in the
dependency order `apply` uses — each entity created, altered or re-applied at its place — so a
function comes before a table whose default or trigger calls it, then any matview restamps, then
the prunes. The run is **not** one transaction: each step commits as it runs, so a step that fails
stops the run with the steps before it already applied. That step itself rolls back — one table's
`ALTER` script, or one enum's recreation, runs as a single statement batch. (A batch transaction
around the whole run is not possible: an enum value added by `ALTER TYPE … ADD VALUE` cannot be used
until its transaction commits, and a default or `CHECK` naming it later in the run would fail.)

The diff is scoped to the schemas the design declares, so reconcile never touches tables in other
schemas, and it leaves out everything an extension owns — a table, view, sequence, enum or function
an extension created is the extension's, never drift or an orphan. Two kinds of destruction each
need an explicit opt-in, and without it the refusal lists each destructive change by entity
(`app.people: drops column nickname`) and nothing else:

- **`--allow-destructive`** — drop a *column*, constraint, foreign key, or index from a managed table,
  or remove a *value* from an enum (see **Enum value removal** below).
- **`--prune`** — drop a whole *table* still in a managed schema but no longer in the design (an
  orphan): one this project owns, or one nobody owns in a schema no other project uses. Another
  project's table is never dropped — `reconcile` and `diff` do not see it — and one nobody owns where
  another project might is kept and named. Without `--prune`, orphans are reported and left in place.
  See [sharing a database](14-sharing-a-database.md#pruning). Under `--scope`, a table the design
  declares outside the scope is never an orphan — a scope narrows what is applied, not what the
  project owns. **Before 0.24.1 it was:** a scoped prune dropped such tables
  ([#40](https://github.com/sensei-hq/dbd/issues/40)), so upgrade before running `reconcile --prune --scope`.
  On a `supabase` target, Supabase's own schemas are never pruned. `public` is a managed schema
  only when the design puts something in it: a role, or an extension with no `schema:`, does not
  make it one. **Before 0.25 it did**, and `--prune` dropped every table in `public` the design
  never declared — upgrade before your next `reconcile --prune`.
- **`--prune-unowned`** — with `--prune`, which it implies, also drop the tables nobody owns that might
  be another project's — in a schema another project uses, or any while a project in the database
  has not upgraded dbd — naming each.

Orphaned *enums* are only warned about, never auto-dropped (columns may still reference them).
Reconcile is **disabled once the project is released** (`project.released: true`) — see `dbd release`.
It checks that flag only: a project with snapshots that was never released (see
[recommended workflows](06-recommended-workflows.md#snapshots-but-not-released)) can still reconcile.
Like `apply`, reconcile claims what its design declares — before its first write — takes
`--adopt-from` and `--allow-ownership-change`, refuses to drop what another project depends on, and
waits for other projects' runs to finish (see [sharing a database](14-sharing-a-database.md)).
`--scope`/`--deps` restrict reconcile to a scope's working set (gap-gated, like `apply`), and
`--dry-run` refuses a database pinned to another scope, as the real run does, and both refuse a
`-e dev` run on a database marked prod (see [`--environment`](#global-options)). Reconcile records
the database at `project.version` — `0` when the design has none, as `apply` records — so a dev
`reset` afterwards is not refused as though migrations had run. It records it even over a newer
version: the older-project guard covers `apply` and `deploy`.

**What reconcile compares** (against the introspected live schema): columns
(name, type, nullability, default, identity), primary-key / unique constraints, **foreign
keys**, secondary **indexes**, **CHECK** constraints and **column comments** — the same
tables-and-enums model `dbd diff` reports on. Table comments are not compared. Bare enum types are schema-qualified to match introspection, and so is a type an extension installs outside `public`, however the DDL reached it (`citext` → `extensions.citext`, `vector(384)` → `extensions.vector(384)`); common type aliases are
normalized (`int4` → `integer`, `timestamptz` → `timestamp with time zone`). Foreign keys are
matched by shape (columns, referenced table/columns, actions), so the design's inline `references …`
and the live DB's auto-named constraint reconcile to no change; a declared FK the live DB lacks is
added (`ADD FOREIGN KEY …`, Postgres auto-names it), and an FK the design dropped is removed
(`DROP CONSTRAINT …`, gated behind `--allow-destructive`). Indexes converge the same way: matched by
shape (unique flag, access method incl. `USING gin/gist/brin/hash`, ordered columns) so a same-shape
index under a different name is no change; a declared index the live DB lacks is added
(`CREATE INDEX IF NOT EXISTS …`) and one the design dropped is removed (`DROP INDEX …`, gated behind
`--allow-destructive`). An index that backs a PK/UNIQUE constraint is ignored on both sides — told
apart by the constraint's name, not its columns, so a separate index over a key's columns is
compared like any other, and dropping it from the database is seen.
Check constraints converge too, matched by canonicalized expression rather than name (an authored
`status in ('a','b')` matches the `status = ANY (ARRAY[…])` Postgres reports).

**Enum value removal** is the other change reconcile performs in place. Postgres has no
`ALTER TYPE … DROP VALUE`, so removing a value from an enum in the design recreates the type:
the managed views that read a table with a column of the enum — and the views over those — are
dropped (deepest first), column defaults are taken off, the type is renamed aside and recreated
with exactly the declared values in declared order, each dependent column is moved across with
`USING <col>::text::<type>` (`::text[]` for an array column), the defaults are restored, the type's
`COMMENT ON TYPE` is carried over, and the displaced type is dropped. An index that reads the column — a partial index's predicate, an
expression key — is dropped and rebuilt around the swap; left in place, Postgres re-reads it
against the renamed type and fails with `operator does not exist`. Only views are dropped: a view is what blocks `ALTER COLUMN … TYPE`, and
tables, functions and procedures that merely reference the table are never dropped. The dropped
views are restored later in the same run, by the pass that re-applies every managed view. Gated
behind `--allow-destructive`.

The recreation runs as one statement batch, in a single implicit transaction, so every way it can
fail undoes the whole recreation and names the object — and stops the run, with the steps before it
applied: a row still holding the removed value fails the cast, naming its column; a default
naming a removed value fails the `SET DEFAULT`; and a dependent view dbd does not manage fails the
`ALTER … TYPE`. (This is distinct from an *orphaned* enum, i.e. the whole type gone from the
design, which is only ever warned about.)

A *materialized view* in the database that reads the column — directly or through views — blocks
the swap, and dbd never drops one. So the run refuses, before anything is written and under
`--dry-run` too, with a non-zero exit, naming each one and the way through:

```
reconcile cannot remove enum value 'archived' from app.mood: materialized view app.people_stats reads a column of that type. dbd never drops a materialized view, so nothing was changed. To go on:
  1. drop it by hand: DROP MATERIALIZED VIEW "app"."people_stats";
  2. re-run `dbd reconcile --allow-destructive`: it removes the value and re-creates, from the design, the materialized views it declares
```

The question is asked of the database, not the design: once the matview is dropped, the re-run
removes the value and re-creates the matview from its DDL. Before 0.25 reconcile decided from the
design, planned nothing for the enum, warned, and exited 0 "Reconciled" — and following its advice,
the re-run re-created the matview and declined again.

**Primary keys and unique constraints** are matched by their **columns**, never by their name, so the
design's unnamed `primary key (a, b)` and the live DB's auto-named `t_pkey` over the same columns
reconcile to no change. Changing which columns a PK covers is a replacement — `DROP CONSTRAINT IF
EXISTS "<live name>"` followed by `ADD PRIMARY KEY (…)`, in that order, gated behind
`--allow-destructive`. It runs in place on a table that already holds rows; no table rebuild and no
snapshot are needed. The new key columns must already be `NOT NULL` and unique across existing rows,
or Postgres rejects the `ADD`: that table's `ALTER` script rolls back — the old key stays — and the
run stops, with what earlier steps wrote left committed.
Because introspected and hand-written DDL can spell a default or exotic type differently,
reconcile may occasionally emit a redundant (harmless) `ALTER … SET DEFAULT`/`TYPE`; review with
`--dry-run` first.

**In a workspace** reconcile goes over the modules a run selects, each as its own
owner. A released module changes by snapshot and `dbd apply`: the module
`--module` names is refused if it is released, and a released module the run
takes in otherwise — a dependency included — is left out, saying so, before the
manifest's hooks attach, so they run around the modules reconciled:

```
module shared is released — it changes by `dbd snapshot` and `dbd apply`; reconcile leaves it out.
```

A module's `--prune` keeps what another module of the workspace declares: it is
moving there, and that module's run takes it over (see
[`dbd split`](#dbd-split) and [sharing a database](14-sharing-a-database.md)).

---

## `dbd diff`

Read-only. Show the complete difference between the live database and the design:
columns, PK/unique, foreign keys, CHECK constraints, indexes, column comments and enum
values, plus materialized-view drift. Never writes, and available even after `dbd release`.

`diff` and `reconcile` compare the same model. The difference is what each is for:
`diff` changes nothing and works on a released project, with JSON and an exit code
for CI; `reconcile` (pre-release) changes the database, and leaves alone what it is
not allowed to touch — orphan tables without `--prune`, orphan enums, drifted
matviews, destructive changes without `--allow-destructive` — all of which `diff`
still shows.

```sh
dbd diff -d $DATABASE_URL                 # Human report (summary + SQL)
dbd diff -d $DATABASE_URL --json          # Structured diff for tooling/CI
dbd diff -d $DATABASE_URL --exit-code     # Exit 0 in sync, 2 on drift, 1 on error
dbd diff --scope hub -d $HUB_URL          # Restrict to a scope's working set
```

The desired schema is parsed from your DDL; the live schema is introspected. dbd normalizes
representation noise so only real drift is reported: type aliases and array/date-time
spellings (`int4` → `integer`, `text[3]` → `text[]`, `time` → `time without time zone`),
default casts (so a `nextval('s')` default matches the `nextval('s'::regclass)` Postgres
reports), a `nextval` default's sequence fully qualified on both sides (the live one by the
schema its sequence is in, whatever path the session has; the design's as written —
`nextval('App.Seq')` and `nextval('"app"."seq"')` are `app.seq`, `public.` kept — and a
bare name in the first schema its file's path declares the sequence in, else the path's
first), a column turned generated (rebuilt, so what its drop takes is shown coming back
after it),
enum and extension-type qualification (`citext` → `extensions.citext`), PK/UNIQUE-backing indexes (matched by the constraint's name, so a
dropped index over a key's columns is still drift), FK `NO ACTION` defaults and
auto-generated FK names, and CHECK-expression *spelling* via the Postgres parser — parens
and case, `IN` ↔ `= ANY (ARRAY[…])`, casts on literals, `BETWEEN` ↔ the `>=`/`<=` pair
Postgres stores in its place, casts on `ARRAY[…]` constructors, and `AND`/`OR` nesting.
A CHECK expression that can't be parsed is still shown, flagged `advisory` so you verify it
by hand.

Under `--scope`, `diff` compares the scope's working set and refuses on a dependency
gap, like `apply`. A database [pinned](#scope-guard) to another scope is noted, not
refused — what that scope does not build would otherwise read as missing with no word
of why. Objects an extension owns are left out on both sides.

**Coverage:** diff covers **tables and enums** (their columns, keys, constraints,
indexes, column comments and values) plus **materialized-view drift** — each
matview is reported as `missing` (in the design, not the DB), `drifted` (deployed
definition differs from the design, by the `dbd:hash` drift stamp dbd recorded
in `dbd.matview_stamps`), `unstamped` (in the DB with no stamp dbd recorded), or
`orphan` (in a managed schema, not in the design). Matview drift counts toward
`--exit-code` and appears in `--json` (`matview_drift`). A key another table's foreign key
holds is shown changing as `reconcile` changes it: that foreign key's drop above the table's
own SQL and, when the design keeps it, its re-add below — or, when the foreign key is on a
table outside the run, that reconcile refuses the change (`--json`: `key_holders`).
Views/functions/roles/
sequences are not structurally diffed, and table, view and enum comments are not
compared.

**Best-effort edges** (reported, but may need a human glance): a foreign key that references a
table in the *same non-public schema* without qualifying it can read as drift; a standalone
index whose columns exactly match a PK/UNIQUE constraint is treated as that constraint's
backing index and suppressed; an unparseable CHECK is shown with an `advisory`. A bare
`nextval('s')` whose sequence the design declares nowhere is taken to be in its file's first
path schema, so one a hook or another project makes elsewhere reads as drift. A negative default (`-1`, which Postgres reports as
`'-1'::integer`) and a `varbit` column read as drift on every run.

**In a workspace** `diff` answers for every module the run selects, under its
header. `--exit-code` exits 2 when any module drifts, and `--json` writes one
document, each module's diff under its name — a project's is its diff, as
before — with the headers on stderr:

```json
{ "modules": [ { "module": "shared", "owner": "shop/shared", "diff": { … } },
               { "module": "core", "owner": "shop/core", "diff": { … } } ] }
```

---

## `dbd release` (alias `dbd baseline`)

Cut the first version: write a **baseline snapshot** at the current `project.version` (the anchor
future `dbd snapshot` diffs against) and set `project.released: true`, locking the project into the
snapshot/migration workflow and disabling `dbd reconcile`.

```sh
dbd release --name "v1 GA"    # Baseline snapshot + released: true
dbd baseline                  # Alias
```

Refuses if the project is already released or the design has entity errors (run `dbd inspect`
first). After release the flow is the ordinary one: edit DDL → `dbd snapshot` → `dbd apply`.

**A project that already has snapshots** — one `dbd init --from-db` or `dbd merge` built, which
write snapshots without setting `released` — is released at the version its latest snapshot
reached, with no second baseline (`--name` is then unused, and says so). It refuses while the
design has changes no snapshot records — capture them with `dbd snapshot`, then release — and
when `project.version` disagrees with the latest snapshot. See
[recommended workflows](06-recommended-workflows.md#snapshots-but-not-released).

`dbd release` sets `released: true` by editing `design.yaml` in place, keeping its comments,
blank lines and flow-style lists.

**In a workspace** each module releases on its own, at its own version, its
baseline in its folder, written under `modules.<name>.released`: `--module`
releases that module; a bare `dbd release` releases every module not yet
released, saying of each that is (`module shared is already released.`), and
refuses only when every module is (`Every module is already released.`). Every
module is checked before any is written, so a release refused for one module
leaves design.yaml as it was.

---

## `dbd export`

Export table data to files.

```sh
dbd export                              # Every table (or what export: lists) as CSV
dbd export --name config.lookups        # One table — it must be in export:, when export: is set
dbd export --format tsv                 # TSV format
dbd export --format jsonl               # JSONL format
dbd export -n activity.hook_events -f jsonl -o import   # → import/activity/hook_events.jsonl
dbd export -n core.accounts -o import/staging            # → import/staging/accounts.csv
dbd export --scope hub                  # Only what the 'hub' scope builds
```

Files land at `export/<schema>/<name>.<format>` in the project (`-s`), wherever you run from
(SQLite, with no schemas: `export/<name>.<format>`). **`-o`/`--output <dir>`** puts the same layout
under `<dir>` instead — a relative `<dir>` taken from the current directory — so `-o import`
exports straight into the `import/` tree for a round trip. The schema folder is what keeps two
same-named tables apart: before 0.25 `-o` wrote every file flat, and the second of two such tables
overwrote the first. Generated columns are left out of every export, as an import refuses a file
that sets one. `-f`/`--format` stays the format (`csv`/`tsv`/`jsonl`, default `csv`; any other is
refused rather than written as CSV); a per-table `format` under `export:` wins over it.

**Into an import schema folder.** When `-o` names a schema folder of the project's own `import/`
tree — `import/<schema>`, or `import/<env>/<schema>` — each file is written there as
`<table>.<format>`, the way that import reads it: `export -n core.accounts -o import/staging`
writes `import/staging/accounts.csv`, which the next import loads into `staging.accounts`. That is
how a long-running system's data moves to a new database (see
[import and export](10-data.md)). Written under a schema folder of its own instead —
`import/staging/core/accounts.csv` — the file would sit where no import reads. Two exported tables
that would share one file there (`core.accounts` and `audit.accounts`) are refused before anything
is written: an import loads each file there into that schema, so export into the folder, with `-n`,
the one table whose rows the file should hold. Exported anywhere outside `import/` — the default
`export/` — each table keeps its own schema folder.

A JSONL file writes every column's key; `null` is SQL NULL, and a json/jsonb column holding the
JSON value `null` is written `{"$json": null}`, which an import reads back as that value. A SQLite
BLOB is written as `\x` and its bytes in hex, as PostgreSQL writes a `bytea`, in every format, and
read back into a BLOB column on import.

With no `export:` list, every table is exported. With one, only what it lists — tables, and
views or materialized views, which export the rows their query returns. A `-n` that selects
nothing is an error: not listed under `export:`, not a table when there is no list, no such
entity, or outside the scope. A listed entry that names no table or view is warned about.
`--scope`/`--deps` further restrict the export to the scope's working set, and each `export:`
entry the scope leaves out is named (`export: X is outside scope 'S' — not exported`). The run
counts what it writes by kind — `Exporting 2 table(s) and 1 view(s)...`. A database
[pinned](#scope-guard) to another scope is refused before a file is written. See
[import and export](10-data.md#export).

**In a workspace** export reads every module, or `--module`'s alone, each into
its own folder (`Files written to ./shared/export`); `-n` exports from the
module declaring the table, and a table no module the run selects declares is
refused (`no module selected declares app.x`).

---

## `dbd init`

Scaffold a new dbd project, then evolve it: edit DDL, apply, and snapshot each change.

<svg viewBox="0 0 600 116" role="img" aria-label="dbd init lifecycle: dbd init scaffolds the project; you edit the ddl/ folder; dbd apply pushes it to the database; dbd snapshot versions a change; then you loop back to edit for the next change." xmlns="http://www.w3.org/2000/svg" style="max-width:600px;width:100%;height:auto;color:currentColor">
  <defs><marker id="ar-init" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker></defs>
  <style>
    .fb{fill:none;stroke:currentColor;stroke-width:1.5}
    .fba{fill:currentColor;fill-opacity:.07;stroke:currentColor;stroke-width:1.5}
    .ft{font-size:12px;fill:currentColor;text-anchor:middle;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
    .fe{fill:none;stroke:currentColor;stroke-width:1.5}
    .fl{font-size:10px;fill:currentColor;fill-opacity:.75;text-anchor:middle;font-family:system-ui,sans-serif}
  </style>
  <rect class="fba" x="6"   y="20" width="118" height="44" rx="9"/><text class="ft" x="65"  y="46">dbd init</text>
  <rect class="fb"  x="166" y="20" width="118" height="44" rx="9"/><text class="ft" x="225" y="46">edit ddl/</text>
  <rect class="fba" x="326" y="20" width="110" height="44" rx="9"/><text class="ft" x="381" y="46">dbd apply</text>
  <rect class="fb"  x="478" y="20" width="116" height="44" rx="9"/><text class="ft" x="536" y="46">dbd snapshot</text>
  <path class="fe" d="M124,42 H162" marker-end="url(#ar-init)"/>
  <path class="fe" d="M284,42 H322" marker-end="url(#ar-init)"/>
  <path class="fe" d="M436,42 H474" marker-end="url(#ar-init)"/>
  <path class="fe" d="M536,64 V94 H225 V68" marker-end="url(#ar-init)"/>
  <text class="fl" x="380" y="90">next change</text>
</svg>

(With `--from-db`/`--from-dbml`, dbd **skips the scaffold + hand-editing** — it reverse-engineers
the DDL and writes the baseline snapshot for you, so you go straight to `dbd apply`. See
[recommended workflows](06-recommended-workflows.md).)

```sh
dbd init                                # Postgres target, name from directory
dbd init --name myproject               # Custom project name
dbd init --target supabase              # Supabase target with externals + ignore patterns
dbd init --dry-run                      # list the files init would write, write nothing
```

`--target` takes `postgres` (the default) or `supabase`, the two targets with a scaffold; any
other is refused rather than given the PostgreSQL scaffold. `--dry-run` lists the files the
scaffold would write and writes none, and is refused as the run would be — an unknown target, a
`design.yaml` already there. (Before 0.27.4 it wrote the scaffold all the same.)

Creates design.yaml, the ddl/ type folders (`table`, `view`, `materialized_view`,
`function`, `procedure`, `enum`), an `import/` folder and a sample table DDL.
The Supabase target adds a `config` schema, the Supabase objects you reference
declared as `external:` (`auth.users`, `auth.uid`, `storage.objects`,
`storage.buckets`) and `ignore:` patterns for the platform schemas. It writes no
grants.

### Reverse-engineer from a database — `--from-db`

Generate a whole project from an existing database instead of the sample scaffold. dbd
introspects the catalog, reconstructs canonical `CREATE …` DDL, and writes
`ddl/<kind>/<schema>/<name>.ddl` for each table, view, materialized view, enum, sequence and
routine (`ddl/role/<name>.ddl` too, with `--roles`). Extensions and schemas get no files, because
dbd reads neither from `ddl/`: the extensions go in the target's `extensions:` list with their
`schema:`, the schemas under `schemas:`, and a table a foreign key reaches outside what was
captured is declared `external:`, so the reference resolves. The DDL is re-runnable
(`IF NOT EXISTS`, `OR REPLACE`; an enum inside a `DO … EXCEPTION WHEN duplicate_object` block),
so the brownfield path — reverse, then `dbd apply` to the same database — records the version and
changes nothing, and the `dbd diff` after it is empty; applied to a fresh database, the project
builds the same schema. A capture narrowed by `--schema` does too, on a database that has what the
run names as needed (below).

```sh
dbd init --from-db postgres://user:pass@host/db   # new project from a live Postgres DB
dbd init --from-db sqlite://./app.db              # …or a SQLite database (file:/sqlite:: also work)
dbd init --from-db                                # connection from $DATABASE_URL (or -d)
dbd init --from-db $DATABASE_URL --version 3      # base project.version (default 1)
dbd init --from-db ... --schema config --schema staging   # only these schemas (Postgres)
dbd init --from-db ... --exclude-schema audit             # drop a schema
dbd init --from-db ... --all-schemas              # include Supabase platform schemas
dbd init --from-db ... --dry-run                  # print the plan, write nothing
```

The **dialect is taken from the connection URL scheme** (`postgres://` → a `postgres` target,
`sqlite://`/`file:` → a `sqlite` target). **SQLite** is captured verbatim from `sqlite_master`:
tables (with their CHECK constraints, type affinity, `AUTOINCREMENT`, `WITHOUT ROWID`), user
indexes, and views — losslessly — with each table's and view's triggers written into its file
after it, as `CREATE TRIGGER IF NOT EXISTS`. SQLite has no
schemas/enums/functions/sequences/roles, so those don't apply. Files land flat
(`ddl/table/<name>.ddl`) and `design.yaml` has an empty `schemas:` list. (`dbd inspect` on a
SQLite project may emit benign schema-qualification warnings for cross-object references; the DDL
is valid and applies.)

| Flag | Description |
|------|-------------|
| `--from-db [CONN]` | Reverse-engineer instead of scaffolding. With no value, the connection resolves from `-d`/`--database` then `$DATABASE_URL`. |
| `--from-dbml <FILE>` | Reverse-engineer from a DBML file instead of a connection; mutually exclusive with `--from-db`. Produces schemas, enums, tables and foreign keys (see note below); `--target postgres` (default) or `supabase`. |
| `--name NAME` | `project.name` (default: the database name from the connection). |
| `--version N` | Base `project.version` written to `design.yaml`, and the version of the baseline snapshot (default `1`). |
| `--schema S` | Limit to exactly these schemas (repeatable). |
| `--exclude-schema S` | Add to the exclusion set (repeatable). |
| `--all-schemas` | Bypass the Supabase platform denylist, which only a Supabase database has (Postgres internals — `pg_catalog`, `information_schema`, `pg_temp*`, `pg_toast*` — are still always excluded). |
| `--roles` | Also reverse-engineer roles (off by default; cluster-global, platform roles filtered — see "What's captured"). |
| `--dry-run` | Print the plan and exit; touch nothing. |

**Version-tracked from the start.** After writing the `ddl/` tree and `design.yaml`,
`init --from-db` emits a **baseline snapshot at `--version`** (default 1) — so the new
project lands with `snapshots/{NNN}.json` and `design.yaml`'s `project.version` set to
that version, ready for `dbd snapshot`/`dbd apply`. `--dry-run` previews this
(`would create baseline snapshot v{N}`) and writes nothing. It does not set
`project.released` — see
[snapshots but not released](06-recommended-workflows.md#snapshots-but-not-released).

The target it writes is named `supabase` when the database has both an `auth` and a `storage`
schema — Supabase's own, so the project gets [Supabase's rules](08-targets.md#what-the-supabase-key-changes)
— and `postgres` (or `sqlite`) otherwise. Supabase's platform schemas identify it even though
they are left out of what is written.

**Schema selection.** Postgres internals are always excluded. On a Supabase database — one with
both an `auth` and a `storage` schema — its platform schemas (`auth`, `storage`, `realtime`,
`extensions`, `graphql*`, `vault`, `pgsodium*`, `supabase_*`, `cron`, `net`, …) are left out by
default, and `--all-schemas` includes them. On any other PostgreSQL database those names are
ordinary schemas and are captured: before 0.25 `auth`, `extensions`, `cron` and `net` were dropped
from every database, pgcrypto with them, without a word. `--schema` (allowlist) and
`--exclude-schema` compose with these. Every schema left out is named with the reason, and so is
an extension installed in one; a foreign key into one makes its table `external:`:

```
  left out: schema `auth` — a Supabase platform schema (pass --all-schemas to include it)
  left out: schema `audit` — excluded by --exclude-schema
  left out: extension `pgcrypto` — installed in schema `extensions`, which is left out
  external: auth.users — a foreign key references it, outside the captured schemas
```

A schema nothing was captured in — an empty `public`, say — is left out of `schemas:` unless
`--schema` named it (`left out: schema `public` — nothing in it to capture`): listed, it would
claim a schema the project holds nothing in, which `reset --clean` would drop.

**A capture of some schemas says what it needs from the rest.** Under `--schema`, an extension the
captured objects use is declared in `extensions:` when it is installed in `public` or in a
captured schema. Anything else they need from a schema left out — a type, a function, a table a
view reads, an extension installed there — is printed as a `needs:` line naming what uses it,
since a database without it cannot take the project (Supabase's platform schemas aside, which a
Supabase database provides):

```
  needs: type `util.mood`, in schema `util`, which is left out — used by app.child; a database without it cannot take this project (capture it with --schema util, or create it first)
```

**Secrets stay out of the repo.** The generated `design.yaml` target URL is written as the
literal `$DATABASE_URL` env reference — never the connection string you passed.

`init --from-db` refuses to run in a directory that already has a `design.yaml` (use
`merge` to sync into an existing project). It reads PostgreSQL/Supabase and SQLite
connections — the database decides, and `--target` is ignored. `--from-dbml` produces a
PostgreSQL project; DBML names no platform, so `--target postgres` (the default) or
`--target supabase` chooses the target, and any other is refused.

**Managed databases & version safety.** `init --from-db` is for a project the database does **not**
manage. If `dbd.meta` (or a legacy `_dbd_meta` in any schema) records the project it is creating, it
**refuses** and points you at `merge`, which knows how to reconcile a managed database into its own
project safely. Another project's record is no bar: a second project onboards beside the first,
leaving out what other projects own — named as it goes — and declaring `external:` what it keeps
that depends on one. It refuses while a project in the database has not yet run a dbd that records
what it owns. See [sharing a database](14-sharing-a-database.md#onboarding-beside-another-project).

**What's captured (and what isn't).** Reverse-engineering covers the **data model**:
schemas, extensions, enums, tables (columns, defaults, identity, PK/FK/unique/check
constraints, indexes incl. `USING gin/gist/brin/hash`, partial (`… WHERE …`) and expression
(`lower(name)`) indexes and key order, and table + column comments), views,
**materialized views** (definition + indexes from `pg_matviews`, written to
`ddl/materialized_view/<schema>/<name>.ddl`),
and **functions & procedures** (full bodies, captured verbatim via `pg_get_functiondef`;
overloads of the same name share one file).

**Triggers** are written into their table's file after the table — `CREATE OR REPLACE TRIGGER`,
or `DROP TRIGGER IF EXISTS` then `CREATE CONSTRAINT TRIGGER` for a constraint trigger, which
PostgreSQL will not replace — and a view's `INSTEAD OF` triggers into the view's file, which is
then applied after the function they fire. A trigger that is disabled, or enabled for replica
sessions only or always, is named, since the file creates it plainly enabled. **EXCLUDE constraints**, a column's **collation** (`COLLATE "C"`), and a foreign key's
`DEFERRABLE`, `INITIALLY DEFERRED` and `MATCH FULL` are captured as written, and `diff` and
`reconcile` compare and converge them.

**Everything an extension owns is left out** — tables, views, materialized views, sequences,
enums, functions and schemas — since the extension creates it; captured, a `reset` would fail on
it. `diff` and `reconcile` leave the same objects out.

**Much of what the model cannot hold is named rather than weakened in silence.** A partitioned
table is captured as one plain table holding every row, its partitions folded into it, with a
warning naming its partition key and each partition's bounds — partitions are not created, and
anything defined on a partition alone is not captured. A constraint that is `NOT VALID`,
`NOT ENFORCED` or `NO INHERIT` is captured as an ordinary one, and named; so are a deferrable
primary key or UNIQUE, an `UNLOGGED` table (captured logged) and a table that inherits another
(captured as a plain table with every column it has):

```
  warning: table app.scratch is UNLOGGED in the database — dbd's table model has no persistence, so it is captured as an ordinary, logged table: …
  warning: constraint booking_room_ck on app.booking is NOT VALID in the database — the rows already there were never checked against it. …
```

Some of it is still dropped without a word. A domain or a composite type is neither captured nor
named, so the project fails on a fresh database at the first column typed with one (`type
"app.posint" does not exist`). An identity column's `START` and `INCREMENT`, a table's storage
parameters (`fillfactor`), a column's `STORAGE`, row-level security and its policies, and the
comments on constraints, indexes and functions are lost. And a default holding a backslash reads
as drift in `diff` even against the database it came from:
`ALTER COLUMN d SET DEFAULT e'a\\b'`.

A view's or materialized view's own comment is written as `COMMENT ON VIEW` /
`COMMENT ON MATERIALIZED VIEW` — except a drift stamp an earlier dbd left as a matview's
comment, which is dbd's and not yours. Every key is written with its name —
`constraint "people_pk" primary key`, and the same for unique constraints, foreign keys and
CHECKs, the names Postgres generated (`users_pkey`) too — so the rebuilt design matches the
database name for name. An enum's comment is kept, as `COMMENT ON TYPE`.

The reversed DDL is fully qualified and opens with no `SET search_path`, so `dbd inspect`,
`apply` and `deploy` note that on every file; the note is harmless here, since no name in the file
depends on the path.

**Roles** are captured only with the opt-in **`--roles`** flag (they are cluster-global, not
owned by the database). System/platform roles are always filtered out — superusers, `pg_*`,
cloud roles (`rds_*`/`azure_*`/`cloudsql*`), and Supabase platform roles (`anon`,
`authenticated`, `service_role`, `authenticator`, `supabase_*`, `pgsodium*`, `dashboard_user`,
`pgbouncer`, `postgres`). Roles are captured as name + memberships only (no attributes, never
passwords); memberships referencing filtered-out roles are dropped so the emitted set is
self-contained.

**Sequences** are captured: standalone sequences become `ddl/sequence/<schema>/<name>.ddl`
(`CREATE SEQUENCE IF NOT EXISTS …`), while sequences owned by a `serial`/`IDENTITY` column are reproduced
through the column itself — such columns emit `serial`/`bigserial`/`smallserial` or
`GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY` rather than a separate sequence file.

**From a DBML file (`--from-dbml`).** A DBML source produces **schemas, enums, tables and foreign
keys** — DBML cannot express functions, procedures, views, standalone sequences or roles, so none
of those appear. What `dbd dbml` writes comes back: CHECK constraints (from `checks`) with
their names, identity columns with `ALWAYS` kept apart from `BY DEFAULT`, `serial` columns,
generated columns, a column's `UNIQUE`, partial and expression indexes and their key order, an
enum column's schema-qualified type, a SQL value keyword default (`current_date`), and a column
comment's line breaks. A table-level UNIQUE comes back as its named constraint, and so do a named
primary key and a named foreign key; an index keeps its method, `INCLUDE` columns, operator classes
and storage parameters. An enum's comment is lost — DBML cannot express one — and so are a foreign
key's deferral and `MATCH FULL`, `EXCLUDE` constraints and collations. Every unnamed index comes
back named `<table>_idx`, so a second unnamed index on one table collides on `apply`: name them in
the DBML. A hand-written document is read as @dbml/core reads it, notes dedented included — see
[reading DBML](#dbd-dbml).

A stub table — one the document carries only because a foreign key lands on it, marked by the
note `dbd dbml` gives it ("External entity — managed outside this project", "Defined in this
project, outside this document — only the referenced columns are shown", "Referenced, but not
defined in this project") — is read back as an `external:` entry, never as a table to build, and
its schema is not added to `schemas:`; a Ref does not add its target's schema either. Each is
named as it is declared (`external: auth.users — a foreign key references it, outside the captured
schemas`). Before 0.25 a stub came back as a one-column managed table with its schema in
`schemas:`, so the next `apply` built a copy of somebody else's table. All the schema-selection
flags (`--schema`/`--exclude-schema`/`--all-schemas`) still filter the parsed entities; `--roles`
is a no-op since DBML has no roles.

Not yet captured from a database:

- Aggregates, operators, domains, composite types — not named either (see above).
- dbd's own bookkeeping — the `dbd` schema (`dbd.meta`, `dbd.migrations`, `dbd.matview_stamps`)
  — is always excluded.

Column order reflects the database's **physical** order (which can differ from a
hand-authored file after `ALTER TABLE ADD COLUMN`). Run `dbd format` on the result to
normalize the output to your project's conventions.

---

## `dbd merge`

Sync a database into the **current** project — reverse-engineer, then reconcile against the
files already on disk. Same introspection and emitter as `init --from-db`, but it never
creates `design.yaml`, and edits only its `project.version`.

<svg viewBox="0 0 540 104" role="img" aria-label="dbd merge flow: dbd merge re-introspects the database, overwrites the DDL and auto-creates a snapshot of the diff, then dbd apply brings the database forward. merge refuses if the database version is behind the project version." xmlns="http://www.w3.org/2000/svg" style="max-width:540px;width:100%;height:auto;color:currentColor">
  <defs><marker id="ar-merge" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker></defs>
  <style>
    .mb{fill:none;stroke:currentColor;stroke-width:1.5}
    .mba{fill:currentColor;fill-opacity:.07;stroke:currentColor;stroke-width:1.5}
    .mt{font-size:12px;fill:currentColor;text-anchor:middle;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
    .me{fill:none;stroke:currentColor;stroke-width:1.5}
    .ml{font-size:10px;fill:currentColor;fill-opacity:.75;text-anchor:middle;font-family:system-ui,sans-serif}
  </style>
  <rect class="mba" x="6"   y="18" width="130" height="44" rx="9"/><text class="mt" x="71"  y="44">dbd merge</text>
  <rect class="mb"  x="196" y="18" width="150" height="44" rx="9"/><text class="mt" x="271" y="44">+ snapshot</text>
  <rect class="mba" x="406" y="18" width="120" height="44" rx="9"/><text class="mt" x="466" y="44">dbd apply</text>
  <path class="me" d="M136,40 H192" marker-end="url(#ar-merge)"/>
  <path class="me" d="M346,40 H402" marker-end="url(#ar-merge)"/>
  <text class="ml" x="160" y="82">refuses if the DB is behind the project version</text>
</svg>

`merge` leaves out what other projects own in the database, naming each, and warns about what to
declare `external:` (see [sharing a database](14-sharing-a-database.md#onboarding-beside-another-project)).

`merge` overwrites the on-disk DDL from the database and **auto-snapshots the diff** as a new
version; you then `dbd apply` to bring the database forward. It **refuses** when the database is
behind the project version (a stale DB can't clobber newer work) — see
[version safety](06-recommended-workflows.md#version-safety).

```sh
dbd merge postgres://user:pass@host/db   # sync into the current project
dbd merge                                # connection from -d / $DATABASE_URL
dbd merge --from-dbml schema.dbml        # sync from a DBML file (no connection)
dbd merge --dry-run                      # preview the plan + the snapshot version
dbd merge --schema config --exclude-schema audit   # same selection flags as init
dbd merge --roles                        # also reverse-engineer roles (opt-in)
```

Every `merge` that proceeds **overwrites the introspected DDL into the project (no `.bak`)
and captures the delta as a new snapshot version** — foreign and dbd-managed databases
behave identically. The new snapshot plus version control are the record of what changed, so
`merge` deliberately clobbers on-disk drift rather than aborting or backing up. Each
generated file is still classified before writing:

- **created** — no file at the path → written.
- **unchanged** — file exists and is byte-identical → left untouched (re-runs are idempotent).
  Because generated DDL is run through the same formatter as `dbd format`, a file you
  generate, then `dbd format`, then re-generate stays **unchanged** (no spurious churn).
- **conflict** — file exists and differs from the database → replaced in place (no `.bak`).
- **orphan** — an existing `.ddl`/`.sql` file of a managed kind (table/enum/view) under a
  selected schema with no matching DB entity. **Orphans are reported, never deleted** — you
  handle removals.

```
[dry-run] 3 created · 12 unchanged · 1 conflict(s) (would overwrite, no .bak)
  conflict (differs from DB): ddl/table/app/orders.ddl
[dry-run] would capture changes as snapshot v4
```

A real run prints `N file(s) written · snapshot vN created`, where *written* counts the
created and conflicting files.

After writing, `dbd` reloads the project and runs `snapshot::create_snapshot`: if the DDL was
already in sync with the latest snapshot it reports `already in sync — no snapshot created`,
otherwise it writes the next snapshot version (a baseline if the project had none yet) and
bumps `design.yaml`'s `project.version`. `--dry-run` previews the plan and the snapshot
version that *would* be created, writing nothing.

`merge` requires an existing project (it refuses if there's no `design.yaml`; use
`init --from-db`). Because it edits nothing in the config but the version, it **warns** about
what the database holds that `design.yaml` should declare and does not — a schema it wrote files
for that is not under `schemas:` (only those: a schema holding nothing it wrote draws no warning),
an extension the target's `extensions:` does not list, and a table a foreign key reaches outside
the captured schemas, unless the project builds that table itself — so you can add each
yourself:

```
  warning: a foreign key references `billing.acct`, outside the captured schemas; declare it under external: in design.yaml
  warning: extension `pg_trgm` is installed in the database but not declared; add `{ name: pg_trgm, schema: ext }` to the target's extensions: list in design.yaml
```

What `design.yaml` already declares draws no warning. A table declared `external:` is never
captured — it is somebody else's, and written as a project file `reset` would drop it — and is
named instead (`left out: table `auth.users` — declared under external:`). A table's triggers are
written into its file, as `init --from-db` writes them — a SQLite table's too — so a merge no
longer deletes the triggers from the table files it overwrites. `merge --from-dbml`, whose
document cannot hold a trigger, keeps the triggers written in each table or view file it rewrites,
each after the `SET search_path` it was written under; a file that does not parse keeps none, and
is named (`… does not parse, so a trigger written in it is not kept by the merge`). What a file
cannot carry — partitioning, `UNLOGGED`, a `NOT VALID` constraint — is warned about as
`init --from-db` warns, and extension-owned objects are left out. On a Supabase database — or in a
project whose target is `supabase` — Supabase's platform schemas are left out, as `init --from-db`
leaves them out. Unlike `init --from-db`, a merge narrowed by `--schema` prints no `needs:` lines.

**Managed databases & version safety.** When the target is a **dbd-managed** database (`dbd.meta`
exists, or a legacy `_dbd_meta` in any schema), `merge` compares the database's applied version
**D** against the project's `design.yaml` `project.version` **Y** (treated as `0` when
unset). The only thing this changes is whether the merge proceeds at all:

- **D < Y → refuse.** The project is ahead of a stale database; overwriting project DDL from
  it would discard newer work. Bring the database up to date with `dbd apply`, or revert the
  project to v`D` via version control if you really mean to discard those changes. (There is
  no override flag.)
- **everything else** (a **foreign** database with no `dbd.meta`/`_dbd_meta`, or a managed database with
  **D ≥ Y**) → the overwrite + auto-snapshot path described above.

---

## `dbd split`

Divide a project into a [workspace](#workspaces) of modules. `--from-scopes` is
the one way it divides one yet: each region of the project's scopes becomes a
module.

```sh
dbd split --from-scopes                          # print the plan; change nothing
dbd split --from-scopes -v                       # … with each module's entities and each file's move
dbd split --from-scopes --rename shared=common   # name a module of the plan otherwise
dbd split --from-scopes --write                  # move the files, write modules: and scopes:
dbd split --undo                                 # put back what an interrupted --write moved
```

**The regions.** Each scope's entities are taken with their dependencies —
`deps: include`, whatever the scope says — and an entity's region is the set of
scopes holding it. What every scope deploys — a role from `ddl/role/` — is in
all of them; the manifest's schemas and extensions are every module's. The
region every scope holds (of two or more) is `shared`; a region only one scope
holds is named for that scope, `default` as `core`; one several hold, their
names joined with `-`; one no scope holds, `unscoped`. Empty regions are
dropped. `--rename <module>=<name>` names a module of the plan otherwise, each a
valid module name.

**The plan.** Without `--write`, `split` prints the plan and changes nothing:

```
sensei splits into 3 modules, by its scopes:
  shared   in default and dojo   1 entity
  core     only in default       1 entity   depends on shared
  dojo     only in dojo          1 entity   depends on shared
Its scopes become:
  default: modules [core], excludes [dojo]
  dojo: modules [dojo]
4 files move into the modules' folders, and 1 import script to scripts/. Run again with --write to move them and write modules: and scopes: into design.yaml.
```

Each module `depends_on` the modules its entities reference — a type a file
creates beside its table is that table's module's, as the workspace's gap check
reads it. Each scope becomes `modules:` — the modules of the regions it holds,
less those another of them depends on, which it takes along — keeping its
`extensions:`, with `excludes:` naming the manifest's schemas it did not deploy
(every module has them); an `all` scope stays `all`. A cycle among the modules'
dependencies is shown, and refuses `--write`.

**`--write`** moves each DDL file to the folder of the module its entity is in,
and each import data file and policy file to the folder of the module its table
is in — with `git mv` in a git work tree, so history follows the files — moves
the manifest's `import.after` scripts under `import/` to `scripts/`, rewriting
their paths, and writes `modules:` and the new `scopes:` into design.yaml in
place, the rest as it was — comments and line endings included, and through its
link if design.yaml is one:

```yaml
import:
  after:
    - scripts/loader.sql

modules:
  shared: {}
  core:
    depends_on: [shared]
  dojo:
    depends_on: [shared]

scopes:
  default:
    modules: [core]
    excludes: [dojo]
  dojo:
    modules: [dojo]
```

It is whole or not at all. It first makes the workspace in a scratch folder and
refuses unless it loads and deploys — under each scope, and with none — exactly
what the project does now (`as a workspace, the project would not deploy what it
does: …`, naming each difference). It writes the new manifest aside and keeps a
journal of its moves, `.dbd-split`, in the project folder, with the manifest as
it was; a move that fails — or a file git tracks that `git mv` will not move —
puts back every move before it, and a move git will not put back is made by
hand, saying to run `git add -A`. The journal goes when the split is done; while
it is there, every command that reads the project — `format`, `doctor` and `snapshot --list` among them — is refused:

```
Error: Failed to load design: Config error: a dbd split was interrupted here — its journal ./.dbd-split lists the files it moved, and design.yaml may not declare the modules yet: run `dbd split --undo` to put them back, then split again
```

`dbd split --undo` puts back each file the journal lists that has moved, newest
first, puts design.yaml back as it was, and removes the journal.

**Refused before anything moves:**

- a workspace (`declares modules: — it is a workspace already`), or a design
  without scopes;
- a project with `snapshots/` or `migrations/`: a migration is SQL over several
  tables and does not divide by module, so a project splits before its first
  release;
- a file that does not parse, since what it references — and so its module — is
  unknown;
- a schema only a file's path names — add it to `schemas:` first;
- a scope that leaves out what its entities need (`deps: report` with a gap) —
  as modules it would deploy that too; include it, or give the scope
  `deps: include`;
- a file of `ddl/`, `import/` or `policies/` that maps to no module, a DDL file
  declaring entities of two modules, two files moving onto one path, or onto a
  path already taken;
- a module name that cannot name an owner or a folder (give it another with
  `--rename`), or one that differs from another only in case;
- a module folder that already holds files, and a folder holding a module's DDL
  — an earlier split's leftovers;
- a file reached through a link from outside the project, which is another
  folder's to keep.

**Each database's first deploy** after the split runs the workspace transfer
rule: each module takes over, as it claims, what it declares from the project as
it was — and from the workspace's other modules, which is how an entity moved
between modules later changes hands — and says so, as `--adopt-from` does
(`[dry-run] would adopt …` in a preview):

```
adopting relation app.orders from "sensei"
```

Once its modules hold everything the project held, the project's record is
retired — its `dbd.meta` row, its uses and its claims of what is gone; its
`dbd.migrations` rows stay, as history:

```
project "sensei" is retired — its modules hold everything it held
```

Nothing else changes hands: an owner by any other name still refuses the run.
See [sharing a database](14-sharing-a-database.md).

---

## `dbd format`

Format DDL files to project conventions.

```sh
dbd format                         # Format all DDL files in-place
dbd format --check                 # Check formatting (exit 1 if any file would change)
```

Configurable via the [`format:` section](03-design-yaml.md#format) of design.yaml:

```yaml
format:
  keyword_case: lower        # lower | upper | preserve
  comma_style: leading       # leading | trailing
  type_alignment: 27         # column for type start, 0 = off
  indent: 2                  # spaces per level
  query_style: river         # river (default) | none — align SELECT bodies
  gutter: 10                 # river keyword-gutter width (fits "inner join")
```

Handles CREATE TABLE (full formatting), CREATE INDEX, SET, COMMENT ON. Inline column comments (`-- …` / `/* … */`) survive a CREATE TABLE reformat — they're recovered from the source and re-attached to the column they documented (trailing comments end-of-line, standalone comments on their own line above the column).

**Formatting changes layout and keyword case, and nothing else.** The file is read with PostgreSQL's own scanner and parser, through libpg_query: statement boundaries are the parser's (a `BEGIN ATOMIC … END` body is one statement; for a file it cannot parse whole — another engine's DDL — a `;` inside parentheses or inside a `CREATE TRIGGER`/`FUNCTION`/`PROCEDURE` `BEGIN … END` body, as SQLite writes triggers, ends nothing). A keyword is recased where PostgreSQL's grammar reads it as one, and a word it reads as a name keeps its case even when it is a keyword elsewhere — a column named `name`, `value` or `level` — while built-in type names are recased with the keywords. Every string literal of any kind (`E'…'`, `U&'…'`, `$$` and `$tag$` bodies), quoted identifier and comment is a single token, never split or recased. A statement's new layout is used only if it parses to the same tree — libpg_query's, deparsed; sqlparser's for SQL libpg_query cannot read — and every literal, body, quoted name and comment in it is byte-for-byte the same; failing that the statement is only recased, and failing that left exactly as written. A file the scanner cannot read at all (an unterminated string or comment) is left unchanged. Formatting twice changes nothing — `format` then `format --check` passes — and a comment beside a statement stays beside it, one above it above it. Before 0.25 the hand-rolled splitter closed `E'it\'s; tricky'` at `\'`, broke the default across three lines, and split a `$tag$` body at its own `;`s.

A table is laid out whatever its literals and names — a `U&'…'` or `$$` default, a quoted table or column name, a column's own CHECK, a named column constraint, and `TEMP`, `WITH (…)`, `PARTITION BY`, `INHERITS` or `TABLESPACE` around the column list. The layout is sqlparser 0.62's, so a table it cannot read is recased, not laid out: `UNLOGGED`, `LIKE`, `EXCLUDE`, `USING <method>`, `OF <type>`, `PARTITION OF`, identity options beyond one (`(START WITH 1 INCREMENT BY 1)`), a key's `INCLUDE`/`WITH`/`USING INDEX TABLESPACE`, `NO INHERIT`, `ON DELETE SET NULL (<columns>)`, a column's `STORAGE`/`COMPRESSION`, an `ARRAY` type (`int ARRAY`), `WITHOUT OIDS` and `U&'…' UESCAPE`; so is an enum type with a label holding a `'`. A word on the formatter's own keyword list — `data`, `type`, `key`, `comment` … — is recased even where it names a column (`DATA JSONB` in `upper` mode).

All three `keyword_case` modes do what they say, for every keyword in every statement: `preserve` leaves each keyword as written (it used to uppercase a lowercase file), and `upper` and `lower` reach `SET` and the column types of a table with a `serial` or `inet` column. Comments, string literals, quoted identifiers and `$$` bodies keep their case. `keyword_case` reaches every keyword the formatter writes, not only the statement's head: `GENERATED`, `ALWAYS`, `IDENTITY`, `STORED`, `MATCH`, `RESTRICT`, `ACTION`, `VALID`, `DATA` and every other keyword the grammar reads (`ATOMIC`, `COLLATE`, `BIT`, `PARTITION BY RANGE` …), index methods (`btree`, `hash`, `gin`, `gist`, `spgist`, `brin`) and built-in type names (`bool`, `int2`/`int4`/`int8`, `float4`/`float8`, `bytea`, `timestamptz`, `timetz`, `json`, `jsonb`, `inet`, `cidr`, `money`) are cased with their neighbours, so a column reads `generated by default as identity`, never `GENERATED by default as IDENTITY`.

A file is written back in the encoding it was read in: a UTF-16 file saved by SSMS behind its byte-order mark stays UTF-16, and a UTF-8 byte-order mark is kept. `inspect --fix` writes the same way.

**River style** is the **default** `query_style` (set `query_style: none` to disable). It right-aligns SQL keywords at the `gutter` column so the clause keywords form a "river" down the left edge, with leading-comma SELECT lists, alias alignment, and one condition per line in WHERE/HAVING/ON. It applies to `CREATE VIEW` bodies and standalone SELECTs. A query the river renderer can't reproduce faithfully (e.g. one using a CTE) is automatically left in plain keyword-cased form rather than risk altering it — so river formatting never changes what your SQL means:

```sql
    select lv.id
         , lv.value     as display_value
      from lookups       lkp
inner join lookup_values lv
        on lv.lookup_id = lkp.id
     where lkp.name     = 'Gender';
```

### Pre-commit

`dbd format --check` exits 1 when any file would change, so it drops into [pre-commit](https://pre-commit.com) directly. Repo ships `.pre-commit-hooks.yaml` with two hooks:

| Hook id | Language | Use when |
|---------|----------|----------|
| `dbd-format` | `rust` (cargo install on first run, cached after) | Contributors don't have `dbd` installed — **needs v0.12.3 or later** |
| `dbd-format-system` | `system` (expects `dbd` on PATH) | Contributors already installed dbd locally — works at any `rev` |

`dbd-format` builds dbd from the tag you pin, so that tag must carry an installable Cargo package at the repo root. Tags before v0.12.3 have a virtual manifest there, and pre-commit aborts during environment setup with `found a virtual manifest instead of a package manifest` — before it formats anything. Use `dbd-format-system` with those.

User's `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/sensei-hq/dbd
  rev: v0.27.3
  hooks:
    - id: dbd-format
```

Both set `pass_filenames: false` — the hook scans the project itself, so pre-commit invokes it with no positional args.

**In a workspace** format goes over each module's folder — or `--module`'s, or
the module folder dbd runs in — reading files, not designs.

---

## `dbd policies`

Apply RLS (Row-Level Security) policies from the `policies/` directory.

```sh
dbd policies                       # Apply all policy files
dbd policies --dry-run             # Show what would be applied
dbd policies --scope hub           # Only policies whose table is in the scope
dbd apply --with-policies          # Apply entities + policies in one step
```

Policy files in `policies/<schema>/<table>.sql` contain idempotent SQL:

```sql
alter table config.users enable row level security;
drop policy if exists "users_select_own" on config.users;
create policy "users_select_own" on config.users for select using (auth.uid() = id);
```

Failed policies are logged and skipped (fail-forward). Exit code 1 if any failures.
The full guide is [access control](09-access-control.md#row-level-security-policies).

### Policies under `--scope`

A scoped run applies only the policies whose table is in the working set. The
target comes from the file's own path — `policies/<schema>/<table>.sql` names
`<schema>.<table>` — so no parsing is involved and the rule is the layout you
already follow.

A policy for a table the scope excludes is skipped with a warning naming the
file, its target, and the scope:

```
Skipped ./policies/dojo/repository_metrics.sql — dojo.repository_metrics is
outside scope 'daemon'
```

This matters when one design serves several planes. Without it, a policy for a
schema that a given plane does not have runs anyway and reports
`FAILED … schema dojo does not exist` on every deploy — an expected condition
dressed as an error, which is how real failures stop being read.

Runs without `--scope` are unaffected: every policy file applies. `dbd policies --dry-run`
applies the same filter as the real run, listing what would apply and each file it would skip,
each under its own heading (`[dry-run] Would apply policies:`, `[dry-run] Would skip policies:`).
`dbd deploy --dry-run` counts them the same way.

**The scope guard.** `dbd policies` refuses a database pinned to another scope, as `apply`,
`deploy`, `reconcile`, `reset`, `import`, `export` and `refresh` do — an unscoped `dbd policies`
on a database built as `hub` would otherwise secure tables that database was never given:

```
Error: scope guard: this database is pinned to scope 'hub', but you requested 'all'.
Applying another scope's policies would secure tables this database was not built with.
→ re-run with --scope hub, or re-point the database with `dbd apply --scope all --allow-scope-change` first.
```

There is no `--allow-scope-change` here: `policies` never re-pins a database. With a database
configured, `policies --dry-run` checks the pin too, so the preview refuses where the run would;
with none configured it lists the files offline.

A file that is not in `policies/<schema>/<table>.sql` form has no derivable
target and always applies — scope filtering is a correctness aid, not a
security boundary. Do not rely on a scope to *prevent* a policy running.

**In a workspace** each module applies its own `policies/`. A policy failing in
one module is reported, the modules after it still run, and the run fails at
the end.

---

## `dbd doctor`

Audit and migrate project configuration and layout.

```sh
dbd doctor                         # Show issues
dbd doctor --fix                   # Apply fixes (config migration creates .yaml.bak)
```

Detects and migrates old Node.js config format:
- `project.database` → `source.dialect` + `target`
- Top-level `extensions`/`roles`, `supabase` and `convex` → under `target`
- `nullValue` → `null_value`
- `project.staging` → `import.staging`
- `project.extensionSchema` → a `schema` on each extension under `target`
- `project.dbdocs` → top-level `dbml`

Removes stale files that dbd now manages internally (`ddl/procedure/staging/import_jsonb_to_table.ddl`), and migrates **every non-canonical DDL type folder the scanner reads** to its singular name — the plurals (`ddl/functions/` → `ddl/function/`, `ddl/sequences/` → `ddl/sequence/`, etc.) and the `materialized_views`, `matviews` and `matview` spellings of `ddl/materialized_view/`. When a singular folder already exists the contents are merged; on a same-name collision the newer file is kept and the older is backed up as `<name>.ddl.bkp` (each backup is reported). See [DDL folder layout](02-getting-started.md#ddl-folder-layout).

**Misfiled view DDL.** dbd types an entity by its folder, so a `CREATE MATERIALIZED VIEW`
under `ddl/view/` is treated as a plain view — and `dbd reset` then fails on it with
`DROP VIEW` (`"… is not a view"`). `doctor` reads every file under `ddl/view/` and
`ddl/materialized_view/` and reports any whose `CREATE` statement disagrees with its folder:

```
Found 1 misfiled DDL file (folder disagrees with the CREATE statement):
  - ./ddl/view/analytics/daily_sales.ddl declares CREATE MATERIALIZED VIEW but lives in ddl/view/
      → move to ./ddl/materialized_view/analytics/daily_sales.ddl
Misfiled DDL files must be moved manually (see above).
```

`--fix` does not move these — the folder is the source of truth, so the move is yours.

**Extension and schema files.** dbd reads neither kind from `ddl/` — extensions are declared in
the target's `extensions:` list and schemas under `schemas:` — so `inspect` refuses a file under
`ddl/extension/`, and one under `ddl/schema/` naming a schema `schemas:` does not list. `doctor`
names the `design.yaml` entry each one becomes:

```
Found 3 DDL files dbd does not read (design.yaml declares extensions and schemas):
  - ./ddl/extension/citext.ddl → add `{ name: citext, schema: ext }` to the target's extensions: list in design.yaml, then delete the file
  - ./ddl/schema/app.ddl → `app` is already listed under schemas: — delete the file
  - ./ddl/schema/reporting.ddl → list `reporting` under schemas: in design.yaml, then delete the file
Extension and schema files must be replaced by hand (see above).
```

`--fix` leaves these too, and counts them the way it counts what it found —
`0 issues fixed, 3 left to fix by hand.` `doctor` always exits 0.

**In a workspace** doctor audits each module's folder, or `--module`'s. A
design.yaml that declares `modules:` is never migrated — the migration would
rebuild it without them — so its old entries are reported, to fix by hand, and
a design.yaml it cannot read is named as an issue, with the error.

---

## `dbd install`

Install dbd's Claude Code assets — the `dbd` skill and the `dbd-pattern-verifier`
agent — so an AI assistant working in your project understands the schema-as-code
conventions. The assets are embedded in the binary, so this needs no repo checkout
or network access.

```sh
dbd install                # Install globally into ~/.claude (skills/ + agents/)
dbd install --project      # Install into the current project's ./.claude instead
dbd install --dry-run      # Preview what would be written, touch nothing
```

Writes `skills/dbd/SKILL.md` and `agents/dbd-pattern-verifier.md` under the target
`.claude` directory, reporting each asset as created, updated, or unchanged (so an
overwrite on upgrade is never silent). Re-run after upgrading dbd to refresh them.
The global target honours `$CLAUDE_CONFIG_DIR` if set, else `~/.claude`. Restart
Claude Code afterwards to load the new skill/agent.

---

## Environment variables

| Variable | Used by |
|----------|---------|
| `DATABASE_URL` | Default database connection — overrides `target.url`, overridden by `-d`. Exported but empty, it does not override `target.url`; a `target.url: $DATABASE_URL` it leaves unset or empty is refused by name, with how to set it |
| `$NAME` in `target.url` | A `url: $NAME` value is read from the environment |
| `GITHUB_TOKEN` | `dbd deploy` from a private GitHub repository — sent as a bearer token to `api.github.com` only |
| `DBD_DIAGRAM_URL` | Base URL of the viewer `dbd diagram` opens (default `https://dbd.sensei-hq.com`); `--site` overrides it |
| `DBD_NO_TX` | Set (to any value) to apply without the single transaction — for DDL such as `CREATE INDEX CONCURRENTLY` |
| `CLAUDE_CONFIG_DIR` | Global target dir for `dbd install` (default: `~/.claude`) |

The GitHub download cache lives in the platform cache directory —
`$XDG_CACHE_HOME/dbd` or `~/.cache/dbd` on Linux, `~/Library/Caches/dbd` on macOS.
