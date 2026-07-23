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
dbd import -n staging.lookups      # Import one table (from the import/ folder)
dbd import -n activity.hook_events -f ~/data/events.jsonl   # load a specific file into a table
dbd import --dry-run               # Show import plan (no DB needed)
dbd import -e dev                  # Load dev-only data
```

**`-f`/`--file`** loads one explicit file into the table named by `-n` (required with `-f`),
instead of the `import/<schema>/<table>.<ext>` convention. The format is inferred from the file
extension (`.jsonl`/`.tsv`/`.csv`). (On `import`, `-f` is the *file*; on `export` it's the
*format* — each is unambiguous within its command.)

**Dry-run output shows the full plan:**
```
  import staging.models (jsonl) ← import/staging/models.jsonl
  import staging.routers (jsonl) ← import/staging/routers.jsonl

  call staging.import_models()
  call staging.import_routers()
```

Procedures are matched by reads/writes analysis (which procedure reads from which staging table), not by naming convention.

**How reads/writes are derived.** Function and procedure bodies are parsed, not text-matched. `LANGUAGE sql` bodies are parsed as an AST; PL/pgSQL bodies are parsed by libpg_query (PostgreSQL's own parser), which sees through `SELECT … INTO`, `PERFORM`, `FOR … IN … LOOP`, `RETURN QUERY`, and `IF` conditions. Either way, reads and writes are detected precisely through sub-selects, joins, set operations (`UNION`/`INTERSECT`/`EXCEPT`), and CTEs, and a write target (`INSERT`/`UPDATE`/`DELETE`) is distinguished from the tables it reads. A best-effort regex scan is used only if a body can't be parsed at all.

**Dynamic SQL is not tracked.** Statements built and run as strings (`EXECUTE format(...)`, `EXECUTE 'INSERT ...'`) are deliberately ignored. They create only a *runtime* dependency — they never determine whether the function can be created — so they are out of scope for dependency ordering. Tables referenced only inside dynamic SQL will not appear in the reads/writes analysis; reference them statically if you need them ordered.

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

Drop the project's own managed objects so you can re-`apply` from scratch. Guarded by the `_dbd_meta` environment check.

```sh
dbd reset                          # Blocked if prod or version >= 1
dbd reset --force                  # Override safety guard
dbd reset --dry-run                # Show what would be dropped
dbd reset --schemas                # Also DROP SCHEMA for managed schemas
dbd reset --extensions             # Also DROP EXTENSION for configured extensions
dbd reset --clean                  # Shorthand for --schemas --extensions
dbd reset --scope hub --dry-run    # Only the objects the 'hub' scope occupies
```

**Entity-level by default.** `reset` drops the project's own objects individually — functions/procedures (every overload, via a `DROP ROUTINE` block), views, tables, sequences and enums — in reverse dependency order. It does **not** `DROP SCHEMA` or `DROP EXTENSION`, so `public`, custom schemas, and installed extensions (e.g. `pgvector`) survive untouched; the next `dbd apply` repopulates the schemas. This avoids churning extensions and never aborts on `public`.

**Opt-in wider drops:**

- `--schemas` — also `DROP SCHEMA … CASCADE` for the project's managed schemas. Always skips the true system schemas (`pg_catalog`, `information_schema`, `pg_toast`); on a `supabase` target also skips the Supabase-managed set (`auth`, `storage`, … and `public`). On a `postgres` target, `public` **is** dropped.
- `--extensions` — also `DROP EXTENSION … CASCADE` for each extension in the target's `extensions:` config.
- `--clean` — shorthand for `--schemas --extensions`.

**Scope-aware.** `--scope` restricts the reset to the objects (and, with `--schemas`, the schemas) the scope's working set occupies. Roles are dropped only on a full reset; a subset scope leaves shared roles intact.

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
dbd deploy --no-cache --source owner/repo/path -d $URL     # Re-download this source
dbd deploy --clear-cache --source owner/repo/path -d $URL  # Wipe the whole cache first
```

