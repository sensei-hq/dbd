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
dbd-core = "0.25"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

| Feature | Default | Gives you |
|---|---|---|
| `postgres` | on | the PostgreSQL adapter |
| `sqlite` | on | the SQLite adapter |
| `deploy` | on | downloading a GitHub source (`deploy::resolve_source`) |

The Convex adapter is always compiled. `Design::deploy` does not need the
`deploy` feature — that feature is only the GitHub download. While the crate is
`0.x`, the **minor** version is the breaking one: `0.24 → 0.25` needs code
changes, and the CHANGELOG lists them under *Library API*.

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
| `warnings` | entities with warnings that the scope builds — all of them without a scope |
| `out_of_scope_warnings` | entities with warnings the scope does not build, kept apart as the errors are (new in 0.25) |
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

An adapter of your own implements `DatabaseAdapter`. 0.25 adds one method you
must write: `forget_project_meta()` deletes the database's record of the
project — its env, version and scope pin — which a full `reset` now does, so
the next apply records the database afresh. The rest arrive with defaults:

- `stamp_matview(qualified, stamp)` records a materialized view's drift stamp,
  and `forget_matview_stamps(qualified)` deletes the stamps of views the caller
  has just dropped — both no-ops unless the target has materialized views
  (Postgres keeps them in `dbd.matview_stamps`). `live_matviews()` returns the
  deployed materialized views with their definition and indexes, which is how
  `apply` recognises one a brownfield database already has; none by default.
- `truncate_table(table)` is the import's truncate step — `TRUNCATE TABLE`
  unless overridden; SQLite runs `DELETE FROM`, Convex nothing, since its
  import replaces the table.
- `existing_objects(objects)` says which of a list of `(kind, name)` exist.
  `reset` asks before and after it drops, so it reports what it dropped rather
  than what it ran — every drop is `IF EXISTS`. The default answers for tables
  and views from `list_entities`.
- `roles_held_elsewhere(roles)` maps each role to the other databases on the
  server where it holds anything, so `reset` can keep a role another database
  still uses. Roles are server-wide; the default, for a target without them, is
  none.

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

`working_set(&scope)` is the set of names a run acts on — the closure under
`deps: include` — and `scope_counts(&scope)` the `(kept, total)` pair every
command prints as `scope 'hub': N of M entities`, externals left out of both.
`resolve_name(name, Some(&scope))` is the `-n` check: `no entity named 'X'`,
or `X is outside scope 'S'` when the design has it and the scope does not
build it.

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

