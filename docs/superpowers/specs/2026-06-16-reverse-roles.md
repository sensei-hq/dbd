# Reverse-engineer roles (opt-in) — design

- **Date:** 2026-06-16
- **Status:** approved (design)
- **Target release:** minor (v0.6.0 → v0.7.0)
- **Builds on:** the reverse engine + functions/procedures (`2026-06-15`, `2026-06-16-…-functions-procedures`)

## Goal

Optionally reverse-engineer **roles** (name + role memberships) from a database. Roles are
**cluster-global**, not owned by the database, and managed/Supabase clusters are dominated by
platform roles — so role capture is **opt-in** (`--roles`) and aggressively filtered.

## Scope (matches dbd's existing role model)

dbd's role model is minimal: `script::generate_role_script` emits an idempotent
`CREATE ROLE "name"` (guarded by a `pg_roles` existence check) plus `GRANT "parent" TO "name"`
for each membership. Reverse therefore captures **name + memberships only** — no role
attributes (`LOGIN`/`SUPERUSER`/`CREATEDB`/…), and **never passwords**. (Attributes would
require extending the role model + emitter; out of scope here.)

## Opt-in flag

- Add `--roles` to both `dbd init --from-db` and `dbd merge`. **Default off** — reverse stays
  database-scoped unless the user explicitly asks for roles.

## Introspection (`introspect_roles`, only called when `--roles`)

New adapter trait method `async fn introspect_roles(&self) -> Result<Vec<Entity>>` with a
default impl returning `Ok(vec![])`; Postgres overrides it. It is **not** part of the default
`introspect()` — the CLI calls it separately and appends the results only when `--roles` is set.

1. Roles: `SELECT r.rolname, r.rolsuper FROM pg_roles r ORDER BY r.rolname`.
2. Memberships: join `pg_auth_members` → `(member rolname, granted rolname)`.

### Role denylist (always applied, even with `--roles`)

Exclude a role when any holds:

- `rolsuper = true` (superusers — admin/platform).
- name starts with any of: `pg_`, `rds_`, `azure_`, `cloudsql`, `supabase_`, `pgsodium`.
- name is one of (a named constant `ROLE_DENYLIST`): `postgres`, `anon`, `authenticated`,
  `service_role`, `authenticator`, `dashboard_user`, `pgbouncer`.

The surviving set is the project's own roles.

### Entity construction

- One `Entity { entity_type: Role, name: rolname, schema: None }` per kept role.
- `refers` = the names of roles this role is a member of **that are also in the kept set**
  (so the emitted role DDL is self-contained — every `GRANT … TO …` target is also emitted;
  memberships in platform/denied roles are dropped to keep the project portable).

## Emit + wiring

- Emit reuses the existing `Role` arm of `emit_entity` → `script::ddl_from_entity` →
  `generate_role_script`. No emitter change.
- `entity_path` already maps `Role` (`has_schema() == false`) to flat `ddl/role/<name>.ddl`.
- Add `EntityType::Role` to `MANAGED_KINDS` so role entities flow through
  `plan_from_entities`. (Role is a flat kind, so the orphan scan — which only walks
  schema-qualified kinds — does not flag role files, consistent with schema/extension files.)
- Roles are schema-less, so `select_and_keep`'s schema filter keeps them whenever present
  (they are gated by the `--roles` opt-in at introspection time, not by schema selection).

## CLI wiring

- `--roles` threads from the `Init`/`Merge` clap variants → dispatch → `cmd_init_from_db` /
  `cmd_merge`. After `adapter.introspect()`, when `roles` is set, append
  `adapter.introspect_roles().await?` to the entity list before planning. Works on both the
  init (baseline-snapshot) and merge (foreign + managed) paths unchanged.

## Testing

- **Filter unit test** (pure): a `role_is_managed(name, is_super)`-style predicate — keep a
  plain role; drop `pg_*`, `rds_*`, `supabase_*`, `pgsodium*`, the named denylist, and any
  superuser.
- **Membership self-containment unit test**: given kept roles {app, app_ro} and memberships
  app_ro→app (kept) and app→authenticated (denied), `refers` for app_ro = ["app"], for
  app = [] (the platform membership dropped).
- **Embedded** (feature `embedded-tests`): `CREATE ROLE app_admin; CREATE ROLE app_ro;
  GRANT app_admin TO app_ro;` (+ leave the platform/system roles present). With roles
  introspected, assert: `app_admin`/`app_ro` captured; `app_ro.refers == ["app_admin"]`;
  no `pg_*`/superuser role appears.
- **Default-off**: a CLI parse test that `--roles` is accepted on init + merge; and that the
  engine emits no role entities when the flag is absent (covered by introspect_roles not
  being called).

## Out of scope (future)

- Role **attributes** (LOGIN/SUPERUSER/CONNECTION LIMIT/VALID UNTIL) and object-level grants.
- **Sequences** — the next and final planned reverse patch (needs `EntityType::Sequence`).
- Role **drift tracking**: roles are not migration- or snapshot-tracked (unlike schemas and
  extensions), so role drift is not captured by snapshots and will not trigger a migration.
