# Multi-Project Isolation in One Database (#7)

**Date:** 2026-10-06
**Status:** Proposed
**Scope:** Make "several independently-maintained dbd projects in one Postgres
database" a supported topology. Projects record which schemas they own in the
shared `dbd` bookkeeping schema. Every mutating entry point refuses to act when
the schemas overlap. Destructive paths never reach past what the project owns.
SQLite gets the same rule in its one namespace; Convex is unchanged.

---

## Overview

Isolation today rests on convention. `project.name` keys the bookkeeping rows.
`reconcile --prune` is bounded by `managed_schemas`, which is recomputed from the
design on every run. Nothing records what a project owns, and nothing checks one
project against another. Issue #7's line numbers are out of date: bookkeeping
moved to `dbd.meta` / `dbd.migrations` in the 2026-08-18 bookkeeping-schema spec.
The gaps it describes are all still there, plus five it did not list.

| # | Hazard | Where | Effect on a co-tenant |
|---|---|---|---|
| H1 | Overlapping schemas | `managed_schemas` (`design/scope.rs`) | `reconcile --prune` drops their tables as orphans; `reset --schemas` drops their schema |
| H2 | `public` enters `managed_schemas` undeclared | a target **role**, or an **extension** without `schema:`, has `schema = None`, which maps to `public` | any project with a role prunes the `public` tables of whichever project owns `public` |
| H3 | Same `project.name` | `ON CONFLICT (project) DO UPDATE` in `Bookkeeping::set_meta` | silent takeover of their version row and migration history |
| H4 | pg_cron refresh jobs | `sync_refresh_jobs` unschedules every `dbd:refresh:%` job not in *this* project's list, on every apply, deploy and reconcile | all their matview refresh jobs are unscheduled |
| H5 | Legacy heal | `Bookkeeping::heal` folds **and drops** `_dbd_meta` / `_dbd_migrations` in every schema, whoever wrote them | a co-tenant still on an older dbd loses its bookkeeping table |
| H6 | `reset` reach | per-entity `DROP … CASCADE`; `DROP EXTENSION … CASCADE`; `DROP ROLE` | their views and FKs on our tables, their extension-typed columns, and shared roles all go |
| H7 | Shared import staging | `staging._dbd_import_tmp` is created, truncated and dropped by every jsonl import | concurrent imports corrupt each other |
| H8 | `reset` guard sees one row | prod/version guard reads only this project's meta | a project with no row resets freely on a DB holding another project's prod data |
| H9 | Onboarding | `init --from-db` refuses on *any* bookkeeping; `merge` introspects every schema | a second project cannot onboard, and `merge` absorbs the first project's schemas |

### Model: a project owns schemas, exclusively

A **schema** is the unit of ownership. A project owns every schema its **whole
design** declares. That means its `Schema` entities, which already include every
schema an entity file lives in, plus `public` if it has unqualified
schema-scoped entities. Ownership is computed from the full design and **never
from the scope**; #40 established that a scope narrows what is applied, not what
the project owns.

Roles and extensions are not schema-scoped and own no schema. They are
**shared** resources. A project records that it *uses* them, so `reset` can
refuse to drop one that another project also uses.

Schema ownership is exclusive, and the database enforces it with a unique index.
The CLI check alone is not enough: two concurrent first runs must not both
succeed. `public` follows the same rule. At most one project on a shared
database may own `public`, and every other project must use named schemas. That
answers #7's open question about `public` without adding a special case.

### Why not per-object ownership

Tracking ownership per table would let two projects share `public`. It needs an
ownership marker on every object, a backfill of every object in every existing
database, and a prune planner that consults it per object. Exclusive schemas
meet every acceptance criterion in #7 with one small table. Per-object ownership
can be layered on later if a real need appears.

### Why not a per-project bookkeeping schema (issue option 5)

Option 5 was motivated by bookkeeping being forced into `public`, which the
`dbd` schema move already fixed. Once ownership is enforced, isolating the
bookkeeping rows adds no safety. Close it as superseded.

### "One design + scopes" vs separate projects

These are complementary, not alternatives. After #40, scopes are the right tool
for **one** team deploying modules of **one** design. Separate projects are for
designs maintained independently, with different repos, owners and release
cadences. The guide will say which to use when. #7 covers the second case.

