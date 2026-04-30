# Backlog

## Current status (2026-04-30)

**26 commits, 7,325 LOC, 195 tests, verified on sensei/daemon/database (116 entities)**

### Working commands

| Command | Offline | With DB | Notes |
|---------|---------|---------|-------|
| `inspect` | yes | — | Full validation pipeline |
| `apply` | `--dry-run` | yes | Entity apply with enum idempotency |
| `import` | `--dry-run` | yes | CSV/TSV/JSONL, reads-based procedure matching |
| `reset` | `--dry-run` | yes | Safety guards (_dbd_meta) |
| `combine` | yes | — | |
| `graph` | yes | — | JSON with nodes/edges/layers |
| `dbml` | yes | — | Verified with dbdocs build |
| `doctor` | yes | — | Config migration from Node.js format |
| `snapshot --list` | yes | — | |

### Adapter support

| Target | Status |
|--------|--------|
| PostgreSQL | Working (sqlx) |
| Supabase | Planned |
| SQLite | Planned |
| Convex | Planned |

---

## P0 — Next session

### `dbd snapshot` (create)
- Parse current table entities into TableSnapshot via adapter
- Diff against previous snapshot using schema-diff
- Generate per-table ALTER SQL files in `migrations/NNN/`
- Write `graph.json` with altered/dropped tables
- Requires: schema diff implementation

### `dbd migrate --apply`
- Read pending migrations from `migrations/` folder
- Execute ALTER SQL in version order
- Record versions in `_dbd_migrations`
- `--status` shows local vs DB version
- `--dry-run` prints SQL without executing
- `--to N` limits to a specific version

### Schema diff (`migration.rs`)
- Compare two `TableSnapshot` values
- Detect: added/dropped/altered columns, indexes, FKs
- Generate ALTER TABLE SQL from diff
- Required by snapshot creation

---

## P1 — Important features

### `dbd deploy --source`
- Download GitHub repo via reqwest + flate2 + tar
- Cache in `~/.cache/dbd/` with TTL
- Apply + import in one step, cleanup temp
- GitHub source parsing already implemented

### `_dbd_meta` integration
- Record env + version on `dbd apply`
- Wire into postgres adapter (schema exists, methods implemented)
- Currently only checked by reset, not written by apply

### Import enhancements
- Truncate staging tables before COPY (`truncate: true/false`)
- Fallback to DELETE FROM on FK constraint failure
- Environment filtering (`import/dev/`, `import/prod/`)
- `import.after` script execution (wired in library, needs DB path resolution)

### Export command
- COPY TO STDOUT streaming via sqlx
- Write to `export/<schema>/<name>.<format>`
- Format support: csv, tsv, json, jsonl

### `dbd init`
- Scaffold project from bundled template
- `--target supabase` variant with grants config

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

---

## Test coverage plan

### Current gaps

The 195 tests cover unit logic and integration with fixtures, but do NOT test:
- Real database operations (apply, import, reset against PostgreSQL)
- Snapshot creation and migration diffing
- Multi-version migration catch-up
- Data integrity after import (procedures actually transform data)
- Error recovery (partial apply, failed migrations)

### Test infrastructure needed

```
tests/
  fixtures/
    projects/                    # Complete test projects (not just DDL snippets)
      basic/                     # Simple project: 2 schemas, 3 tables, 1 view
        design.yaml
        ddl/
          table/config/lookups.ddl
          table/config/lookup_values.ddl
          view/config/genders.ddl
          procedure/staging/import_lookups.ddl
        import/
          staging/lookups.csv
          staging/lookup_values.csv
      with-migrations/           # Project with snapshot history
        design.yaml
        ddl/...
        snapshots/
          001.json
          002.json
        migrations/
          002/
            graph.json
            config/lookup_values.sql
      self-refs/                 # Tables with self-referencing FKs
        design.yaml
        ddl/table/org/categories.ddl   # parent_id → categories.id
      multi-schema/              # Cross-schema FK references
        design.yaml
        ddl/
          table/config/lookups.ddl
          table/staging/lookups.ddl     # FK to config.lookups
          table/activity/events.ddl     # FK to config.users
```

