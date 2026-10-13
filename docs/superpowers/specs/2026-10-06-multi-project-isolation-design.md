# Multi-Project Isolation in One Database (#7)

**Date:** 2026-10-06 (rev 3, 2026-10-07)
**Status:** Phases 1 and 2 shipped (Phase 1 in 0.25.x, Phase 2 in 0.26.0);
Phase 3 in progress (plan: `docs/superpowers/plans/2026-10-13-workspaces-phase-3.md`);
Phase 4 later; the decisions below are settled.
**Scope:** Let several owners share one database safely: independent projects
from different repositories, and modules of one workspace kept under
`database/<module>/`. Ownership is per **entity**, recorded in the shared `dbd`
bookkeeping schema. No schema is special, `public` included. A module may use
another module's entities but never alter or drop them. Entities that several
modules need live in a module of their own. Phase 1 fixes four existing bugs
that are wrong even with a single project.

**Rev 2 changes rev 1 (schema-level ownership)** in three ways:

- Ownership moves from schemas to entities.
- `public` is no longer a special case.
- Modules get their own roots.

**Rev 3** settles configuration: one root manifest with a `modules:` block, a
comparison of projects, modules and scopes, and `dbd split` for converting
sensei.

Sensei is the motivating case. Its `dojo` scope pulls 31 entities out of the
`sensei` and `staging` schemas, which it shares with the app's own tables.
Schema-level ownership could not express that.

---

## Overview

### Hazards today (unchanged from rev 1, and still the target)

| # | Hazard | Where | Effect on a co-tenant |
|---|---|---|---|
| H1 | Prune by schema membership | `managed_schemas` + `restrict_snapshot_to_schemas` | `reconcile --prune` drops another owner's tables in any schema both touch |
| H2 | `public` enters undeclared | a target role, or an extension without `schema:`, maps to `public` | a project with a role prunes `public` |
| H3 | Same `project.name` | `ON CONFLICT (project) DO UPDATE` in `Bookkeeping::set_meta` | silent takeover of version and history |
| H4 | pg_cron refresh jobs | `sync_refresh_jobs` unschedules every `dbd:refresh:%` job not its own | another owner's refresh jobs disappear |
| H5 | Legacy heal | folds and drops `_dbd_*` tables whoever wrote them | an owner on an older dbd loses its bookkeeping |
| H6 | `reset` reach | `DROP … CASCADE`, `DROP SCHEMA/EXTENSION … CASCADE`, `DROP ROLE` | another owner's views, FKs, columns, schemas and roles |
| H7 | Shared import staging | `staging._dbd_import_tmp` | concurrent imports corrupt each other |
| H8 | `reset` guard sees one row | prod/version guard reads only this owner's meta | resets freely beside another owner's prod data |
| H9 | Onboarding | `init --from-db` refuses on any bookkeeping; `merge` reads every schema | a second owner cannot onboard; `merge` absorbs the first |

### Concepts

