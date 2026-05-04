# Backlog

## Current status (2026-05-04)

**96 commits, 14,842 LOC, 364 tests (326 unit + 38 integration), v0.3.0**

### Working commands

| Command | Offline | With DB | Notes |
|---------|---------|---------|-------|
| `inspect` | yes | — | Validation pipeline, `--fix` auto-formats DDL |
| `apply` | `--dry-run` | yes | Version-aware migrations, `--with-policies` |
| `import` | `--dry-run` | yes | CSV/TSV/JSONL, truncate, procedure matching |
| `export` | — | yes | COPY TO STDOUT → csv/tsv/jsonl |
| `reset` | `--dry-run` | yes | Whitelist-only schema drops, protected schemas |
| `combine` | yes | — | Merge all DDL into single SQL file |
| `graph` | yes | — | JSON with nodes/edges/layers |
| `dbml` | yes | — | Include/exclude filters, external entity stubs |
| `doctor` | yes | — | Config migration from Node.js format |
| `snapshot` | yes | — | Smart multi-snapshot (rename, type change, enum removal) |
| `migrate --status` | — | yes | Read-only version diagnostic |
| `init` | yes | — | Scaffold project (postgres/supabase) |
| `deploy` | `--dry-run` | yes | Local path or GitHub source |
| `format` | yes | — | DDL formatting with `--check` for CI |
| `policies` | `--dry-run` | yes | RLS policy application |

### Core features

- **Schema diff engine** — columns, constraints, indexes, enum values with SQL generation
- **Smart multi-snapshot** — auto-splits renames/type changes/enum removal into safe migration stages
- **`_dbd_meta`** — authoritative version source, env/version/applied_at
- **Migration data corrections** — `*.data.sql` after ALTERs, CAST heuristics, TODO for business logic
- **Execution plan** — pure function builds plan, thin I/O wrapper executes
- **Change classification** — `is_castable()`, `classify_changes()`, `generate_data_sql()`
- **DDL formatter** — keyword case, comma style, type alignment (configurable in design.yaml)
- **RLS policies** — scan policies/ folder, apply with fail-forward, `--with-policies` on apply
- **Adapter catalog** — namespace-aware pg_proc/pg_type/pg_extension queries with file cache
- **Supabase support** — grants after apply, protected reset, default externals, DBML stubs
- **DBML filters** — include/exclude by schema or table name
- **Deploy** — local path or GitHub source (reqwest + tar, cached)

### Adapter support

| Target | Status |
|--------|--------|
| PostgreSQL | Working (sqlx, PG17+, namespace-aware catalog) |
| Supabase | Working (grants, protected reset, externals) |
| SQLite | Planned |
| Convex | Planned |

---

## Next up

### DDL formatter v2 — river formatting
- River-style SQL formatting where keywords, commas, and operators form a vertical channel
- **Right-aligned keywords:** select, from, where, and, on, inner join, left join, order by, group by, having
- **Leading comma alignment** in SELECT lists, INSERT column lists, UPDATE SET clauses
- **Alias alignment:** column aliases (AS) and table aliases aligned to a consistent column
  ```sql
  select lv.id
       , lv.value          as display_value
       , lv.is_active      as active
    from lookups            lkp
   inner join lookup_values lv
      on lv.lookup_id       = lkp.id
   where lkp.name           = 'Gender'
     and lv.is_active       = true
  ```
- **Right-aligned operators:** `=`, `!=`, `>=`, `<=`, `~*`, `like`, `in` aligned to form the river
- **Parenthesized conditions:** `or`/`and` inside parens indent to the opening paren, outer `and`/`or` stays at clause level:
  ```sql
   where (    lkp.status    = 'active'
           or lkp.status    = 'pending')
     and lkp.is_visible     = true
  ```
- **Subquery indentation** with consistent nesting
- VIEW body formatting (currently keyword-case only)
- Enum CREATE TYPE multi-line value formatting with leading commas
- Pre-commit hook integration

### Config gaps
- `target.schema_prefix` — multi-tenant schema prefix
- Per-table `export.format` — override per table (currently CLI `--format` only)

### Database inspection
- `dbd inspect --database` resolves warnings against live DB catalog
- DB reference cache for offline use

### data.sql validation
- `dbd inspect` verifies all TODO comments in data.sql have been resolved
- Block apply if unresolved TODOs exist

---

## Future

### Import environment filtering
- Only load import files matching `--environment` (dev/prod)
- Path convention: `import/dev/staging/test_data.csv`, `import/prod/staging/seed.csv`

### Parallel file parsing
- `rayon::par_iter` for DDL file read + parse
- Benchmark: measure parsing time on large projects (100+ DDL files)

### DBML enhancements
- Multi-document support (multiple dbml output files)
- Table group generation
- Composite FK ref support

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

## Test coverage

**364 tests** (326 unit + 38 integration) covering:
- Schema diff engine (45): D1-D21, S1-S14, warnings, edge cases
- Snapshot create (17): SC1-SC10, entity conversion, backward compat
- Multi-snapshot (7): B1-B3, S1-S2, baseline, no-changes
- Change classification (8): C1-C7, enum rename
- Castability (9): CA1-CA5, type categories
- data.sql generation (5): D1-D5
- Execution plan (12): A1-A6, edge cases
- DDL formatter (10): F1-F12
- RLS policies (6): P1-P8
- Adapter catalog (4): C1, C4, C10, C11
- DBML filters (4): include/exclude schema/table
- Init (7): postgres/supabase targets
- Deploy (5): local resolve, not-found, subpath, cache hit
- Config/entity/parser/scanner/dependency/references (225)

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
