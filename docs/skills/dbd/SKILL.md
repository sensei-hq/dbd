---
name: dbd
description: >-
  Use when working in a project whose database schema is managed with dbd —
  SQL DDL files under ddl/<type>/<schema>/<name>, a design.yaml, and dbd
  commands (init, inspect, apply, deploy, migrate, snapshot, reconcile, dbml,
  diagram, import/export, policies, reset, format). Also use when embedding the
  dbd-core Rust crate. Covers the file-as-source-of-truth model, the full CLI
  surface, scopes, targets (PostgreSQL/Supabase/SQLite/Convex), and the library
  API. Load before editing DDL, changing design.yaml, or running any dbd command.
---

# dbd — database schema as code

dbd parses SQL DDL files, resolves dependencies, generates migrations, imports
data, and deploys to **PostgreSQL, Supabase, SQLite, or Convex** — one binary,
zero runtime deps. Same engine is embeddable as the `dbd-core` Rust crate.

Full reference (exhaustive, always current): **https://dbd.sensei-hq.com/llms-full.txt**

## Mental model — files are the source of truth

Path = `ddl/<type>/<schema>/<name>.<ext>` (roles have no schema).
`<type>` ∈ `table|view|function|procedure|enum|role`; ext `.ddl` or `.sql`.
Folder = type, parent dir = schema, filename = entity name. **Singular** type
folder is canonical (`dbd doctor --fix` migrates plural → singular).

- `ddl/table/config/lookups.ddl` → entity `config.lookups` (table)
- `ddl/view/public/genders.ddl` → entity `public.genders` (view)
- `ddl/procedure/staging/import_lookups.ddl` → `staging.import_lookups`

Each file is parsed once into `Entity` + `TableDef`; that structure drives every
feature (dependency graph, apply, migrations, DBML, diagram, import).

`design.yaml` holds `project`, `source.dialect`, `target.<name>.url` (use
`$ENV_VAR` — never hardcode secrets), `schemas`, `import`/`export`, `dbml`,
`scopes`, and `ignore`. See the full reference for the annotated schema.

## CLI command map

| Command | What it does |
|---|---|
| `dbd init` | Scaffold a project; or reverse-engineer one with `--from-db <conn>` / `--from-dbml <file>` (refuses over a dbd-managed DB — use `merge`) |
| `dbd merge` | Sync a live DB / DBML back into the project (reverse + reconcile; never edits `design.yaml`) |
| `dbd inspect` | **Validate**: report entity errors/warnings + scope gaps. `--from-db` resolves refs against the live catalog and caches to `.dbd/refcache.json`. **Offline & no SQL execution** |
| `dbd apply` | Apply schemas + entities + pending migrations (blocks on migration TODOs). `--with-policies` also applies RLS |
| `dbd deploy --source owner/repo/path` | Fetch (GitHub) → apply → import → **apply RLS policies**. `--no-cache` / `--clear-cache` |
| `dbd reconcile` | Pre-release declarative diff+`CREATE`/`ALTER` in place (no snapshots). `--allow-destructive`, `--prune`. Disabled after `release` |
| `dbd release` (`baseline`) | Cut v1: baseline snapshot + `project.released: true` (locks in snapshot/migration flow) |
| `dbd snapshot` | Diff vs latest snapshot → migration SQL (smart multi-stage for renames/type changes) |
| `dbd migrate --status` | Show migration status (read-only; `apply` runs them) |
| `dbd import` / `export` | Load / dump table data (CSV/TSV/JSONL) via the `import/` & `export/` conventions |
| `dbd dbml` / `graph` / `diagram` | DBML docs / dependency JSON / hosted interactive viewer |
| `dbd policies` | Apply RLS from `policies/<schema>/<table>.sql` (idempotent, fail-forward) |
| `dbd doctor` | Audit/migrate config + layout (old design.yaml, stale files, plural dirs) |
| `dbd reset` | Drop the project's own objects (guarded by `_dbd_meta` env check; blocked in prod / post-v1) |
| `dbd format [--check]` | DDL formatter (river-style SELECT bodies; `--check` for pre-commit) |

**Global scope flags** (honored by `inspect`/`apply`/`import`/`deploy`/`reconcile`
and the filter-only `dbml`/`combine`/`graph`/`export`/`reset`):
`--scope <name>` selects a `scopes:` subset; `--deps report|include` overrides
its gap policy. One design → many DBs: `dbd deploy --scope hub --database $HUB_URL`.

## Critical gotcha — inspect ≠ apply

`inspect` validates the **parsed model**, and only at the **entity level**: it
checks that referenced *tables/views/types* exist (`resolve_references`). It does
**not** validate *column-level* references (index columns, `CHECK`, generated
columns, FK column lists) and does **not** execute SQL. `apply` for a table ships
the **raw .ddl text** straight to the database. So a missing/renamed **column**
referenced by an index or constraint passes `inspect` cleanly and only fails at
`apply`, as a database error. Column-level correctness is the database's job, not
`inspect`'s.

## Library usage (dbd-core)

```toml
# Cargo.toml
dbd-core = { git = "https://github.com/sensei-hq/dbd" }
```

```rust
use dbd_core::{connect, Design};
use std::path::Path;

// 1. Load the declarative design for an environment (sync; scans ddl/ next to the config).
let design = Design::from_config(Path::new("design.yaml"), "prod")?;

// 2. Validate offline — no DB needed.
let report = design.report(None, None);
//    report.issues   → entities with errors
//    report.warnings → entities with warnings (e.g. unresolved entity refs)

// 3. Connect an adapter — Postgres/SQLite/Convex chosen from the URL scheme.
let adapter = connect("postgres://localhost/mydb", &design.config().project.name).await?;

// 4. Apply schema + entities + pending migrations. Callbacks are (on_start, on_done, on_complete).
let scope = design.resolve_scope(None, None)?; // None,None ⇒ full design, default deps policy
design
    .apply(&*adapter, None, /*dry_run*/ false, Some(&scope),
        |desc| println!("→ {desc}"),
        |desc, err| if let Some(e) = err { eprintln!("✗ {desc}: {e}") },
        |summary| println!("applied {} entities", summary.applied))
    .await?;

// Or apply + import in one call:
design.deploy(&*adapter, false, Some(&scope), |s| println!("deployed to v{}", s.apply.to_version)).await?;
```

Key public types (re-exported at the crate root): `Design`, `DatabaseAdapter`,
`Entity` / `EntityType`, `SchemaModel`, `ResolvedScope`, `ApplyComplete` /
`DeployComplete` / `ImportComplete`, `DbdError` / `Result`. RLS policies are
orchestrated at the CLI layer via the free fn `dbd_core::design::apply_policies`.
