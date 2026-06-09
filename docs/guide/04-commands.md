# Commands reference

## Global options

All commands accept these options:

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--config` | `-c` | `design.yaml` | Config file name (inside project dir) |
| `--database` | `-d` | `$DATABASE_URL` | Database connection URL |
| `--environment` | `-e` | `prod` | Environment (dev or prod) |
| `--source` | `-s` | `.` | Project directory or GitHub repo |
| `--target` | `-t` | first in config | Target name from design.yaml |
| `--scope` | | (all) | Named scope from `scopes:` in design.yaml |
| `--deps` | | (scope default) | Override scope's `deps` setting: `report` or `include` |
| `--verbose` | `-v` | | Show all details (entity list, full JSON) |
| `--help` | `-h` | | Print help |
| `--version` | `-V` | | Print version |

### Output modes

| Mode | Flag | What shows |
|------|------|-----------|
| Normal | (default) | Errors, warnings, progress, summary |
| Verbose | `-v` | Everything: entity details, apply order, full JSON |

### `--source`

Defines the project root. Config file, DDL folder, import folder, snapshots — all relative to this.

```sh
dbd inspect                               # Current directory
dbd inspect -s ~/projects/mydb            # Explicit local path
dbd inspect -s sensei-hq/daemon/database  # GitHub repo
```

---

## `dbd inspect`

Validate project configuration and report errors/warnings.

```sh
dbd inspect                       # Validate all entities
dbd inspect -n config.lookups     # Inspect one entity
dbd inspect -v                    # Verbose: show entity JSON
dbd inspect --silent              # Just the count
dbd inspect --scope hub           # Validate scope + report dependency gaps
```

---

## `dbd apply`

Apply DDL scripts to the database in dependency order.

```sh
dbd apply                          # Apply all entities
dbd apply -n config.lookups        # Apply one entity
dbd apply --dry-run                # Print apply order (no DB needed)
dbd apply -v                       # Show entity list then execute
dbd apply -d postgres://...        # Explicit database URL
```

**Apply order:** schemas → extensions → roles → enums → tables → views → functions/procedures.

---

## `dbd import`

Load staging data from CSV/TSV/JSONL files.

```sh
dbd import                         # Import all tables + call procedures
dbd import -n staging.lookups      # Import one table
dbd import --dry-run               # Show import plan (no DB needed)
dbd import -e dev                  # Load dev-only data
```

**Dry-run output shows the full plan:**
```
  import staging.models (jsonl) ← import/staging/models.jsonl
  import staging.routers (jsonl) ← import/staging/routers.jsonl

  call staging.import_models()
  call staging.import_routers()
