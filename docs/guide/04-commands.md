# Commands reference

## Global options

All commands accept these options:

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--config` | `-c` | `design.yaml` | Config file name (inside project dir) |
| `--database` | `-d` | `$DATABASE_URL` | Database connection URL |
| `--environment` | `-e` | `prod` | Environment (dev or prod) |
| `--source` | `-s` | `.` | Project directory or GitHub repo |
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
dbd inspect --fix                 # Auto-fix DDL formatting (runs the formatter in place)
dbd inspect --scope hub           # Validate scope + report dependency gaps
dbd inspect --from-db -d $DATABASE_URL    # Resolve references against the live catalog
```

**`--from-db`** resolves "Unresolved reference" warnings against the live database catalog (tables, views, enums), using the `-d`/`--database` connection — useful when DDL references objects created outside the project. The resolved catalog is cached to `<project>/.dbd/refcache.json`, so subsequent **offline** `inspect` runs consult the cache and stay quiet without a connection.

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

**Environment-specific data.** Files placed under `import/<env>/<schema>/<file>` are loaded only when `-e <env>` matches; files directly under `import/<schema>/` load in every environment. So `import/dev/staging/seed.csv` loads on `dbd import -e dev` but not under `prod`, letting you ship dev seed data without it reaching production.

---

## `dbd combine`

Combine all DDL into a single SQL file.

```sh
dbd combine                        # Writes init.sql
dbd combine -f bootstrap.sql       # Custom filename
dbd combine --scope hub -f hub.sql # Only the 'hub' scope's working set
dbd combine --scope hub --deps include  # Expand to the dependency closure first
```

**Scope-aware.** `--scope` filters the combined SQL to that scope's working set; `--deps include` (or a scope with `deps: include`) first expands to the dependency closure so the script is self-contained. Filter-only — `combine` does not gate on dependency gaps (use `inspect --scope` to surface them). Roles are kept in every scope; extensions are too, unless the scope sets an `extensions` allowlist (then only the listed extensions — `[]` for none).

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
dbd graph -n config.lookups        # Subgraph reachable from one entity
dbd graph --scope hub              # Only the 'hub' scope's working set
```

Output: `{ "nodes": [...], "edges": [...], "layers": [...] }`

`--scope`/`--deps` filter the graph to the scope's working set (closure under `include`). `-n` (entity subgraph) and `--scope` compose.

---

## `dbd diagram`

Open the schema in the **hosted interactive viewer** — sidebar schema→table navigation, a pannable/zoomable ER diagram, and a per-table detail panel. The model is gzip-compressed into the URL fragment (client-side only, never sent to a server), so the link is private and self-contained.

```sh
dbd diagram                        # build the model and open it in your browser
dbd diagram --print-url            # print the viewer URL instead of opening a browser
dbd diagram --site http://localhost:5173   # point at a different site (or set $DBD_DIAGRAM_URL)
dbd diagram --json -f schema.json  # write the raw SchemaModel JSON (upload it at <site>/diagram)
dbd diagram --scope hub            # scope-aware (only the scope's tables/refs)
```

`dbd diagram` prints the URL and opens your default browser; on a headless machine use `--print-url`. The `--json` output is the dbd-native schema model (schemas, tables, columns, FK refs); upload it on the site's `/diagram` page, or feed it to other tooling. The model is JSON (not DBML), so it extends to views/functions/procedures later.

---

## `dbd reset`

Drop project schemas. Guarded by `_dbd_meta` environment check.

```sh
dbd reset                          # Blocked if prod or version >= 1
dbd reset --force                  # Override safety guard
dbd reset --dry-run                # Show what would be dropped
dbd reset --scope hub --dry-run    # Only the schemas the 'hub' scope occupies
```

**Scope-aware.** `--scope` restricts the drop to the schemas the scope's working set occupies (`reset` is schema-granular — `DROP SCHEMA … CASCADE`). Roles are dropped only on a full reset; a subset scope leaves shared roles intact. The all-scope (no `--scope`) drops every managed schema, as before.

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
dbd export --scope hub                  # Only tables in the 'hub' scope
```

Writes to `export/<schema>/<name>.<format>`.
If export entries are configured in design.yaml, only those tables are exported.
`--scope`/`--deps` further restrict the export to the scope's working set.

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

### Reverse-engineer from a database — `--from-db`

