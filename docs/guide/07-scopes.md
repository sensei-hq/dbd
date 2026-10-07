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
build a table whose foreign key points nowhere.

```
$ dbd inspect --scope hub
scope 'hub': 2 entities
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
  dependency closure before the command runs, and `inspect` reports
  `2 gap(s) will be auto-included (--deps include)`.

`--deps report|include` overrides the scope's own setting for one run.

A reference to an [`external:`](03-design-yaml.md#external) entry is never a gap
— declaring a table as external is how you say "this lives elsewhere". There is
no "ignore gaps" mode.

> **Gaps are found from the references dbd records**: foreign keys, and the
> reads, writes and calls in views and routines. A column whose *type* is an
> enum is not one of them in 0.24.1 — a table in scope whose column uses an
> enum from an out-of-scope schema is not reported as a gap, and
> `deps: include` does not pull the enum in. Include the enum's schema in the
> scope yourself.

## Which commands honour `--scope`

| Behaviour | Commands |
|---|---|
| **Filter, and refuse on a gap** under `deps: report` | `inspect`, `apply`, `deploy`, `import`, `reconcile`, `diff` |
| **Filter only** — no gap check (`inspect --scope` is where gaps surface) | `combine`, `emit`, `dbml`, `graph`, `diagram`, `export`, `refresh`, `policies`, `reset` |
| **Ignore, with a warning** | `snapshot`, `release`, `migrate`, `doctor`, `init`, `merge`, `format`, `install` |

The ignoring commands print
`⚠ --scope hub ignored: this command operates on the whole design, not a scope`.

Commands that filter announce it — `scope 'hub': 2 of 7 entities` — except
`emit` and `diagram`, which filter without the line, and `graph`, which records
the scope in its JSON instead (`"scope": "hub"`) because its stdout is a
contract.

Three paths do not filter, and say nothing about it:

- `dbd import -n <table> -f <file>` loads the one file you named, scope or not.
- `dbd policies --dry-run` lists every policy file; the real run filters.
- `dbd deploy --dry-run` counts every policy file the same way.

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
The allowlist applies wherever the scope does. `reset --extensions` is the
exception: it drops every extension the target lists. Roles and externals are
never filtered by a scope.

## Migrations under a scope

`snapshot`, `release` and `migrate` ignore `--scope` on purpose. A snapshot
records the **design**, not one database: a scoped baseline would leave
entities out, and every later `dbd snapshot` would diff against that hole and
emit a spurious `CREATE` for them on every database.

So a scope decides which migration steps **run**, not which exist. On a
released project, `dbd deploy --scope hub` applies the pending migration steps
for entities in the hub's working set, skips the rest, and still advances the
database to the latest version.

One consequence to know: a table removed from the design is in no named
scope's working set any more, so its drop step does not run under a named
scope. The version advances and the table stays. Drop it by hand on those
databases.

## Hooks and policies under a scope

Both are filtered by what they touch:

- **Hook scripts** (`apply.before`, `apply.after`, `import.after`) run only when
  every table they depend on is in the working set. dbd derives a script's
  tables by parsing it; when the tables are *data* (inside `format()` or an
  `array[…]`), declare them with `writes:`. A script with no derivable tables
  and no `writes:` always runs. See [Hook scripts](03-design-yaml.md#hook-scripts).
- **Policies** are filtered by path: `policies/<schema>/<table>.sql` names its
  table, and a policy whose table is outside the working set is skipped and
  reported rather than run against a database that lacks its schema. A file off
  that layout always applies.

Both filters are a correctness aid, not a security boundary — never rely on a
scope to *stop* a script or a policy running.

## The scope guard

The first `apply`, `deploy` or `reconcile` **pins** a database to the scope it
was built with, in the `scope` column of the bookkeeping table (`dbd.meta` on
PostgreSQL, `_dbd_meta` on SQLite). A later `apply`, `deploy`, `reconcile` or
`reset` under a different scope is refused:

```
scope guard: this database is pinned to scope 'public', but you requested 'internal'.
Applying a different scope would build a divergent schema.
→ re-run with --scope public, or pass --allow-scope-change to re-point this database to 'internal'.
```

That stops a forgotten or mistyped `--scope` from building half a schema in the
wrong database. To re-point a database on purpose, pass `--allow-scope-change`;
the run re-pins it. `reset --force` also bypasses the check. A database created
before scopes existed is unpinned and pins on its next write.

To host several modules in one database, define one scope that includes them
all, rather than switching a database back and forth between scopes.

## Reconcile and prune under a scope

**A scope narrows what is applied, not what the project owns.** Under a scope,
`dbd reconcile` (and `dbd diff`) hide what the design declares outside the
scope from the live side, so those tables are neither altered nor pruned.
Anything the design does not declare at all is still an orphan, and
`--prune` still drops it.

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