```

Procedures are matched by reads/writes analysis (which procedure reads from which staging table), not by naming convention.

---

## `dbd combine`

Combine all DDL into a single SQL file.

```sh
dbd combine                        # Writes init.sql
dbd combine -f bootstrap.sql       # Custom filename
```

---

## `dbd dbml`

Generate DBML documentation for dbdocs.io / dbdiagram.io.

```sh
dbd dbml                           # Writes design.dbml (when no dbml keys configured)
dbd dbml -f schema.dbml            # Custom filename
dbd dbml --scope hub -f hub.dbml   # Document only the entities in the 'hub' scope
dbd dbml --scope hub --deps include  # Expand to the dependency closure first
```

Generated natively — no `@dbml/core` dependency. Includes Project block, enums, tables with columns/indexes/comments, standalone Ref blocks with FK actions (including composite refs as `t.(c1, c2) > o.(c1, c2)`), `TableGroup` blocks, and stub tables for external FK targets.

**Scope-aware.** `--scope` documents only the entities that deploy under that scope (its working set). `--deps include` (or a scope whose `deps: include`) first expands the selection to its dependency closure, so the diagram is self-contained. Unlike `apply`/`deploy`, `dbml` does not gate on dependency gaps — it is documentation, so it simply emits the filtered set (out-of-scope FK targets still appear as external stub tables).

**Multi-document.** If `design.yaml` declares multiple `dbml.<key>` entries (each with its own `include`/`exclude`, `output`, `auto_group_by_schema`, `groups`), `dbd dbml` writes one file per key into the parent directory of `-f`, using each key's `output` (default `<key>.dbml`). See [design.yaml reference](03-design-yaml.md#dbml) for the full schema.

---

## `dbd graph`

Output the dependency graph as JSON.

```sh
dbd graph                          # Full graph
dbd graph -n config.lookups        # Scoped to one entity's subgraph
```

Output: `{ "nodes": [...], "edges": [...], "layers": [...] }`

---

## `dbd reset`

Drop all project schemas. Guarded by `_dbd_meta` environment check.

```sh
dbd reset                          # Blocked if prod or version >= 1
dbd reset --force                  # Override safety guard
dbd reset --dry-run                # Show what would be dropped
```

---

## `dbd snapshot`

Create versioned schema snapshots and generate migration scripts.

```sh
dbd snapshot --name "add notes column"    # Create a snapshot
dbd snapshot --list                        # List existing snapshots
```

Smart multi-snapshot: When complex changes are detected (column rename, type change, enum value removal) dbd automatically generates multiple snapshots with correct intermediate states.

- Column rename: 2 snapshots (add new column + data copy, drop old column)
- Column type change: 2 snapshots (add new column + CAST, drop old column)
- Enum value removal: 3 snapshots (data correction, TEXT intermediary, enum recreation)

Data corrections (*.data.sql) are generated automatically where possible. Non-derivable corrections (enum value mapping) generate TODO comments.

---

## `dbd migrate`

Show migration status (read-only).

```sh
dbd migrate --status     # Show DB version vs latest, list pending
```

Use `dbd apply` to execute pending migrations. There is no separate migrate --apply.

---

## `dbd deploy`

Deploy from a local path or GitHub source. Runs apply + import in one step.

```sh
dbd deploy --source owner/repo/path -d $DATABASE_URL    # From GitHub
dbd deploy --source ./local/project                       # From local path
dbd deploy --dry-run --source owner/repo/path             # Preview
dbd deploy --scope hub --database $HUB_URL               # Deploy a named scope
dbd deploy --scope hub --deps include -d $HUB_URL        # Auto-expand dependencies
```

GitHub sources are cached in ~/.cache/dbd/.

---

## `dbd export`

Export table data to files.

```sh
dbd export                              # Export all tables as CSV
dbd export --name config.lookups        # Export one table
dbd export --format tsv                 # TSV format
dbd export --format jsonl               # JSONL format
```

Writes to `export/<schema>/<name>.<format>`.
If export entries are configured in design.yaml, only those tables are exported.

---

## `dbd init`

Scaffold a new dbd project.

```sh
dbd init                                # Postgres target, name from directory
dbd init --name myproject               # Custom project name
dbd init --target supabase              # Supabase target with grants + externals
```

Creates design.yaml, ddl/ directory structure, and a sample table DDL.
Supabase target includes default external entities (auth.users, storage.objects)
and ignore patterns for managed schemas.

---

## `dbd format`

Format DDL files to project conventions.

```sh
dbd format                         # Format all DDL files in-place
dbd format --check                 # Check formatting (exit 1 if any file would change)
```

Configurable via `format:` section in design.yaml:

```yaml
format:
  keyword_case: lower        # lower | upper | preserve
  comma_style: leading       # leading | trailing
  type_alignment: 27         # column for type start, 0 = off
  indent: 2                  # spaces per level
```

Handles CREATE TABLE (full formatting), CREATE INDEX, SET, COMMENT ON. Function/procedure `$$` bodies are preserved verbatim. SQLite `CREATE TRIGGER … BEGIN … END;` blocks are kept atomic (the inner statements aren't split or reformatted).

### Pre-commit

`dbd format --check` exits 1 when any file would change, so it drops into [pre-commit](https://pre-commit.com) directly. Repo ships `.pre-commit-hooks.yaml` with two hooks:

| Hook id | Language | Use when |
|---------|----------|----------|
| `dbd-format` | `rust` (cargo install on first run, cached after) | Contributors don't have `dbd` installed |
| `dbd-format-system` | `system` (expects `dbd` on PATH) | Contributors already installed dbd locally |

User's `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/sensei-hq/dbd
  rev: v0.4.1
  hooks:
    - id: dbd-format
```

Both set `pass_filenames: false` — the hook scans the project itself, so pre-commit invokes it with no positional args.

---

## `dbd policies`

Apply RLS (Row-Level Security) policies from the `policies/` directory.

```sh
dbd policies                       # Apply all policy files
dbd policies --dry-run             # Show what would be applied
dbd apply --with-policies          # Apply entities + policies in one step
```

Policy files in `policies/<schema>/<table>.sql` contain idempotent SQL:

```sql
alter table config.users enable row level security;
drop policy if exists "users_select_own" on config.users;
create policy "users_select_own" on config.users for select using (auth.uid() = id);
```

Failed policies are logged and skipped (fail-forward). Exit code 1 if any failures.

---

## `dbd doctor`

Audit and migrate design.yaml configuration.

```sh
dbd doctor                         # Show issues
dbd doctor --fix                   # Migrate to new format (creates .yaml.bak)
```

Detects and migrates old Node.js config format:
- `project.database` → `source.dialect` + `target`
- Top-level `extensions`/`roles` → under `target`
- `nullValue` → `null_value`
- `project.staging` → `import.staging`
- `project.dbdocs` → top-level `dbml`

---

## Environment variables

| Variable | Used by |
|----------|---------|
| `DATABASE_URL` | Default database connection |
| `GITHUB_TOKEN` | Private GitHub repository access |
| `DBD_CATALOG_TTL` | Catalog cache TTL in hours (default: 24) |
