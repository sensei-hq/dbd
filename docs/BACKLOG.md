# Backlog

## Status (2026-04-30)

**25 commits, 7,325 LOC, 195 tests, verified on sensei/daemon/database (116 entities)**

### Working

| Command | Offline | With DB | Notes |
|---------|---------|---------|-------|
| `inspect` | yes | — | 116 entities, 0 issues |
| `apply` | dry-run | yes | Full entity apply with enum idempotency |
| `import` | dry-run | yes | CSV/TSV/JSONL via COPY + temp table |
| `reset` | dry-run | yes | Safety guards (prod/version) |
| `combine` | yes | — | |
| `graph` | yes | — | JSON output |
| `dbml` | yes | — | Verified with dbdocs build |
| `doctor` | yes | — | Config migration from Node.js format |
| `snapshot --list` | yes | — | |

---

## P0 — Next up

### `dbd snapshot` (create)
- Parse current table entities into TableSnapshot
- Diff against previous snapshot
- Generate per-table ALTER SQL in migrations/ folder
- Write graph.json with altered/dropped tables
- **Blocked by:** needs schema diff implementation (`migration.rs`)

### `dbd migrate --apply`
- Read pending migrations from migrations/ folder
- Execute ALTER SQL in version order
- Record versions in `_dbd_migrations`
- `--status` shows local vs DB version
- `--dry-run` prints SQL without executing
- `--to N` limits to a specific version

### `dbd deploy --source`
- Download GitHub source (reqwest + flate2 + tar)
- Apply + import in one step
- Cleanup temp directory
- Uses `github.rs` (parsing done, download not yet implemented)

---

## P1 — Important

### Schema diff (`migration.rs`)
- Compare two TableSnapshots
- Detect added/dropped/altered columns, indexes, FKs
- Generate ALTER TABLE SQL
- Handle column renames (heuristic)
- Required for `dbd snapshot` creation

### `_dbd_meta` integration
- Record env and version on `dbd apply`
- Check on `dbd reset` (already implemented in mock)
- Wire into real postgres adapter

### Import truncate
- Truncate staging tables before COPY (respects `truncate: true/false`)
- Fallback to DELETE FROM on FK constraint failure

### Import environment filtering
- `import/dev/staging/` files only loaded with `-e dev`
- `import/prod/staging/` files only loaded with `-e prod`
- Shared files always loaded

### Export command
- COPY TO STDOUT streaming
- Write to `export/<schema>/<name>.<format>`

---

## P2 — Future

### Supabase adapter
- Extends Postgres adapter
- Filters managed schemas/extensions
- Grants script generation

### SQLite adapter
- `rusqlite` integration
- Subset features (no schemas, extensions, roles, enums)

### Convex adapter
- TypeScript schema generation from TableDef
- No SQL execution

### GitHub source download
- `reqwest` + `flate2` + `tar` (pure Rust, no curl/tar)
- Caching in `~/.cache/dbd/`

### Adapter catalog queries
- Load pg_proc, pg_type for reference classification
- Replace static pattern matching with catalog lookup
- Cache per connection URL

### Parallel file parsing
- `rayon::par_iter` for DDL file parsing
- Already in dependencies, not yet wired

### `dbd init`
- Scaffold project from template

### Policy application
- `dbd policies` command
- Scan policies/ folder

---

## Known limitations

### sqlparser workarounds
- `COMMENT ON VIEW/FUNCTION/PROCEDURE` — stripped before parsing
- `CREATE [OR REPLACE] PROCEDURE` — rewritten to FUNCTION
- PL/pgSQL body — opaque string, reads/writes via regex
- See `parser/mod.rs` WORKAROUND_REGISTRY for upgrade instructions

### search_path injection
- `public` appended to SET search_path in DDL files
- Needed when extensions are installed in public schema
- May cause issues if DDL intentionally excludes public
