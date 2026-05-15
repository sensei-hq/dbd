# Backlog

## Current status (2026-05-08)

**v0.4.0, 423 tests (384 unit + 38 integration + 1 doc)**

### Working commands

| Command | Offline | With DB | Notes |
|---------|---------|---------|-------|
| `inspect` | yes | — | Validation pipeline, `--fix` auto-formats DDL |
| `apply` | `--dry-run` | yes | Version-aware migrations, `--with-policies` |
| `import` | `--dry-run` | yes | CSV/TSV/JSONL, truncate, procedure matching, env filtering |
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

- **Per-table `export.format`** — override `--format` per table via `ExportEntry::WithOptions`, precedence: config → CLI flag → csv
- **`inspect --database`** — resolves "Unresolved reference" warnings against live DB catalog (tables/views/enums); persists snapshot to `<project>/.dbd/refcache.json` and subsequent offline `inspect` runs consult it
- **Schema diff engine** — columns, constraints, indexes, enum values with SQL generation
- **Smart multi-snapshot** — auto-splits renames/type changes/enum removal into safe migration stages
- **`_dbd_meta`** — authoritative version source, env/version/applied_at
- **Migration data corrections** — `*.data.sql` after ALTERs, CAST heuristics, TODO for business logic
- **Execution plan** — pure function builds plan, thin I/O wrapper executes
- **Change classification** — `is_castable()`, `classify_changes()`, `generate_data_sql()`
- **DDL formatter v2** — river-style query formatting (see below), keyword case, comma style, type alignment
- **RLS policies** — scan policies/ folder, apply with fail-forward, `--with-policies` on apply
- **Adapter catalog** — namespace-aware pg_proc/pg_type/pg_extension queries with file cache
- **Supabase support** — grants after apply, protected reset, default externals, DBML stubs
- **DBML filters** — include/exclude by schema or table name
- **Deploy** — local path or GitHub source (reqwest + tar, cached)
- **Embedded PostgreSQL tests** — full-cycle integration tests via `postgresql_embedded`
- **`connect(url, project)`** — public factory for external callers; adapter selection is internal
- **On-complete callbacks** — `apply`, `import`, `deploy` all call `on_complete(Summary)` with version info and counts
- **data.sql TODO validation** — `inspect` surfaces unresolved `-- TODO:` in migration files; `apply` blocks on pending TODOs
- **Import environment filtering** — `import/{env}/{schema}/file` convention; env-specific files only loaded for matching env

### DDL formatter v2 — river style (✓ done)

```sql
    select lv.id
         , lv.value     as display_value
         , lv.is_active as active
      from lookups       lkp
inner join lookup_values lv
        on lv.lookup_id = lkp.id
     where lkp.name     = 'Gender'
       and lv.is_active = true;
```

- Right-aligned keywords at configurable gutter (default 10, fits `inner join`)
- Leading-comma SELECT lists
- Column alias alignment (`as` column aligned across all SELECT items)
- Table alias alignment (table names padded so aliases line up across FROM + JOINs)
- Operator alignment (`=`, `!=`, `>=`, `<=`, `~`, `~*` etc. aligned in WHERE/HAVING/ON)
- AND-split and OR-split WHERE / HAVING / ON — each condition on its own line
- OR-within-AND expansion — `(a OR b)` inside AND chains rendered as parenthesized OR group
- Subquery indentation — derived tables in FROM rendered with nested river SELECT
- GROUP BY, ORDER BY, LIMIT, OFFSET river-aligned
- CREATE VIEW body formatted with river SELECT
- CREATE TYPE AS ENUM — multi-line with leading commas
- Config: `query_style: river`, `gutter: 10` in design.yaml

### Adapter support

| Target | Status |
|--------|--------|
| PostgreSQL | Working (sqlx, PG17+, namespace-aware catalog) |
| Supabase | Working (grants, protected reset, externals) |
| SQLite | Working (sqlx-sqlite, bare-name catalog, CSV/TSV/JSONL import) |
| Convex | Working (codegen `convex/schema.ts`, sidecar `.dbd_state.json`) |

---

## Next up

_(empty)_

---

## Future

### River formatter enhancements
- Pre-commit hook integration (`dbd format --check` already works, hook wiring is separate)
- Multi-document DBML output

### DBML enhancements
- Multi-document support (multiple dbml output files)
- Table group generation
- Composite FK ref support

### SQLite enhancements
- Static-pattern reference classification for richer offline analysis
- Multi-statement DDL splitter aware of SQLite trigger bodies
- Concurrent batch INSERT for very large CSV imports (currently a single tx)

### Convex enhancements
- Optional `npx convex deploy` shell-out after codegen
- Enum entities → `v.union(v.literal(...))` codegen (currently errors as unsupported)
- Foreign-key columns → typed `v.id("table")` references
- Round-trip `import_data` / `export_data` via the Convex CLI

---

## Test coverage

**423 tests** (384 unit + 38 integration + 1 doc) covering:
- Schema diff engine (45): D1-D21, S1-S14, warnings, edge cases
- Snapshot create (17): SC1-SC10, entity conversion, backward compat
- Multi-snapshot (7): B1-B3, S1-S2, baseline, no-changes
- Change classification (8): C1-C7, enum rename
- Castability (9): CA1-CA5, type categories
- data.sql generation (5): D1-D5
- data.sql TODO scan (8): DS1-DS6, apply-blocking (2)
- Execution plan (12): A1-A6, edge cases
- DDL formatter (25): F1-F12, R1-R15 (river style incl. OR conditions, OR-within-AND, subquery FROM)
- RLS policies (6): P1-P8
- Adapter catalog (4): C1, C4, C10, C11
- RefCache (7): roundtrip, missing, parent-dir, empty, write-via-adapter, resolve-via-cache (hit + noop)
- SQLite adapter (10): S1–S10 — apply, list, resolve, schema-noop, unsupported types, migrations, meta, classify, CSV import, bare-name
- Convex adapter (10): CV1–CV10 — name flattening, type mapping, schema.ts emit, indexes, internal-column skip, batch apply, unsupported types, sidecar state, URL parsing, import/export errors
- DBML filters (4): include/exclude schema/table
- Init (7): postgres/supabase targets
- Deploy (5): local resolve, not-found, subpath, cache hit
- Embedded integration (5): fresh deploy, idempotent redeploy, data acceptance, dry-run, migration cycle
- Scanner (7): DDL, import (no-env, env-match, env-exclude), policies
- Config/entity/parser/scanner/dependency/references (225+)

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
