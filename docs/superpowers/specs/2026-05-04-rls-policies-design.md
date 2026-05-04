# RLS Policies Design

**Date:** 2026-05-04  |  **Status:** Draft
**Scope:** `dbd policies` command, Row-Level Security policy application

---

## Overview

Add a `dbd policies` command that discovers and applies RLS policy files from the `policies/` directory. Policy files contain raw SQL (`ALTER TABLE ... ENABLE ROW LEVEL SECURITY` + `CREATE POLICY`) and are executed after tables exist. Design is intentionally simple: scan, sort, execute. No parsing -- policies are opaque SQL scripts.

## Data Flow

```
dbd policies:      scan_policies() -> sort by path -> execute each via adapter.execute_script()
dbd apply --with-policies:  normal apply -> then policies pass
```

## Policy File Convention

Files in `policies/<schema>/<table>.ddl` (or `.sql`). Each file is self-contained and idempotent using `DROP POLICY IF EXISTS` + `CREATE POLICY`:

```sql
-- policies/config/users.sql
alter table config.users enable row level security;
drop policy if exists "users_select_own" on config.users;
create policy "users_select_own" on config.users for select using (auth.uid() = id);
```

## Pure Logic

```rust
/// Ordered list of policy files to apply (scanner already returns sorted).
pub fn policy_plan(project_dir: &Path) -> Vec<PathBuf> {
    scanner::scan_policies(project_dir)
}
```

## I/O Boundary

```rust
pub async fn apply_policies(
    adapter: &dyn DatabaseAdapter, project_dir: &Path, dry_run: bool,
) -> Result<PolicyReport>

pub struct PolicyReport {
    pub applied: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}
```

Error handling: failed files are logged and skipped (fail-forward). Command exits 1 if any failed, 0 if all succeeded. Empty/missing `policies/` is a no-op.

## CLI

```rust
Policies { #[arg(long)] dry_run: bool }                // new command
Apply { ..., #[arg(long)] with_policies: bool }         // extend existing
```

## Test Scenarios

| ID | Scenario | Assert |
|----|----------|--------|
| P1 | Scan finds sorted files | `[config/lookups.sql, config/users.sql, staging/events.sql]` |
| P2 | Empty policies dir | Empty plan, no error |
| P3 | Missing policies dir | Empty plan, no error |
| P4 | Dry-run shows files | File paths printed, no DB connection |
| P5 | Policies applied | `execute_script` called per file, report.applied populated |
| P6 | Failed file skipped | Error logged, other files still applied |
| P7 | --with-policies | Entities applied first, then policies |
| P8 | Only .ddl/.sql discovered | `.md` files ignored |
| P9 | Report summary | `applied.len()` + `failed.len()` matches file count |

## Files

| File | Action |
|------|--------|
| `crates/dbd-cli/src/cli.rs` | Modify -- `Policies` command, `--with-policies` on `Apply` |
| `crates/dbd-cli/src/commands.rs` | Modify -- `cmd_policies`, update `cmd_apply` |
| `crates/dbd-core/src/design.rs` | Modify -- `apply_policies()` method |

No new core modules. Scanner already has `scan_policies()`, adapter has `execute_script()`.

## Future Work

- Policy diffing against live `pg_policy` catalog to detect drift
- Dependency-ordered application based on table dependency graph
- Policy templates generated from design.yaml configuration
