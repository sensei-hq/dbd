# Scope Guard Design

**Date:** 2026-08-10
**Status:** Approved
**Scope:** Persist the applied scope in `_dbd_meta` and guard all write paths (`apply`, `deploy`, `reconcile`, `reset`) against a scope mismatch, mirroring the existing prod guard.

---

## Overview

dbd has a prod guard (`Design::reset`, `design.rs:1965`) that reads `_dbd_meta` and blocks a destructive reset when `env == "prod"` or migrations have been applied, overridable with `--force`. It has **no** equivalent protection against applying the *wrong scope* to a database.

Scopes (`design.yaml` `scopes:`, selected per-run via the global `--scope` flag) filter which entities a write path touches. Running `apply`/`deploy`/`reconcile` with a different `--scope` than the one a database was built with silently produces a **divergent schema** — a different set of tables — with no warning. Forgetting `--scope` entirely is the same hazard, because a missing flag resolves to the `default` scope (or `all`).

This change pins each database to exactly one scope, recorded in `_dbd_meta`, and refuses any write path that requests a different scope unless the operator explicitly opts in with `--allow-scope-change`.

### Model: one database = one scope

The **resolved** scope name (never `None` — a missing `--scope` resolves to `default`/`all` via `scope::resolve`) is stored on first write and enforced on every subsequent write. A team that legitimately wants multiple modules in one database expresses that as a named scope in `design.yaml` that includes both — not as two separate pins.

An override (`--allow-scope-change`) re-points the database: after a successful write it re-pins `_dbd_meta` to the newly-applied scope, so meta always reflects reality.

**Backward compatible:** existing databases have no scope recorded (`NULL` = unpinned). The first write after upgrade records the resolved scope without blocking; subsequent writes are guarded.

---

## Changes

### 1. Schema: add `scope` column to `_dbd_meta`

```sql
CREATE TABLE IF NOT EXISTS _dbd_meta (
    project    ... PRIMARY KEY,
    env        ...,
    version    ...,
    scope      varchar NULL,   -- NEW: resolved scope this DB is pinned to; NULL = unpinned
    created_at ...,
    updated_at ...
);
```

`ensure_meta_table` uses `CREATE TABLE IF NOT EXISTS`, so the new column will not appear on databases whose meta table already exists. Each adapter's `ensure_meta_table` therefore also runs an **idempotent add-column** step after the create:

- **Postgres** (`postgres.rs:1708`): `ALTER TABLE _dbd_meta ADD COLUMN IF NOT EXISTS scope varchar`.
- **SQLite** (`sqlite.rs:429`): SQLite has no `ADD COLUMN IF NOT EXISTS`; check `PRAGMA table_info(_dbd_meta)` for a `scope` column and run `ALTER TABLE _dbd_meta ADD COLUMN scope TEXT` only when absent.
- **Convex** sidecar (`convex.rs:722`): add `scope` to the sidecar meta document shape; absent field reads as `NULL`/unpinned.

### 2. `ProjectMeta` gets `scope`

`adapter/mod.rs:20`:

```rust
pub struct ProjectMeta {
    pub project: String,
    pub env: String,
    pub version: u32,
    pub scope: Option<String>,   // NEW: resolved scope pin; None = unpinned
    pub applied_at: Option<String>,
}
```

`get_project_meta` in every adapter (Postgres `postgres.rs:1722`, SQLite `sqlite.rs:442`, Convex `convex.rs:726`, Mock `mock.rs:227`) reads the new column into `scope`.

### 3. `set_project_meta` records the scope (the re-pin path)

The trait method (`adapter/mod.rs:239`) changes from `(env, version)` to `(env, version, scope)`:

```rust
async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()>;
```

All four impls (Postgres `postgres.rs:1748`, SQLite `sqlite.rs:461`, Convex `convex.rs:739`, Mock `mock.rs:231`) persist `scope` in the UPSERT. Call sites pass the **resolved scope name**:

- `SetVersion` execution step in `Design::apply` (`design.rs:1121`).
- `diff_live` write (`design.rs:1473`).

Because every successful write persists the current resolved scope, an overridden run re-pins automatically — no dedicated re-pin code path is needed.

### 4. The guard

New helper on `Design`:

```rust
fn check_scope_guard(
    &self,
    meta: Option<&ProjectMeta>,
    resolved_scope_name: &str,
    allow_scope_change: bool,
) -> Result<()>
```

Logic:

1. `allow_scope_change == true` → `Ok(())` (skip).
2. `meta.scope == Some(pinned)` and `pinned != resolved_scope_name` → `Err(DbdError::SafetyGuard(msg))`.
3. Otherwise (`meta` absent, or `scope` is `None`, or it matches) → `Ok(())`; this write pins/keeps the scope.

Reuses the existing `DbdError::SafetyGuard` variant (`error.rs:15`). The resolved scope name comes from `ResolvedScope.name` (`scope.rs:14`), produced by `Design::resolve_scope` (`design.rs:668`).

**Error message:**

```
scope guard: this database is pinned to scope 'public', but you requested 'internal'.
Applying a different scope would build a divergent schema.
→ re-run with --scope public, or pass --allow-scope-change to re-point this DB to 'internal'.
```

### 5. Call sites

The guard runs before any DDL, after the scope is resolved and meta is available:

