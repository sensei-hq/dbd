# Backlog

## Current status (2026-06-05)

**v0.4.1, 486 tests (442 unit + 43 integration + 1 doc)**

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
- **Convex enum + FK codegen** — `Entity::Enum` emits `export const X = v.union(v.literal(...))` above `defineSchema`; columns whose type matches an enum reference the const; inline FKs and single-column table-level FKs emit `v.id("table")`
- **Convex auto-deploy + import** — `convex://./out?deploy=true` (or `ConvexAdapter::with_auto_deploy(true)`) runs `npx convex deploy` from the parent of the schema dir after `apply_entities`; `import_data` shells out to `npx convex import --table <flat_name> --replace -y <file>` (honors `--dry-run`); `with_cli_dry_run(true)` logs commands instead of spawning (for tests and `dbd apply --dry-run`)
- **SQLite offline ref classification** — `classify_reference` now matches a 150+ entry alphabetical builtin table (aggregates, window/math/date/JSON1/scalar fns) plus a `sqlite_*` prefix rule for system tables; no DB call required for richer `inspect` analysis
- **SQLite trigger-aware splitter** — `format`'s statement splitter keeps `CREATE TRIGGER … BEGIN <stmts;> END;` as a single block (CASE…END inside the body and bare `BEGIN; … COMMIT;` transactions both behave correctly)
- **SQLite batched imports** — `import_delimited` + `import_jsonl` now flush rows in `(?,?), (?,?), …` multi-row VALUES batches (≤500 rows or 32k binds per batch, whichever is tighter), still inside a single transaction; JSONL detects column-set changes and flushes between groups
- **DBML multi-document + groups** — `design.yaml` `dbml.<key>` entries can each declare their own `include`/`exclude`, `output: filename.dbml`, `auto_group_by_schema`, and explicit `groups: [{name, tables}]`; `dbd_core::dbml::generate_all` returns one `DbmlDocument` per key; CLI writes each into the parent directory of the user-supplied output path. Composite FKs (single-column and multi-column constraints) render as `Ref: t.(c1, c2) > o.(c1, c2)`
- **Scopes — multi-target subset deploy** — `design.yaml` `scopes:` declare named entity selections (`includes`/`excludes` of schemas or specific entities) so one design deploys to multiple databases (e.g. full primary DB + smaller embedded-postgres hub). Orthogonal to `target` (DB platform) and connection — paired at run time via global `--scope`/`--deps`. Pure `scope.rs` (`resolve`/`analyze_gaps`/`closure` over the `refers` graph): `deps: report` (default) makes a dependency gap a hard error (`inspect --scope` lists gaps with their chain and exits non-zero; `apply`/`import`/`deploy` refuse, incl. `--dry-run`); `deps: include` auto-expands to the transitive closure; `external:` is the only "satisfied elsewhere" door. Per-scope migrations fall out via `build_execution_plan` intersection (version meta is per-database). Phase 1: `inspect`/`apply`/`import`/`deploy` (others full-set — see Next up)

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

### Scopes — Phase 2 (extend scope-awareness to remaining commands)

Phase 1 (shipped) wires scopes into `inspect`/`apply`/`import`/`deploy`. Phase 2
extends the `scope: Option<&ResolvedScope>` filtering to the remaining
entity-selecting commands, which currently always operate on the full set:

- `dbml`, `combine`, `graph`, `export`, `reset` — accept `--scope` and filter to
  the resolved working set (signatures already carry the scope arg where it
  threads through `Design`; this is behavior, not API churn).
- Optional `schema.*` wildcard matching in `includes`/`excludes` to align with
  the `ignore:` list's existing `prefix.*` syntax.

---

## Future

### River formatter enhancements
_(empty — `.pre-commit-hooks.yaml` ships at the repo root with `dbd-format` and `dbd-format-system` hooks; see README)_

### DBML enhancements
_(empty — see Core features for multi-document output, table groups, and composite FKs)_

### SQLite enhancements
_(empty — see Core features for the offline ref classifier, trigger-aware splitter, and batched imports)_

### Convex enhancements
- Per-table `export_data` via the Convex CLI — currently errors with a clear message because `npx convex export` only supports whole-deployment dumps; revisit if the CLI grows a `--table` flag or by extracting from the export zip

---

## Test coverage

**486 tests** (442 unit + 43 integration + 1 doc) covering:
- Scopes (23): `scope.rs` (18) — resolve include/exclude/all, schema-token expansion, deps override, unknown-name/item errors, gap analysis (direct + hierarchical chains, external/self-ref exemption, deterministic ordering), closure expansion + exclude-conflict; design/config integration (5) — `resolve_scope`/`working_set` (report filter + include closure), scope-aware `report` gaps, `build_execution_plan` migration intersection, scope-aware `apply` (gap gate blocks before writes), `import_entry_in_scope`, `check_scope_gaps` policy gating
- Schema diff engine (45): D1-D21, S1-S14, warnings, edge cases
- Snapshot create (17): SC1-SC10, entity conversion, backward compat
- Multi-snapshot (7): B1-B3, S1-S2, baseline, no-changes
- Change classification (8): C1-C7, enum rename
- Castability (9): CA1-CA5, type categories
- data.sql generation (5): D1-D5
- data.sql TODO scan (8): DS1-DS6, apply-blocking (2)
- Execution plan (12): A1-A6, edge cases
- DDL formatter (29): F1-F12, R1-R15 (river style incl. OR conditions, OR-within-AND, subquery FROM), R16-R19 (SQLite trigger-aware splitter: BEGIN…END preserved, plain BEGIN transactions still split, CASE…END handled, `$$` regression)
- RLS policies (6): P1-P8
- Adapter catalog (4): C1, C4, C10, C11
- RefCache (7): roundtrip, missing, parent-dir, empty, write-via-adapter, resolve-via-cache (hit + noop)
- SQLite adapter (16): S1–S10 — apply, list, resolve, schema-noop, unsupported types, migrations, meta, classify, CSV import, bare-name; S11–S12 — builtin list sortedness invariant, sqlite_* + extended-builtin classification; S13–S16 — `batch_row_size` bounds, placeholder shape, 1500-row CSV multi-batch round-trip, JSONL batching with mixed types and column-set changes
- Convex adapter (18): CV1–CV10 — name flattening, type mapping, schema.ts emit, indexes, internal-column skip, batch apply, unsupported types, sidecar state, URL parsing (incl. `?deploy=true`), export helpful-error; CV11–CV14 — enum const emit, enum column refs (qualified + array), inline & table-level FK → `v.id`, enum apply_entity buffering; CV15–CV18 — deploy/import argv shape, import dry-run skips shell-out, auto-deploy fires through `with_cli_dry_run`, auto-deploy no-op on empty input
- DBML filters + groups + multi-doc (12): include/exclude schema/table; composite FK tuple syntax (emit + round-trip); auto-group-by-schema; explicit groups (precedence over auto, drop-filtered, no-op when neither set); `generate_all` empty-config fallback; `generate_all` per-key documents with sorted output
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
