# Access control

Three separate questions decide who can read a table, and dbd manages each from
files:

| Question | Managed by | Applied by |
|---|---|---|
| Can a role use the schema and its tables at all? | `grants:` in `design.yaml` | `dbd apply`, `dbd deploy` |
| Is the schema reachable from outside the database? | `exposed:` in `design.yaml` | nothing — it drives a report |
| Which rows can a role see? | RLS policies in `policies/` | `dbd deploy`, `dbd policies`, `dbd apply --with-policies` |

Grants, `exposed:` and policies are PostgreSQL features. SQLite and Convex have
no grant model: `dbd apply` skips grants there with
`Skipping schema grants (target has no grant model).`

## Grants

Grants can be declared in two places, and the two merge.

**On a schema entry**, for any target:

```yaml
schemas:
  - config
  - app:
      grants:
        app_user: [usage, select]
        app_admin: [usage, select, insert, update, delete]
```

**Under the target**, keyed by schema:

```yaml
target:
  supabase:
    url: $DATABASE_URL
    grants:
      app:
        anon: [usage, select]
        authenticated: [usage, select, insert, update, delete]
```

`target.grants` is read from the first target, whatever its key — it is not a
Supabase-only setting. For each schema, the target's roles are added to the
schema entry's, and for a role named in both places the target's list replaces
the schema entry's.

### The SQL they become

For each schema and role:

- `usage` becomes `GRANT USAGE ON SCHEMA "<schema>" TO "<role>";`
- every other permission becomes
  `GRANT <PERMS> ON ALL TABLES IN SCHEMA "<schema>" TO "<role>";` plus
  `ALTER DEFAULT PRIVILEGES IN SCHEMA "<schema>" GRANT <PERMS> ON TABLES TO "<role>";`,
  so tables created later get the same grant.

Write `usage` in lower case: the match is exact, and `USAGE` would be emitted as
a table privilege. The script ends with `NOTIFY pgrst, 'reload config';` so a
running PostgREST picks the change up.

Config grants cover schemas and tables only. Grants on functions, sequences or
anything else belong in an [`apply.after` hook](03-design-yaml.md#apply).

### When they are applied

`dbd apply` and `dbd deploy` both apply grants, once the schema is in place:
after every entity, migration and `apply.after` hook, and before the import
phase and the policies. An application embedding dbd gets them from
`Design::deploy` the same way, or from `Design::apply_grants` on its own.

They are not applied by `--dry-run`. Under `--scope`, only the grants on
schemas the scope builds are applied — a grant on a schema that database was
never given would fail the run — and on a `supabase` target the PostgREST
`USAGE` grants below are narrowed the same way.

### PostgREST on Supabase

When the first target is named `supabase` and the design declares any grants,
dbd also grants `USAGE` on every schema listed under `schemas:` to `anon`,
`authenticated` and `service_role` — the roles PostgREST connects as. With no
grants configured nothing is added, so a Supabase project that manages access
itself sees no change.

## Exposed schemas

`exposed:` says whether something **outside** the database serves a schema —
PostgREST, on Supabase. A table in an exposed schema is reachable by `anon` over
HTTP; a table anywhere else is not.

```yaml
schemas:
  - app:
      exposed: true
  - internal            # private: the default
```

A schema is internal unless declared exposed, with one exception: on a
`supabase` target, `public` is exposed by default (when it is listed). An
explicit `exposed: false` overrides that; a bare `public` does not, because
saying nothing is not saying no.

`exposed:` grants nothing. What it buys you is a report: `dbd inspect` lists
every table in an exposed schema that has no policy file.

```
⚠ No RLS policy on 1 table(s) in exposed schema(s) app:
  app.orders
  A policy lives at policies/<schema>/<table>.sql. Declare `exposed: false`
  on the schema if nothing outside the database serves it.
```

The report is advisory — it never changes the exit code, because dbd cannot
know you did not mean it. dbd checks that `policies/<schema>/<table>.sql` (or
`.ddl`) exists; it does not parse the policy.

Exposure is a different axis from platform ownership. Supabase's `extensions`
schema is Supabase's and not exposed; your own `app` schema can be exposed
without being Supabase's. Ownership decides what `reconcile --prune` may touch;
exposure decides what the internet can read.

## Row-level security policies

A policy file sits at `policies/<schema>/<table>.sql` and holds idempotent SQL:

```sql
alter table app.orders enable row level security;
drop policy if exists "orders_select_own" on app.orders;
create policy "orders_select_own" on app.orders
  for select using (auth.uid() = customer_id);
```

Three commands apply them, and they differ in what a failure means:

| Command | When policies run | A failing file |
|---|---|---|
| `dbd deploy` | always, as the last phase | a warning, counted in the summary; the deploy exits 0 |
| `dbd policies` | that is all it does | reported, the rest still run, exit 1 |
| `dbd apply --with-policies` | after the entities and grants | reported, exit 1 |
| `dbd apply` | **never** | — |

"The deploy succeeded" is therefore not evidence that RLS is in place — read
the summary's failure count.

### Policies under a scope

A policy protects one table, and dbd reads which from the path:
`policies/<schema>/<table>.sql` names `<schema>.<table>`. Under `--scope`, a
policy whose table is outside the working set is skipped and reported instead
of failing against a database that does not have its schema:

```
Skipped ./policies/dojo/repository_metrics.sql — dojo.repository_metrics is
outside scope 'daemon'
```

A file off that layout has no derivable table and always applies.
`dbd policies --dry-run` applies the same filter, listing what would apply and
what it would skip. Scope filtering is a correctness aid, not a security
boundary.