---

## Changes

### Phase 1: stop the bleeding (no new tables; patch release)

Each of these is a standalone fix that is wrong even on a single-project
database. They ship first, whatever happens to the rest.

**1a. Roles and extensions stop contributing to `managed_schemas`** (H2).
`Design::managed_schemas` skips `EntityType::Role` and `EntityType::Extension`.
The search-path prelude still appends `public`, so name resolution does not
change. Only the prune and diff boundary does.

**1b. pg_cron jobs carry the project** (H4). The job name becomes
`dbd:refresh:<project>:<schema>.<name>`, and the unschedule filter becomes
`jobname LIKE 'dbd:refresh:<project>:%'`, with the project name escaped for both
`LIKE` and quotes. Untagged legacy jobs (`dbd:refresh:<schema>.<name>`) are
unscheduled only if `<schema>` is one this design declares. That migrates this
project's old jobs and leaves everyone else's alone.

**1c. Heal folds only this project's legacy rows** (H5). For each legacy
`_dbd_meta` / `_dbd_migrations`, heal copies and deletes the rows
`WHERE project = $1`, then drops the table only if it is now empty. Other
projects' rows stay where their own (possibly older) binary reads them, and
they fold when that project upgrades. The current fold-all copies a row with
`DO NOTHING`, which would freeze a stale copy in `dbd.meta` the moment the older
binary writes again. Folding per project avoids that.

**1d. Import staging moves into `dbd`, one table per project** (H7).
`staging.import_jsonb_to_table` becomes `dbd.import_jsonb_to_table`. The staging
table becomes `dbd."import_tmp:<project>"`, with a hash suffix when the name
would pass 63 bytes. Imports run through the pool (`execute_script`), so a
session `TEMP` table is not an option. `staging` reverts to being an ordinary
user schema, which finishes the `staging` item the bookkeeping spec deferred.
Stale `staging.import_jsonb_to_table` procedures in existing databases are left
in place, since an older co-tenant binary may still call them.

### Phase 2: the ownership registry (minor release)

**2a. `dbd.ownership`**, added to `LAYOUT_DDL` (so heal creates it on every
existing database):

```sql
CREATE TABLE IF NOT EXISTS dbd.ownership (
  project    varchar     NOT NULL,
  kind       varchar     NOT NULL CHECK (kind IN ('schema', 'extension', 'role')),
  name       varchar     NOT NULL,
  claimed_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (project, kind, name)
);
-- A schema has exactly one owner; extensions and roles are shared.
CREATE UNIQUE INDEX IF NOT EXISTS ownership_schema_exclusive
  ON dbd.ownership (name) WHERE kind = 'schema';
```