| Path | Location | Notes |
|------|----------|-------|
| `apply` | `design.rs:931` (after scope resolve `949`) | Primary write path. |
| `deploy` | `design.rs:1841` | Inherits the guard through its call to `apply`; only needs the flag threaded in. |
| `reconcile` | `design.rs:1207` (near gap-gate `1238`) | Diffs against live under the resolved scope. |
| `reset` | `design.rs:1965` (alongside the existing prod guard `1974`) | See §6. |

### 6. Reset specifics

`reset` runs the scope guard alongside its existing prod/version guard, so a wrong-`--scope` teardown (which would drop the wrong schemas — or, with no `--scope`, everything) is refused. Bypass rule on reset: **`force || allow_scope_change`** skips the scope check (the existing `--force` already means "override all safety").

Because `reset` takes `version` back to `0`, it also clears the pin (`scope = NULL`) so the database returns to a clean unpinned state and the next `apply` re-pins fresh.

### 7. CLI

Add a `--allow-scope-change` boolean flag to the `Apply`, `Deploy`, `Reconcile`, and `Reset` subcommands in `cli.rs` (`Apply` 69, `Deploy` 133, `Reconcile` ~286, `Reset` 161), threaded through `commands/mod.rs` (`47`, `82`, `185`, `59`) to each handler:

- `cmd_apply` (`schema.rs:299`) → `Design::apply`
- `cmd_deploy` (`project.rs:309`) → `Design::deploy`
- `cmd_reconcile` (`project.rs:453`) → `Design::reconcile`
- `cmd_reset` (`migration.rs:10`) → `Design::reset`

Flag help text: `Allow this database to be re-pointed to a different scope (bypasses the scope guard).`

---

## Files Modified

| File | Change |
|------|--------|
| `crates/dbd-core/src/adapter/mod.rs` | Add `scope` to `ProjectMeta`; change `set_project_meta` signature to take `scope: Option<&str>` |
| `crates/dbd-core/src/adapter/postgres.rs` | `ensure_meta_table` add-column; read/write `scope` in `get_project_meta`/`set_project_meta` |
| `crates/dbd-core/src/adapter/sqlite.rs` | Same, with `PRAGMA table_info` guard for the add-column |
| `crates/dbd-core/src/adapter/convex.rs` | Same, in the sidecar meta document |
| `crates/dbd-core/src/adapter/mock.rs` | Track/return `scope` in mock meta |
| `crates/dbd-core/src/design.rs` | `check_scope_guard` helper; call it in `apply`/`reconcile`/`reset`; pass resolved scope to `set_project_meta` (`SetVersion` step + `diff_live`); clear pin on `reset` |
| `crates/dbd-cli/src/cli.rs` | `--allow-scope-change` on `Apply`/`Deploy`/`Reconcile`/`Reset` |
| `crates/dbd-cli/src/commands/mod.rs` | Thread the flag to each handler |
| `crates/dbd-cli/src/commands/schema.rs` | `cmd_apply` accepts + forwards the flag |
| `crates/dbd-cli/src/commands/project.rs` | `cmd_deploy` / `cmd_reconcile` accept + forward the flag |
| `crates/dbd-cli/src/commands/migration.rs` | `cmd_reset` accepts + forwards the flag; `force || allow_scope_change` bypass |
| `docs/skills/dbd/*` + relevant docs | Document the scope guard and the `scope` meta field |

## Test Scenarios

### T1: Fresh DB pins scope on first apply
```
Given: mock adapter with no meta row
When:  apply resolved scope "public"
Then:  no guard error; meta.scope == "public", version bumped
```

### T2: Matching scope passes
```
Given: meta pinned to scope "public"
When:  apply resolved scope "public"
Then:  no guard error; write proceeds
```

### T3: Mismatched scope blocks
```
Given: meta pinned to scope "public"
When:  apply resolved scope "internal" (no override)
Then:  DbdError::SafetyGuard naming both "public" and "internal"; no DDL executed
```

### T4: Forgotten --scope is guarded via the default
```
Given: meta pinned to scope "public"; project defines a "default" scope != "public"
When:  apply with no --scope (resolves to "default")
Then:  DbdError::SafetyGuard (default != public)
```

### T5: --allow-scope-change bypasses and re-pins
```
Given: meta pinned to scope "public"
When:  apply resolved scope "internal" with allow_scope_change = true
Then:  no guard error; write proceeds; meta.scope re-pinned to "internal"
```

### T6: Pre-existing unpinned DB does not block
```
Given: meta row with scope == NULL (upgraded DB)
When:  apply resolved scope "public"
Then:  no guard error; meta.scope pinned to "public"
```

### T7: Reset guard + pin clear
```
Given: non-prod, version 0 meta pinned to scope "public"
When:  reset with --scope internal, no --force / --allow-scope-change
Then:  DbdError::SafetyGuard (scope mismatch); nothing dropped
And:   reset with --scope public (or --force) succeeds; meta.scope cleared to NULL
```

### T8: Adapter meta round-trip
```
Given: each adapter (Postgres/SQLite via integration harness, Mock)
When:  set_project_meta(env, version, Some("public")) then get_project_meta
Then:  returned ProjectMeta.scope == Some("public")
```

### T9: reconcile and deploy are guarded
```
Given: meta pinned to scope "public"
When:  reconcile / deploy with resolved scope "internal", no override
Then:  DbdError::SafetyGuard before any DDL
```

## Open / Deferred

- No change to the `scope` value semantics for `all` — pinning to `all` and later requesting a subset is a mismatch (intentional: forces an explicit `--allow-scope-change`).
- `doctor` could later surface the pinned scope and warn on drift; out of scope for this change beyond documenting the field.
