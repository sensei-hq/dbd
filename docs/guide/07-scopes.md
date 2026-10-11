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
| **Ignore, with a warning** | `snapshot`, `release`, `migrate`, `doctor`, `init`, `merge`, `format`, `install` |

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
  scope, or reset without `--scope`.
- **pg_cron refresh jobs.** A scoped apply syncs the jobs of every materialized
  view in the design, because the sync unschedules any `dbd:refresh:` job
  missing from the set it is given. A scope whose `extensions:` allowlist
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
  parsing it, reading a bare table name on the path the script runs on —
  `source.search_path`, else the session default — not on `public`; when the
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

To host several modules in one database, define one scope that includes them
all, rather than switching a database back and forth between scopes.

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
