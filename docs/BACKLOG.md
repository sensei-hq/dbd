# Backlog

## Current status (2026-04-30)

**78 commits, 13,500+ LOC, 341 tests, verified on sensei/daemon/database (116 entities)**

### Working commands

| Command | Offline | With DB | Notes |
|---------|---------|---------|-------|
| `inspect` | yes | — | Full validation pipeline |
| `apply` | `--dry-run` | yes | Version-aware: fresh install, migrations, idempotent re-apply |
| `import` | `--dry-run` | yes | CSV/TSV/JSONL, truncate, procedure matching, dependency ordering |
| `export` | — | yes | COPY TO STDOUT → csv/tsv/jsonl files |
| `reset` | `--dry-run` | yes | Safety guards (_dbd_meta env/version) |
| `combine` | yes | — | Merge all DDL into single SQL file |
| `graph` | yes | — | JSON with nodes/edges/layers |
| `dbml` | yes | — | Include/exclude schema+table filters |
| `doctor` | yes | — | Config migration from Node.js format |
| `snapshot` | yes | — | Smart multi-snapshot: rename/type change (2 stages), enum removal (3 stages) |
| `migrate --status` | — | yes | Read-only version diagnostic |
| `init` | yes | — | Scaffold new project (postgres/supabase) |
| `deploy` | `--dry-run` | yes | Deploy from local path or GitHub source |

### Core features

- **Schema diff engine** — columns, constraints, indexes, enum values with SQL generation
- **Smart multi-snapshot** — auto-detects renames/type changes/enum removal, generates intermediate states + data.sql
- **`_dbd_meta`** — authoritative version source, env/version/applied_at tracking
- **Migration data corrections** — `*.data.sql` after ALTERs, CAST heuristics, TODO for business logic
- **Execution plan** — pure function builds plan from version state, thin I/O wrapper executes
- **Change classification** — `is_castable()`, `classify_changes()`, `generate_data_sql()`
- **DBML filters** — include/exclude by schema or table name from config
- **Import truncate** — TRUNCATE staging tables before COPY (default: true)
- **Deploy** — local path or GitHub source (reqwest + tar, cached)

### Adapter support

| Target | Status |
|--------|--------|
| PostgreSQL | Working (sqlx, PG17+ assumed) |
| Supabase | Working (config-driven: grants, protected reset, externals) |
| SQLite | Planned |
| Convex | Planned |

---

## P2 — Quality & tooling

### DDL formatter
- `dbd format` — format all DDL files to project conventions
- Configurable: keyword case, comma style (leading/trailing), type alignment column
- Default style: lowercase keywords, leading commas, types at column 27
- `dbd inspect --fix` integration (auto-fix formatting issues)
- Pre-commit hook integration

### Supabase support — DONE
- Whitelist-only reset: only drops schemas declared in config
- Protected schemas: auth, storage, realtime etc. can never be dropped (even with --force)
- `target.skip_schemas` wired: excludes entities from apply/scan
- Grants after apply: GRANT per schema/role + NOTIFY pgrst
- Default externals in init: auth.users, auth.uid, storage.objects, storage.buckets
- External entities render as DBML stub tables for FK targets

### Config gaps remaining
- `target.schema_prefix` — multi-tenant schema prefix
- Per-table `export.format` — override per table (currently CLI `--format` only)

### SQLite adapter
- `rusqlite` integration
- No schemas, extensions, roles, enums, stored procedures
- Sync execution model
- Import via INSERT statements (no COPY)

### Convex adapter
- Generate `convex/schema.ts` from Entity + TableDef
- SQL type → Convex validator mapping
- `prefersBatchApply()` → single-pass generation
- Optional `npx convex deploy`

---

## P3 — Advanced features

### Import environment filtering
- Only load import files matching `--environment` (dev/prod)
- Path convention: `import/dev/staging/test_data.csv`, `import/prod/staging/seed.csv`

### Adapter catalog queries
- Load pg_proc + pg_type + pg_extension on connect
- Replace static pattern matching with live catalog lookup
- Cache per connection URL in `~/.cache/dbd/`
- Eliminates false positive reference warnings

### Parallel file parsing
- `rayon::par_iter` for DDL file read + parse
- Dependencies: rayon already in Cargo.toml
- Benchmark: measure parsing time on large projects (100+ DDL files)

### DBML enhancements
- Multi-document support (multiple dbml output files)
- Table group generation
- Composite FK ref support

### Policy application
- `dbd policies` command
- Scan `policies/` folder
- Apply RLS policies via adapter

### Database inspection
- `dbd inspect` with `--database` resolves warnings against live DB catalog
- DB reference cache for offline use

### data.sql validation
- `dbd inspect` verifies all TODO comments in data.sql have been resolved
- Block apply if unresolved TODOs exist

---

## Test coverage

**335 tests** (297 unit + 38 integration) covering:
- Schema diff engine (45 tests): D1-D21, S1-S14, warnings, edge cases
- Snapshot create (17 tests): SC1-SC10, entity conversion, backward compat
- Multi-snapshot (7 tests): B1-B3, S1-S2, baseline, no-changes
- Change classification (8 tests): C1-C7, enum rename
- Castability (9 tests): CA1-CA5, type categories
- data.sql generation (5 tests): D1-D5
- Execution plan (12 tests): A1-A6, edge cases
- DBML filters (4 tests): include/exclude schema/table
- Init (7 tests): postgres/supabase targets, directory creation
- Deploy (4 tests): local resolve, not-found, subpath
- Config/entity/parser/scanner/dependency/references (221 tests)

### CI configuration

```yaml
# .github/workflows/test.yml
name: Test
on: [push, pull_request]
jobs:
  unit-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test
      - run: cargo clippy --all-targets -- -D warnings

  db-tests:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: pgvector/pgvector:pg17
        env:
          POSTGRES_PASSWORD: test
          POSTGRES_DB: dbd_test
        ports: ['5432:5432']
        options: --health-cmd pg_isready --health-interval 10s --health-timeout 5s --health-retries 5
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --features test-db
        env:
          DATABASE_URL: postgres://postgres:test@localhost:5432/dbd_test
```
