# `_dbd_meta` Integration Design

**Date:** 2026-04-30
**Status:** Approved
**Scope:** Make `_dbd_meta` the authoritative version source, wire into apply, add `applied_at`

---

## Overview

Currently `_dbd_meta` exists but is only read by `reset()` for safety guards. `apply()` has a `SetVersion` execution step that is a no-op. `get_db_version()` reads from `_dbd_migrations` (counting records). This change makes `_dbd_meta` the single source of truth for project version.

## Changes

### 1. Schema: add `applied_at`

```sql
CREATE TABLE IF NOT EXISTS _dbd_meta (
    project TEXT PRIMARY KEY,
    env TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,
    applied_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
);
```

`applied_at` is updated on every `set_project_meta` call via `DEFAULT NOW()` in the UPSERT.

### 2. `get_db_version()` reads from `_dbd_meta`

Current: `SELECT MAX(version) FROM _dbd_migrations WHERE project = $1`
New: `SELECT version FROM _dbd_meta WHERE project = $1` (returns 0 if no row)

This is simpler and consistent — `_dbd_meta` is the one place to check version.

### 3. `set_project_meta()` always called on apply

The `SetVersion` execution step in `design.rs` calls `adapter.set_project_meta(env, version)` instead of being a no-op comment.

### 4. `ProjectMeta` gets `applied_at`

```rust
pub struct ProjectMeta {
    pub project: String,
    pub env: String,
    pub version: u32,
    pub applied_at: Option<String>,  // ISO 8601 timestamp
}
```

### 5. `_dbd_migrations` remains as audit log

No changes to `_dbd_migrations`. It continues to record which migration scripts ran. The version is no longer derived from it.

## Files Modified

| File | Change |
|------|--------|
| `adapter/mod.rs` | Add `applied_at` to `ProjectMeta` |
| `adapter/postgres.rs` | Update `ensure_meta_table` SQL, `get_db_version`, `set_project_meta`, `get_project_meta` |
| `adapter/mock.rs` | Update mock to track version in meta, return version from `get_db_version` |
| `design.rs` | Wire `SetVersion` step to call `set_project_meta` |

## Test Scenarios

### T1: Mock adapter get_db_version returns 0 when no meta
```
Given: fresh mock adapter (no meta set)
When:  get_db_version()
Then:  returns 0
```

### T2: Mock adapter get_db_version returns meta version
```
Given: mock with meta version=3
When:  get_db_version()
Then:  returns 3
```

### T3: set_project_meta updates version and env
```
Given: mock adapter
When:  set_project_meta("prod", 5)
Then:  get_project_meta() returns env="prod", version=5
       get_db_version() returns 5
```

### T4: apply SetVersion step writes meta
```
Given: execution plan with SetVersion(3) step
When:  apply executes
Then:  adapter.get_db_version() returns 3
```

### T5: reset safety guards still read from meta
```
Given: meta with env="prod", version=1
When:  reset(force=false)
Then:  SafetyGuard error
```

### T6: ProjectMeta includes applied_at
```
Given: set_project_meta called
When:  get_project_meta()
Then:  applied_at is Some (non-empty string)
```