Generate a whole project from an existing database instead of the sample scaffold. dbd
introspects the catalog (schemas, extensions, enums, tables — columns, defaults,
PK/FK/unique/check constraints, indexes, comments — and views), reconstructs canonical
`CREATE …` DDL, and writes the usual `design.yaml` + `ddl/<kind>/<schema>/<name>.ddl` tree.

```sh
dbd init --from-db postgres://user:pass@host/db   # new project from a live DB
dbd init --from-db                                # connection from $DATABASE_URL (or -d)
dbd init --from-db $DATABASE_URL --version 3      # base project.version (default 1)
dbd init --from-db ... --schema config --schema staging   # only these schemas
dbd init --from-db ... --exclude-schema audit             # drop a schema
dbd init --from-db ... --all-schemas              # include Supabase platform schemas
dbd init --from-db ... --dry-run                  # print the plan, write nothing
```

| Flag | Description |
|------|-------------|
| `--from-db [CONN]` | Reverse-engineer instead of scaffolding. With no value, the connection resolves from `-d`/`--database` then `$DATABASE_URL`. |
| `--name NAME` | `project.name` (default: the database name from the connection). |
| `--version N` | Base `project.version` written to `design.yaml` (default `1`). |
| `--schema S` | Limit to exactly these schemas (repeatable). |
| `--exclude-schema S` | Add to the exclusion set (repeatable). |
| `--all-schemas` | Bypass the Supabase platform denylist (Postgres internals — `pg_catalog`, `information_schema`, `pg_temp*`, `pg_toast*` — are still always excluded). |
| `--force-overwrite` | On conflict, back up the existing file to `.bak` and write the new one. |
| `--dry-run` | Print the plan and exit; touch nothing. |

**Schema selection.** Postgres internals are always excluded. On Supabase, platform schemas
(`auth`, `storage`, `realtime`, `extensions`, `graphql*`, `vault`, `pgsodium*`,
`supabase_*`, `cron`, `net`, …) are excluded by default; `--all-schemas` includes them.
`--schema` (allowlist) and `--exclude-schema` compose with these.

**Secrets stay out of the repo.** The generated `design.yaml` target URL is written as the
literal `$DATABASE_URL` env reference — never the connection string you passed.

`init --from-db` refuses to run in a directory that already has a `design.yaml` (use
`merge` to sync into an existing project). Reverse-engineering supports Postgres/Supabase
connections only; `--target sqlite` with `--from-db` is rejected.

**Managed databases & version safety.** `init --from-db` is for databases **not** managed by
dbd. If it detects a `_dbd_meta` table (in any schema — dbd tracks the applied version there),
it **refuses** and points you at `merge`, which knows how to reconcile a managed database into
its own project safely.

**What v1 captures (and what it doesn't).** This release reverse-engineers the **data
model**: schemas, extensions, enums, tables (columns, defaults, identity, PK/FK/unique/check
constraints, indexes incl. `USING gin/gist/brin/hash`, and table + column comments) and
views. Not yet captured:

- **Functions, procedures, roles, sequences** — coming in later patches.
- **Partial indexes** (`… WHERE …`) and **expression indexes** (e.g. `lower(name)`) — these
  are skipped, since they can't be represented losslessly yet.
- dbd's own bookkeeping tables (`_dbd_meta`, `_dbd_migrations`) are always excluded.

Column order reflects the database's **physical** order (which can differ from a
hand-authored file after `ALTER TABLE ADD COLUMN`). Run `dbd format` on the result to
normalize the output to your project's conventions.

---

## `dbd merge`

Sync a database into the **current** project — reverse-engineer, then reconcile against the
files already on disk. Same introspection and emitter as `init --from-db`, but it never
creates or edits `design.yaml`.

```sh
dbd merge postgres://user:pass@host/db   # sync into the current project
dbd merge                                # connection from -d / $DATABASE_URL
dbd merge --dry-run                      # preview the plan (create/conflict/orphan)
dbd merge --force-overwrite              # back up conflicts to .bak, then overwrite
dbd merge --schema config --exclude-schema audit   # same selection flags as init
```

Each generated file is classified before anything is written:

- **create** — no file at the path → written.
- **skip** — file exists and is byte-identical → left as-is (re-runs are idempotent).
- **conflict** — file exists and differs. Without `--force-overwrite` the run **aborts**
  and lists the conflicts (nothing is written) — except under `--dry-run`, which always
  prints the plan (conflicts included) and exits cleanly. With `--force-overwrite`, the
  existing file is renamed to `<name>.ddl.bak` (`.bak.1`, `.bak.2`, … on collision) and the
  new file is written. Because generated DDL is run through the same formatter as
  `dbd format`, a file you generate, then `dbd format`, then re-generate stays a **skip**
  (no spurious conflicts/`.bak` churn).
