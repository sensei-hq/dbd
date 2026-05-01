# Supabase Support Design

**Date:** 2026-04-30
**Status:** Approved
**Scope:** Wire Supabase-specific behavior into existing PostgresAdapter via config

---

## Overview

Supabase is Postgres with managed schemas (auth, storage, realtime). dbd already has grants generation, reset protection, and external entity support — but they're not wired together. This spec connects the pieces and adds safety defaults.

## Design Decisions

- **No separate SupabaseAdapter** — same PostgresAdapter, behavior driven by target config
- **Whitelist-only reset** — only drop schemas declared in user's `schemas:` config
- **Protected schemas** — hardcoded list that can never be dropped, even with `--force`
- **Grants after apply** — run grants script when target has grants config
- **External entities in DBML** — render as stub tables with `[note: 'external']`

---

## 1. Reset protection

### Whitelist-only schema drops

Change `build_reset_script()` to only drop schemas listed in the user's `schemas:` config. Currently it collects all schemas from entities + config and drops them. New behavior: **only schemas in `DesignConfig.schemas` are candidates for DROP**.

### Protected schemas (Supabase)

Hardcoded list that blocks drops regardless of target or `--force`:

```rust
const SUPABASE_PROTECTED: &[&str] = &[
    "auth", "storage", "realtime", "graphql_public",
    "supabase_functions", "pgbouncer", "pgsodium", "vault",
    "extensions", "supabase_migrations",
];
```

For Supabase targets, if any schema in the drop list matches this, emit an error. For non-Supabase targets, the whitelist-only rule still protects — you can't drop a schema you didn't declare.

### System schemas (always protected)

```rust
const SYSTEM_PROTECTED: &[&str] = &[
    "pg_catalog", "information_schema", "pg_toast", "public",
];
```

`public` is protected from DROP because Postgres recreates it but Supabase may not. Users should use `TRUNCATE` patterns instead.

### `skip_schemas` config

If `target.skip_schemas` is set, also exclude those from reset. This is for edge cases where the user has custom schemas they don't want dropped.

## 2. Grants after apply

After `Design::apply()` completes successfully, if the target has `grants` config:

1. Build grants script via existing `build_grants_script()`
2. Execute via adapter
3. Print summary

This happens in the CLI `cmd_apply`, not in the core library. The existing `build_grants_script()` already generates the correct SQL including `NOTIFY pgrst, 'reload config'`.

### Config format (already supported):

```yaml
target:
  supabase:
    url: $DATABASE_URL
    grants:
      config:
        anon: [usage, select]
        authenticated: [usage, select, insert, update, delete]
      staging:
        service_role: [usage, select, insert, update, delete]
```

## 3. Default externals for Supabase

`dbd init --target supabase` generates:

```yaml
external:
  - name: auth.users
    note: Supabase auth users table
  - name: auth.uid
    note: Supabase auth function (returns current user ID)
  - name: storage.objects
    note: Supabase storage objects table
  - name: storage.buckets
    note: Supabase storage buckets table
```

This prevents reference warnings when DDL files have FKs like `REFERENCES auth.users(id)`.

## 4. External entities in DBML

Currently `generate_dbml()` skips External entities. Change: render External entities as stub tables in DBML with a note:

```dbml
Table "auth"."users" {
  id uuid [pk, note: 'external table']

  Note: 'External: Supabase auth users table'
}
```

This ensures FK refs to `auth.users` render correctly in dbdocs. External entities don't have `table_def`, so we generate a minimal stub with just the columns referenced by FKs in user tables.

### Finding referenced columns

Scan all user tables' FK constraints and inline_fks. For any FK pointing to an external table, collect the `ref_columns`. Use those as the stub table's columns. If no FKs point to the external entity, skip it in DBML (it's a function or unused reference).

## 5. `skip_schemas` in entity filtering

During `Design::from_config()`, if `target.skip_schemas` is set, filter out any scanned entities whose schema matches the skip list. This prevents DDL files in managed schemas from being processed.

## 6. PostgresAdapter builder

Add fields and builder method:

```rust
pub struct PostgresAdapter {
    // ... existing fields
    protected_schemas: Vec<String>,
    skip_schemas: Vec<String>,
}

impl PostgresAdapter {
    pub fn with_target_config(mut self, target_name: &str, config: &TargetConfig) -> Self {
        if target_name == "supabase" {
            self.protected_schemas = SUPABASE_PROTECTED.iter().map(|s| s.to_string()).collect();
        }
        if let Some(ref skip) = config.skip_schemas {
            self.skip_schemas = skip.clone();
        }
        self
    }
}
```

---

## Test Scenarios

### Reset protection

#### R1: Reset only drops declared schemas
```
Given: schemas: [config, staging], entities also reference "auth" schema
When:  build_reset_script(schemas from config only)
Then:  DROP config, DROP staging. auth NOT dropped.
```

#### R2: Supabase protected schemas never dropped
```
Given: target=supabase, schemas: [config, auth]
When:  build_reset_script()
Then:  error: "auth" is a Supabase-protected schema
```

#### R3: System schemas never dropped
```
Given: schemas: [pg_catalog]
When:  build_reset_script()
Then:  error or skip: pg_catalog is system-protected
```

#### R4: skip_schemas excluded from reset
```
Given: schemas: [config, staging], skip_schemas: [staging]
When:  build_reset_script()
Then:  only DROP config
```

### Grants

#### G1: Grants script runs after apply for Supabase target
```
Given: target supabase with grants config
When:  cmd_apply completes
Then:  grants SQL executed, includes NOTIFY pgrst
```

#### G2: No grants for plain postgres target
```
Given: target postgres with no grants config
When:  cmd_apply completes
Then:  no grants executed
```

### Init

#### I1: Supabase init includes default externals
```
Given: dbd init --target supabase
Then:  design.yaml has external: [auth.users, auth.uid, storage.objects, storage.buckets]
```

### DBML

#### D1: External entity renders as stub table
```
Given: external entity "auth.users", user table has FK to auth.users(id)
When:  generate_dbml()
Then:  DBML has Table "auth"."users" with id column and external note
```

#### D2: External entity without FK refs is skipped
```
Given: external entity "auth.uid" (function, no FKs point to it)
When:  generate_dbml()
Then:  not in DBML output (functions don't need table stubs)
```

### Entity filtering

#### F1: skip_schemas filters entities
```
Given: DDL files in staging/ and auth/, skip_schemas: [auth]
When:  Design::from_config()
Then:  auth entities not in design.entities()
```

---

## Files Modified

| File | Change |
|------|--------|
| `script.rs` | Update `build_reset_script()` for whitelist-only + protected schemas |
| `adapter/postgres.rs` | Add `protected_schemas`, `skip_schemas` fields + `with_target_config()` |
| `design.rs` | Wire `skip_schemas` into entity filtering |
| `init.rs` | Add default externals + ignore patterns for Supabase template |
| `dbml.rs` | Render external entities as stub tables with referenced columns |
| `commands.rs` | Run grants script after apply when target has grants |