GitHub sources are cached in `~/.cache/dbd/` (macOS: `~/Library/Caches/dbd`), keyed by
`owner-repo-ref`. `--no-cache` ignores the cached copy of the current source and re-downloads it;
`--clear-cache` removes the entire cache directory before deploying. Both are no-ops for local path
sources (`--clear-cache` still clears the cache).

---

## `dbd reconcile`

Pre-release (pre-v1) **declarative** apply: diff the live database against the design and apply the
difference in place — `CREATE` for new tables, `ALTER` for changed ones — with **no snapshot files
and no version bump**. Ideal while the schema is still churning and cutting a snapshot per change is
overkill.

```sh
dbd reconcile --dry-run -d $DATABASE_URL              # Preview the plan
dbd reconcile -d $DATABASE_URL                        # Apply (create + alter)
dbd reconcile --allow-destructive -d $DATABASE_URL    # Also drop columns/constraints
dbd reconcile --prune -d $DATABASE_URL                # Also drop orphaned tables
```

The diff is scoped to the schemas the design declares, so reconcile never touches tables in other
schemas. Two kinds of destruction each need an explicit opt-in:

- **`--allow-destructive`** — drop a *column* or constraint from a managed table.
- **`--prune`** — drop a whole *table* still in a managed schema but no longer in the design (an
  orphan). Without `--prune`, orphans are reported and left in place.

Orphaned *enums* are only warned about, never auto-dropped (columns may still reference them).
Reconcile is **disabled once the project is released** (`project.released: true`) — see `dbd release`.
`--scope`/`--deps` restrict reconcile to a scope's working set (gap-gated, like `apply`).

**What reconcile compares** (against the introspected live schema): columns
(name, type, nullability, default, identity) and primary-key / unique constraints. Bare enum types
are schema-qualified to match introspection, and common type aliases are normalized (`int4` →
`integer`, `timestamptz` → `timestamp with time zone`). **Not** reconciled on existing tables:
foreign keys, check constraints, and indexes — their introspected and parsed forms differ too much
to diff reliably, so change those via `dbd snapshot` (they're still created with the initial
`CREATE`). Because introspected and hand-written DDL can spell a default or exotic type differently,
reconcile may occasionally emit a redundant (harmless) `ALTER … SET DEFAULT`/`TYPE`; review with
`--dry-run` first.

---

## `dbd release` (alias `dbd baseline`)

Cut the first version: write a **baseline snapshot** at the current `project.version` (the anchor
future `dbd snapshot` diffs against) and set `project.released: true`, locking the project into the
snapshot/migration workflow and disabling `dbd reconcile`.

```sh
dbd release --name "v1 GA"    # Baseline snapshot + released: true
dbd baseline                  # Alias
```

Refuses if the project is already released, if snapshots already exist (it's already on the
migration track), or if the design has entity errors (run `dbd inspect` first). After release the
flow is the ordinary one: edit DDL → `dbd snapshot` → `dbd apply`.

---

## `dbd export`

Export table data to files.

```sh
dbd export                              # Export all tables as CSV
dbd export --name config.lookups        # Export one table
dbd export --format tsv                 # TSV format
dbd export --format jsonl               # JSONL format
dbd export -n activity.hook_events -f jsonl -o import/staging   # → import/staging/hook_events.jsonl
dbd export --scope hub                  # Only tables in the 'hub' scope
```

By default writes to `export/<schema>/<name>.<format>`. **`-o`/`--output <dir>`** changes the
destination directory — files are written flat as `<dir>/<name>.<format>` (so you can export
straight into the `import/` tree for a round-trip). `-f`/`--format` stays the format
(`csv`/`tsv`/`jsonl`, default `csv`). If export entries are configured in design.yaml, only
those tables are exported. `--scope`/`--deps` further restrict the export to the scope's working set.

---

## `dbd init`

Scaffold a new dbd project, then evolve it: edit DDL, apply, and snapshot each change.

