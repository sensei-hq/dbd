# Scopes and multi-database deploys

A **scope** is a named subset of the design's entities. It lets one
`design.yaml` build more than one database — a full primary database and a
smaller one that needs only some schemas, say — without forking the DDL.

A scope says *what* to build, never *where*. The database is chosen per run
with `-d`/`--database` (or `$DATABASE_URL`), and the platform comes from the
first `target:` entry (see [Targets and adapters](08-targets.md)). So one design
deployed to two databases is one target, two scopes, two URLs:

```sh
dbd deploy --scope hub -d $HUB_URL      # only what the hub needs
dbd deploy -d $MAIN_URL                 # no --scope: the `default` scope, or everything
```

## Defining a scope

```yaml
scopes:
  hub:
    includes: [config, app.users, app.sessions]
    deps: report
  reporting:
    excludes: [staging.*, app.audit_log]
  everything: all
```

Each entry in `includes` and `excludes` takes one of three forms:

| Form | Selects |
|---|---|
| `schema` | every entity in the schema **plus the `CREATE SCHEMA` entity itself** |
| `schema.entity` | one entity (`app.users`) |
| `schema.*` | every entity in the schema, **without** the schema entity — the same `prefix.*` syntax `ignore:` uses |

The difference matters for `excludes`: `excludes: [staging]` drops the staging
schema's entities *and* its `CREATE SCHEMA`, while `excludes: [staging.*]` drops
the entities and keeps an empty `staging` schema. For `includes` the two are
equivalent — a schema whose entities are present is added back.

`includes` omitted means "start from everything"; `excludes` removes from
whatever `includes` produced. A name or wildcard that matches nothing is an
error, so a typo cannot quietly select less than you meant.

A scope selects tables, views, materialized views, functions, procedures,
enums and sequences. A sequence belongs to its schema the way a table does: a
bare schema or `schema.*` takes the schema's sequences with it, and
`schema.sequence` names one.

