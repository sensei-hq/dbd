# Backlog

## Current status (2026-04-30)

**67 commits, 13,000+ LOC, 330 tests, verified on sensei/daemon/database (116 entities)**

### Working commands

| Command | Offline | With DB | Notes |
|---------|---------|---------|-------|
| `inspect` | yes | — | Full validation pipeline |
| `apply` | `--dry-run` | yes | Version-aware: fresh install, migrations, idempotent re-apply |
| `import` | `--dry-run` | yes | CSV/TSV/JSONL, reads-based procedure matching, dependency ordering |
| `reset` | `--dry-run` | yes | Safety guards (_dbd_meta env/version) |
| `combine` | yes | — | Merge all DDL into single SQL file |
| `graph` | yes | — | JSON with nodes/edges/layers |
| `dbml` | yes | — | Verified with dbdocs build |
| `doctor` | yes | — | Config migration from Node.js format |
| `snapshot` | yes | — | Smart multi-snapshot: rename/type change (2 stages), enum removal (3 stages) |
| `migrate --status` | — | yes | Read-only version diagnostic |
| `init` | yes | — | Scaffold new project (postgres/supabase) |
| `deploy` | `--dry-run` | yes | Deploy from local path or GitHub source |

### Completed features

- **Schema diff engine** (`diff.rs`) — columns, constraints, indexes, enum values
- **SQL generation** — ALTER TABLE, DROP TABLE, CREATE INDEX, ALTER TYPE, with FK actions + index ordering
- **Smart multi-snapshot** — auto-detects complex changes, generates intermediate states + data.sql
- **`_dbd_meta`** — authoritative version source, env/version/applied_at tracking
- **Migration data corrections** — `*.data.sql` runs after schema ALTERs, CAST heuristics, TODO for business logic
- **Execution plan** — pure function builds plan from version state, thin I/O wrapper executes
- **Change classification** — `is_castable()`, `classify_changes()`, `generate_data_sql()`

### Adapter support

| Target | Status |
|--------|--------|
| PostgreSQL | Working (sqlx, PG17+ assumed) |
| Supabase | Planned |
| SQLite | Planned |
| Convex | Planned |

---

## P1 — Next up

### `dbd init` — DONE
- `dbd init [--name project] [--target postgres|supabase]`
- Generates design.yaml, ddl/ directory tree, sample table DDL
- Supabase variant with ignore patterns for managed schemas
- Sample DDL follows project conventions (lowercase, leading commas, aligned types)

### `dbd deploy` — DONE
- `dbd deploy --source ./local/path` or `--source owner/repo/path`
- GitHub download via reqwest + flate2 + tar (cached in ~/.cache/dbd/)
- Resolves source → loads design → apply + import in one step
- `--dry-run` previews without executing
- Tarball extraction with path traversal protection

### Export command
- COPY TO STDOUT streaming via sqlx
- Write to `export/<schema>/<name>.<format>`
- Format support: csv, tsv, json, jsonl

### Import enhancements (deferred)
- Truncate staging tables before COPY (`truncate: true/false`)
- Fallback to DELETE FROM on FK constraint failure
- Environment filtering (`import/dev/`, `import/prod/`)

---

## P2 — Adapter expansion

### Supabase adapter
- Extends PostgresAdapter
- Filters 9 managed schemas (auth, storage, realtime, etc.)
- Filters 10 pre-installed extensions
- Grants script with PostgREST notification

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
- Multi-document support (project.dbml config with include/exclude)
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

**319 tests** (281 unit + 38 integration) covering:
- Schema diff engine (45 tests): D1-D21, S1-S14, warnings, edge cases
- Snapshot create (17 tests): SC1-SC10, entity conversion, backward compat
- Multi-snapshot (7 tests): B1-B3, S1-S2, baseline, no-changes
- Change classification (8 tests): C1-C7, enum rename
- Castability (9 tests): CA1-CA5, type categories
- data.sql generation (5 tests): D1-D5
- Execution plan (12 tests): A1-A6, edge cases
- Config/entity/parser/scanner/dependency/references (221 tests)

### Integration test infrastructure (requires PostgreSQL)

Run with `cargo test --features test-db` or in CI with a Postgres container.

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
