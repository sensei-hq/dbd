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
`<type>` ∈ `table|view|materialized_view|function|procedure|enum|role`; ext
`.ddl` or `.sql`. Folder = type, parent dir = schema, filename = entity name.
**Singular** type folder is canonical (`dbd doctor --fix` migrates plural →
singular).

- `ddl/table/config/lookups.ddl` → entity `config.lookups` (table)
- `ddl/view/public/genders.ddl` → entity `public.genders` (view)
- `ddl/materialized_view/analytics/daily_sales.ddl` → `analytics.daily_sales` (matview)
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
| `dbd apply` | Apply schemas + entities + pending migrations (blocks on migration TODOs). `--with-policies` also applies RLS. Scope-guarded — `--allow-scope-change` to re-pin |
| `dbd deploy --source owner/repo/path` | Fetch (GitHub) → apply → import → **apply RLS policies**. `--no-cache` / `--clear-cache`. Scope-guarded — `--allow-scope-change` to re-pin |
| `dbd reconcile` | Pre-release declarative diff+`CREATE`/`ALTER` in place (no snapshots). `--allow-destructive`, `--prune`, `--allow-scope-change`. Disabled after `release` |
| `dbd release` (`baseline`) | Cut v1: baseline snapshot + `project.released: true` (locks in snapshot/migration flow) |
| `dbd snapshot` | Diff vs latest snapshot → migration SQL (smart multi-stage for renames/type changes) |
| `dbd migrate --status` | Show migration status (read-only; `apply` runs them) |
| `dbd import` / `export` | Load / dump table data (CSV/TSV/JSONL) via the `import/` & `export/` conventions |
| `dbd refresh` | `REFRESH MATERIALIZED VIEW [CONCURRENTLY]` now (`-n <entity>`/`<schema>.*` for a subset). Scheduled refresh is managed via pg_cron under `materialized_views:` in design.yaml |
| `dbd dbml` / `graph` / `diagram` | DBML docs / dependency JSON / hosted interactive viewer |
| `dbd policies` | Apply RLS from `policies/<schema>/<table>.sql` (idempotent, fail-forward) |
| `dbd doctor [--fix]` | Audit/repair config + layout: old design.yaml, stale files, plural dirs (`--fix` migrates these). Also flags **misfiled view/matview DDL** (a `CREATE MATERIALIZED VIEW` under `ddl/view/`, or a plain view under `ddl/materialized_view/`) — reported with a move hint, **not** auto-fixed. Run it to verify layout before `reset`/`apply` |
| `dbd reset` | Drop the project's own objects (guarded by the bookkeeping env check — `dbd.meta` on Postgres/Supabase, `_dbd_meta` on SQLite; blocked in prod / post-v1). Also scope-guarded — `--force` or `--allow-scope-change` bypasses |
| `dbd format [--check]` | DDL formatter (river-style SELECT bodies; `--check` for pre-commit) |

**Global scope flags** (honored by `inspect`/`apply`/`import`/`deploy`/`reconcile`
and the filter-only `dbml`/`combine`/`graph`/`export`/`reset`):
`--scope <name>` selects a `scopes:` subset; `--deps report|include` overrides
its gap policy. One design → many DBs: `dbd deploy --scope hub --database $HUB_URL`.

**Scope guard**: the bookkeeping table (`dbd.meta` on Postgres/Supabase, `_dbd_meta` on
SQLite) records the scope a database was built with (nullable `scope` column; `NULL` =
unpinned). `apply`/`deploy`/`reconcile`/`reset` refuse to
run under a different scope than the one the database is pinned to:
`scope guard: this database is pinned to scope 'X', but you requested 'Y'.
Applying a different scope would build a divergent schema.` Pass `--scope X` to
match the pin, or `--allow-scope-change` to override — a successful write then
re-pins the database to the new scope (`reset --force` also bypasses the check).
Databases created before this feature are unpinned and aren't blocked until their
next write pins them; a missing `--scope` resolves to the `default` scope (or
`all`) and pins on first write. To legitimately host multiple modules in one
database, define a named scope in `design.yaml` that includes them, rather than
re-pinning back and forth.

## Critical gotcha — inspect ≠ apply

`inspect` validates the **parsed model**, and only at the **entity level**: it
checks that referenced *tables/views/types* exist (`resolve_references`). It does
**not** validate *column-level* references (index columns, `CHECK`, generated
columns, FK column lists) and does **not** execute SQL. `apply` for a table ships
the **raw .ddl text** straight to the database. So a missing/renamed **column**
referenced by an index or constraint passes `inspect` cleanly and only fails at
`apply`, as a database error. Column-level correctness is the database's job, not
`inspect`'s.