**2b. `Design::owned()`** returns an `Ownership { schemas, extensions, roles }`
computed from `entities_in_scope(None, None, None)`, the whole design.
`declared_out_of_scope` (#40) stays as it is. It answers a different question:
which tables to hide from a *scoped* prune.

**2c. Adapter trait** gains three methods. Convex and other adapters get no-op
defaults.

- `ownership(&self) -> Result<Vec<OwnershipRow>>` reads every project's rows. It
  is read-only and returns empty if the table does not exist yet.
- `claim(&self, owned: &Ownership) -> Result<()>` inserts this project's missing
  rows. A unique violation on `ownership_schema_exclusive` maps to
  `DbdError::Ownership { schema, owner }`.
- `release_unowned(&self, owned: &Ownership) -> Result<Vec<String>>` deletes
  this project's rows that the design no longer declares, and returns the
  released schema names.

**2d. One preflight, in core, for every mutating entry point.** The check lives
in core so library embedders get it too. There is no single choke point today,
so each entry calls one function:

```rust
// design/ownership.rs
pub(crate) async fn preflight(&self, adapter: &dyn Adapter, mode: Preflight) -> Result<()>
// Preflight::Claim  — apply, deploy (via apply), reconcile, reset: heal → check → claim
// Preflight::Check  — import, policies, refresh, reconcile --dry-run, diff: check only, no writes
```

The check refuses, with no DDL executed, when either of these holds:

1. **Overlap:** a schema in `owned.schemas` is owned by another project:
   `schema "billing" is owned by project "billing-svc"; projects sharing a
   database must use disjoint schemas`.
2. **Name reuse:** this project already owns schemas and the design declares
   none of them: `project "app" here owns {app, audit}; this design declares
   {shop}. If this is a different project, give it its own project.name; if the
   project moved its schemas, re-run with --allow-ownership-change`.

If the `ownership` table does not exist yet, check mode passes. Claim mode
re-checks atomically through the unique index, so
a concurrent loser gets the same error as a sequential one. After a successful
apply or reconcile, `release_unowned` runs and prints `released schema "x" — it
still exists in the database` for each schema released. `reset` keeps its
ownership, so nobody can take the schemas between a reset and the apply that
follows it.

**2e. Destructive paths consult ownership** (the issue's belt-and-braces, H1/H6/H8):

- `reset --schemas` emits `DROP SCHEMA` only for schemas this project owns.
- `reset --extensions` and the unscoped `DROP ROLE` skip any extension or role
  that another project also uses, and print a notice for each one skipped.
- Before any reset on a database with other registered projects, a `pg_depend`
  probe lists the objects outside this project's schemas that depend on
  something reset would drop. If there are any, reset refuses (`view
  billing.v_orders (project billing-svc) depends on app.orders`). `CASCADE`
  stays for the project's own objects.
- reset's prod guard also refuses when any **other** project's row is
  `env = 'prod'`, unless `--force` is given.
- `reconcile --prune` restricts the live snapshot to `managed ∩ owned-by-me`.
  After 2d this is redundant by construction; it is kept as the second lock.

**2f. Onboarding** (H9). `init --from-db` refuses only when **this**
`project.name` already has a row. On a shared database it introspects only
schemas no project owns, and notes the ones it skipped. `merge` excludes
schemas owned by other projects in the same way.

**2g. `dbd inspect --shared`** (read-only, needs `-d`). It lists each project
with its env, version, owned schemas and used extensions and roles, then flags:

- overlaps with the current design
- projects registered in `dbd.meta` that have no ownership rows yet (not upgraded)
- schemas that exist in the database but no project owns

It is the diagnosis step before a first deploy to a shared database.

**2h. Backfill.** No migration step is needed. The first upgraded mutating run
of each project claims its owned set, so existing single-project databases
upgrade on their next apply or reconcile. On a database that is already shared,
upgrades are first come, first served. A project whose schemas overlap one
already claimed gets the overlap error. That is the silent hazard #7 is about,
now made loud. `inspect --shared` shows it before anyone runs.

**2i. SQLite.** It has one namespace (`main`), so a SQLite project owns `main`.
`_dbd_ownership` mirrors the table, and the same overlap check refuses a second
project in the same file. Convex is one deployment per project and is unchanged.

### Phase 3: docs (with phase 2)

- A new guide section, "Sharing a database between projects": the disjoint
  schemas rule, `public`, `inspect --shared`, the upgrade order, and when to use
  scopes instead.
- `docs/design/architecture.md`: update the "No schema_prefix" rationale to say
  a shared database is supported when schemas are disjoint.
- Update guide 04 (`--allow-ownership-change`, `inspect --shared`), `llms.txt`,
  `llms-full.txt`, both `SKILL.md` copies, and the website mirrors. The
  `dbd-pattern-verifier` agent learns to flag unqualified entities in a project
  that targets a shared database.

---

## Files Modified

| File | Change |
|---|---|
| `crates/dbd-core/src/design/scope.rs` | 1a: `managed_schemas` skips Role/Extension |
| `crates/dbd-core/src/adapter/postgres/mod.rs` | 1b: cron job naming and filter; 1d: import staging in `dbd` |
| `crates/dbd-core/src/internal/import_jsonb_to_table.ddl` | 1d: procedure moves to `dbd` |
| `crates/dbd-core/src/adapter/postgres/bookkeeping.rs` | 1c: per-project fold; 2a: `dbd.ownership`; 2c: `ownership` / `claim` / `release_unowned` |
| `crates/dbd-core/src/adapter/mod.rs` | 2c: trait methods with no-op defaults; `OwnershipRow` |
| `crates/dbd-core/src/adapter/sqlite.rs` | 2i: `_dbd_ownership`, owner of `main` |
| `crates/dbd-core/src/design/ownership.rs` (new) | 2b/2d: `owned()`, `preflight()` |
| `crates/dbd-core/src/design/{apply,reconcile,reset,import}.rs`, `design/mod.rs` (`apply_policies`) | 2d: call `preflight` |
| `crates/dbd-core/src/script.rs`, `design/reset.rs` | 2e: ownership-aware schema, extension and role drops; dependency probe; prod guard |
| `crates/dbd-core/src/error.rs` | `DbdError::Ownership` |
| `src/commands/reverse.rs` | 2f: `init --from-db` and `merge` |
| `src/commands/schema.rs`, `src/cli.rs` | 2g: `inspect --shared`; `--allow-ownership-change` on apply, deploy, reconcile and reset |

---

## Test Scenarios

All Postgres scenarios are embedded-PG tests (`--features embedded-tests`). Each
is written first and seen failing before the change it covers.

| # | Phase | Scenario | Asserts |
|---|---|---|---|
| T1 | 1a | Project B declares a target role and an extension without a schema; A owns `public.items` | `B reconcile --prune` leaves `public.items` |
| T2 | 1b | Job SQL for projects `a` and `b` | `b`'s unschedule filter cannot match `a`'s job names; legacy jobs are filtered by declared schema |
| T3 | 1c | Legacy `public._dbd_meta` holds rows for `a` and `b`; `a` heals | `a` is folded and deleted; `b`'s legacy row is untouched; the table survives |
| T4 | 1d | Two projects run jsonl imports | each uses its own staging table; neither truncates the other's |
| T5 | 2 | A owns `app`, B owns `billing`; both apply, reconcile `--prune` and reset | no cross-drop: every table in both schemas survives |
| T6 | 2 | B declares `app` | apply, reconcile and reset error with `DbdError::Ownership`; the catalog is unchanged and B has no ownership rows |
| T7 | 2 | Same name, disjoint schemas | refused; `--allow-ownership-change` proceeds and moves ownership |
| T8 | 2 | Two concurrent first claims on one schema | exactly one succeeds |
| T9 | 2 | Existing DB with `dbd.meta` rows but no ownership; A runs, then B overlaps | A claims; B gets the overlap error |
| T10 | 2 | `reset --clean` on a shared DB; A and B both use `vector`; B has a view on A's table | A's reset keeps `vector` and refuses with the dependency named |
| T11 | 2 | A's design drops schema `old` | after reconcile A no longer owns `old`; `old` still exists; a notice is printed |
| T12 | 2 | `init --from-db` as project C on A's database | succeeds; the design omits A's schemas |
| T13 | 2 | `inspect --shared` | lists projects with their owned schemas; flags the overlap, the unupgraded project and the unowned schema |
| T14 | 2 | SQLite file holding project `a`; project `b` applies | refused |
| T15 | 2 | Scoped run of A (scope covers `app` only; design also declares `audit`) | A owns both `app` and `audit` |

---

## Decisions to confirm

1. **Exclusive schemas, `public` included.** At most one project per database
   may own `public`. Recommended: it is the simplest rule that closes H1 and H2,
   and it needs no per-object tracking.
2. **Detect name reuse from disjoint ownership, with no new `project.id`
   field.** Recommended. A UUID in `design.yaml` would also catch two
   independent projects that pick the same name *and* the same schema names.
   That is rare, and copying a repository copies the id anyway, so the UUID
   cannot tell two checkouts apart. It can be added later without changing
   anything here.
3. **Release a schema when the design stops declaring it** rather than holding
   it until an explicit release command. Recommended: the design is the source
   of truth everywhere else in dbd, and this adds no new command.

## Open / Deferred

- The scope guard (`check_scope_guard`) is still CLI-only and reads meta before
  heal. Folding it into `preflight` would give embedders the same protection.
  That is a separate change.
- Roles are cluster-wide. Ownership sees only the projects in **this** database,
  so a role shared with another database in the cluster is still Postgres's to
  protect: `DROP ROLE` fails while the role owns objects there.
- Two projects declaring the same qualified function in *different* schemas is
  fine. In the *same* schema it is now impossible, because the schema has one
  owner.