- **orphan** — an existing `.ddl`/`.sql` file of a managed kind (table/enum/view) under a
  selected schema with no matching DB entity. **Orphans are reported, never deleted** — you
  handle removals. (Orphaned *schema* and *extension* files aren't flagged in v1.)

`merge` requires an existing project (it refuses if there's no `design.yaml`; use
`init --from-db`). Because it never edits config, if it writes files for a schema not
listed in `design.yaml`'s `schemas:`, it **warns** so you can add the schema yourself. The
report summarises `created · unchanged · overwritten · orphans`, listing the orphan paths.

**Managed databases & version safety.** When the target is a **dbd-managed** database (a
`_dbd_meta` table exists in any schema), `merge` compares the database's applied version
**D** against the project's `design.yaml` `project.version` **Y** (treated as `0` when
unset):

- **D < Y → refuse.** The project is ahead of a stale database; overwriting project DDL from
  it would discard newer work. Bring the database up to date with `dbd apply`, or revert the
  project to v`D` via version control if you really mean to discard those changes. (There is
  no override flag.)
- **D ≥ Y → auto-snapshot.** The introspected DDL is written into the project (**overwriting
  drift in place — no `.bak`**, since the new snapshot plus version control are the record),
  then `dbd` captures the delta as a **new snapshot version**. `--dry-run` previews the plan
  and the snapshot version that would be created, writing nothing.

These checks apply only to managed databases. A **foreign** database (no `_dbd_meta`) takes
the normal create/skip/conflict/orphan path described above — the primary use of `merge`.

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
  query_style: river         # river (default) | none — align SELECT bodies
  gutter: 10                 # river keyword-gutter width (fits "inner join")
```

Handles CREATE TABLE (full formatting), CREATE INDEX, SET, COMMENT ON. Function/procedure `$$` bodies are preserved verbatim. SQLite `CREATE TRIGGER … BEGIN … END;` blocks are kept atomic (the inner statements aren't split or reformatted).

**River style** is the **default** `query_style` (set `query_style: none` to disable). It right-aligns SQL keywords at the `gutter` column so the clause keywords form a "river" down the left edge, with leading-comma SELECT lists, alias alignment, and one condition per line in WHERE/HAVING/ON. It applies to `CREATE VIEW` bodies and standalone SELECTs. A query the river renderer can't reproduce faithfully (e.g. one using a CTE) is automatically left in plain keyword-cased form rather than risk altering it — so river formatting never changes what your SQL means:

```sql
    select lv.id
         , lv.value     as display_value
      from lookups       lkp
inner join lookup_values lv
        on lv.lookup_id = lkp.id
     where lkp.name     = 'Gender';
```

### Pre-commit

`dbd format --check` exits 1 when any file would change, so it drops into [pre-commit](https://pre-commit.com) directly. Repo ships `.pre-commit-hooks.yaml` with two hooks:

| Hook id | Language | Use when |
|---------|----------|----------|
| `dbd-format` | `rust` (cargo install on first run, cached after) | Contributors don't have `dbd` installed |
| `dbd-format-system` | `system` (expects `dbd` on PATH) | Contributors already installed dbd locally |

User's `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/sensei-hq/dbd
  rev: v0.4.11
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

Audit and migrate project configuration and layout.

```sh
dbd doctor                         # Show issues
dbd doctor --fix                   # Apply fixes (config migration creates .yaml.bak)
```

Detects and migrates old Node.js config format:
- `project.database` → `source.dialect` + `target`
- Top-level `extensions`/`roles` → under `target`
- `nullValue` → `null_value`
- `project.staging` → `import.staging`
- `project.dbdocs` → top-level `dbml`

Removes stale files that dbd now manages internally, and migrates **plural DDL type folders to singular** (`ddl/functions/` → `ddl/function/`, etc.). When a singular folder already exists the contents are merged; on a same-name collision the newer file is kept and the older is backed up as `<name>.ddl.bkp` (each backup is reported). See [DDL folder layout](02-getting-started.md#ddl-folder-layout).

---

## Environment variables

| Variable | Used by |
|----------|---------|
| `DATABASE_URL` | Default database connection |
| `GITHUB_TOKEN` | Private GitHub repository access |
| `DBD_CATALOG_TTL` | Catalog cache TTL in hours (default: 24) |