<svg viewBox="0 0 600 116" role="img" aria-label="dbd init lifecycle: dbd init scaffolds the project; you edit the ddl/ folder; dbd apply pushes it to the database; dbd snapshot versions a change; then you loop back to edit for the next change." xmlns="http://www.w3.org/2000/svg" style="max-width:600px;width:100%;height:auto;color:currentColor">
  <defs><marker id="ar-init" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker></defs>
  <style>
    .fb{fill:none;stroke:currentColor;stroke-width:1.5}
    .fba{fill:currentColor;fill-opacity:.07;stroke:currentColor;stroke-width:1.5}
    .ft{font-size:12px;fill:currentColor;text-anchor:middle;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
    .fe{fill:none;stroke:currentColor;stroke-width:1.5}
    .fl{font-size:10px;fill:currentColor;fill-opacity:.75;text-anchor:middle;font-family:system-ui,sans-serif}
  </style>
  <rect class="fba" x="6"   y="20" width="118" height="44" rx="9"/><text class="ft" x="65"  y="46">dbd init</text>
  <rect class="fb"  x="166" y="20" width="118" height="44" rx="9"/><text class="ft" x="225" y="46">edit ddl/</text>
  <rect class="fba" x="326" y="20" width="110" height="44" rx="9"/><text class="ft" x="381" y="46">dbd apply</text>
  <rect class="fb"  x="478" y="20" width="116" height="44" rx="9"/><text class="ft" x="536" y="46">dbd snapshot</text>
  <path class="fe" d="M124,42 H162" marker-end="url(#ar-init)"/>
  <path class="fe" d="M284,42 H322" marker-end="url(#ar-init)"/>
  <path class="fe" d="M436,42 H474" marker-end="url(#ar-init)"/>
  <path class="fe" d="M536,64 V94 H225 V68" marker-end="url(#ar-init)"/>
  <text class="fl" x="380" y="90">next change</text>
</svg>

(With `--from-db`/`--from-dbml`, dbd **skips the scaffold + hand-editing** — it reverse-engineers
the DDL and writes the baseline snapshot for you, so you go straight to `dbd apply`. See
[recommended workflows](06-recommended-workflows.md).)

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
dbd init --from-db postgres://user:pass@host/db   # new project from a live Postgres DB
dbd init --from-db sqlite://./app.db              # …or a SQLite database (file:/sqlite:: also work)
dbd init --from-db                                # connection from $DATABASE_URL (or -d)
dbd init --from-db $DATABASE_URL --version 3      # base project.version (default 1)
dbd init --from-db ... --schema config --schema staging   # only these schemas (Postgres)
dbd init --from-db ... --exclude-schema audit             # drop a schema
dbd init --from-db ... --all-schemas              # include Supabase platform schemas
dbd init --from-db ... --dry-run                  # print the plan, write nothing
```

The **dialect is taken from the connection URL scheme** (`postgres://` → a `postgres`
target, `sqlite://`/`file:` → a `sqlite` target). **SQLite** is captured verbatim from
`sqlite_master`: tables (with their CHECK constraints, type affinity, `AUTOINCREMENT`,
`WITHOUT ROWID`), user indexes, and views — losslessly. SQLite has no schemas/enums/
functions/sequences/roles, so those don't apply, and **triggers are skipped** (no `CREATE
TRIGGER` support yet). Files land flat (`ddl/table/<name>.ddl`) and `design.yaml` has an
empty `schemas:` list. (`dbd inspect` on a SQLite project may emit benign
schema-qualification warnings for cross-object references; the DDL is valid and applies.)

| Flag | Description |
|------|-------------|
| `--from-db [CONN]` | Reverse-engineer instead of scaffolding. With no value, the connection resolves from `-d`/`--database` then `$DATABASE_URL`. |
| `--from-dbml <FILE>` | Reverse-engineer from a DBML file instead of a connection; mutually exclusive with `--from-db`. Produces schemas + enums + tables + foreign keys only (see note below). |
| `--name NAME` | `project.name` (default: the database name from the connection). |
| `--version N` | Base `project.version` written to `design.yaml`, and the version of the baseline snapshot (default `1`). |
| `--schema S` | Limit to exactly these schemas (repeatable). |
| `--exclude-schema S` | Add to the exclusion set (repeatable). |
| `--all-schemas` | Bypass the Supabase platform denylist (Postgres internals — `pg_catalog`, `information_schema`, `pg_temp*`, `pg_toast*` — are still always excluded). |
| `--roles` | Also reverse-engineer roles (off by default; cluster-global, platform roles filtered — see "What's captured"). |
| `--dry-run` | Print the plan and exit; touch nothing. |

**Version-tracked from the start.** After writing the `ddl/` tree and `design.yaml`,
`init --from-db` emits a **baseline snapshot at `--version`** (default 1) — so the new
project lands with `snapshots/{NNN}.json` and `design.yaml`'s `project.version` set to
that version, ready for `dbd snapshot`/`dbd apply`. `--dry-run` previews this
(`would create baseline snapshot v{N}`) and writes nothing.

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

**What's captured (and what isn't).** Reverse-engineering covers the **data model**:
schemas, extensions, enums, tables (columns, defaults, identity, PK/FK/unique/check
constraints, indexes incl. `USING gin/gist/brin/hash`, and table + column comments), views,
and **functions & procedures** (full bodies, captured verbatim via `pg_get_functiondef`;
overloads of the same name share one file; extension-provided routines are excluded).

**Roles** are captured only with the opt-in **`--roles`** flag (they are cluster-global, not
owned by the database). System/platform roles are always filtered out — superusers, `pg_*`,
cloud roles (`rds_*`/`azure_*`/`cloudsql*`), and Supabase platform roles (`anon`,
`authenticated`, `service_role`, `authenticator`, `supabase_*`, `pgsodium*`, `dashboard_user`,
`pgbouncer`, `postgres`). Roles are captured as name + memberships only (no attributes, never
passwords); memberships referencing filtered-out roles are dropped so the emitted set is
self-contained.

**Sequences** are captured: standalone sequences become `ddl/sequence/<schema>/<name>.ddl`
(`CREATE SEQUENCE …`), while sequences owned by a `serial`/`IDENTITY` column are reproduced
through the column itself — such columns emit `serial`/`bigserial`/`smallserial` or
`GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY` rather than a separate sequence file.

**From a DBML file (`--from-dbml`).** A DBML source produces **schemas + enums + tables + foreign
keys only** — DBML cannot express functions, procedures, views, standalone sequences, roles, or
check constraints, so none of those appear. `serial`/identity columns survive (dbd's DBML carries
`bigserial`/`[increment]`, so the reconstructed column keeps it). All the schema-selection flags
(`--schema`/`--exclude-schema`/`--all-schemas`) still filter the parsed entities; `--roles` is a
no-op since DBML has no roles.

Not yet captured:

- **Partial indexes** (`… WHERE …`) and **expression indexes** (e.g. `lower(name)`) — these
  are skipped, since they can't be represented losslessly yet.
- **Triggers**, aggregates, operators, domains, composite types.
- dbd's own bookkeeping tables (`_dbd_meta`, `_dbd_migrations`) are always excluded.

Column order reflects the database's **physical** order (which can differ from a
hand-authored file after `ALTER TABLE ADD COLUMN`). Run `dbd format` on the result to
normalize the output to your project's conventions.

---

## `dbd merge`

Sync a database into the **current** project — reverse-engineer, then reconcile against the
files already on disk. Same introspection and emitter as `init --from-db`, but it never
creates or edits `design.yaml`.

<svg viewBox="0 0 540 104" role="img" aria-label="dbd merge flow: dbd merge re-introspects the database, overwrites the DDL and auto-creates a snapshot of the diff, then dbd apply brings the database forward. merge refuses if the database version is behind the project version." xmlns="http://www.w3.org/2000/svg" style="max-width:540px;width:100%;height:auto;color:currentColor">
  <defs><marker id="ar-merge" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker></defs>
  <style>
    .mb{fill:none;stroke:currentColor;stroke-width:1.5}
    .mba{fill:currentColor;fill-opacity:.07;stroke:currentColor;stroke-width:1.5}
    .mt{font-size:12px;fill:currentColor;text-anchor:middle;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
    .me{fill:none;stroke:currentColor;stroke-width:1.5}
    .ml{font-size:10px;fill:currentColor;fill-opacity:.75;text-anchor:middle;font-family:system-ui,sans-serif}
  </style>
  <rect class="mba" x="6"   y="18" width="130" height="44" rx="9"/><text class="mt" x="71"  y="44">dbd merge</text>
  <rect class="mb"  x="196" y="18" width="150" height="44" rx="9"/><text class="mt" x="271" y="44">+ snapshot</text>
  <rect class="mba" x="406" y="18" width="120" height="44" rx="9"/><text class="mt" x="466" y="44">dbd apply</text>
  <path class="me" d="M136,40 H192" marker-end="url(#ar-merge)"/>
  <path class="me" d="M346,40 H402" marker-end="url(#ar-merge)"/>
  <text class="ml" x="160" y="82">refuses if the DB is behind the project version</text>
</svg>

`merge` overwrites the on-disk DDL from the database and **auto-snapshots the diff** as a new
version; you then `dbd apply` to bring the database forward. It **refuses** when the database is
behind the project version (a stale DB can't clobber newer work) — see
[version safety](06-recommended-workflows.md#version-safety).

```sh
dbd merge postgres://user:pass@host/db   # sync into the current project
dbd merge                                # connection from -d / $DATABASE_URL
dbd merge --from-dbml schema.dbml        # sync from a DBML file (no connection)
dbd merge --dry-run                      # preview the plan + the snapshot version
dbd merge --schema config --exclude-schema audit   # same selection flags as init
dbd merge --roles                        # also reverse-engineer roles (opt-in)
```

Every `merge` that proceeds **overwrites the introspected DDL into the project (no `.bak`)
and captures the delta as a new snapshot version** — foreign and dbd-managed databases
behave identically. The new snapshot plus version control are the record of what changed, so
`merge` deliberately clobbers on-disk drift rather than aborting or backing up. Each
generated file is still classified before writing:

- **create** — no file at the path → written.
- **skip** — file exists and is byte-identical → left untouched (re-runs are idempotent).
  Because generated DDL is run through the same formatter as `dbd format`, a file you
  generate, then `dbd format`, then re-generate stays a **skip** (no spurious churn).
- **overwrite** — file exists and differs → replaced in place (no `.bak`).
- **orphan** — an existing `.ddl`/`.sql` file of a managed kind (table/enum/view) under a
  selected schema with no matching DB entity. **Orphans are reported, never deleted** — you
  handle removals. (Orphaned *schema* and *extension* files aren't flagged in v1.)

After writing, `dbd` reloads the project and runs `snapshot::create_snapshot`: if the DDL was
already in sync with the latest snapshot it reports `already in sync — no snapshot created`,
otherwise it writes the next snapshot version (a baseline if the project had none yet) and
bumps `design.yaml`'s `project.version`. `--dry-run` previews the plan and the snapshot
version that *would* be created, writing nothing.

`merge` requires an existing project (it refuses if there's no `design.yaml`; use
`init --from-db`). Because it never edits config, if it writes files for a schema not
listed in `design.yaml`'s `schemas:`, it **warns** so you can add the schema yourself.

**Managed databases & version safety.** When the target is a **dbd-managed** database (a
`_dbd_meta` table exists in any schema), `merge` compares the database's applied version
**D** against the project's `design.yaml` `project.version` **Y** (treated as `0` when
unset). The only thing this changes is whether the merge proceeds at all:

- **D < Y → refuse.** The project is ahead of a stale database; overwriting project DDL from
  it would discard newer work. Bring the database up to date with `dbd apply`, or revert the
  project to v`D` via version control if you really mean to discard those changes. (There is
  no override flag.)
- **everything else** (a **foreign** database with no `_dbd_meta`, or a managed database with
  **D ≥ Y**) → the overwrite + auto-snapshot path described above.

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
  rev: v0.8.15
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