| Term | Meaning |
|---|---|
| **Owner** | Whatever deploys into the database and holds ownership: a single-root project (today's layout) or one module of a workspace. Its identity is `project.name` or `<project>/<module>`. |
| **Entity ownership** | Each entity has exactly one owner per database. Only the owner creates, alters or drops it. |
| **Use** | An owner may reference another owner's entity (FK, view, join, function body) but not change it. dbd records each use, and the entity's owner cannot break it. |
| **Workspace** | `database/design.yaml` with a `modules:` block, plus one folder per module (`database/<module>/`). A project without modules is a workspace of one, so today's layout is unchanged. |
| **Module** | A partition: entities never overlap between modules. Declared under `modules:` with `depends_on`; its files live in its folder. |
| **Scope** | A selection that may overlap. In a workspace, a scope selects modules (and can still select entities). |

Modules partition and scopes select. That is why scopes could never carry
ownership (they share entities by design) and modules can.

---

## 1. Ownership at the entity level

### Registry

Two tables are added to `LAYOUT_DDL`, so heal creates them on every existing
database:

```sql
-- One owner per entity. The key mirrors Postgres's own namespaces, so two
-- owners can never both hold something that could collide.
CREATE TABLE IF NOT EXISTS dbd.objects (
  namespace  varchar NOT NULL CHECK (namespace IN ('relation', 'type', 'routine')),
  schema     varchar NOT NULL,
  name       varchar NOT NULL,
  kind       varchar NOT NULL,   -- table, view, materialized view, sequence, enum, function, procedure, ...
  owner      varchar NOT NULL,
  claimed_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (namespace, schema, name)
);
-- Non-exclusive relationships: an owner uses a schema, an extension, a role,
-- or another owner's entity (and through which of its own entities).
CREATE TABLE IF NOT EXISTS dbd.uses (
  owner   varchar NOT NULL,
  kind    varchar NOT NULL CHECK (kind IN ('schema', 'extension', 'role', 'entity', 'owner')),
  target  varchar NOT NULL,        -- 'app', 'vector', 'basic', 'relation:identity.users'
  via     varchar NOT NULL DEFAULT '',  -- 'view billing.v_invoices'
  PRIMARY KEY (owner, kind, target, via)
);
```

The `relation` namespace covers tables, views, materialized views, sequences
and foreign tables, which all share `pg_class`. A table and a view of the same
name cannot both exist, so they cannot have two owners. Every type a design
creates is keyed in `relation` too — an enum, and a composite, domain or range
(with the multirange a range makes) a file creates beside its entity: Postgres gives every table, view and
materialized view a row type of its own name, so none of them can share a
name with a type. `type` is left unused. `routine` is per name: dbd already
drops every overload of a name together. Indexes, constraints, triggers, policies and comments
belong to their table.

**Schemas, extensions and roles are shared containers**, recorded in
`dbd.uses`. No schema is special. An owner never "owns" `public`, `app`, or any
other schema; it owns the entities it puts there.

### Rules

**Claim (apply, deploy, reconcile, reset).** Before any hook or DDL, the owner
checks every entity its whole design declares. "Whole design" means the full
design, never the scope (#40). A design declaring an entity held by another
owner is refused with no DDL executed:

`relation app.orders is owned by "shop/billing"; owners sharing a database must not declare the same entity`

When the owner claims those entities depends on whether its work can roll
back:

- `apply` in one transaction claims as its last step, in that transaction, so
  the claim commits with the work or not at all. A run that fails, or is
  killed, claims nothing. A version that adds enum values commits part-way;
  the run claims before that commit too, so nothing commits unclaimed.
- `reconcile`, `reset`, and an `apply` outside one transaction (`DBD_NO_TX`)
  commit as they go. They claim, committed, after their last refusal and
  before their first write — `apply.before` hooks included — and keep the
  claim if they then fail: what the design declares is the owner's, built
  yet or not. Claimed after its writes, such a run could find an entity
  unowned while another owner was building it, wait on that owner's lock,
  then write over — or drop — what that owner had just claimed.

`apply.before` hooks are the project's own scripts, and an `apply` in one
transaction runs them, as it always has, outside it: after the check, before
its claim. A hook that writes an entity another owner claims in that window is
not stopped by ownership. Claiming first would close it, at the cost of a
failed or killed apply keeping its claims — the wrong database, noticed in
time, would be left claimed.

Uses are recorded once the work is built.

The primary key makes this atomic: of two concurrent first claims, exactly
one wins. The other finds the entity held, and is refused before it writes —
or, in an `apply` in one transaction, its DDL rolls back with the claim.

**Prune.** `reconcile --prune` drops a live entity only when one of these holds:

- (a) this owner owns it and no longer declares it; or
- (b) nobody owns it, and its schema is used by no other owner.

Rule (b) keeps today's drift clean-up for a database with one owner. In a
schema that another owner also uses, unowned entities are reported, and
dropped only when the run confirms it with `--prune-unowned` (decision 4), each
named as it goes:

`unowned: public.tmp_report — not dropped because public is shared with "billing" (--prune-unowned drops it)`

With `--prune-unowned` the run reports instead `unowned: public.tmp_report —
dropped by --prune-unowned (public is shared with "billing")`.

Entities owned by someone else are never candidates: `reconcile` and `diff`
hide them, so they are neither drift nor orphans. A table the design creates —
whatever its file's path calls it — is never an orphan. A prune claims each
table it is about to drop in the transaction that drops it, after the run's
other writes: one another owner has claimed since the run read the registry is
kept, naming that owner; a run that fails or is killed mid-prune keeps neither
its drops nor those claims; and what is claimed is released once dropped. Removing an enum value rewrites
every column of that type, so a column in another owner's table refuses it,
before any write and under a dry run, naming the column and its owner. Ownership decides which
orphans go; `managed_schemas`, through `restrict_snapshot_to_schemas`, stays as
a second lock on where a candidate can be at all.

**Release.** An entity is released when its owner's successful run no longer
declares it and the entity no longer exists: it was pruned, or a migration's
`.drop.sql` dropped it. An entity removed from the design but left in the
database stays owned. So a later run of the same owner prunes it under rule (a),
and nobody else can claim it by accident.

**Transfer** (moving an entity between owners):

- Within one workspace deploy, dbd sees both sides. The entity leaves module A's
  folder and appears in module B's, and ownership moves with no prune.
- Between independent repositories, the receiver runs with
  `--adopt-from <owner>`, which is explicit and printed. The giver's next run
  sees it no longer owns the entity and does not prune it.

  The adoption happens in the claim's own transaction: `<owner>`'s claims of
  what the design declares change hands, then the design claims as usual. An
  entity a third owner holds refuses the run, and the adoption rolls back
  with it. Each adopted entity is printed before the run, and a dry run
  prints what it would adopt and claims nothing. An adopted entity is no sign
  of a rename, so the guards do not read the giver's record as the
  receiver's — nor do the receiver's own claims, should a run that adopted fail
  before it wrote its record. A reconcile plans what it adopts as its own. A
  giver whose design still declares the entity is refused, as any owner
  declaring another's entity is.

**Uses are guarded.** At deploy time an owner records every reference its
design makes to another owner's entity, from dbd's parsed references, the same
ones scopes and the dependency graph use. Parsed references see into function
bodies, which `pg_depend` does not. Before an owner drops or destructively
alters an entity, dbd checks both `dbd.uses` and `pg_depend`. If another owner
depends on it, dbd refuses:

`identity.users.email is used by "billing" (view billing.v_invoices); remove that use first`

A call is a use of a routine; anything else — a read, a write, a column's type,
a default's sequence — is a use of a relation. A name written bare is looked
up along the referring entity's search path, as Postgres looks it up: a read
is of the first relation on the path that exists, owned or not. A call is
resolved by its arguments as well, which dbd does not weigh, so it is a use of
every routine on the path it could reach.

A use is recorded whoever owns its target, or nobody yet: the guard asks the
registry who owns a target when it runs, so a use made before the target's
owner claimed it still guards it. A routine another entity's file creates — a
table's trigger function — records its body's uses as its own.

The guard walks `pg_depend` from what goes, as a `CASCADE` reaches it: a table
with its row type and that type's array; a type and its array; a column, with
the same column of every table inheriting it (dropped through inheritance,
which `pg_depend` does not record); a constraint, with the index behind a
primary key, unique or exclusion constraint; an index; a routine's overloads;
a whole schema — its relations, types and routines, and its collations, text
search objects, operators, operator classes and families and conversions, since
a column collated by one goes with it; an extension's members. Each dependent is walked as itself — a
column as its column, a constraint, default, trigger or policy as itself — so a
part reaches only what depends on the part; a view's rewrite rule stands for
the whole view, and a generated column's expression leads to its column. A
`TRUNCATE … CASCADE` reaches what it empties and drops nothing: the table, the
partitions and inheriting tables of what it empties, and every table whose
foreign key points at what it empties. Each is named by its owning
relation, type or routine. A dependent nobody owns is guarded by prune's rule
(b): where a project might own it — in a schema another owner uses, or
anywhere while a recorded owner does not record its uses — it refuses as
another owner's would, named `view billing.v_orders (owned by nobody; maybe by
"billing")`; elsewhere it goes with the drop, as it always did. Only one that
stands on its own counts: not an owned sequence, a partition, an array type,
an extension's member, nor the object dropped. For a part
of a table, `dbd.uses` adds only the uses recorded through routine bodies,
which the catalog does not track. It runs before any write, dry runs
included:

- `reconcile` refuses a dropped column, constraint, index or enum value another
  owner depends on; a prune keeps a table another owner depends on, named:
  `app.old is used by "billing" (view billing.v_old) — not pruned; remove that
  use first`.
- `apply` reads what each pending migration it runs drops — in its `.sql`,
  `.drop.sql` and `.data.sql`, inside `DO` blocks too: a `DROP` of a relation,
  type, routine, index, schema or extension, an `ALTER TABLE … DROP COLUMN` or
  `DROP CONSTRAINT`, a `TRUNCATE … CASCADE` — with a bare name on the path the
  migration runs on, and refuses the run when another owner depends on any of
  it. A script dbd cannot parse is refused where another project is recorded:
  what it drops is unknown. A design not read as Postgres — SQLite's — may keep
  migrations in its own dialect: a script libpg_query cannot read is passed
  over there, and what it can read is guarded as any other's.

This is the contract between owners. It plays the role an API plays between
services.

**Reset.** Reset drops only entities this owner owns; it claims its design's
entities before it drops any (see Claim). On top of that:

- The prod guard refuses while any owner's row is `env = 'prod'` — or an env
  dbd does not know — unless `--force` is given. It reads before the claim, so
  a refused reset writes nothing.
- `CASCADE` stays, after the same uses guard: every entity the reset drops,
  whole, as its `DROP` names it — not the rest of what its file creates, which
  the reset leaves. Postgres's cascade cannot reach another owner's entity,
  because the guard refused first — `--force` or not.
- The reset holds the database's dbd write lock — an advisory lock on
  `hashtext('dbd:write')`, exclusive — from its first read to its last drop. So does a
  reconcile, and an apply running migrations, whose drops are guarded the
  same way; an apply that only builds holds it shared, as do imports,
  policies and grants. So no other project's run writes between what a
  guarded run reads and what it drops: it waits for the runs before it, at
  most 5 minutes, and the runs after it wait for it. A run waits by asking
  again, not in one long statement — whose snapshot the holder's `CREATE
  INDEX CONCURRENTLY` would wait out — and its session says so in its
  `application_name`. The session holding the lock sits idle through the run,
  so it turns `idle_session_timeout` off; a lock found lost at release fails
  the run; a run dropped part-way drops the session, and the lock with it.
- `--schemas` keeps a schema that holds an entity owned by anyone else, an
  unowned entity, or an extension another owner uses or depends on, and one
  another owner records a use of. An extension the design installs there and
  no other owner needs is the design's: it goes with the schema. A schema it
  drops is guarded whole, since its `CASCADE` takes this owner's entities the
  design no longer declares too.
- `--extensions` keeps an extension another owner uses or depends on, and a
  full reset keeps a role another owner uses — and its grants, which that
  owner relies on. A role kept because another database holds it still has
  this database's grants revoked.
- A `supabase` target is Postgres, and resets as one.
- What it keeps is left out of the script and named, one line each, on
  `ResetComplete::kept`: `schema app kept: it holds table app.invoices
  ("billing")`. `Design::reset_preview` answers the same for `reset --dry-run`,
  from the same database.

**Name reuse** (H3) is detected at claim time. Suppose a design arrives under an
owner name that already owns entities in this database, and it declares none of
them. dbd refuses with `owner "app" here owns 42 entities; this design declares
none of them`. `--allow-ownership-change` overrides it. Only entities that still
exist count — a claim of one dropped since is no sign of another project — and
the check runs wherever ownership is checked or claimed: dry runs, `diff` and
`reset` too, and before the version guard, which reads the record the name
keys — the previous design's. Overridden, what the owner held stays the
owner's, now this design's, and a prune drops it as undeclared; the run passes
the version guard, the recorded version being the previous design's history,
but not the environment guard. A project replacing every live entity at once —
a one-table project renaming its table — reads as a reused name too, and takes
the override once.

**Inspection.** `dbd inspect --shared` reads the database's records and its
registry: each project, its env and version, what it owns, the schemas it uses
and what of others' it uses through what of its own, and which projects record
nothing of what they own yet. It lists what would refuse this design there — an
entity another owner holds, a reused owner name — and exits non-zero over any.

**Onboarding** (H9). `init --from-db` refuses only a database dbd manages for
the project it is creating; another project's record is no bar. It and `merge`
leave out what other owners hold, naming each, and declare `external:` — or,
for `merge`, name — what the project keeps that depends on one: a foreign key's
table, what a view reads, found as a drop's `CASCADE` would reach it. While a
project the database records has not run a dbd that records what it owns and
uses, both refuse: its entities cannot be told from nobody's. A database that
keeps no registry leaves nothing out, and `init --from-db` refuses one dbd
manages.

**Backfill.** The first upgraded run of each owner claims the declared entities
that already exist and nobody owns. Undeclared live entities stay unowned. On a
database with one owner, rule (b) keeps pruning them exactly as today. No
migration step is needed.

Rule (b) is suspended while any owner in `dbd.meta` has not upgraded yet: a
meta row whose owner has no `owner` row in `dbd.uses`. Every run that records
its uses writes that row, naming itself, so a design that uses nothing still
counts as upgraded. A not-yet-upgraded owner's schema uses are unknown, so an
unowned entity might be its own. The prune report names the owners still to
upgrade, and `--prune-unowned` drops such entities anyway, each named.

**SQLite** uses the same model in its single namespace (`_dbd_objects`,
`_dbd_uses`), with no special case: an object is its namespace and name, without
regard to case — SQLite has no schemas, so `auth.users` is the table `users` —
and exists when `sqlite_master` names it as a table or view. It has no catalog
of dependencies, so its uses guard has what owners record and the foreign keys
that reference a table; its reset runs the guard on what it drops. **Convex** is one deployment per project and
is unchanged.

### Where it runs

There is one core function, so library embedders are protected too:

```rust
pub(crate) async fn preflight(&self, adapter: &dyn DatabaseAdapter, mode: Preflight) -> Result<()>
// Write — apply, deploy: heal → check, before any hook
// Read — import, policies, refresh, --dry-run, diff, reconcile before it plans: check only, no writes
pub(crate) async fn claim_built(&self, adapter: &dyn DatabaseAdapter) -> Result<()>
// apply in one transaction: claim → record uses, in it, last — and before a part-way commit
pub(crate) async fn claim_first(&self, adapter: &dyn DatabaseAdapter) -> Result<()>
// reconcile, reset: heal → claim, committed, before the first write; an apply outside
// one transaction claims there too, and each records its uses once built
```

---

## 2. Workspaces and module roots

### Projects, modules and scopes

Three ways to divide a database. The first two are owners; the third is not.

| | Separate projects (multi-tenant) | Modules (one workspace) | Scopes |
|---|---|---|---|
| **What it is** | Independent designs that happen to share a database | One design partitioned into parts that evolve separately | Named selections of one design, for deploying subsets |
| **Where it lives** | One repository each, with its own `database/design.yaml` | One repository: `database/design.yaml` with a `modules:` block, plus a folder per module | `scopes:` in `design.yaml`; one `ddl/` tree |
| **Can entities overlap?** | No: the registry refuses | No: `dbd inspect` refuses before any database is touched | Yes, freely; that is their purpose |
| **Owner in the database** | Each project (`project.name`) | Each module (`<project>/<module>`) | None of their own. Every scope of a project is the same owner (#40) |
| **Version, snapshots, migrations** | Per project | Per module | Shared by the whole project |
| **References across the boundary** | Allowed to entities the other owner owns, recorded and guarded | Allowed along `depends_on`, recorded and guarded | Gaps are reported or pulled in (`deps`) |
| **Use when** | Different teams, repositories or release cadences | One team, but the design is large, or parts need to deploy to different databases or release separately | Same parts, different deployment targets |

Scopes keep working inside a workspace and become coarser and simpler: a scope
selects modules, and can still select individual entities.

### One manifest

There is a single `database/design.yaml`. Module folders hold DDL and data, not
configuration:

```
database/
  design.yaml
  shared/  ddl/ import/ policies/ snapshots/ migrations/
  core/    ddl/ import/ policies/ snapshots/ migrations/
  dojo/    ddl/ import/ ...
```

```yaml
project:
  name: sensei
target:
  postgres:
    url: $DATABASE_URL
    extensions: [{ name: vector, schema: extensions }]
modules:
  shared:
    note: Reference data the app and Dōjō both need
  core:
    depends_on: [shared]
  dojo:
    depends_on: [shared]
scopes:
  dojo:    { modules: [dojo], extensions: [] }   # + shared, via depends_on
  default: { modules: [core] }                    # + shared
import:
  staging: [staging]
  options: { truncate: true, null_value: "" }
```

Why one manifest:

- Everything that configures the project already names entities by their
  qualified names: `import.tables`, `export`, `materialized_views`, `ignore`,
  hook `writes:`. The owning module of `sensei.rule_packs` is known from which
  folder its DDL is in. A per-module manifest would only repeat names the folder
  already settles.
- The only per-module facts are `depends_on`, an optional `note`, an optional
  `path` (default `<module>/`), its own `version` and `released` (decision 2),
  and apply hooks that belong to one module (`modules.<name>.apply`). All of
  those fit in one block.
- One file shows the whole database: what is in it, what depends on what, and
  what deploys where.
- A module that later moves to its own repository takes its folder, and its
  `modules.<name>` block becomes that repository's `design.yaml`.

`export:` keeps its current meaning: the data export behind `dbd export`.
Typically that is views that dereference foreign keys so the output matches
the import staging tables, which lets the data of a long-running system move
from one database to another. Each `export:` entry belongs to the module that
owns the view, and `dbd export --module core` narrows to one module. Nothing in
this design reuses the word "export" for module boundaries.

A project without a `modules:` block is a workspace of one module rooted at
`database/`. Its identity stays `project.name`, so nothing changes for existing
projects.

### Behaviour

- **Static overlap check.** A workspace in which two modules declare the same
  entity does not load, so `dbd inspect` — and every other command — refuses
  it. This is the workspace version of the claim rule, caught before any
  database is involved.
- **Cross-module references** must follow `depends_on`, directly or through a
  dependency's own. A reference into a module that is not a dependency is a
  gap: the workspace does not load, and the refusal names the module to add to
  `depends_on`. (Unlike a scope gap there is nothing to auto-include: the fix
  is the dependency.) A module references what its dependencies declare without
  `external:` entries; those entities resolve its references and are never its
  own to build, claim or own — their definitions stay in their module.
- **Deploy order** is topological by `depends_on`: shared, then core, then dojo.
  Each module claims, applies and records its own meta row
  (`<project>/<module>`). Each module also has its own version, `snapshots/`
  and `migrations/`, so modules release independently.
- **Selecting what to run.** `--module core` selects one module, plus its
  dependencies. Workspace scopes select modules (above). Sensei's 32-entry
  `dojo` includes list becomes one line.
- **Running inside a module folder** finds the workspace root by walking up, as
  Cargo does, and selects that module.
- **The viewer** groups the diagram and sidebar by module, alongside the schema
  tint.

### Converting an existing project: `dbd split`

Sensei is the only project using scopes, and its scopes already encode the
partition. Each region of the scopes' Venn diagram, computed on the expanded
sets after `deps: include`, is a module:

| Region | Becomes | Sensei today |
|---|---|---|
| in `dojo` and `default` | `shared` | the 29 listed lookup, rule-pack and metric entities and their imports, plus whatever `deps: include` adds |
| only in `dojo` | `dojo` | the `dojo` schema, `staging.tenants`, `staging.import_tenants` |
| only in `default` | `core` | everything else |

`dbd split --from-scopes` prints that plan: modules, entity counts, the
`depends_on` it infers from references, and the rewritten `scopes:`.
`--write` then:

- moves the files (`git mv` when the folder is in git)
- splits `import/` by the module that owns each target table
- writes `modules:` and the new `scopes:`

Module names default to the region (`shared`, `<scope>`, `core`) and can be
renamed in the plan. With more than two scopes there can be more regions. Empty
regions are dropped, and the plan says which regions came from which scopes.

On the database side there is no manual step. Sensei is pre-release and has no
snapshots or migrations to divide. On each database, the first deploy after the
split sees that the predecessor owner `sensei` is the same project. It retires
that meta row, and each module claims its entities: the workspace transfer
rule. The main database gets `sensei/shared` and `sensei/core`; the Dōjō
database gets `sensei/shared` and `sensei/dojo`.

---

## 3. Shared entities and keeping them in sync

Ownership is always single. "Shared" means one owner and many users. Three
patterns cover the cases, ordered by how tightly they couple modules:

| Pattern | Use when | How | Sync |
|---|---|---|---|
| **Shared module** | Several modules need the same reference data or types: sensei's rule packs, reason codes and metric types | A `shared` module owns them; others `depends_on: [shared]` | Same database: nothing to sync. Other databases: the same module deploys to each, so the definition comes from one source. Data comes from the module's seed imports (as sensei does today) |
| **Direct use** | A module reads or joins another module's live data and performance matters | FK, join or view straight onto the owner's entity | None. One copy, always current; the uses guard protects it |
| **Projection** | The consumer needs its own shape, indexes or isolation, or the modules may later split into separate databases | The consumer owns a view or materialized view over the owner's entity | A view is live. A materialized view refreshes on the existing `refresh_every` and pg_cron schedule |

Data that changes at runtime and must reach **another database** needs logical
replication: a publication in the owner's database and a subscription in the
consumer's. dbd's part would be generating the subscriber's table from the
owner's definition and managing the publication and subscription as entities.
That is Phase 4: real, but no current project needs it.

Deliberately not supported: two modules each declaring the same entity and
"syncing definitions". That is co-ownership. It is what turns into
last-writer-wins today, and the shared-module pattern covers the need.

---

## 4. Phase 1: four standalone fixes (patch release)

These are carried over from rev 1. Each is wrong today even with one project:

- **1a. Roles and extensions stop pulling in `public`** (H2). They stop
  contributing to `managed_schemas`, and later to `dbd.uses` schemas.
- **1b. pg_cron job names carry the owner** (H4). The name becomes
  `dbd:refresh:<owner>:<schema>.<name>`, and only this owner's jobs are
  unscheduled. Legacy untagged jobs are unscheduled only for entities this
  owner declares.
- **1c. Heal folds only this owner's legacy rows** (H5). It drops a legacy table
  only once it is empty.
- *Done: 1a in 0.25.0 (`managed_schemas` skips roles and extensions); 1b, 1c
  and 1d on `develop` after 0.25.1, each with the test the table below names
  (T2 runs against a pg_cron stand-in, T4 records the staging tables an event
  trigger sees). 1c also layers the bookkeeping reads — a project's record is
  its `dbd.meta` row, else its legacy row — since a heal that leaves another
  project's rows behind must not hide them from that project's guards. Review
  moved 1b's names to `dbd-refresh:<owner>:<schema>.<name>`: inside
  `dbd:refresh:`, a co-tenant not yet upgraded — whose sweep takes every
  `dbd:refresh:%` job it was not given — would still take them, the hazard
  the heal tolerates for bookkeeping. Names fit 63 bytes (pg_cron 1.3/1.4
  keep `name`); a legacy job whose view is gone is unscheduled. The heal locks
  each legacy table before reading it and passes over one its role cannot
  reach.*
- **1d. Import staging moves into `dbd`, one table per owner** (H7).
  `dbd.import_jsonb_to_table` and `dbd."import_tmp:<owner>"` are used, with a
  hashed name past 63 bytes. Imports run through the pool, so `TEMP` tables
  cannot work.

---

## Phasing

| Phase | Ships | Closes |
|---|---|---|
| 1 | The four fixes above (patch) | H2, H4, H5, H7 |
| 2 | Entity ownership for single-root projects: `dbd.objects` and `dbd.uses`, `preflight`, prune and release rules, uses guard, reset rules, backfill, `--adopt-from`, `--allow-ownership-change`, `dbd inspect --shared`, `init --from-db` and `merge` skipping other owners' entities, SQLite (minor) | H1, H3, H6, H8, H9; all of #7's acceptance criteria |
| 3 | Workspaces: `database/<module>/`, the `modules:` block, `depends_on`, static overlap check, per-module bookkeeping, snapshots and migrations, `--module`, module-selecting scopes, viewer grouping, `dbd split --from-scopes`, and converting sensei as the proving case (minor) | the clutter, plus scopes standing in for modules |
| 4 | Logical replication: publication and subscription entities, subscriber tables generated from the owner's definition | runtime cross-database sync |

Docs ship with each phase:

- guide 03 (`modules:`, module-selecting `scopes`)
- guide 04 (new flags, `inspect --shared`)
- a new guide, "Sharing a database: projects, modules and shared entities"
- `llms.txt`, `llms-full.txt`, both `SKILL.md` copies, the site mirrors
- `dbd-pattern-verifier`

---

## Test Scenarios

These are embedded-PG unless marked unit. Each is written first and seen
failing.

| # | Phase | Scenario | Asserts |
|---|---|---|---|
| T1 | 1 | B declares a role and an extension without a schema; A has `public.items` | B's `reconcile --prune` keeps `public.items` |
| T2 | 1 | (unit) cron SQL for owners `a`, `b` | `b`'s unschedule filter cannot match `a`'s jobs |
| T3 | 1 | Legacy `public._dbd_meta` holds `a` and `b`; `a` heals | `b`'s row and the table survive |
| T4 | 1 | Two owners import jsonl | separate staging tables |
| T5 | 2 | A and B both use `public` and `app`, with disjoint entities; each applies, then `reconcile --prune`, then `reset` | every entity of the other survives |
| T6 | 2 | B declares `app.orders`, owned by A | apply, reconcile and reset refuse; the catalog is unchanged; B has no rows |
| T7 | 2 | Two concurrent first claims on one entity | exactly one succeeds |
| T8 | 2 | A drops `orders.note`; B's view reads it | A refuses and names B's view |
| T9 | 2 | A's function body reads B's table; B drops that table | refused (`dbd.uses`, not `pg_depend`) |
| T10 | 2 | Single owner, hand-made `app.tmp` | still pruned (rule b) |
| T11 | 2 | Shared `public`, hand-made `public.tmp` | reported, not pruned |
| T12 | 2 | Backfill: two pre-registry owners share `public`; A upgrades and runs `reconcile --prune` before B | A claims only its own entities; B's `public` tables survive (rule (b) suspended, B named as not upgraded) |
| T13 | 2 | A removes `app.old` from its design without pruning | still owned by A; B's claim on it refused |
| T14 | 2 | `--adopt-from` transfer between repositories | B owns the entity; A's next prune leaves it |
| T15 | 2 | Same owner name, all-new entities | refused; `--allow-ownership-change` proceeds |
| T16 | 2 | Scoped run of A | claims the whole design, not the scope |
| T17 | 2 | `reset --clean` beside another owner using `vector` and schema `app` | `vector` and `app` survive |
| T18 | 2 | SQLite, second owner declares an existing table | refused |
| T19 | 3 | Two modules declare the same entity | `dbd inspect` fails before any database work |
| T20 | 3 | `core` references `dojo` without `depends_on` | gap error |
| T21 | 3 | Workspace deploy of shared, core and dojo into one database | three meta rows; ownership by module |
| T22 | 3 | An entity file moves from `core/` to `shared/` | ownership transfers; nothing is pruned |
| T23 | 3 | `dbd split --from-scopes` on a fixture shaped like sensei | three modules; `shared` is the intersection; inferred `depends_on`; imports split by owning module |
| T24 | 3 | Sensei converted; `--scope dojo` and default deploys | the same entities deploy to each database as with today's scopes; the `sensei` meta row is retired; module rows exist |

---

## Decisions

Settled 2026-10-12: 1, 2, 3 and 5 as recommended; 4 changed — unowned entities
in a shared schema are pruned after confirming.

1. **No list of what a module offers to others.** What a module protects is
   derived from what other owners actually use. If a restriction is ever
   needed, it gets a name that does not clash with `export:`, such as
   `provides:`. Recommended: no ceremony until a real need appears.
2. **Per-module versions, snapshots and migrations.** Recommended, because
   modules exist to evolve independently. The alternative is one workspace
   version.
3. **Owner identity `<project>/<module>`.** Recommended, because two workspaces
   may both have a `shared` module. The alternative is bare module names.
4. **Unowned entities in a shared schema are reported, and pruned only with
   `--prune-unowned`**, which names each one it drops. (Proposed: reported,
   never pruned; changed so a shared schema can be cleaned without dropping to
   SQL.) An explicit `dbd adopt <entity>` can still come later.
5. **One root manifest**, with module folders holding no configuration.
   Recommended (see section 2). The alternative is a `module.yaml` per module.

## Open / Deferred

- Owner roles: making each module's entities `OWNER TO` a per-module role would
  let Postgres itself refuse cross-owner drops. That is strong, but it needs
  `CREATEROLE`, ownership reassignment on backfill, and Supabase's `postgres`
  role complicates it. It can be layered on later.
- The scope guard is still CLI-only, and could fold into `preflight`.
- Roles are cluster-wide. The registry sees only this database, so a role used
  by another database is protected only by Postgres's own `DROP ROLE` checks.
- Objects created by hook scripts are not entities. They stay unowned, and
  under rule (b) they are pruned only where a single owner uses the schema.
