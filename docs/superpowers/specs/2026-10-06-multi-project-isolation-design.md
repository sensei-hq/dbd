# Multi-Project Isolation in One Database (#7)

**Date:** 2026-10-06 (rev 3, 2026-10-07)
**Status:** Proposed
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
  kind    varchar NOT NULL CHECK (kind IN ('schema', 'extension', 'role', 'entity')),
  target  varchar NOT NULL,        -- 'app', 'vector', 'basic', 'relation:identity.users'
  via     varchar NOT NULL DEFAULT '',  -- 'view billing.v_invoices'
  PRIMARY KEY (owner, kind, target, via)
);
```

The `relation` namespace covers tables, views, materialized views, sequences
and foreign tables, which all share `pg_class`. A table and a view of the same
name cannot both exist, so they cannot have two owners. `type` covers enums,
domains and composites. `routine` is per name: dbd already drops every overload
of a name together. Indexes, constraints, triggers, policies and comments
belong to their table.

**Schemas, extensions and roles are shared containers**, recorded in
`dbd.uses`. No schema is special. An owner never "owns" `public`, `app`, or any
other schema; it owns the entities it puts there.

### Rules

**Claim (apply, deploy, reconcile, reset).** Before any DDL, the owner claims
every entity its whole design declares. "Whole design" means the full design,
never the scope (#40). A claim on an entity held by another owner fails with no
DDL executed:

`relation app.orders is owned by "shop/billing"; owners sharing a database must not declare the same entity`

The primary key makes this atomic: of two concurrent first runs, exactly one
wins.

**Prune.** `reconcile --prune` drops a live entity only when one of these holds:

- (a) this owner owns it and no longer declares it; or
- (b) nobody owns it, and its schema is used by no other owner.

Rule (b) keeps today's drift clean-up for a database with one owner. In a
schema that another owner also uses, unowned entities are reported and never
dropped:

`unowned: public.tmp_report — not dropped because public is shared with "billing"`

Entities owned by someone else are never candidates. This replaces
`managed_schemas` as the prune boundary. `restrict_snapshot_to_schemas` stays
as a second lock.

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

**Uses are guarded.** At deploy time an owner records every reference its
design makes to another owner's entity, from dbd's parsed references, the same
ones scopes and the dependency graph use. Parsed references see into function
bodies, which `pg_depend` does not. Before an owner drops or destructively
alters an entity, dbd checks both `dbd.uses` and `pg_depend`. If another owner
depends on it, dbd refuses:

`identity.users.email is used by "billing" (view billing.v_invoices); remove that use first`

This is the contract between owners. It plays the role an API plays between
services.

**Reset.** Reset drops only entities this owner owns. On top of that:

- `--schemas` drops a schema only when it holds no entity owned by anyone else
  and no unowned entity.
- `--extensions` and `DROP ROLE` skip anything another owner uses.
- `CASCADE` stays, after the same uses guard: Postgres's cascade cannot reach
  another owner's entity, because the guard refused first.
- The prod guard refuses while any owner's row is `env = 'prod'`, unless
  `--force` is given.

**Name reuse** (H3) is detected at claim time. Suppose a design arrives under an
owner name that already owns entities in this database, and it declares none of
them. dbd refuses with `owner "app" here owns 42 entities; this design declares
none of them`. `--allow-ownership-change` overrides it.

**Backfill.** The first upgraded run of each owner claims the declared entities
that already exist and nobody owns. Undeclared live entities stay unowned. On a
database with one owner, rule (b) keeps pruning them exactly as today. No
migration step is needed.

Rule (b) is suspended while any owner in `dbd.meta` has not upgraded yet (a
meta row with no `dbd.uses` rows). That owner's schema uses are unknown, so an
unowned entity might be its own. The prune report names the owners still to
upgrade.

**SQLite** uses the same model in its single namespace (`_dbd_objects`,
`_dbd_uses`), with no special case. **Convex** is one deployment per project and
is unchanged.

### Where it runs

There is one core function, so library embedders are protected too:

```rust
pub(crate) async fn preflight(&self, adapter: &dyn Adapter, mode: Preflight) -> Result<()>
// Claim — apply, deploy, reconcile, reset: heal → claim → record uses
// Check — import, policies, refresh, --dry-run, diff: read-only, same refusals, no writes
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
  `path` (default `<module>/`), and apply hooks that belong to one module
  (`modules.<name>.apply`). All of those fit in one block.
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

- **Static overlap check.** `dbd inspect` refuses when two modules declare the
  same entity. This is the workspace version of the claim rule, caught before
  any database is involved.
- **Cross-module references** must follow `depends_on`. A reference into a
  module that is not a dependency is a gap, reported with the same machinery
  and wording as scope gaps. Within a workspace dbd knows the real definition,
  so a dependency's entities are full FK targets, not `external:` stubs.
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

## Decisions to confirm

1. **No list of what a module offers to others.** What a module protects is
   derived from what other owners actually use. If a restriction is ever
   needed, it gets a name that does not clash with `export:`, such as
   `provides:`. Recommended: no ceremony until a real need appears.
2. **Per-module versions, snapshots and migrations.** Recommended, because
   modules exist to evolve independently. The alternative is one workspace
   version.
3. **Owner identity `<project>/<module>`.** Recommended, because two workspaces
   may both have a `shared` module. The alternative is bare module names.
4. **Unowned entities in a shared schema are reported, never pruned.** An
   explicit `dbd adopt <entity>` can come later if reports pile up.
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