A scope also keeps the `CREATE SCHEMA` of the schema each extension it installs
goes into (the extension's `schema:`), so a scope that selects nothing in that
schema still has it. A scope that excludes that schema by name while keeping
the extension contradicts itself — the extension would have nowhere to go — and
every command refuses it where it resolves the scope:

```
Error: Config error: scope 'reporting' keeps an extension whose schema it excludes ('vector' installs into 'extensions') — stop excluding that schema, or leave the extension out of the scope's `extensions:` allowlist
```

A scope may also be the bare string `all` (`everything: all` above): the full
set under another name. Any other bare string is refused.

| Field | Default | Meaning |
|---|---|---|
| `includes` | (everything) | Schemas, entities or `schema.*` wildcards to start from |
| `excludes` | (nothing) | Schemas, entities or `schema.*` wildcards to remove |
| `deps` | `report` | `report`: a dependency gap is an error. `include`: pull the dependencies in |
| `extensions` | (all) | Which of `target.extensions` this scope installs — see [below](#extensions-per-scope) |

## The reserved names: `all` and `default`

- **`--scope all`** is always the full design. It is resolved before the
  `scopes:` block is consulted, so a scope you define *named* `all` can never be
  selected — nothing warns about it, so don't define one.
- **`default`** is what a run without `--scope` uses, when you define one.
  Without a `default:` entry, no `--scope` means `all`.

Define `default:` only when a bare `dbd deploy` should itself build a subset —
the worked example at the end of this page is the usual reason.

The name a run resolves to is what the [scope guard](#the-scope-guard) records.
A database first built with no `--scope` while `default:` exists is pinned as
`default`, not `all`, even when `default` is defined as `all`.

## Dependency gaps

An in-scope entity that references a managed entity *outside* the scope has a
**dependency gap**: building the scope alone would fail at the database, or
build a table whose foreign key points nowhere. With a scope `hub` defined as
`includes: [app]`, and `app.orders` referencing `config.lookup_values`:

```
$ dbd inspect --scope hub
scope 'hub': 2 of 5 entities
✗ dependency gap: app.orders requires config.lookup_values (out of scope)
    chain: app.orders → config.lookup_values
✗ dependency gap: app.orders requires config.lookups (out of scope)
    chain: app.orders → config.lookup_values → config.lookups
Error: 2 dependency gap(s) in scope 'hub' — add them to the scope, or run with --deps include
```

What happens next is the scope's `deps` policy:

- **`deps: report`** (the default) — a gap is an error. `inspect --scope` lists
  every gap with its chain and exits 1, and the commands that gate on gaps
  (see [the table below](#which-commands-honour-scope)) refuse — including
  their `--dry-run`, so a dry run shows the same failure the real run would.
- **`deps: include`** — the working set is expanded to its transitive
  dependency closure before the command runs. `inspect` and `deploy --dry-run`
  list each gap as the step it is, not a failure, and the count covers the
  closure:

  ```
  $ dbd inspect --scope hub --deps include
  scope 'hub': 5 of 5 entities
  + auto-included: config.lookup_values (required by app.orders)
      chain: app.orders → config.lookup_values
  + auto-included: config.lookups (required by app.orders)
      chain: app.orders → config.lookup_values → config.lookups
  2 gap(s) will be auto-included (--deps include)
  ```

  A closure that needs something the scope *excludes* contradicts itself, and
  every command refuses it — `inspect`, `apply`, `dbml` and `diagram` alike.

`--deps report|include` overrides the scope's own setting for one run.

A reference to an [`external:`](03-design-yaml.md#external) entry is never a gap
— declaring a table as external is how you say "this lives elsewhere". There is
no "ignore gaps" mode.

Gaps are found from the references dbd records: foreign keys; the enum a
column is typed with and the sequence its default draws from
(`DEFAULT nextval('s')`); the function a trigger written in the table's file
executes; and the reads, writes and calls in views and routines. A table whose
enum or sequence lives in a schema the scope leaves out has a gap, and
`deps: include` pulls the enum or sequence in.

## Which commands honour `--scope`

| Behaviour | Commands |
|---|---|
| **Filter, and refuse on a gap** under `deps: report` | `inspect`, `apply`, `deploy`, `import`, `reconcile`, `diff` |
| **Filter only** — no gap check (`inspect --scope` is where gaps surface) | `combine`, `emit`, `dbml`, `graph`, `diagram`, `export`, `refresh`, `policies`, `reset` |
| **Ignore, with a warning** | `snapshot`, `release`, `migrate`, `doctor`, `init`, `merge`, `split`, `format`, `install` |

The ignoring commands print
`⚠ --scope hub ignored: this command operates on the whole design, not a scope`,
and `--deps` given to one of them is warned about the same way.

Every command that honours a scope announces the narrowing with one line,
counted the same way by each — `scope 'hub': 2 of 7 entities`: the working set,
`deps: include` closure included, out of every entity the design declares,
externals left out of both. That is `inspect`, `apply`, `deploy`, `import`,
`reconcile`, `diff`, `combine`, `emit`, `dbml`, `diagram`, `export`, `refresh`,
`policies` and `reset`, real runs and dry runs alike. `diff --json` prints the
line on stderr, so stdout stays JSON; `graph` records the scope in its JSON
instead (`"scope": "hub"`), because its stdout is a contract. An unscoped run
prints no line.

A dry run filters exactly as its real run does: `apply --dry-run` honours the
scope's `extensions:` allowlist and refuses what `apply` refuses;
`import --dry-run` and `deploy --dry-run` name the reason for every import entry
the scope leaves out; `policies --dry-run` and `deploy --dry-run` list and count
only the policies the real run applies. A `-n` that names something the scope
does not build is an error — `X is outside scope 'S'` — on `inspect`, `apply`,
`graph`, `export`, `refresh` and `import -f`.

**Grants follow the scope.** A scoped `apply` or `deploy` grants only on the
schemas the scope builds, and on a `supabase` target the PostgREST `USAGE`
grants are narrowed the same way.

What a scope does not narrow:

- **Roles** are cluster-wide, so every scope creates them; `reset` drops them
  only when it is not scoped.
- **A scoped `reset` never cascades.** It drops only what its scope holds,
  without `CASCADE`, all or nothing: when an object outside the scope depends on
  one it would drop — an out-of-scope table's column typed with an in-scope
  enum — it drops nothing and names the dependent. Add the dependents to the
  scope, or reset without `--scope`. The policies and triggers on the tables
  and views it drops go first, with them, so one calling an in-scope function
  does not refuse it.
- **pg_cron refresh jobs.** A scoped apply syncs the jobs of every materialized
  view in the design, because the sync unschedules any of the project's
  `dbd:refresh:` jobs missing from the set it is given. A scope whose `extensions:` allowlist
  leaves out `pg_cron` therefore fails when a materialized view outside it has
  a schedule.
- `dbd import -n <table> -f <file>` into a table the design does not declare —
  no scope can contain it — loads, with a warning that the scope was not
  applied.

## Extensions per scope

```yaml
scopes:
  hub:
    includes: [config]
    extensions: []          # install no extensions on the hub database
  search:
    includes: [docs]
    extensions: [vector]    # only pgvector
```

Omitted, every `target.extensions` entry is installed under the scope; a list
installs only those; `[]` installs none. Name each extension by its bare name
(`vector`, `postgis`, `uuid-ossp`) even when it is declared with a `schema:`.
The allowlist applies wherever the scope does — `apply` and its `--dry-run`,
`deploy`, `combine`, and `reset --extensions`, which drops only the extensions
the scope installs. An allowed extension's `schema:` comes with it (see
[above](#defining-a-scope)). Roles and externals are never filtered by a scope.

## Migrations under a scope

`snapshot`, `release` and `migrate` ignore `--scope` on purpose. A snapshot
records the **design**, not one database: a scoped baseline would leave
entities out, and every later `dbd snapshot` would diff against that hole and
emit a spurious `CREATE` for them on every database.

So a scope decides which migration steps **run**, not which exist. On a
released project, `dbd deploy --scope hub` applies the pending migration steps
for entities in the hub's working set, skips the rest, and still advances the
database to the latest version. A migration that adds a role or an extension the
scope keeps installs it, as a fresh scoped install does — so a table typed with
a new extension's type migrates under a scope too — and the summary counts each
entity once. When nothing in the pending versions belongs to the scope, the
dry run says so — the run records them and moves the database on — rather than
promise there is nothing to do.

A table removed from the design is in no working set any more, so its drop step
belongs to the scopes that occupy its schema: a scoped `apply` or `deploy` runs
the drop of a table in one of its schemas — `migrations/<NNN>/<schema>/<table>.sql`,
as `dbd snapshot` wrote it — and only where the table still exists, so a
database whose scope never built it skips the step rather than fail on it. Only
a drop that ran is counted in the summary.

## Hooks and policies under a scope

Both are filtered by what they touch:

- **Hook scripts** (`apply.before`, `apply.after`, `import.after`) are skipped
  when they depend on a table the design manages outside the working set. A
  table the design does not manage — an `external:` one, or one the hook
  creates itself — never causes a skip. dbd derives a script's tables by
  parsing it, reading a bare table name as Postgres resolves it where the
  statement naming it runs — along the path in effect there (the script's last
  `SET search_path`, else `source.search_path`, else the session default, with
  `public` after it), to the first schema where the design has that table or
  view — not on `public` alone; when the
  tables are *data* (inside `format()` or an `array[…]`), declare them with
  `writes:`. A script with no derivable tables and no `writes:` always runs.
  An `apply.*` skip is named before `apply`, `deploy` or `reconcile` starts and
  in their dry runs; an `import.after` skip in `import --dry-run` and
  `deploy --dry-run`, as the real run names it. See
  [Hook scripts](03-design-yaml.md#hook-scripts).
- **Policies** are filtered by path: `policies/<schema>/<table>.sql` names its
  table, and a policy whose table is outside the working set is skipped and
  reported rather than run against a database that lacks its schema. A file off
  that layout always applies.

Both filters are a correctness aid, not a security boundary — never rely on a
scope to *stop* a script or a policy running.

## The scope guard

The first `apply`, `deploy` or `reconcile` **pins** a database to the scope it
was built with, in the `scope` column of the bookkeeping table (`dbd.meta` on
PostgreSQL, `_dbd_meta` on SQLite). A later `apply`, `deploy`, `reconcile`,
`reset`, `import`, `export`, `refresh` or `policies` under a different scope is
refused — the match is exact, so a database pinned to `all` refuses
`export --scope hub` — as are `reconcile --dry-run`, and the `apply`, `deploy`,
`import`, `policies` and `reset` dry runs when a database is configured:

```
Error: Safety guard: scope guard: this database is pinned to scope 'public', but you requested 'internal'.
Applying a different scope would build a divergent schema.
→ re-run with --scope public, or pass --allow-scope-change to re-point this database to 'internal'.
```

That stops a forgotten or mistyped `--scope` from building half a schema in the
wrong database. To re-point a database on purpose, pass `--allow-scope-change`:
an `apply`, `deploy` or `reconcile` then re-pins it to the new scope. On `reset`
the flag (like `--force`) only bypasses the check — a reset does not re-pin.
`import`, `export`, `refresh` and `policies` take no such flag, since none of
them re-pins: run them under the pinned scope, or re-point the database with
`apply --allow-scope-change` first. Each says what running another scope would
do — load, read, refresh or secure tables the database was not built with —
and the way through. A database created before scopes existed is unpinned and
pins on its next write.

`diff` and `migrate --status` write nothing, so they note a pin rather than
refuse it: an unscoped `diff` of a database pinned to `hub` says so before it
lists what `hub` does not build as missing, and `--scope hub` compares what
the database was built with.

The pin is recorded under `project.name`. A renamed project has no record of
its own, so on a database that holds its tables, views, matviews, enums or
sequences it is held to the record the old name left — the refusal names that
project and says to set `project.name` back. Two projects that share a
database each keep their own record.

The pin lives in the same bookkeeping row as the environment the database was
built for. A `reset` of everything the database was built with — under
`--scope all`, or under the scope it is pinned to, which is what a bare `reset`
resolves to in a design whose `default:` built it — deletes that row, so the
next `apply` pins the database afresh. A reset under a narrower scope leaves
the pin, and the rest of the row, where it was. The
environment is guarded the way the scope is: a `-e dev` run against a database
marked prod is refused (see [global options](04-commands.md#global-options)).

To host several parts of the design in one database, define one scope that
includes them all, rather than switching a database back and forth between
scopes. In a workspace, each [module](#scopes-over-modules) keeps its own record,
so each module of a database is pinned to the scope it was built under.

## Reconcile and prune under a scope

**A scope narrows what is applied, not what the project owns.** Under a scope,
`dbd reconcile` (and `dbd diff`) hide what the design declares outside the
scope from the live side, so those tables are neither altered nor pruned.
Anything the design does not declare at all is still an orphan, and
`--prune` still drops it — in the schemas the design manages. `public` is one of
those only when the design puts something there: a role, or an extension with
no `schema:`, does not make it managed.

> **Upgrade to 0.24.1 or later before running `reconcile --prune --scope`.**
> In every earlier release with `--scope`, a table the design declared outside
> the scope — but in a schema the scope touches — read as an orphan, and a
> scoped prune dropped it with `DROP TABLE … CASCADE`
> ([#40](https://github.com/sensei-hq/dbd/issues/40)).

## Scopes over modules

In a workspace — a `design.yaml` with [`modules:`](03-design-yaml.md#modules) —
a scope selects **modules**, and entities within them. Sensei's 32-entry `dojo`
include list becomes one line:

```yaml
modules:
  shared: {}
  core:
    depends_on: [shared]
  dojo:
    depends_on: [shared]

scopes:
  default:
    modules: [core]              # core, and shared, which it depends on
  dojo:
    modules: [dojo]              # dojo and shared
  reporting:
    modules: [core]
    excludes: [app.audit_log]    # one of core's entities left out
  rates:
    includes: [app.currencies]   # one of shared's entities
```

- **`modules:`** selects each module it names with every module it depends on,
  directly or through another — whole.
- **`includes:`** selects entities of modules the scope does not select whole.
  A module it takes entities from runs with the modules it depends on, whole,
  as `--module` would run it. An include names entities: a bare schema alone
  selects no module, since every module has the manifest's schemas.
- **`excludes:`** leaves entities of the selected modules out, as in one
  design.
- A scope naming no module and including nothing selects **every** module —
  `excludes:` alone narrows the whole workspace.
- **`deps:`** and **`extensions:`** apply within each module, as in one design.

Each scope is projected onto each module it selects, under the same name, when
the workspace loads: every module resolves it as its own, so each command
filters, guards and pins a module under the scope exactly as it does a project,
and each module's record is pinned to the scope it was built under. Refused
when the workspace loads: a scope naming a module `modules:` does not declare,
an item no module declares, and `modules:` in a scope of a design that has no
modules.

**Which modules a run goes over.** With no `--scope`, the `default` scope if the
manifest has one, else every module; `--scope all` is every module. A run goes
over the modules the scope selects, in `depends_on` order. `--module` within a
scope must be one of the scope's modules — `dbd deploy --scope default --module
dojo` is refused:

```
Error: Config error: module "dojo" is not in scope default, which runs shared, core
```

A module you ask for — with `--module`, or by running in its folder — that the
`default` scope leaves out runs on its own, with its dependencies, when no
`--scope` is given, rather than being refused: `dbd deploy` in `dojo/` deploys
dojo and shared.

**Gaps between modules.** In one design a gap is caught by the design's own
analysis; between modules it is not, since the module that requires an entity
holds it as an external. So a run that builds or reads — `inspect`, `apply`,
`deploy`, `reconcile`, `diff`, `import`, `refresh`, `policies`,
`migrate --status` — checks, before it starts, that what each of its modules
requires is in the run's scope in the module declaring it, each module resolving
the scope and `--deps` as the run does. With a scope `lean` of
`modules: [core]` and `excludes: [app.currencies]`, which core's orders
reference:

```
Error: Config error: 1 dependency gap(s) between modules — stop excluding what is required, or exclude what requires it too:
  app.orders (module "core") requires app.currencies, which scope 'lean' leaves out of module "shared"
```

It is checked when a run starts, not when the workspace loads, so other scopes,
and runs that do not go over that module, still run. `dbml`, `diagram`,
`graph`, `combine` and `emit` read a workspace as one design and do not refuse
a gap: a table a scope leaves out of a dependency is drawn as a stub, as in one
design.

A reference into a module the requiring module does not depend on at all is a
different mistake, refused whenever the workspace loads — see
[`modules`](03-design-yaml.md#modules).

### From scopes to modules: `dbd split`

A project that already divides its work with scopes has, in its scopes, the
partition a workspace makes explicit. `dbd split --from-scopes` turns one into
the other: each **region** of the scopes' Venn diagram becomes a module.

```sh
dbd split --from-scopes                          # print the plan; change nothing
dbd split --from-scopes --rename shared=common   # name a module of the plan otherwise
dbd split --from-scopes --write                  # move the files, write modules: and scopes:
dbd split --undo                                 # put back what an interrupted --write moved
```

Sensei in miniature — `app.lookups`, with its data file, deployed to both
databases, `app.orders` to the main one, `dojo.sessions` (which references the
lookups) to the Dōjō's:

```yaml
scopes:
  default:
    excludes: [dojo]
  dojo:
    includes: [dojo, app.lookups]
```

```
$ dbd split --from-scopes
sensei splits into 3 modules, by its scopes:
  shared   in default and dojo   1 entity
  core     only in default       1 entity   depends on shared
  dojo     only in dojo          1 entity   depends on shared
Its scopes become:
  default: modules [core], excludes [dojo]
  dojo: modules [dojo]
4 files move into the modules' folders. Run again with --write to move them and write modules: and scopes: into design.yaml.
```

`-v` lists each module's entities and each file's move.

**The regions.** Each scope's entities are taken with their dependencies —
`deps: include`, whatever the scope says — and an entity's region is the set of
scopes holding it. What every scope deploys — a role — is in every scope; the
manifest's schemas and extensions stay in `design.yaml`, every module's. A region
becomes a module named:

| Region | Module |
|---|---|
| in every scope (of two or more) | `shared` |
| in one scope only | that scope's name — `default` as `core` |
| in several scopes, not all | their names joined with `-` (`core-hub`) |
| in no scope | `unscoped` |

Empty regions are dropped. `--rename <module>=<name>` names one otherwise.

**What it infers.** Each module `depends_on` the modules its entities reference
— a type created beside a table in its file included. A cycle is shown, and
refuses `--write`. Each scope becomes `modules:` — the modules of the regions it
holds, less those another of them depends on — keeping its `extensions:`, and
`excludes:` the manifest's schemas it did not deploy, which every module would
otherwise create; an `all` scope stays `all`.

**What `--write` does.** It moves each DDL file to the folder of the module its
entity is in, and each import data file and policy file to the folder of the
module its table is in — with `git mv` in a git work tree — moves the manifest's
`import.after` scripts under `import/` to `scripts/`, rewriting their paths, and
writes `modules:` and the new `scopes:` into `design.yaml` in place: the rest of
the file, comments and line endings, as it was. It is whole or not at all. It
first builds the workspace in a scratch folder and refuses unless that loads and
deploys, under each scope and with none, exactly what the project does. Until it
is done it keeps a journal of its moves (`.dbd-split`) and the manifest as it
was: a move that fails puts back every move before it, and while the journal is
there every command that reads the project is refused, naming `dbd split --undo`, which puts the files
and `design.yaml` back.

**What it refuses** before anything moves, naming why:

- a workspace, and a design without scopes;
- a project with `snapshots/` or `migrations/` — a migration is SQL over several
  tables and does not divide by module, so a project splits before its first
  release;
- a file that does not parse — what it references, and so its module, is
  unknown;
- a schema only a file's path names — add it to `schemas:` first;
- a scope that leaves out what its entities need (`deps: report` with a gap) —
  as modules it would deploy them;
- a file of `ddl/`, `import/` or `policies/` that maps to no module or to two,
  two files onto one path, and a file reached through a link from outside the
  project;
- a module name that cannot name an owner, or that differs from another only in
  case; a module folder already holding files; and an earlier split's leftovers.

On each database, each module's first deploy takes over what it declares from
the project as it was, and the project's record is retired once its modules
hold everything it held — see [sharing a database](14-sharing-a-database.md).

## Worked example: a schema as an optional add-on

An optional `hive` schema ships to its own database, while the main database
gets everything except it:

```yaml
scopes:
  hive:
    includes: [hive]      # the hive schema: its entities and its CREATE SCHEMA
    deps: include         # pull in any shared tables hive references
  default:
    excludes: [hive]      # everything else; the main database never creates hive
```

```sh
dbd deploy --scope hive -d $HIVE_URL   # the hive database: only hive
dbd deploy -d $MAIN_URL                # no --scope ⇒ `default` ⇒ everything but hive
dbd deploy --scope all -d $MAIN_URL    # the true full set, hive included (bypasses `default`)
```

Two choices worth understanding:

- **`excludes: [hive]` vs `excludes: [hive.*]`.** The bare token drops hive's
  entities *and* its `CREATE SCHEMA`, so the main database never has the schema.
  Use `hive.*` if an empty `hive` schema should still exist there.
- **`deps: include` on `hive`.** If hive's tables reference shared tables in
  other schemas, `deps: report` would refuse with a gap. `include` pulls them
  in; alternatively list them in `includes`, or declare them under `external:`.
  Run `dbd inspect --scope hive` to see the gaps before deploying.

Remember that the third command meets the scope guard: a database pinned as
`default` refuses `--scope all` until you pass `--allow-scope-change`.
