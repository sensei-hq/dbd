# Lifecycle script hooks — design spec

Scope-aware SQL hooks around the apply and import phases, so a project can run
custom SQL dbd does not model — enabling Supabase realtime, granting on objects,
attaching publications — at the right point in the pipeline.

## Problem

Two gaps, related enough to solve together.

**1. `import.after` ignores scope.** It is the only script hook dbd has, and
`design/import.rs:314` states it is deliberately unfiltered:

> *"These are intentionally NOT scope-filtered — they are post-import hooks, not tied to individual entries; scoped callers ensure their after-scripts are safe."*

The asymmetry is sharp. `import_data` filters its plan through
`import_entry_in_scope` and warns per excluded table
(`"staging table X not imported — outside scope 'Y'"`), then runs every
after-script regardless. A loader doing
`INSERT INTO target SELECT … FROM staging_a JOIN staging_b` produces silently
wrong results when `staging_b` was scoped out. dbd warns about the exclusion and
then runs the script against partial data anyway.

**2. There is no hook around the DDL phase at all.** The pipeline is
apply (DDL) → import (data) → policies (RLS). `policies/` is the only post-DDL
hook and is constrained to RLS policies. So schema-adjacent operations dbd does
not model have nowhere to live:

```sql
alter publication supabase_realtime add table app.messages;
grant select on all tables in schema app to app_ro;
```

These must run *after* the tables exist and, for a scoped run, only when the
tables they name are actually in scope.

There are no "before" scripts of any kind today. `import.staging` is a schema
allowlist, not scripts.

## Shape

Phase-local, so each phase's hooks sit with that phase's other settings and the
shipped `import.after` key does not move:

```yaml
apply:
  before:
    - sql/pre_ddl.sql
  after:
    - sql/realtime.sql
    - script: sql/grants.sql
      writes: [app.messages]

import:
  after:
    - import/loader.sql          # unchanged, still valid
```

`apply:` is a new top-level block; `DesignConfig` has no such key today.

### Entry form

An entry is either a bare path (derive its dependencies) or an object with an
explicit override:

```rust
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ScriptEntry {
    /// Bare path — dependencies derived from the SQL.
    Path(String),
    /// Explicit override, for SQL dbd cannot analyse (dynamic `EXECUTE`).
    WithWrites { script: String, writes: Vec<String> },
}
```

This mirrors `ImportTableEntry`, which already accepts a bare name or
`{name: options}` — same house pattern, same untagged-enum mechanism.

## Deriving dependencies

dbd could not do this before this release. It can now: the parser migration put
libpg_query in the DDL path, and `pg_query::parse(sql)` exposes `select_tables()`
and `dml_tables()` — the same machinery `pg::procs` uses to derive a routine's
reads and writes from its body.

So a hook script's table references come from parsing it, and it is then scoped
by exactly the rule import entries already use — `import_entry_in_scope`:
in scope when **every** referenced table is in the working set.

Unqualified names resolve against the script's `set search_path` if present, else
`public` — matching `pg::common::extract_search_paths_via_pg_query`'s contract.

**When derivation finds nothing** — an empty result, or SQL libpg_query cannot
parse — the script is treated as **unscoped and always runs**, with a warning
naming it. Silently skipping a hook because analysis failed would be the worse
failure: a user would see their realtime setup quietly not happen.

### Derivation fails on the flagship case, measured

The real `sensei` project's only after-script,
`import/after/realtime_publication.sql`, is a `do $$ … $$` block that attaches
relay tables to `supabase_realtime`. Run against it, every accessor returns
empty:

```
select_tables: []   dml_tables: []   tables: []   call_functions: []
```

Its table names never appear as SQL identifiers. They live as *data* — inside
`array['relay_sessions', 'relay_segments', 'relay_inbox']` and
`format('alter publication … dojo.%I', t)`. The PL/pgSQL tier does recover the
embedded statements, but they contain the format string, not the tables.

**No parser can derive this, and none ever will.** So `writes:` is not an escape
hatch for an exotic edge — it is the **primary** mechanism for exactly the kind
of hook this feature exists to serve. Derivation earns its place on the simpler
shape (`insert into target select … from staging_a join staging_b`), which is
the case that silently produced wrong data.

Note also that the real script already hand-guards itself three ways — absent
publication, absent table, already-a-member — precisely because it runs for every
scope. The feature's value there is replacing defensive PL/pgSQL with one
declarative line, not enabling something impossible today. The larger value is
the loader shape, where nothing guards anything.

## Behaviour under scope

Skip and warn, matching how excluded staging tables already behave:

```
⚠ after-script import/loader.sql skipped — needs staging.values,
  which is outside scope 'partial'
```

The import completes; nothing runs against partially-loaded data. This is
deliberately *not* a refusal: scoped imports are a normal workflow, and turning
one into a hard stop would punish routine use.

The all-scope (`is_all`) short-circuits to "everything in scope", as it does for
import entries — no derivation cost on the common path.

## Where the hooks run

- `apply.before` — in `Design::apply`, after `ensure_fully_parsed` and scope
  resolution (so a bad design still refuses first), before the first
  `apply_entity`.
- `apply.after` — after the DDL entities are applied and matviews stamped, but
  **before** `policies/`. Publications and grants attach to objects that must
  already exist; RLS policies are independent of them.
- `import.after` — unchanged position, gains the scope filter.

All three must honour `--dry-run` by reporting what would run without executing,
matching `import_run_after_scripts`'s existing `if dry_run` handling.

## Counts and reporting

`ApplyComplete` gains `before_scripts` / `after_scripts` counts;
`ImportComplete.after_scripts` already exists. Skipped hooks go through the same
`warnings` channel the import path already uses, so the CLI needs no new
rendering.

## Non-goals

- **Not a replacement for `policies/`.** RLS policies keep their folder and their
  unconditional-on-deploy behaviour.
- **No `import.before`.** Nothing has asked for one, and a pre-import hook has no
  clear contract — staging tables may not exist yet.
- **No ordering guarantees beyond list order.** Scripts run in the order written.
  Deliberately unlike `policies/`, whose filename-alphabetical ordering is a
  filed bug (`docs/BACKLOG.md`) — a list in config is explicit and reviewable.

## Risks

- **`apply.after` runs before `policies/`, which may surprise.** Someone writing
  a hook that depends on a policy existing will find it does not. Documented, not
  prevented — the reverse order would break the more common case (publications
  and grants on freshly-created tables).
- **Derivation is best-effort.** A script using dynamic SQL derives nothing and
  therefore always runs. That is the safe direction, but it means scope
  filtering is not a security boundary — it is a correctness aid. Say so in the
  guide, so nobody uses a scope to *prevent* a hook running.
- **A new top-level `apply:` key.** `DesignConfig` has no `deny_unknown_fields`,
  so a typo like `aply:` is silently ignored. That is pre-existing, but a new
  block widens the surface for it. Worth considering a validation pass
  separately.