### Scenario tests (require PostgreSQL)

Run with `cargo test --features test-db` or in CI with a Postgres container.
Each test creates a fresh database and drops it after.

#### Scenario 1: Fresh apply
```
Given: empty database, basic project fixtures
When:  Design::apply()
Then:  all entities exist in DB, correct types, correct order
```

#### Scenario 2: Idempotent re-apply
```
Given: database already has all entities from Scenario 1
When:  Design::apply() again
Then:  no errors, all entities still correct (CREATE IF NOT EXISTS)
```

#### Scenario 3: Apply + import + verify data
```
Given: empty database, basic project with CSV import files
When:  Design::apply() then Design::import_data()
Then:  staging tables have CSV data, procedures ran, config tables populated
```

#### Scenario 4: Snapshot creation
```
Given: basic project at v0 (no snapshots), database applied
When:  snapshot::create_snapshot()
Then:  snapshots/001.json exists with correct table structure
```

#### Scenario 5: Schema change + migration
```
Given: project at v1, add a column to DDL file
When:  snapshot::create_snapshot() (creates v2)
Then:  migrations/002/graph.json has altered table
       migrations/002/<schema>/<table>.sql has ALTER TABLE ADD COLUMN
```

#### Scenario 6: Apply with pending migration
```
Given: database at v1, project has snapshot v2 with migration
When:  Design::apply()
Then:  ALTER runs before CREATE OR REPLACE, column exists, v2 recorded
```

#### Scenario 7: Multi-version catch-up
```
Given: database at v1, snapshots v2 + v3 both exist
When:  Design::apply()
Then:  migrations v2 and v3 both applied in order, both recorded
```

#### Scenario 8: Reset + rebuild
```
Given: database at v3 with data
When:  Design::reset() then Design::apply()
Then:  all schemas dropped, rebuilt from DDL, single _dbd_migrations row
```

#### Scenario 9: Reset blocked in prod
```
Given: _dbd_meta has env=prod
When:  Design::reset(force=false)
Then:  Err(SafetyGuard), database unchanged
```

#### Scenario 10: Import with truncate
```
Given: staging table has old data, truncate=true
When:  import_data()
Then:  old data gone, new CSV data loaded
```

#### Scenario 11: Import without truncate (append)
```
Given: staging table has existing data, truncate=false
When:  import_data()
Then:  old data preserved, new data appended
```

#### Scenario 12: Import dependency ordering
```
Given: import_lookups writes config.lookups
       import_lookup_values writes config.lookup_values (FK to config.lookups)
When:  import_data()
Then:  import_lookups runs first, then import_lookup_values
```

#### Scenario 13: JSONL import via temp table
```
Given: staging table, import file is .jsonl
When:  import_data()
Then:  _temp table created, data loaded, import_jsonb_to_table called, _temp dropped
```

#### Scenario 14: Enum idempotency
```
Given: enum type already exists in DB
When:  Design::apply()
Then:  no error (skipped via pg_type check)
```

#### Scenario 15: Self-referencing FK
```
Given: table with parent_id → self.id
When:  dependency sort + apply
Then:  not marked cyclic, applied successfully
```

#### Scenario 16: DBML round-trip verification
```
Given: project with tables, FKs, indexes, comments
When:  generate_dbml()
Then:  output parses successfully with dbml-core (or verified syntax)
       contains: Project block, Table blocks, Ref blocks, Note blocks
```

#### Scenario 17: Deploy from local source
```
Given: project in a different directory
When:  deploy(source="./path/to/project", database_url=...)
Then:  apply + import succeed, equivalent to cd + apply + import
```

#### Scenario 18: Doctor migration round-trip
```
Given: old-format design.yaml
When:  doctor::migrate_config()
Then:  output parses as DesignConfig
       dbd inspect on migrated config produces same entity count
```

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