## Workflow — pre-release vs upgrades (read this before changing a schema)

dbd has **two** schema-change workflows and using the wrong one corrupts the
project's history. Decide which you're in **first**:

**Which am I in?** The project is **released** iff `design.yaml` has
`project.released: true` **or** `snapshots/` contains a baseline snapshot.
Otherwise it is **pre-release**.

| | Pre-release (still iterating, pre-v1) | Released (upgrades) |
|---|---|---|
| Signal | no `project.released`, no snapshots | `project.released: true` **or** `snapshots/` exists |
| Change a schema | edit DDL, then **`dbd reconcile`** (diffs live↔design, applies `CREATE`/`ALTER` in place — no snapshot, no version bump) | edit DDL, then **`dbd snapshot`** (writes the migration) → **`dbd apply`** (runs it) |
| Fresh DB | `dbd apply` | `dbd apply` (runs all migrations) |
| `dbd reconcile` | ✅ the whole point | ❌ **disabled** — do not use |
| Drops | `reconcile --allow-destructive` (columns) / `--prune` (orphan tables) | expressed as migrations via `dbd snapshot` |

- **Pre-release example:** you renamed a column in `ddl/table/app/orders.ddl`; run
  `dbd reconcile -d $DATABASE_URL` to converge the dev DB. No snapshot is written.
- **Released example:** same rename after `dbd release`; run `dbd snapshot --name "rename …"`
  (dbd generates the migration, splitting a rename into safe stages) then `dbd apply`.
- **Never** hand-write SQL against a released database, and never run `reconcile` on one —
  `dbd release` disables it precisely to force changes through the migration trail.

## Self-check before you touch a dbd project

- **Right workflow for the release state** (above) — the #1 mistake is `reconcile` on a released project or hand-migrations on a pre-release one.
- **Singular type folder**: `ddl/table/…` not `ddl/tables/…` (`dbd doctor --fix` migrates plural).
- **Idempotent DDL** so re-`apply` is safe: `create table if not exists`, `create or replace view`, `create materialized view if not exists` (+ `create [unique] index if not exists`). Postgres has no `create or replace materialized view`.
- **Secrets via `$ENV_VAR`** in `design.yaml` target URLs — never a literal connection string.
- **String-set `CHECK` → consider an enum**: `check (status in ('a','b'))` is better modeled as a Postgres `enum` (`ddl/enum/…`) for type safety + introspection. `dbd inspect` suggests these.
- **`inspect` doesn't check column-level refs** (index/CHECK/FK column lists) — those fail at `apply` as DB errors, not at `inspect`.
- **Run `dbd doctor` to verify layout** before `reset`/`apply`. Beyond old-format config, stale files, and plural folders (all `--fix`-able), it flags **misfiled view/matview DDL**: dbd types an entity by its folder, so a `CREATE MATERIALIZED VIEW` sitting under `ddl/view/` is treated as a plain view and `dbd reset` emits `DROP VIEW` on it → `"… is not a view"`. doctor prints the file and a move hint (`→ ddl/materialized_view/<schema>/…`); move the file to fix (not auto-fixed — folder = type is the source of truth).

## Materialized views

`ddl/materialized_view/<schema>/<name>.ddl` holds `create materialized view if
not exists … with data;` + trailing `create [unique] index if not exists …`.
Scheduled refresh is declared under `materialized_views:` in design.yaml
(shared `options` + per-view `overrides`) and runs in-database via **pg_cron**
(synced on `apply`/`deploy`; needs the `pg_cron` extension; `concurrently`
needs a unique index). `dbd refresh` refreshes on demand. **Reconcile only
*warns* on a drifted matview definition — it never auto-drops one** (a recreate
is `DROP … CASCADE`, losing data/dependents); to apply a changed definition,
drop it manually, then `apply`/reconcile recreates it. `dbd diff` reports matview
drift (`missing`/`drifted`/`unstamped`/`orphan`). PostgreSQL/Supabase only.

**The folder is the type.** A matview MUST live under `ddl/materialized_view/<schema>/`
— one placed under `ddl/view/` is classified as a plain view, so `dbd reset` runs
`DROP VIEW` on it and fails (`"… is not a view"`). `dbd doctor` flags this mismatch
with a move hint; see the self-check above.

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
