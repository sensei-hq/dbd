# Supabase Support — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire Supabase-specific behavior (grants, reset protection, externals, DBML stubs) into existing PostgresAdapter via config.

**Architecture:** No new adapter struct. Existing PostgresAdapter gains protected_schemas/skip_schemas fields and a builder method. Reset uses whitelist-only schema drops. Grants run after apply when config has grants. External entities render as DBML stub tables. All behavior is config-driven.

**Tech Stack:** Rust, existing adapter/script/dbml/init modules

**Spec:** `docs/superpowers/specs/2026-04-30-supabase-support-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/dbd-core/src/script.rs` | Modify | Whitelist-only reset + protected schema constants |
| `crates/dbd-core/src/init.rs` | Modify | Add default externals to Supabase template |
| `crates/dbd-core/src/dbml.rs` | Modify | Render external entities as stub tables |
| `crates/dbd-core/src/design.rs` | Modify | Wire skip_schemas into entity filtering |
| `crates/dbd-cli/src/commands.rs` | Modify | Run grants after apply, pass target to reset |

---

### Task 1: Reset protection — whitelist-only + protected schemas

**Files:** `crates/dbd-core/src/script.rs`

Update `build_reset_script()` to:
1. Add `SUPABASE_PROTECTED` and `SYSTEM_PROTECTED` constants
2. Accept `user_schemas` (from config) and `skip_schemas` parameters
3. Only drop schemas in `user_schemas`, minus `skip_schemas`
4. Error if any user_schema is in the protected list

Tests: R1-R4 from spec + update existing tests.

### Task 2: Supabase init with default externals

**Files:** `crates/dbd-core/src/init.rs`

Add external entries to the Supabase template:
```yaml
external:
  - name: auth.users
    note: Supabase auth users table
  - name: auth.uid
    note: Supabase auth function
  - name: storage.objects
    note: Supabase storage objects
  - name: storage.buckets
    note: Supabase storage buckets
```

### Task 3: External entities in DBML

**Files:** `crates/dbd-core/src/dbml.rs`

Render External entities as stub tables. Scan user tables' FKs to find referenced columns. Generate minimal Table block with `[note: 'external']`.

Tests: D1, D2 from spec.

### Task 4: skip_schemas in entity filtering

**Files:** `crates/dbd-core/src/design.rs`

During `from_config_with_dir()`, read `target.skip_schemas` and filter out entities whose schema matches.

Test: F1.

### Task 5: Grants after apply in CLI

**Files:** `crates/dbd-cli/src/commands.rs`

After `design.apply()` in `cmd_apply`, if target has grants config, build and execute grants script.

### Task 6: Final verification + backlog update