// The configured grants, on the schemas this scope builds — what `dbd apply` runs next.
let grants = design.apply_grants(&*adapter, Some(&scope)).await?;
println!("grants: {grants:?}");
```

`apply(adapter, name, dry_run, scope, progress)` applies schemas, entities and
pending migrations — on PostgreSQL in one transaction — then stamps new
materialized views, syncs pg_cron refresh jobs and runs `apply.after`. `name`
limits the run to one entity, and is refused when it names nothing the scope
builds, as `resolve_name` says. `entities_to_apply(name, scope)` is what it
would act on, behind the same gates and without a database — the list
`dbd apply --dry-run` prints.

Before its first hook or write, `apply` asks the database two questions, and
refuses on either answer — under `dry_run` too, since the database is there to
ask:

- **Is this run's env allowed here?** `Design::check_env_guard(meta, env)`
  refuses a run whose env is not prod against a database marked prod. A dev run
  would re-mark it dev — lifting `reset`'s prod guard — and its import would
  load `import/dev/` data into production. It is one-way: a dev database may be
  promoted to prod. `deploy`, `reconcile` and `import_data` check it too.
- **Is the project at least as new as the database?**
  `Design::check_version_guard(db_version, design_version)` refuses a design
  whose `project.version` is behind the version the database records (an
  unversioned design counts as 0). The plan would otherwise come out current
  and re-apply the older DDL over the newer schema. Neither guard has an
  override.

`pending_migration_preview(adapter, name, scope)` lists the migration steps the
run would take against that database, from the version it records, under the
same gates and scope — a `MigrationPreview` of steps, each with the statements
its files run and a `drops` flag on the destructive ones; a drop of a table the
database does not have is marked as one the run skips. It writes nothing; it is
what `dbd apply --dry-run` lists when it has a database.

The three callbacks travel together in one `Progress` value; they are not three
arguments. `Progress::none()` is a silent run. `ApplyComplete` carries
`strategy` (`Fresh`, `Migrate` or `Current`), `from_version`, `to_version`,
`applied`, `migrated`, `created`, `dropped` (the drops that ran), the hook
counts, any warnings, and — new in 0.25 — `previously_built`: on a `Fresh` run,
whether the database was already built without a recorded version, which is not
a fresh install.

The hooks a scope skips are in `ApplyComplete::warnings`, which only a run that
succeeds produces. `hook_skips(scope)` returns the same lines without running
anything — the decision the run makes — so a caller can say them first, as
`dbd apply` and `deploy` do, and a run that then fails still explains itself.
A hook file that is missing is an error there too.

`apply` does not apply RLS policies or the configured grants.
`apply_grants(adapter, scope)` applies the grants — under a scope, only on the
schemas the scope builds — and returns a `GrantsOutcome`: `None` when the design
declares none, `Applied`, or `Unsupported` on SQLite and Convex, which have no
grant model. `deploy` does both; `design::apply_policies` applies the policies
alone.

What is granted is `design.config().effective_schema_grants()` — the
`schemas:` grants with the first target's `grants:` merged on top, keyed schema
→ role → permissions. It is one definition for both sides: `apply_grants` grants
exactly it, and `reset` revokes exactly it before dropping the roles, so the
revoke cannot drift from the grant. (`script::build_reset_script` takes that map
as its final argument.)

## Deploy

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
let scope = design.resolve_scope(None, None)?;

design
    .deploy(&*adapter, /* dry_run */ false, Some(&scope), |s| {
        println!("v{}: grants {:?}, {} table(s) imported", s.apply.to_version, s.grants, s.import.tables);
        println!("{} policy file(s) applied, {} failed", s.policies.applied.len(), s.policies.failed.len());
        for w in s.warnings() {
            eprintln!("warning: {w}");
        }
    })
    .await?;
```

