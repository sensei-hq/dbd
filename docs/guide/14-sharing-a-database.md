# Sharing a database

Several dbd projects can deploy into one database: a product's tables in `app`,
billing's in `billing`, both reading `app.orders`. dbd keeps them apart by
**ownership**. Each entity has exactly one owner — the project whose design
declares it — and no run changes or drops an entity another project owns.

This guide covers what each command does in a shared database, how to move an
entity from one project to another, and how to see what shares a database.

## Owners and the registry

A project's owner name is its `project.name`. dbd records ownership in the
database itself, beside its other bookkeeping:

| Table | Holds |
|---|---|
| `dbd.objects` | each entity an owner holds: its namespace, schema, name and kind |
| `dbd.uses` | what each owner uses: schemas, extensions, roles, and another owner's entities |

On SQLite these are `_dbd_objects` and `_dbd_uses` (see [SQLite](#sqlite)).

**What a project owns** is every entity its *whole* design declares — tables,
views, materialized views, sequences, enums, functions, procedures, and the
composite, domain and range types a file creates beside them. The whole design,
not the `--scope` a run is under: a scope narrows what is applied, not what the
project owns. An entity is owned as Postgres names it — in the schema its file
states, else on its search path, an unquoted name folded to lower case.

**Claiming.** Every run that writes claims what its design declares. A project
declaring an entity another project holds is refused before its first hook or
statement, and so is its dry run, `diff`, `import`, `refresh` and `policies`:

```
relation app.orders is owned by "billing"; owners sharing a database must not declare the same entity
```

When a run claims depends on whether its work can roll back. An `apply` in one
transaction claims as its last step, in that transaction, so a run that fails
claims nothing. `reconcile`, `reset`, and an `apply` under `DBD_NO_TX` commit as
they go, so they claim before their first write and keep the claim if they then
fail: what the design declares is the project's, built yet or not.

**Uses.** Every successful run records what its project uses: the schemas its
entities live in, its extensions and roles, and each reference one of its
entities makes to another project's — a view over `app.orders`, a foreign key to
it, a function body reading it. These are what protect a project from its
neighbours: see [the uses guard](#what-another-project-depends-on-is-guarded).

**Upgrading.** No migration step is needed. The first run of a project on a dbd
that keeps the registry claims the entities its design declares that already
exist and nobody owns. Entities no design declares stay unowned. A project that
has not run such a dbd yet records no uses; while one is in the database, dbd
cannot tell its entities from nobody's, and says so where it matters.

## What a run may do

### Apply, deploy and reconcile build only their own

A project builds, alters and drops only what it owns. Another project's tables
are hidden from `reconcile` and `diff`: they are neither drift nor orphans.

### Pruning

`reconcile --prune` drops a table only when:

- **(a)** this project owns it and no longer declares it; or
- **(b)** nobody owns it, and no other project uses its schema.

Rule (b) keeps the old clean-up for a database with one project. A table nobody
owns in a schema another project uses — or anywhere, while a project in the
database has not upgraded — might be that project's, so it is kept and named:

```
unowned: app.tmp_report — not dropped because app is shared with "billing" (--prune-unowned drops it)
```

`reconcile --prune-unowned` drops those too, naming each:

```
unowned: app.tmp_report — dropped by --prune-unowned (app is shared with "billing")
```

A table another project owns is never a candidate. A prune claims each table in
the transaction that drops it, so one another project claims meanwhile is kept.
A table this project no longer declares stays its own until it is gone — pruned,
or dropped by a migration — and only then is released.

### What another project depends on is guarded

Before a run drops anything — a table, a column, a constraint, an index, an enum
value, a schema, an extension — dbd asks what a `CASCADE` would reach, part by
part, and what other projects recorded using. Anything another project depends
on refuses the run, before any write and dry run included:

```
app.orders.total is used by "billing" (view billing.v_totals); remove that use first
```

This covers `reconcile` (a dropped column, constraint, index or enum value), a
pending migration `apply` runs (its `DROP`s, `ALTER TABLE … DROP COLUMN`, and a
`TRUNCATE … CASCADE` that would empty another project's table), and `reset`. A
`--prune` keeps a table another project depends on, naming the use.

What depends on it but nobody owns — a hand-made view, or one of a project not yet
upgraded — is guarded the way rule (b) keeps tables: where a project might own
it, in a schema another project uses or while a project has not upgraded:

```
app.orders is used by view billing.v_orders (owned by nobody; maybe by "billing"); remove that use first
```

Elsewhere it goes with the drop, as it always did.

### Reset

A reset drops only what its design declares, behind the same guard: one another
project depends on refuses the reset, `--force` or not. On top of that:

- **A database any project marked prod is prod for every project in it.** An
  unforced reset is refused while another project's record says `prod` — or an
  env dbd does not know.
- **`--schemas`** keeps a schema holding anything that is not this project's —
  another project's entity, one nobody owns, an extension another project needs
  — and one another project uses. It refuses to drop one holding something of
  its own another project depends on.
- **`--extensions`** keeps an extension another project uses or depends on — a
  column of its type, say.
- A full reset keeps a role another project uses, and its grants.

Each is named after what the reset dropped, and `reset --dry-run` asks the same
database and prints the same lines:

```
Reset dropped 1 view, 2 tables.
schema app kept: it holds table app.invoices ("billing").
extension citext kept: "billing" uses it.
```

### Runs wait for each other

A reset, a reconcile, or an apply running migrations reads what other projects
have and then drops `CASCADE`. So that no other project writes in between, every
writing run holds the database's dbd write lock: shared by an apply, import,
`policies` run or grant that only builds — those of different projects still run
side by side — and exclusive for a reset, a reconcile and an apply running
migrations. A run waits for the lock at most five minutes, then fails:

```
waited 5 minutes for another dbd run writing to this database to finish; try again once it has
```

A session waiting for it is named `dbd: waiting for the write lock` in
`pg_stat_activity`, and the one holding it `dbd: holding the write lock`.

## Moving entities and names

### Another project under the same name

A design arriving under an owner name that holds entities in the database, and
declaring none of them, is another project reusing the name. Left alone, it
would take over that project's version and history, and its first prune would
drop what it never declared. It is refused, before the version guard and dry
runs included:

```
owner "shop" here owns 1 entity (relation app.orders); this design declares none of them — another project under the same name? If this design is that owner now, run apply, deploy, reconcile or reset with --allow-ownership-change
```

Only entities that still exist count. `--allow-ownership-change` (on `apply`,
`deploy`, `reconcile` and `reset`) says this design *is* that owner now: what the
owner held stays its own — this design's — and a prune drops it as undeclared.
The run passes the version guard, the recorded version being the previous
design's, but not the environment guard. A project that replaces every live
entity at once — a one-table project renaming its table — reads as a reused
name too, and takes the override once.

### Moving an entity to another project: `--adopt-from`

To move an entity between projects kept in separate repositories, add it to the
receiver's design and run the receiver with `--adopt-from <owner>` (on `apply`,
`deploy` and `reconcile`):

```sh
dbd apply --adopt-from shop
```

```
adopting relation app.orders from "shop"
```

The receiver takes over what its design declares that `<owner>` holds, in the
claim's own transaction, each named before the run; a dry run says what it would
adopt (`[dry-run] would adopt …`) and claims nothing. An entity a third project
holds still refuses the run, and nothing is adopted. Then remove it from the
giver's design: the giver's next run no longer owns it, so neither declares it —
declaring it still is refused — nor prunes it.

## Seeing what shares a database

```sh
dbd inspect --shared -d $DATABASE_URL
```

```
Projects in this database:
  billing (dev, v0) — owns 1: relation billing.v_orders
    uses schemas: billing
    uses relation:app.orders through view billing.v_orders
  oldie (prod, v2) — records nothing of what it owns and uses: it has not run a dbd that keeps the registry
  shop (prod, v3) — owns 1: relation app.orders
    uses schemas: app
No conflicts for project 'shop'.
```

It lists what would refuse this design there — an entity another project owns,
a reused owner name — and exits non-zero over any:

```
Conflicts for project 'taker':
  relation app.widgets is owned by "clilive"
Error: 1 conflict for project 'taker' in this database
```

From a library, the same report is `Design::shared_database`.

## Onboarding beside another project

`dbd init --from-db` refuses only a database dbd manages for the project it is
creating — `dbd merge` syncs that one. Another project's record is no bar: a
second project onboards beside the first. It leaves out what other projects own,
naming each, and declares `external:` what it keeps that depends on one — a
foreign key's table, what a view reads — so its references resolve:

```
⚠ left out 2 entities other projects own here: relation app.widgets ("clilive"), relation app.named ("clilive")
  external: app.widgets — another project's ("clilive"), referenced here
```

`dbd merge` leaves them out the same way, warns about what to declare
`external:`, and says whose a file's entity is rather than calling the file an
orphan. Both refuse while a project in the database has not run a dbd that
records what it owns and uses: its entities cannot be told from nobody's.

## SQLite

SQLite keeps the same registry, in `_dbd_objects` and `_dbd_uses`, and the same
rules apply: a second project sharing a SQLite file is refused a table the first
owns, a reset cannot drop another project's table, and name reuse,
`--adopt-from`, `inspect --shared` and onboarding work as on Postgres. SQLite has
no schemas, so an entity is its name — `auth.users` is the table `users` —
compared without regard to case. It has no catalog of what depends on what, so
its uses guard has what projects record and the foreign keys that reference a
table.

## Limits

- **Objects hook scripts create are not entities.** They stay unowned, and are
  pruned under rule (b) only where a single project uses the schema.
- **Roles are server-wide.** The registry sees one database; a role another
  database uses is protected by Postgres's own `DROP ROLE` checks, and a reset
  keeps a role another database holds (see [`dbd reset`](04-commands.md#dbd-reset)).
- **`apply.before` hooks run before the claim** of an `apply` in one
  transaction. A hook that writes an entity another project claims in that
  window is not stopped by ownership.
