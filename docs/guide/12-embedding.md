# Embedding dbd-core

Everything the `dbd` command does lives in the **`dbd-core`** library crate; the
CLI is a thin layer over it. An application can load a design, validate it,
apply it, deploy it, diff it against a live database or just parse SQL — with
the same code path the CLI runs. A few safety checks live in the CLI's layer
rather than the library; [What the CLI adds](#what-the-cli-adds) lists them, so
an embedder can make the same checks.

Every Rust example on this page is compiled by dbd's own test suite against the
current crate.

## Add the crate

```toml
[dependencies]
dbd-core = "0.24"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Feature | Default | Gives you |
|---|---|---|
| `postgres` | on | the PostgreSQL adapter |
| `sqlite` | on | the SQLite adapter |
| `deploy` | on | downloading a GitHub source (`deploy::resolve_source`) |

The Convex adapter is always compiled. `Design::deploy` does not need the
`deploy` feature — that feature is only the GitHub download. While the crate is
`0.x`, the **minor** version is the breaking one: `0.24 → 0.25` may need code
changes, and the CHANGELOG says which.

Async methods need a Tokio runtime.

## Load a design

```rust
use dbd_core::Design;
use std::path::Path;

let mut design = Design::from_config(Path::new("db/design.yaml"), "prod")?;

let report = design.report(None, None);
for e in &report.issues {
    eprintln!("{}: {}", e.name, e.errors.join("; "));
}
println!(
    "{} entities, {} blocking error(s), {} with warnings",
    design.entities().len(),
    report.issues.len(),
    report.warnings.len()
);
```

`from_config` reads the config file and scans `ddl/` and `import/` next to it —
relative to the config's directory, so `Path::new("design.yaml")`
means the current directory. `from_config_with_dir(config, env, Some(dir))`
names the project directory explicitly. Loading is synchronous and needs no
database.

`report(name, scope)` is offline validation — what `dbd inspect` prints:

| Field | Holds |
|---|---|
| `issues` | entities with errors that block the given scope — what `apply` refuses on |
| `out_of_scope_issues` | entities with errors the scope does not build |
| `warnings` | entities with warnings |
| `gaps` | dependency gaps, when a scope is passed |

`report` takes `&mut self` because it caches the validation.

### The environment

The second argument is the environment the CLI's `-e` sets. It is recorded in
the bookkeeping table (the `reset` guard reads it back) and selects which
`import/<env>/` folders load. Pass `"dev"` or `"prod"`. The CLI accepts
`development` and `production` too; `config::normalize_env` is the mapping it
uses:

```rust
let env = dbd_core::config::normalize_env(Some("production"))?; // "prod"
let design = dbd_core::Design::from_config(path, &env)?;
```

## Connect

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
```

`connect` picks the adapter from the URL scheme exactly as the CLI does —
PostgreSQL, SQLite or Convex — and refuses MySQL and SQL Server URLs by name
(see [Targets and adapters](08-targets.md)). It returns a
`Box<dyn DatabaseAdapter>`; the `Design` methods take `&dyn DatabaseAdapter`, so
pass `&*adapter`. The concrete adapters are public too —
`adapter::postgres::PostgresAdapter::new(url, project)` and so on.

## Scope

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let all = design.resolve_scope(None, None)?;               // `default`, else the full design
let hub = design.resolve_scope(Some("hub"), None)?;        // a named scope, its own deps policy
let entities = design.scoped_entities(&hub)?;              // what it selects — no gap check
design.check_scope_gaps(&hub)?;                            // the gap check apply runs
```

The second argument overrides the scope's `deps` policy, like `--deps`. See
[Scopes](07-scopes.md).

## Apply

```rust
use dbd_core::design::Progress;
use dbd_core::{ApplyComplete, Design};

let design = Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
let scope = design.resolve_scope(None, None)?;

design
    .apply(&*adapter, None, /* dry_run */ false, Some(&scope), Progress {
        on_start: |step: &str| println!("→ {step}"),
        on_done: |step: &str, err: Option<&str>| {
            if let Some(e) = err {
                eprintln!("✗ {step}: {e}");
            }
        },
        on_complete: |s: ApplyComplete| {
            println!("v{} → v{}: {} applied, {} migrated", s.from_version, s.to_version, s.applied, s.migrated)
        },
    })
    .await?;
```

`apply(adapter, name, dry_run, scope, progress)` applies schemas, entities and
pending migrations — on PostgreSQL in one transaction — then stamps new
materialized views, syncs pg_cron refresh jobs and runs `apply.after`. `name`
limits the run to one entity.

The three callbacks travel together in one `Progress` value; they are not three
arguments. `Progress::none()` is a silent run. `ApplyComplete` carries
`strategy` (`Fresh`, `Migrate` or `Current`), `from_version`, `to_version`,
`applied`, `migrated`, `created`, `dropped`, the hook counts and any warnings.

`apply` does not apply RLS policies or the configured grants — `dbd apply` adds
the grants itself. Use `deploy` for both, or `design::apply_policies` for the
policies alone.

## Deploy

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
let scope = design.resolve_scope(None, None)?;

design
    .deploy(&*adapter, /* dry_run */ false, Some(&scope), |s| {
        println!("v{}: {} table(s) imported", s.apply.to_version, s.import.tables);
        println!("{} policy file(s) applied, {} failed", s.policies.applied.len(), s.policies.failed.len());
        for w in s.warnings() {
            eprintln!("warning: {w}");
        }
    })
    .await?;
```

`deploy` is the pipeline `dbd deploy` runs: apply, then the configured grants,
then import, then every policy under `policies/`. A failing policy file does not
fail the deploy — it lands in `policies.failed`, and `warnings()` lists it with
the apply and import warnings and any policy the scope skipped.
`deploy_with_progress(adapter, dry_run, scope, Progress { … })` adds the per-step
callbacks.

To deploy from GitHub as the CLI does, resolve the source to a local directory
first (this is the `deploy` feature):

```rust
let dir = dbd_core::deploy::resolve_source("owner/repo/db@v2.1", /* no_cache */ false).await?;
let design = dbd_core::Design::from_config(&dir.join("design.yaml"), "prod")?;
```

## Import and policies

```rust
use dbd_core::design::{apply_policies, Progress};

let design = dbd_core::Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;

design.import_data(&*adapter, None, false, None, Progress::none()).await?;

let report = apply_policies(&*adapter, design.project_dir(), false, None).await?;
for (file, err) in &report.failed {
    eprintln!("{}: {err}", file.display());
}
```

`import_data` takes the same five arguments as `apply`, and runs the same import
`dbd import` does — load, procedures, `import.after`. `apply_policies` takes an
optional `(scope_name, working_set)` pair; `None` applies every file.

## Diff and reconcile

```rust
use dbd_core::design::Progress;

let design = dbd_core::Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;

let diff = design.diff_live(&*adapter, None).await?;
for change in &diff.changes {
    println!("{} {:?}", change.entity_name, change.action);
}

let plan = design
    .reconcile(&*adapter, /* dry_run */ true, /* allow_destructive */ false, /* prune */ false, None, Progress::none())
    .await?;
for stmt in &plan.altered {
    println!("-- {}\n{}", stmt.entity_name, stmt.sql);
}
```

`diff_live` is `dbd diff`: read-only, returning a `SchemaDiff` (`changes`,
`warnings`, `advisories`, `matview_drift`; `is_empty()` when in sync).
`reconcile` returns the `ReconcilePlan` and, unless `dry_run`, executes it.

Both refuse a project whose reader builds no structured model (T-SQL, MySQL,
SQLite) and a Convex adapter. **`reconcile` does not check `project.released`**
— the CLI does; see [What the CLI adds](#what-the-cli-adds).

## Documentation output

```rust
use std::path::Path;

let design = dbd_core::Design::from_config(path, "prod")?;

let graph = design.graph(None, None)?;                 // `dbd graph`
for layer in &graph.layers {
    println!("{}", layer.join(", "));
}

design.combine(Path::new("init.sql"), None)?;          // `dbd combine`

let mut model = dbd_core::schema_model::build(&design, None);
model.history = dbd_core::history::load(design.project_dir())?;
std::fs::write("schema.json", serde_json::to_string_pretty(&model)?)?;
```

`schema_model::build(design, scope)` is the model `dbd diagram` encodes (see
[The schema viewer](11-schema-viewer.md#the-model)); it does not read
`snapshots/`, so attach the changelog yourself. Under a scope, filter the
history with `history::load_scoped(project_dir, keep)`, where `keep(schema,
name)` says which tables and enums to keep:

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let history = dbd_core::history::load_scoped(design.project_dir(), |schema, _name| schema == "app")?;
```

DBML comes from `dbml::generate_dbml(&DbmlParams)` and
`dbml::generate_all(&DbmlMultiParams)`: parameter structs carrying the entities
to document (`design.entities()`, or `design.scoped_entities(&scope)?`), the
project name, the database type and the filters. Each returns `DbmlDocument`s.

## Translate to another engine

```rust
use dbd_core::emit_dialect::emit_schema;
use dbd_core::parser::Dialect;

let design = dbd_core::Design::from_config(path, "prod")?;
let (sql, downgrades) = emit_schema(&design, Dialect::MySql, None)?;
std::fs::write("schema.mysql.sql", sql)?;
for d in &downgrades {
    println!("{} {:?}: {} -> {} ({})", d.entity, d.column, d.from, d.to, d.reason);
}
```

`emit_schema(design, target, scope)` is `dbd emit`. See
[Other dialects](13-other-dialects.md#dbd-emit).

## Parse SQL without a project

A tool with its own file scanner — a code indexer, say — wants entities and
their references, not a deploy:

```rust
use dbd_core::parser::{parse_sql, parse_sql_as, Dialect};

let parsed = parse_sql(sql)?; // PostgreSQL
for err in &parsed.errors {
    eprintln!("not read: {err}"); // SQL the reader rejected still returns Ok
}
for e in &parsed.entities {
    println!("{:?} {}", e.entity_type, e.name);
    for r in e.reads() {
        println!("  reads {}", r.name);
    }
    for r in e.writes() {
        println!("  writes {}", r.name);
    }
}

let tsql = parse_sql_as(Dialect::TSql, sql)?;
println!("the file itself reads {:?}", tsql.references.reads);
```

`parse_sql` asks the SQL what it declares; `parse_entity(path, sql)` instead
takes identity from a `ddl/<type>/<schema>/<name>.ddl` path, which is right
inside a dbd project and wrong outside one.

- `ParsedFile` holds `entities`; `errors` — check it, because SQL the reader
  rejects comes back as `Ok` with no entities and the reason here; the file's
  own `references` (reads, writes and calls made outside any declaration — only
  the T-SQL and MySQL readers fill it); the `dialect` it was read as; its `kind`
  (`Declaration`, `Migration`, `Data`, `Mixed`, `Empty`); and its
  `schema_path`.
- Each `Entity` keeps every reference in one list, `refs`, each with a `kind`
  (`Reads`, `Writes`, `Calls`, `Member`), a `schema_source` (stated, inferred or
  resolved) and an `unresolved` flag. `reads()`, `writes()`, `calls()` and
  `refs_of(kind)` filter it; `refers()` yields each referenced name once and
  skips unresolved ones.
- `Dialect::detect(sql)` guesses a dialect from the text and fails closed —
  SQL that every dialect accepts is `Unstated`.
- `references::resolve_references(&mut entities, &externals, &ignore)` resolves
  references once every file is read; the per-file parse touches no shared
  state, so a scan parallelises.
- `source_text::read_to_string(path)` decodes a file the way dbd does — UTF-8,
  or UTF-16 with a byte-order mark — rather than failing on what SSMS writes.

To learn about a directory without parsing it:

```rust
if let Some(survey) = dbd_core::survey(project_dir)? {
    println!("{} v{}: {} DDL file(s), read as {:?}", survey.project, survey.version, survey.ddl_files.len(), survey.parser);
}
```

## What the CLI adds

These checks are made by the `dbd` command, not by the `Design` methods. An
embedder that wants them makes them itself:

- **The scope guard.** `dbd apply`, `deploy`, `reconcile` and `reset` refuse to
  write under a scope other than the one the database is pinned to.
  `Design::apply`, `deploy` and `reconcile` do not check — and they re-pin the
  database to whatever scope they are given.
- **The release check.** `dbd reconcile` refuses on a released project;
  `Design::reconcile` does not.
- **Grants on apply.** `dbd apply` applies the configured grants after
  `Design::apply`; `Design::deploy` includes them.

```rust
use dbd_core::Design;

let design = Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
let scope = design.resolve_scope(None, None)?;

// What `dbd apply` checks before it writes. `true` would be --allow-scope-change.
let meta = adapter.get_project_meta().await?;
Design::check_scope_guard(meta.as_ref(), &scope.name, false)?;

if design.config().project.released {
    // released: change the schema with snapshots and apply, not reconcile
}
```

## Errors

Almost every fallible call returns `dbd_core::Result<T>`, whose error is
`DbdError`: `Config`, `Parse { file, message }`, `SafetyGuard` (the `reset`
guards and `check_scope_guard`), `GitHubSource`, `Migration`, and wrapped `Io`,
`Yaml` and `Json` errors. (`diagram::encode_payload` and `diagram::fragment_url`
return `serde_json`'s error directly.)

```rust
match dbd_core::Design::from_config(path, "prod") {
    Ok(design) => println!("{} entities", design.entities().len()),
    Err(dbd_core::DbdError::Yaml(e)) => eprintln!("design.yaml is not valid YAML: {e}"),
    Err(dbd_core::DbdError::Config(message)) => eprintln!("could not load the project: {message}"),
    Err(other) => return Err(other),
}
```