`deploy` is the pipeline `dbd deploy` runs: apply, then the configured grants
(`apply_grants`, reported in `DeployComplete::grants`), then import, then every
policy under `policies/`. Before the first statement it runs `check_import_plan`,
so what the import phase would refuse — a data file outside `import.staging`, a
staging table whose loader is ambiguous — is refused before anything is applied.
A failing policy file does not
fail the deploy — it lands in `policies.failed`, and `warnings()` lists it with
the apply and import warnings and any policy the scope skipped, in pipeline
order; `policy_warnings()` is the policy phase's share alone, for a caller that
reports phase by phase. `deploy_with_progress(adapter, dry_run, scope,
Progress { … })` adds the per-step callbacks.

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
`dbd import` does — truncate, load, procedures, `import.after`. The truncate goes
through the adapter (`DatabaseAdapter::truncate_table`), so it runs on SQLite and
Convex too. Only a procedure is a loader: `ImportPlanEntry.procedure` names the
procedure chosen to load the staging table — one per table, called once, picked
as [import and export](10-data.md#which-procedure-loads-a-table) describes —
never a function. A table with two data files is two entries and one load: the
procedure is on the first entry only.

`check_import_plan(name)` is every refusal an import path makes before it
touches the database: a `name` that selects no data file, a data file outside
`import.staging`, a staging table with two equal claims to its loader. It needs
no database, so a caller can refuse before it connects; `import_data`,
`scoped_import_plan` and `deploy` all run it.

`scoped_import_plan(name, scope)` is the plan a run under `scope` keeps, with one
line per entry it leaves out saying which table is outside the scope — an entry
needs every table its procedure writes and, when the design declares it, its
staging table. `import_warnings(name)` lists what the scan and the plan left out
before any scope: a missing `import/`, another environment's files, a data file
that failed to parse, a function reading a staging table no procedure loads, a
procedure reading one it does not load.
`design::import_entry_in_scope` is deprecated in favour of `scoped_import_plan`.

`apply_policies` takes an optional `(scope_name, working_set)` pair; `None`
applies every file. `design::plan_policies(project_dir, scope)` is the same filter
without a database — what `dbd policies --dry-run` lists.

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
`reconcile` returns the `ReconcilePlan` and, unless `dry_run`, executes it —
with the design's `apply.before` and `apply.after` hooks around the writes,
scoped as `apply` scopes them, so a reconciled database is the one an apply
builds. `ReconcileComplete` counts them in `before_scripts` and
`after_scripts`. Like `apply`, it does not apply the configured grants: call
`apply_grants` after it, as `dbd reconcile` does. It checks the env guard, not
the version guard — a reconcile records the project's version even over a newer
one.

Both refuse a project whose reader builds no structured model (T-SQL, MySQL,
SQLite) and a Convex adapter. **`reconcile` does not check `project.released`**
— the CLI does; see [What the CLI adds](#what-the-cli-adds).

## Reset

```rust
use dbd_core::{Design, EntityType};

let design = Design::from_config(path, "dev")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
let scope = design.resolve_scope(None, None)?;

let done = design
    .reset(&*adapter, "postgres", /* force */ false, /* schemas */ false, /* extensions */ false, Some(&scope))
    .await?;
println!("{} table(s) and {} view(s) dropped", done.count(EntityType::Table), done.count(EntityType::View));
for kept in &done.roles_kept {
    println!("kept role {}: {} still uses it", kept.role, kept.databases.join(", "));
}
```

`reset(adapter, target, force, drop_schemas, drop_extensions, scope)` is
`dbd reset`. `target` names the protected set — `postgres`, `supabase` or
`sqlite`; the CLI passes the design's target key unless `--target` overrides it.
Unless `force`, it runs `check_reset_guard` first. It returns a
`ResetComplete`: `dropped`, every `(kind, name)` it found and dropped — read
from the database before and after, because every drop is `IF EXISTS` and the
script alone cannot tell a reset that emptied the schema from one that found
nothing — with `count(kind)` to tally them, as `dbd reset` prints; and
`roles_kept`, a `KeptRole` (`role`, `databases`) for each role it left in place
because another database on the server holds something of it.

A full reset — one whose scope resolves to `all` — also forgets the database's
record of the project — env, version and scope pin — so the next apply records
it afresh; a scoped reset leaves that record, and the rest of the project, in
place. (Through `resolve_scope(None, None)`, a design that defines a `default:`
scope resolves to it, so that reset is a scoped one.) A scoped reset drops without
`CASCADE`, so it fails, naming the dependent, rather than reach outside its
scope.

`reset_script(target, drop_schemas, drop_extensions, scope)` is the SQL alone,
with no database and no guard. `roles_kept_by_reset(adapter, target, scope)` asks
the server which roles a reset must keep, and `reset_script_keeping(…, keep)`
leaves their `DROP ROLE` out — what `dbd reset --dry-run` prints when it has a
database, so the preview does not list a drop the run will not make. Underneath,
`script::build_reset_script_keeping`, `build_scoped_reset_script` and
`build_sqlite_reset_script` build the full, scoped and SQLite scripts.

## Documentation output

```rust
use std::path::Path;

let design = dbd_core::Design::from_config(path, "prod")?;

let graph = design.graph(None, None)?;                 // `dbd graph`
for layer in &graph.layers {
    println!("{}", layer.join(", "));
}

design.combine(Path::new("init.sql"), None)?;          // `dbd combine`

let mut model = dbd_core::schema_model::build(&design, None)?;
model.history = dbd_core::history::load(design.project_dir())?;
std::fs::write("schema.json", serde_json::to_string_pretty(&model)?)?;
```

`combine` writes each file as `apply` runs it, and `script::runnable_ddl(entity)`
is that DDL: the file's own text, with
`SET search_path TO <source.search_path>, public` ahead of it when the file
states no path of its own (one that does keeps its own, which wins). The file's
text is never rewritten — the path is a statement in front of it — so the script
`combine` writes creates each object where `apply` would.

`schema_model::build(design, scope)` is the model `dbd diagram` encodes (see
[The schema viewer](11-schema-viewer.md#the-model)). It returns
`Result<SchemaModel>`: a scope whose `deps: include` closure needs something the
scope excludes is refused, as every command refuses it. A file dbd could not
parse is not in the model; `Design::unparsed(&scope)` lists those entities — what
a document generated under that scope leaves out — so you can name them, as
`dbd diagram` and `dbd dbml` do. It does not read `snapshots/`, so attach the
changelog yourself. Under a scope, filter the
history with `history::load_scoped(project_dir, keep)`, where `keep(schema,
name)` says which tables and enums to keep. `scope::admits` is the predicate
`dbd diagram` passes: it answers from the scope's working set — closure
included — and, for a table the design no longer has, from the scope's own
`includes` and `excludes`:

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let scope = design.resolve_scope(Some("hub"), None)?;
let working_set = design.working_set(&scope)?;
let existing: std::collections::HashSet<String> = design.entities().iter().map(|e| e.name.clone()).collect();
let history = dbd_core::history::load_scoped(design.project_dir(), |schema, name| {
    dbd_core::scope::admits(&design.config().scopes, &scope, &working_set, &existing, &format!("{schema}.{name}"))
})?;
```

DBML comes from `dbml::generate_dbml(&DbmlParams)` and
`dbml::generate_all(&DbmlMultiParams)`: parameter structs carrying the entities
to document, the project name, the database type and the filters. Each returns
`DbmlDocument`s. `DbmlMultiParams.design_entities` is every entity in the design
— wider than `entities` when a scope narrowed them — and gives the stub table for
a referenced table the document leaves out its real column types:

```rust
let design = dbd_core::Design::from_config(path, "prod")?;
let scope = design.resolve_scope(Some("hub"), None)?;
let entities = design.scoped_entities(&scope)?;
let docs = dbd_core::dbml::generate_all(&dbd_core::dbml::DbmlMultiParams {
    entities: &entities,
    design_entities: design.entities(),
    project_name: &design.config().project.name,
    database_type: &design.config().source.dialect,
    project_note: design.config().project.note.as_deref(),
    docs: &design.config().dbml,
});
for doc in &docs {
    std::fs::write(&doc.file_name, &doc.content)?;
}
```

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

`emit_schema(design, target, scope)` is `dbd emit`: the script — with a comment
at every downgrade, and for SQL Server a `GO` after every statement — and the
same downgrades as a list. See [Other dialects](13-other-dialects.md#dbd-emit).

## Reverse-engineer a database

`dbd init --from-db` and `dbd merge` are built from the `reverse` module's
pieces, which an embedder can reuse. Three are new in 0.25, for what a reversed
project declares in `design.yaml` rather than as DDL files:

- `SchemaSelect.supabase` says the source is Supabase, so its platform schemas
  are left out unless `all` is set. It is off by default — on any other
  PostgreSQL, `auth`, `extensions`, `cron` and `net` are the project's own —
  and the CLI sets it from `looks_like_supabase(entities)` (both an `auth` and a
  `storage` schema). `left_out_schemas(db_schemas, &select)` returns each schema
  the selection drops with its `LeftOut` reason (`Platform`, `Excluded`,
  `NotListed`), so a caller can name every one.
- `Declared::from_entities(entities, selected)` collects the database's
  extensions (with their schema) and every table a foreign key reaches outside
  the selected schemas; `design_yaml_with(project, dialect, schemas, version,
  &declared)` writes them as the target's `extensions:` and as `external:`
  entries, since the scanner refuses `ddl/extension/` and `ddl/schema/` files.
- `doctor::detect_declared_kind_files(project_dir, config)` finds such files in
  an existing project, each with the name and schema its `CREATE` statement
  gives and whether `design.yaml` already declares it — what `dbd doctor`
  reports.

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
  (`Reads`, `Writes`, `Calls`, `Member`, `Uses`), a `schema_source` (stated,
  inferred or resolved) and an `unresolved` flag. `Uses` is a column's type or
  the sequence its default draws from — what makes a table depend on its enum
  and its sequence; like `Calls` it is soft, so one that resolves to nothing (a
  built-in type) is dropped. `reads()`, `writes()`, `calls()` and
  `refs_of(kind)` filter it; `refers()` yields each referenced name once and
  skips unresolved ones. An exhaustive `match` on `RefKind` needs a `Uses` arm.
- `Dialect::detect(sql)` guesses a dialect from the text and fails closed —
  SQL that every dialect accepts is `Unstated`.
- `references::resolve_references(&mut entities, &externals, &ignore)` resolves
  references once every file is read; the per-file parse touches no shared
  state, so a scan parallelises.
- `source_text::read_to_string(path)` decodes a file the way dbd does — UTF-8,
  or UTF-16 with a byte-order mark — rather than failing on what SSMS writes.
  `source_text::read_with_encoding(path)` also returns the `Encoding` it found
  (`Utf8`, `Utf8Bom`, `Utf16Le`, `Utf16Be`), and `Encoding::encode(text)` writes
  text back in it — what `dbd format` uses to leave a file's encoding as it was.

To learn about a directory without parsing it:

```rust
if let Some(survey) = dbd_core::survey(project_dir)? {
    println!("{} v{}: {} DDL file(s), read as {:?}", survey.project, survey.version, survey.ddl_files.len(), survey.parser);
}
```

## What the CLI adds

These checks are made by the `dbd` command, not by the `Design` methods. An
embedder that wants them makes them itself:

- **The scope guard.** `dbd apply`, `deploy`, `reconcile`, `reset`, `import`,
  `export`, `refresh` and `policies` refuse a database pinned to a scope other
  than the one the run asks for. `Design::apply`, `deploy` and `reconcile` do
  not check — and they re-pin the database to whatever scope they are given —
  and neither do `Design::reset`, `import_data` or `design::apply_policies`.
  (The env guard and the version guard are the library's own; see
  [Apply](#apply).)
- **The dry runs' questions.** Given a database, `dbd apply --dry-run` and
  `deploy --dry-run` check the scope pin and `check_version_guard`, and list
  `pending_migration_preview`; `policies --dry-run` checks the pin.
- **Reset's preview.** `Design::reset` checks its own guard (a database marked
  prod, or with applied migrations) unless forced; `dbd reset --dry-run` checks
  it too, given a database, through the public
  `Design::check_reset_guard(&marks)` (`marks` from `Design::database_marks`),
  so a preview refuses where the reset would.
  `reset_script` alone checks nothing.
- **The release check.** `dbd reconcile` refuses on a released project;
  `Design::reconcile` does not.
- **Grants on apply and reconcile.** `dbd apply` and `dbd reconcile` call
  `Design::apply_grants` after `Design::apply` and `Design::reconcile`;
  `Design::deploy` includes them.

```rust
use dbd_core::Design;

let design = Design::from_config(path, "prod")?;
let adapter = dbd_core::connect(database_url, &design.config().project.name).await?;
let scope = design.resolve_scope(None, None)?;

// What `dbd apply` checks before it writes. `true` would be --allow-scope-change.
// The marks are this project's record — or, after a `project.name` rename, the
// record of the name the database was built under.
let marks = design.database_marks(&*adapter).await?;
Design::check_scope_guard(&marks, &scope.name, false)?;

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
