use std::path::Path;

use anyhow::{Context, Result, bail};
use dbd_core::config::FormatConfig;
use dbd_core::reverse::{self, SchemaSelect};

/// Returns schemas that appear in `written` but not in `declared`, sorted and deduped.
fn undeclared_schemas(written: &[String], declared: &[String]) -> Vec<String> {
    let declared_set: std::collections::HashSet<&String> = declared.iter().collect();
    let mut result: Vec<String> = written.iter().filter(|s| !declared_set.contains(s)).cloned().collect();
    result.sort();
    result.dedup();
    result
}

/// The schemas `plan` writes a file into — the `<schema>` of every
/// `ddl/<kind>/<schema>/<name>.ddl` it holds. A schema-less kind (a role)
/// contributes none.
fn written_schemas(plan: &reverse::WritePlan) -> Vec<String> {
    plan.items
        .iter()
        .filter_map(|it| {
            let parts: Vec<&str> = it.path.iter().filter_map(|c| c.to_str()).collect();
            match parts.as_slice() {
                ["ddl", _kind, schema, _file] => Some(schema.to_string()),
                _ => None,
            }
        })
        .collect()
}

/// The tables the project at `project_dir` builds, as `schema.name`: every
/// `ddl/table/<schema>/<name>` file under a schema `config` lists — the files
/// a load of the project reads as tables. Read from the paths alone, so a file
/// that does not parse still counts as the project's.
fn managed_tables(
    project_dir: &Path,
    config: &dbd_core::config::DesignConfig,
) -> Result<std::collections::HashSet<String>> {
    let schemas = config.schema_names();
    Ok(dbd_core::scanner::scan_ddl(project_dir)
        .context("failed to scan the project's ddl/ folder")?
        .iter()
        .map(|path| dbd_core::Entity::from_file(path))
        .filter(|e| {
            e.entity_type == dbd_core::EntityType::Table
                && e.errors.is_empty()
                && e.schema.as_ref().is_some_and(|s| schemas.contains(s))
        })
        .map(|e| e.name)
        .collect())
}

/// The extensions among `entities` that `config`'s target does not list, as
/// `(name, schema)` with `schema` `None` for `public`.
fn undeclared_extensions(
    entities: &[dbd_core::Entity],
    config: &dbd_core::config::DesignConfig,
) -> Vec<(String, Option<String>)> {
    let listed: Vec<&str> = config
        .target
        .values()
        .next()
        .map(|t| t.extensions.iter().map(|e| e.name()).collect())
        .unwrap_or_default();
    reverse::Declared::from_entities(entities, &[])
        .extensions
        .into_iter()
        .filter(|(name, _)| !listed.contains(&name.as_str()))
        .collect()
}

/// Resolve the connection string from the single pre-resolved candidate.
///
/// The caller is responsible for combining explicit flag values with the
/// global `-d`/`$DATABASE_URL` value (which clap already reads from the
/// environment). This function simply bails with an actionable error when
/// nothing was resolved.
pub(crate) fn resolve_conn(candidate: Option<&str>) -> Result<String> {
    match candidate {
        Some(c) if !c.is_empty() => Ok(c.to_string()),
        _ => anyhow::bail!(
            "no connection given — pass a connection URL via the positional argument, \
             --from-db <url>, -d <url>, or $DATABASE_URL"
        ),
    }
}

/// The version-safety decision for a `merge` against a (possibly managed) DB.
///
/// Every `merge` that proceeds ends in a snapshot, so foreign and managed databases
/// behave identically — there are only two outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeDecision {
    /// Managed DB behind the project (D < Y) — refuse; the project is ahead of a stale DB.
    Refuse { db: u32, project: u32 },
    /// Foreign DB (no `_dbd_meta`) OR managed DB at/ahead of the project (D ≥ Y) —
    /// overwrite the introspected DDL (no `.bak`) + auto-snapshot the delta as a new version.
    Snapshot,
}

/// Decide what a `merge` should do given the DB's managed version (`None` = foreign)
/// and the project's `design.yaml` version. This is the single source of truth for
/// the gate — the handler routes through it so the tested logic is the real logic.
///
/// Only a managed DB strictly behind the project (`D < Y`) is refused; everything
/// else (a foreign DB, or a managed DB at/ahead of the project) snapshots.
pub(crate) fn merge_decision(managed: Option<u32>, project_version: u32) -> MergeDecision {
    match managed {
        Some(d) if d < project_version => MergeDecision::Refuse {
            db: d,
            project: project_version,
        },
        _ => MergeDecision::Snapshot,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_init_from_db(
    project_dir: &Path,
    // Explicit --from-db value (None = flag absent; Some("") = bare flag → fall back).
    from_db: Option<&str>,
    // Global -d / $DATABASE_URL, already resolved by clap.
    database_url: Option<&str>,
    env: &str,
    // The design.yaml path (already resolved by the dispatcher).
    config_path: &Path,
    name: Option<&str>,
    version: u32,
    sel: SchemaSelect,
    // Opt-in: also reverse-engineer cluster-global roles.
    roles: bool,
    dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!("design.yaml already exists here — use `dbd merge` to sync a DB into an existing project");
    }
    // Precedence: explicit --from-db URL > global -d/$DATABASE_URL.
    let candidate = match from_db {
        Some(s) if !s.is_empty() => Some(s),
        _ => database_url,
    };
    let conn = resolve_conn(candidate)?;

    // Resolve the project name used both for the connection (the per-project meta
    // read keys on it) and as the generated `design.yaml` `project.name`:
    // --name > db name parsed from the conn > "project".
    let project_name = name
        .map(String::from)
        .unwrap_or_else(|| db_name_from_conn(&conn).unwrap_or_else(|| "project".into()));

    // Version-safety gate: `init --from-db` is only for databases NOT managed by
    // dbd. Detect a `_dbd_meta` table in any schema and refuse if found.
    let adapter = dbd_core::connect(&conn, &project_name)
        .await
        .context("failed to connect to the database")?;
    if let Some(d) = adapter.reverse_managed_version().await? {
        bail!(
            "database is managed by dbd (version {d}); `dbd init --from-db` is only for \
             databases not managed by dbd — use `dbd merge` from the project's repository instead"
        );
    }

    // A fresh project has no design.yaml yet, so use the default format settings —
    // matching what `dbd format` would later apply with a freshly-scaffolded config.
    let mut entities = adapter.introspect().await.context("introspection failed")?;
    // Roles are cluster-global and gated behind the `--roles` opt-in: append them
    // only when asked (introspect_roles is NOT part of introspect()).
    if roles {
        entities.extend(adapter.introspect_roles().await.context("role introspection failed")?);
    }
    // Decided before `entities` is consumed: the platform schemas that identify
    // Supabase are usually filtered out of what gets written.
    let target = init_target(&conn, &entities);
    init_with_entities(
        project_dir,
        config_path,
        env,
        entities,
        &project_name,
        version,
        sel,
        dry_run,
        target,
        "init from database",
    )
}

/// Reverse-engineer a fresh project from a DBML file (no DB connection).
///
/// DBML is always a foreign source — there is no `_dbd_meta` to gate against, so
/// this skips the version-safety check entirely and otherwise shares the exact
/// init tail (write-plan + baseline snapshot) with `cmd_init_from_db` via
/// [`init_with_entities`].
#[allow(clippy::too_many_arguments)]
pub fn cmd_init_from_dbml(
    project_dir: &Path,
    dbml_path: &Path,
    env: &str,
    config_path: &Path,
    name: Option<&str>,
    // `postgres` or `supabase`: DBML names no platform, so the caller's
    // `--target` is the only source for it.
    target: &str,
    version: u32,
    sel: SchemaSelect,
    dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!(
            "design.yaml already exists here — use `dbd merge --from-dbml <file>` to sync a DBML file into an existing project"
        );
    }
    let entities = parse_dbml_file(dbml_path)?;
    // The project name is derived from --name or the file stem (there is no DB to
    // read it from).
    let project_name = name.map(String::from).unwrap_or_else(|| {
        dbml_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_else(|| "project".into())
    });
    init_with_entities(
        project_dir,
        config_path,
        env,
        entities,
        &project_name,
        version,
        sel,
        dry_run,
        target,
        "init from dbml",
    )
}

/// Read + parse a DBML file into entities, with file-level error context.
///
/// `dbml_path` is an operator-supplied CLI argument (the `--from-dbml <FILE>`
/// value), intentionally an arbitrary path — the user may keep their `.dbml`
/// anywhere — so it is not constrained to the project root, mirroring how
/// `--from-db <CONN>` accepts an arbitrary connection string.
fn parse_dbml_file(dbml_path: &Path) -> Result<Vec<dbd_core::Entity>> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path — operator-supplied CLI path, not external input
    let text = std::fs::read_to_string(dbml_path)
        .with_context(|| format!("failed to read DBML file {}", dbml_path.display()))?;
    dbd_core::dbml_parse::parse_dbml(&text)
        .with_context(|| format!("failed to parse DBML file {}", dbml_path.display()))
}

/// Pick the `design.yaml` target dialect from the connection URL scheme:
/// `sqlite:`/`file:` → `"sqlite"`, everything else → `"postgres"`. (The reverse
/// source's dialect comes from the connection, not the `--target` flag.)
fn dialect_for_conn(conn: &str) -> &'static str {
    if conn.starts_with("sqlite:") || conn.starts_with("file:") {
        "sqlite"
    } else {
        "postgres"
    }
}

/// The `design.yaml` target for a project reverse-engineered from `conn`, whose
/// introspection returned `entities`.
///
/// The URL scheme separates SQLite from PostgreSQL, but a Supabase database is
/// PostgreSQL behind a `postgres://` URL, so that alone named every Supabase
/// project's target `postgres` — losing its protected schemas, PostgREST
/// grants and exposed-`public` default. Its platform schemas identify it.
fn init_target(conn: &str, entities: &[dbd_core::Entity]) -> &'static str {
    match dialect_for_conn(conn) {
        "postgres" if reverse::looks_like_supabase(entities) => "supabase",
        dialect => dialect,
    }
}

/// Shared `init` tail for both the DB and DBML sources: build the write-plan,
/// write `design.yaml`, apply by overwriting, then emit a baseline snapshot at
/// `version`. The caller owns acquiring `entities` (introspection or DBML parse)
/// and any source-specific safety gating before calling this.
#[allow(clippy::too_many_arguments)]
fn init_with_entities(
    project_dir: &Path,
    config_path: &Path,
    env: &str,
    entities: Vec<dbd_core::Entity>,
    project_name: &str,
    version: u32,
    sel: SchemaSelect,
    dry_run: bool,
    dialect: &str,
    snapshot_label: &str,
) -> Result<()> {
    // Only on Supabase are its platform schemas the platform's rather than the
    // project's. The target says which: detected from the database's own
    // schemas, or — DBML names no platform — the caller's `--target`.
    let sel = SchemaSelect {
        supabase: dialect == "supabase",
        ..sel
    };
    run_plan(
        project_dir,
        config_path,
        entities,
        Some(project_name),
        Some(version),
        sel,
        dry_run,
        true,
        dialect,
        &FormatConfig::default(),
    )?;

    // Emit a baseline snapshot at `--version` so the new project is version-tracked
    // from the start (end state: snapshots/{version}.json + design.yaml
    // project.version = version). Skip entirely on --dry-run.
    if dry_run {
        println!("[dry-run] would create baseline snapshot v{version}");
        return Ok(());
    }

    // Reload from the freshly-written DDL so the snapshot reflects exactly what landed
    // on disk (and what `dbd apply` would later read), then write the baseline.
    let design = dbd_core::Design::from_config_with_dir(config_path, env, Some(project_dir))
        .context("failed to reload project after writing DDL")?;
    dbd_core::snapshot::create_baseline_snapshot(design.entities(), project_dir, config_path, snapshot_label, version)
        .context("failed to create baseline snapshot after writing DDL")?;
    println!("baseline snapshot v{version} created");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_merge(
    project_dir: &Path,
    // Explicit positional connection argument.
    conn: Option<&str>,
    // Global -d / $DATABASE_URL, already resolved by clap.
    database_url: Option<&str>,
    env: &str,
    // The design.yaml path (already resolved by the dispatcher).
    config_path: &Path,
    sel: SchemaSelect,
    // Opt-in: also reverse-engineer cluster-global roles.
    roles: bool,
    dry_run: bool,
) -> Result<()> {
    if !config_path.exists() {
        bail!("no design.yaml here — use `dbd init --from-db <conn>` to start a new project");
    }
    // Load the existing project's config once: its `project.name` keys the per-project
    // meta read AND the live connection; its `format` settings drive emit-side
    // formatting; its declared schemas drive the undeclared-schema warning below.
    let config = dbd_core::config::read(config_path).context("failed to read design.yaml")?;
    let project_name = config.project.name.clone();
    // Precedence: explicit positional conn > global -d/$DATABASE_URL.
    let candidate = conn.or(database_url);
    let conn = resolve_conn(candidate)?;

    // Connect with the REAL project name (not the literal "reverse") so the
    // `_dbd_meta` lookup keys on the right (project, env).
    let adapter = dbd_core::connect(&conn, &project_name)
        .await
        .context("failed to connect to the database")?;

    // Version-safety gate. Every merge that proceeds ends in a snapshot, so foreign
    // and managed databases take the same overwrite+snapshot path; only a managed DB
    // strictly behind the project is refused.
    let managed = adapter.reverse_managed_version().await?;
    // Deliberately NOT `config.project.version()`, which defaults to 1. This is
    // not asking "what version is this project" — it is choosing the floor for a
    // safety gate, and the permissive floor is the safe one here. A project that
    // declares no version has made no claim to be ahead of anything, so treating
    // it as 0 lets the merge proceed; treating it as 1 would refuse against a
    // managed database that reports 0 (its `_dbd_meta` exists but has no row for
    // this project), which is a normal first-merge situation.
    let project_version = config.project.version.unwrap_or(0);
    match merge_decision(managed, project_version) {
        MergeDecision::Refuse { db, project } => {
            bail!(
                "refusing to merge: database is at version {db} but this project is at version \
                 {project} — the project is ahead of a stale database. Bring the database up to \
                 date (`dbd apply`), or revert the project to v{db} via version control if you \
                 mean to discard those changes."
            );
        }
        MergeDecision::Snapshot => {
            let mut entities = adapter.introspect().await.context("introspection failed")?;
            // Roles are cluster-global and gated behind the `--roles` opt-in: append
            // them only when asked (introspect_roles is NOT part of introspect()).
            if roles {
                entities.extend(adapter.introspect_roles().await.context("role introspection failed")?);
            }
            // The database says what it is; a project already targeting
            // Supabase says so too.
            let sel = SchemaSelect {
                supabase: reverse::looks_like_supabase(&entities) || config.default_target() == Some("supabase"),
                ..sel
            };
            merge_snapshot(project_dir, config_path, env, &entities, sel, dry_run, &config)
        }
    }
}

/// Sync a DBML file into the current project (no DB connection).
///
/// DBML is always a foreign source — there is no `_dbd_meta` to gate against, so
/// this skips the version-safety decision entirely and goes straight to the
/// shared snapshot path with the parsed entities. Schema selection, orphan
/// reporting, the undeclared-schema warning, and `--dry-run` all behave exactly
/// as the DB merge path because both call [`merge_snapshot`].
pub fn cmd_merge_from_dbml(
    project_dir: &Path,
    dbml_path: &Path,
    env: &str,
    config_path: &Path,
    sel: SchemaSelect,
    dry_run: bool,
) -> Result<()> {
    if !config_path.exists() {
        bail!("no design.yaml here — use `dbd init --from-dbml <file>` to start a new project");
    }
    let config = dbd_core::config::read(config_path).context("failed to read design.yaml")?;
    let entities = parse_dbml_file(dbml_path)?;
    // DBML names no platform; the project's target does.
    let sel = SchemaSelect {
        supabase: config.default_target() == Some("supabase"),
        ..sel
    };
    merge_snapshot(project_dir, config_path, env, &entities, sel, dry_run, &config)
}

/// The unified `merge` apply path (foreign DB, or managed DB at/ahead of the project):
/// write the introspected DDL into the project, **overwriting drift with NO `.bak`**,
/// then auto-snapshot the delta as a new version. `--dry-run` previews the plan + the
/// snapshot version and writes nothing.
///
/// Reports orphans (never deleted), unsafe-path skips, and warns about schemas written
/// but not declared in `design.yaml` — the same surfacing the foreign path used.
///
/// # Non-transactionality (accepted limitation)
/// `apply_overwrite` writes all DDL files before the project is reloaded and
/// `create_snapshot` is called. If the reload or snapshot step fails after files
/// have been written, the project directory is in a partial-apply state: files are
/// already overwritten with no `.bak` and no snapshot exists. Recover via version
/// control (`git checkout -- .` or equivalent). This mirrors the non-transactionality
/// note on `apply_overwrite`.
fn merge_snapshot(
    project_dir: &Path,
    config_path: &Path,
    env: &str,
    entities: &[dbd_core::Entity],
    sel: SchemaSelect,
    dry_run: bool,
    config: &dbd_core::config::DesignConfig,
) -> Result<()> {
    let format = &config.format;
    // Select schemas + keep only entities under a selected schema (same logic as
    // the normal path).
    let (selected, kept) = select_and_keep(entities, &sel);
    report_left_out(entities, &sel);
    // A table the design declares `external:` is somebody else's — the
    // Supabase scaffold's `auth.users` is the usual one. Captured, it became a
    // project file `reset` would then `DROP … CASCADE`.
    let declared_externals: Vec<&str> = config.external.iter().map(|e| e.name.as_str()).collect();
    let (externals, kept): (Vec<_>, Vec<_>) = kept
        .into_iter()
        .partition(|e| e.entity_type == dbd_core::EntityType::Table && declared_externals.contains(&e.name.as_str()));
    for e in &externals {
        println!("  left out: table `{}` — declared under external:", e.name);
    }
    // Gate on entities, not schemas: a schema-less source (SQLite) has an empty
    // `selected` but non-empty `kept`.
    if kept.is_empty() {
        println!("Nothing to reverse-engineer (no entities after filtering).");
        return Ok(());
    }

    let plan = reverse::plan_from_entities(project_dir, &kept, &selected, format);

    // Warn about schemas written but not declared in design.yaml (spec line 67).
    // `merge` never edits config — it surfaces the gap so the user adds the schema.
    // Only the schemas files land in: a schema the database has but nothing
    // was written to — an empty `public`, or one holding only an external —
    // has nothing for the project to include.
    let declared = config.schema_names();
    for schema in undeclared_schemas(&written_schemas(&plan), &declared) {
        eprintln!("  warning: schema `{schema}` written but not listed in design.yaml; add it to include those files");
    }
    // A table a foreign key reaches outside the captured schemas is named, so
    // the user can declare it `external:` — unless it is declared already, or
    // the project builds it. A scoped DBML document stands a stub in for
    // both kinds alike, and a merge narrowed by `--schema` reaches the
    // project's own tables in the schemas it left out; an external is
    // somebody else's table, and declaring one the project builds would stop
    // the project building it.
    let managed = managed_tables(project_dir, config)?;
    for name in reverse::Declared::from_entities(&kept, &selected).externals {
        if !declared_externals.contains(&name.as_str()) && !managed.contains(&name) {
            eprintln!(
                "  warning: a foreign key references `{name}`, outside the captured schemas; declare it under external: in design.yaml"
            );
        }
    }
    // An extension is declared in design.yaml, never written as a file, and
    // `merge` never edits config — so one the target does not list is named,
    // as an undeclared schema is.
    for (name, schema) in undeclared_extensions(&kept, config) {
        let entry = match schema {
            None => format!("`{name}`"),
            Some(schema) => format!("`{{ name: {name}, schema: {schema} }}`"),
        };
        eprintln!(
            "  warning: extension `{name}` is installed in the database but not declared; add {entry} to the target's extensions: list in design.yaml"
        );
    }

    if dry_run {
        // Count straight off the plan — a dry-run writes nothing, so there's no
        // apply step to perform; the overwrite path has no conflict gate.
        let created = plan
            .items
            .iter()
            .filter(|i| i.action == reverse::FileAction::Create)
            .count();
        let unchanged = plan
            .items
            .iter()
            .filter(|i| i.action == reverse::FileAction::Skip)
            .count();
        let conflicts = plan
            .items
            .iter()
            .filter(|i| i.action == reverse::FileAction::Conflict)
            .count();
        println!(
            "[dry-run] {created} created · {unchanged} unchanged · {conflicts} conflict(s) (would overwrite, no .bak)"
        );
        for it in plan.items.iter().filter(|i| i.action == reverse::FileAction::Conflict) {
            println!("  conflict (differs from DB): {}", it.path.display());
        }
        for o in &plan.orphans {
            println!("  orphan (no DB entity): {}", o.display());
        }
        for label in &plan.skipped_unsafe {
            eprintln!("  warning: skipped unsafe path segment: {label}");
        }
        // Only promise a snapshot when there is actually something to write.
        // If every item is Skip the real run would print "already in sync — no
        // snapshot created", so the dry-run preview must match.
        //
        // Note: this is a *byte-level* check (the write-plan). The real run's
        // `create_snapshot` decides no-changes by a *semantic* diff of the parsed
        // entity model vs the latest snapshot, so a file that differs only
        // cosmetically (a byte Conflict that parses to an identical entity) is
        // previewed here as "would snapshot" yet the real run reports in-sync. The
        // overwrite still happens correctly either way — this is preview accuracy
        // only, not a data-safety gap.
        let has_changes = plan.items.iter().any(|i| i.action != reverse::FileAction::Skip);
        if has_changes {
            let n = dbd_core::snapshot::next_version(project_dir);
            println!("[dry-run] would capture changes as snapshot v{n}");
        } else {
            println!("[dry-run] already in sync — no snapshot needed");
        }
        return Ok(());
    }

    // Apply: overwrite Create + Conflict in place (no `.bak`), skip Skip.
    let report = reverse::apply_overwrite(project_dir, &plan)?;
    for o in &plan.orphans {
        println!("  orphan (no DB entity): {}", o.display());
    }
    for label in &plan.skipped_unsafe {
        eprintln!("  warning: skipped unsafe path segment: {label}");
    }
    let files_written = report.created + report.overwritten;

    // Reload the project from the freshly-written DDL, then snapshot the delta.
    // `create_snapshot` bumps design.yaml's version and writes snapshot+migration
    // files itself — do not duplicate that here.
    //
    // NOTE: both calls below happen *after* apply_overwrite has written all DDL
    // files. A failure here leaves the project in a partial-apply state (files
    // overwritten, no snapshot). Recover via version control.
    let design = dbd_core::Design::from_config_with_dir(config_path, env, Some(project_dir)).context(
        "failed to reload project after writing DDL — recover via version control if files were partially written",
    )?;
    let snap = dbd_core::snapshot::create_snapshot(design.entities(), project_dir, config_path, "merge from database")
        .context(
            "failed to create snapshot after writing DDL — recover via version control if files were partially written",
        )?;

    // `create_snapshot` handles the no-previous-snapshot (baseline) case, so a
    // project without prior snapshots still snapshots cleanly. A single result
    // flagged `no_changes` means the DDL was already in sync with the latest snapshot.
    if snap.snapshots.len() == 1 && snap.snapshots[0].no_changes {
        println!("{files_written} file(s) written — already in sync — no snapshot created");
    } else {
        let version = snap.snapshots.last().map(|s| s.snapshot.version).unwrap_or(0);
        println!("{files_written} file(s) written · snapshot v{version} created");
    }
    Ok(())
}

/// Select schemas from the introspected entities and keep only the entities under
/// a selected schema.
///
/// A schema entity "belongs to" itself (its own name), so it is filtered by name —
/// otherwise a denylisted/excluded schema (e.g. Supabase `extensions`) would still
/// get a stray `ddl/schema/<name>.ddl` file even though it's left out of
/// design.yaml. Truly schema-less entities (e.g. roles) have no owning schema and
/// are always kept.
fn select_and_keep(entities: &[dbd_core::Entity], sel: &SchemaSelect) -> (Vec<String>, Vec<dbd_core::Entity>) {
    let db_schemas = db_schemas(entities);
    let selected = reverse::select_schemas(&db_schemas, sel);
    let kept: Vec<_> = entities
        .iter()
        .filter(|e| {
            let owning = if e.entity_type == dbd_core::EntityType::Schema {
                Some(e.name.as_str())
            } else {
                e.schema.as_deref()
            };
            owning.is_none_or(|s| selected.iter().any(|sel| sel == s))
        })
        .cloned()
        .collect();
    (selected, kept)
}

/// Every schema `entities` come from — the schemas themselves included, so an
/// empty schema is the database's as much as one with tables in it.
fn db_schemas(entities: &[dbd_core::Entity]) -> Vec<String> {
    let mut s: Vec<String> = entities
        .iter()
        .filter_map(|e| match e.entity_type {
            dbd_core::EntityType::Schema => Some(e.name.clone()),
            // An external is somebody else's table; its schema is not the
            // database's to capture.
            dbd_core::EntityType::External => None,
            _ => e.schema.clone(),
        })
        .collect();
    s.sort();
    s.dedup();
    s
}

/// Name every schema `sel` leaves out of `entities`, with why, and every
/// extension that goes with one. Leaving them out is right — they are the
/// platform's, or the user asked — but leaving them out silently was not.
fn report_left_out(entities: &[dbd_core::Entity], sel: &SchemaSelect) {
    let left_out = reverse::left_out_schemas(&db_schemas(entities), sel);
    for (schema, why) in &left_out {
        let why = match why {
            reverse::LeftOut::Platform => "a Supabase platform schema (pass --all-schemas to include it)",
            reverse::LeftOut::Excluded => "excluded by --exclude-schema",
            reverse::LeftOut::NotListed => "not named by --schema",
        };
        println!("  left out: schema `{schema}` — {why}");
    }
    for ext in entities
        .iter()
        .filter(|e| e.entity_type == dbd_core::EntityType::Extension)
    {
        if let Some(schema) = ext.schema.as_deref()
            && left_out.iter().any(|(s, _)| s == schema)
        {
            println!(
                "  left out: extension `{}` — installed in schema `{schema}`, which is left out",
                ext.name
            );
        }
    }
}

/// Build the write-plan from already-introspected entities, write `design.yaml`
/// (init only), apply by **overwriting** in place (no `.bak`), and report. The caller
/// owns the connection + introspection (so the version-safety gate can act on the
/// adapter before any writes).
///
/// This is the `init --from-db` path. A fresh project directory has no managed files
/// yet, so every item is a `Create`; `apply_overwrite` handles that the same as the
/// unified `merge` path (and harmlessly clobbers any pre-existing drift, since the
/// baseline snapshot + version control are the record).
#[allow(clippy::too_many_arguments)]
fn run_plan(
    project_dir: &Path,
    config_path: &Path,
    entities: Vec<dbd_core::Entity>,
    name: Option<&str>,
    version: Option<u32>,
    sel: SchemaSelect,
    dry_run: bool,
    write_config: bool,
    dialect: &str,
    format: &FormatConfig,
) -> Result<Vec<String>> {
    // 1. select schemas + keep only entities under a selected schema
    let (selected, kept) = select_and_keep(&entities, &sel);
    report_left_out(&entities, &sel);
    // Bail only when there's genuinely nothing to write. A schema-less source
    // (SQLite) yields entities with no schema → `selected` is empty but `kept`
    // is not, so gate on `kept`, not `selected`.
    if kept.is_empty() {
        println!("Nothing to reverse-engineer (no entities after filtering).");
        return Ok(vec![]);
    }

    // 2. build write-plan (emitted DDL is formatted so it matches `dbd format`)
    let plan = reverse::plan_from_entities(project_dir, &kept, &selected, format);

    // 3. write design.yaml (init only; only if absent; skip on dry-run)
    // Extensions go in the target's `extensions:` list — the project format
    // has no DDL file for one (see `reverse::MANAGED_KINDS`) — and a table a
    // foreign key reaches outside the captured schemas under `external:`.
    // Named on a dry run too: it is part of the plan.
    let declared = write_config.then(|| reverse::Declared::from_entities(&kept, &selected));
    for name in declared.iter().flat_map(|d| &d.externals) {
        println!("  external: {name} — a foreign key references it, outside the captured schemas");
    }
    if let Some(declared) = declared.filter(|_| !dry_run) {
        let project = name.map(String::from).unwrap_or_else(|| "project".into());
        let yaml = reverse::design_yaml_with(&project, dialect, &selected, version.unwrap_or(1), &declared);
        // Write to the resolved config path (project_dir/<--config>), matching what the
        // baseline-snapshot reload reads — not a hardcoded "design.yaml" (which would
        // diverge under `--config custom.yaml`).
        std::fs::write(config_path, yaml).context("failed to write design.yaml")?;
    }

    // 4. apply + report
    if dry_run {
        let created = plan
            .items
            .iter()
            .filter(|i| i.action == reverse::FileAction::Create)
            .count();
        let unchanged = plan
            .items
            .iter()
            .filter(|i| i.action == reverse::FileAction::Skip)
            .count();
        println!(
            "[dry-run] {created} created · {unchanged} unchanged · {} orphan(s) left as-is",
            plan.orphans.len()
        );
    } else {
        let report = reverse::apply_overwrite(project_dir, &plan)?;
        let files_written = report.created + report.overwritten;
        println!(
            "{files_written} file(s) written · {} unchanged · {} orphan(s) left as-is",
            report.unchanged, report.orphans
        );
    }
    for o in &plan.orphans {
        println!("  orphan (no DB entity): {}", o.display());
    }
    for label in &plan.skipped_unsafe {
        eprintln!("  warning: skipped unsafe path segment: {label}");
    }
    Ok(selected)
}

/// Parse the database name out of a connection string for the default project name.
/// Best-effort heuristic: a URL with a trailing slash and no db name (e.g. `postgres://host/`)
/// yields `None` and falls back to `"project"`; `--name` always overrides this entirely.
fn db_name_from_conn(conn: &str) -> Option<String> {
    let after = conn.rsplit('/').next()?;
    let db = after.split(['?', '#']).next()?;
    if db.is_empty() { None } else { Some(db.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// All written schemas are declared — no warnings.
    #[test]
    fn undeclared_schemas_all_declared_returns_empty() {
        let written = strs(&["app", "config"]);
        let declared = strs(&["app", "config", "staging"]);
        assert_eq!(undeclared_schemas(&written, &declared), Vec::<String>::new());
    }

    /// One written schema is missing from declared — returned.
    #[test]
    fn undeclared_schemas_one_missing() {
        let written = strs(&["app", "new_schema"]);
        let declared = strs(&["app"]);
        assert_eq!(undeclared_schemas(&written, &declared), strs(&["new_schema"]));
    }

    /// Duplicates in `written` are deduped in the result.
    #[test]
    fn undeclared_schemas_deduplicates() {
        let written = strs(&["app", "app", "new_schema", "new_schema"]);
        let declared = strs(&["app"]);
        assert_eq!(undeclared_schemas(&written, &declared), strs(&["new_schema"]));
    }

    /// Result is sorted alphabetically.
    #[test]
    fn undeclared_schemas_sorted() {
        let written = strs(&["zzz", "aaa", "mmm"]);
        let declared = strs(&[]);
        assert_eq!(undeclared_schemas(&written, &declared), strs(&["aaa", "mmm", "zzz"]));
    }

    /// Empty written — empty result regardless of declared.
    #[test]
    fn undeclared_schemas_empty_written() {
        let written = strs(&[]);
        let declared = strs(&["app"]);
        assert_eq!(undeclared_schemas(&written, &declared), Vec::<String>::new());
    }

    // ── merge_decision ────────────────────────────────────────────────────────

    /// Foreign DB (no `_dbd_meta`) → snapshot (unified overwrite + auto-snapshot path).
    #[test]
    fn merge_decision_foreign_snapshots() {
        assert_eq!(merge_decision(None, 5), MergeDecision::Snapshot);
    }

    /// Managed DB behind the project (D < Y) → refuse.
    #[test]
    fn merge_decision_refuses_when_db_behind() {
        assert_eq!(merge_decision(Some(2), 5), MergeDecision::Refuse { db: 2, project: 5 });
    }

    /// Managed DB at the project version (D == Y) → snapshot.
    #[test]
    fn merge_decision_snapshots_when_equal() {
        assert_eq!(merge_decision(Some(5), 5), MergeDecision::Snapshot);
    }

    /// Managed DB ahead of the project (D > Y) → snapshot.
    #[test]
    fn merge_decision_snapshots_when_db_ahead() {
        assert_eq!(merge_decision(Some(7), 5), MergeDecision::Snapshot);
    }

    /// Both at zero (unset project version) → snapshot.
    /// This gate's floor for an undeclared version is 0, and deliberately not
    /// `ProjectConfig::version`'s 1. The two answer different questions:
    /// "what version is this project" (1 — a project that exists is at v1) and
    /// "what floor makes this safety gate permissive" (0).
    ///
    /// Collapsing them would make a first merge refuse. A managed database with
    /// no row for this project reports `Some(0)`, so a v1 project reads as ahead
    /// of it and `merge_decision` refuses — blocking the ordinary case of
    /// merging into a database this project has not applied to yet.
    #[test]
    fn the_merge_gate_floors_an_undeclared_version_at_zero_not_one() {
        let cfg: dbd_core::config::ProjectConfig = serde_yaml::from_str("name: p").unwrap();
        assert_eq!(cfg.version(), 1, "the project's own answer is 1");

        // What this gate uses instead, and why it must.
        assert_eq!(
            merge_decision(Some(0), cfg.version.unwrap_or(0)),
            MergeDecision::Snapshot
        );
        assert_eq!(
            merge_decision(Some(0), cfg.version()),
            MergeDecision::Refuse { db: 0, project: 1 },
            "using the project-level default here would refuse an ordinary first merge"
        );
    }

    #[test]
    fn merge_decision_snapshots_at_zero() {
        assert_eq!(merge_decision(Some(0), 0), MergeDecision::Snapshot);
    }

    // ── resolve_conn ──────────────────────────────────────────────────────────

    /// Explicit non-empty value is returned as-is.
    /// Supabase is PostgreSQL with platform schemas of its own, and its
    /// connection URL is an ordinary `postgres://` one — so the URL scheme
    /// named every Supabase project's target `postgres`. The `auth` and
    /// `storage` schemas are what identify it.
    #[test]
    fn a_database_with_supabases_schemas_gets_a_supabase_target() {
        let supabase: Vec<dbd_core::Entity> = ["public", "auth", "storage"]
            .iter()
            .map(|s| dbd_core::Entity::schema(s))
            .collect();
        assert_eq!(
            init_target("postgres://db.example.supabase.co/postgres", &supabase),
            "supabase"
        );

        let plain: Vec<dbd_core::Entity> = ["public", "auth"].iter().map(|s| dbd_core::Entity::schema(s)).collect();
        assert_eq!(
            init_target("postgres://localhost/app", &plain),
            "postgres",
            "an `auth` schema alone is not Supabase"
        );
        assert_eq!(init_target("sqlite://./app.db", &supabase), "sqlite");
    }

    #[test]
    fn resolve_conn_explicit_wins() {
        let got = resolve_conn(Some("postgres://explicit/db")).unwrap();
        assert_eq!(got, "postgres://explicit/db");
    }

    /// None candidate bails with an actionable message.
    #[test]
    fn resolve_conn_none_bails() {
        let err = resolve_conn(None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("-d") || msg.contains("$DATABASE_URL"),
            "message should mention -d or $DATABASE_URL: {msg}"
        );
    }

    /// Empty string candidate bails (same as None — sentinel for bare --from-db with no fallback).
    #[test]
    fn resolve_conn_empty_bails() {
        let err = resolve_conn(Some("")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("-d") || msg.contains("$DATABASE_URL"),
            "message should mention -d or $DATABASE_URL: {msg}"
        );
    }

    // ── db_name_from_conn ─────────────────────────────────────────────────────

    /// Standard postgres URL → database name extracted.
    #[test]
    fn db_name_from_conn_standard_url() {
        assert_eq!(db_name_from_conn("postgres://user:pass@host/mydb"), Some("mydb".into()));
    }

    /// URL with query string → query stripped.
    #[test]
    fn db_name_from_conn_strips_query() {
        assert_eq!(
            db_name_from_conn("postgres://host/mydb?sslmode=require"),
            Some("mydb".into())
        );
    }

    /// URL with trailing slash and no db name → None (falls back to "project" at call site).
    #[test]
    fn db_name_from_conn_trailing_slash_returns_none() {
        assert_eq!(db_name_from_conn("postgres://host/"), None);
    }

    // ── DBML init/merge paths (no DB connection) ──────────────────────────────

    use crate::commands::testutil;

    /// Write a minimal, schema-qualified DBML file and return its path.
    fn write_dbml(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("schema.dbml");
        std::fs::write(
            &p,
            "Table \"config\".\"widgets\" {\n  \"id\" int [pk]\n  \"name\" text\n}\n",
        )
        .unwrap();
        p
    }

    fn all_schemas() -> SchemaSelect {
        SchemaSelect {
            all: true,
            ..Default::default()
        }
    }

    /// A stub table in a DBML document `dbd dbml` wrote is not the project's: it
    /// stands in for a table a foreign key reaches that the document does not
    /// define. `init --from-dbml` wrote it as a managed table — its referenced
    /// columns and nothing else — and added its schema to `schemas:`, so the
    /// next apply created a one-column copy of somebody else's table. It is
    /// declared `external:` instead, whether its schema holds managed tables
    /// or not.
    #[test]
    fn init_from_dbml_declares_a_stub_external_instead_of_building_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dbml = tmp.path().join("schema.dbml");
        std::fs::write(
            &dbml,
            "Table \"app\".\"profiles\" {\n  \"id\" int [pk]\n  \"account_id\" uuid\n  \"lookup_id\" int\n}\n\n\
             Ref: \"app\".\"profiles\".\"account_id\" > \"billing\".\"accounts\".\"id\"\n\
             Ref: \"app\".\"profiles\".\"lookup_id\" > \"app\".\"lookups\".\"id\"\n\n\
             Table \"billing\".\"accounts\" {\n  \"id\" uuid\n\n  indexes {\n    id [unique]\n  }\n\n\
             \x20 Note: 'External entity — managed outside this project'\n}\n\n\
             Table \"app\".\"lookups\" {\n  \"id\" int\n\n  indexes {\n    id [unique]\n  }\n\n\
             \x20 Note: 'Defined in this project, outside this document — only the referenced columns are shown'\n}\n",
        )
        .unwrap();
        let cfg = tmp.path().join("design.yaml");
        cmd_init_from_dbml(
            tmp.path(),
            &dbml,
            "dev",
            &cfg,
            Some("proj"),
            "postgres",
            1,
            all_schemas(),
            false,
        )
        .unwrap();

        assert!(tmp.path().join("ddl/table/app/profiles.ddl").exists());
        for stub in ["ddl/table/billing/accounts.ddl", "ddl/table/app/lookups.ddl"] {
            assert!(
                !tmp.path().join(stub).exists(),
                "{stub} is a stub, not a table to build"
            );
        }
        let config = dbd_core::config::read(&cfg).unwrap();
        let mut externals: Vec<&str> = config.external.iter().map(|e| e.name.as_str()).collect();
        externals.sort();
        assert_eq!(externals, ["app.lookups", "billing.accounts"]);
        assert!(
            !config.schema_names().iter().any(|s| s == "billing"),
            "a schema only a stub lives in is not the project's: {:?}",
            config.schema_names()
        );
    }

    /// The introspected extensions go where the project format declares them —
    /// the target's `extensions:` list, with the schema when it is not
    /// `public` — and nowhere under `ddl/`, so the project loads with no error.
    /// They were written to `ddl/extension/<name>.ddl`, which the scanner does
    /// not read, so `inspect` and `apply` refused every reversed project whose
    /// database had an extension.
    #[test]
    fn init_declares_the_databases_extensions_in_design_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("design.yaml");
        let mut citext = dbd_core::Entity::new(dbd_core::EntityType::Extension, "citext");
        citext.schema = Some("public".into());
        let mut pgcrypto = dbd_core::Entity::new(dbd_core::EntityType::Extension, "pgcrypto");
        pgcrypto.schema = Some("app".into());
        let mut table = dbd_core::Entity::new(dbd_core::EntityType::Table, "app.t");
        table.table_def = Some(dbd_core::entity::TableDef {
            columns: vec![dbd_core::entity::ColumnDef {
                name: "id".into(),
                data_type: "integer".into(),
                nullable: false,
                default_value: None,
                is_pk: false,
                is_unique: false,
                identity: None,
                generated: None,
                comment: None,
                inline_fk: None,
            }],
            constraints: vec![],
            indexes: vec![],
            comments: Default::default(),
        });
        let entities = vec![
            dbd_core::Entity::schema("public"),
            dbd_core::Entity::schema("app"),
            citext,
            pgcrypto,
            table,
        ];

        init_with_entities(
            tmp.path(),
            &cfg,
            "dev",
            entities,
            "proj",
            1,
            all_schemas(),
            false,
            "postgres",
            "init from database",
        )
        .unwrap();

        assert!(!tmp.path().join("ddl/extension").exists());
        assert!(!tmp.path().join("ddl/schema").exists());
        let config = dbd_core::config::read(&cfg).unwrap();
        let target = config.target.get("postgres").expect("a postgres target");
        let declared: Vec<(&str, Option<&str>)> = target.extensions.iter().map(|e| (e.name(), e.schema())).collect();
        assert_eq!(declared, vec![("citext", None), ("pgcrypto", Some("app"))]);

        let design = dbd_core::Design::from_config_with_dir(&cfg, "dev", Some(tmp.path())).unwrap();
        let errors: Vec<&String> = design.entities().iter().flat_map(|e| &e.errors).collect();
        assert!(errors.is_empty(), "the reversed project must load clean: {errors:?}");
    }

    fn table(name: &str, fk_to: Option<(&str, &str)>) -> dbd_core::Entity {
        use dbd_core::entity::{ColumnDef, ForeignKey, TableConstraint, TableDef};
        let mut t = dbd_core::Entity::new(dbd_core::EntityType::Table, name);
        let constraints = fk_to
            .map(|(schema, table)| {
                vec![TableConstraint::ForeignKey(ForeignKey {
                    name: Some(format!("{}_fk", t.name.replace('.', "_"))),
                    columns: vec!["id".into()],
                    ref_schema: Some(schema.into()),
                    ref_table: table.into(),
                    ref_columns: vec!["id".into()],
                    on_delete: None,
                    on_update: None,
                    ref_schema_source: Default::default(),
                })]
            })
            .unwrap_or_default();
        t.table_def = Some(TableDef {
            columns: vec![ColumnDef {
                name: "id".into(),
                data_type: "uuid".into(),
                nullable: false,
                default_value: None,
                is_pk: false,
                is_unique: false,
                identity: None,
                generated: None,
                comment: None,
                inline_fk: None,
            }],
            constraints,
            indexes: vec![],
            comments: Default::default(),
        });
        t
    }

    /// A database with `auth`, `extensions`, `cron` and `net` schemas — and
    /// `storage` too when `supabase`, which is what identifies Supabase.
    fn platform_named_schemas(supabase: bool) -> Vec<dbd_core::Entity> {
        let mut pgcrypto = dbd_core::Entity::new(dbd_core::EntityType::Extension, "pgcrypto");
        pgcrypto.schema = Some("extensions".into());
        let mut entities: Vec<dbd_core::Entity> = ["public", "app", "auth", "extensions", "cron", "net"]
            .iter()
            .map(|s| dbd_core::Entity::schema(s))
            .collect();
        entities.extend([
            pgcrypto,
            table("auth.users", None),
            table("app.profiles", Some(("auth", "users"))),
            table("cron.jobs", None),
            table("net.requests", None),
        ]);
        if supabase {
            entities.push(dbd_core::Entity::schema("storage"));
            entities.push(table("storage.objects", None));
        }
        entities
    }

    fn init_into(dir: &std::path::Path, entities: Vec<dbd_core::Entity>, sel: SchemaSelect, dialect: &str) {
        init_with_entities(
            dir,
            &dir.join("design.yaml"),
            "dev",
            entities,
            "proj",
            1,
            sel,
            false,
            dialect,
            "init from database",
        )
        .unwrap();
    }

    /// `init --from-db` applied Supabase's platform denylist to every
    /// database, so a plain one lost its `auth`, `extensions`, `cron` and
    /// `net` schemas — pgcrypto with them — without a word.
    #[test]
    fn init_from_a_plain_database_keeps_the_schemas_supabase_reserves_for_itself() {
        let tmp = tempfile::tempdir().unwrap();
        init_into(
            tmp.path(),
            platform_named_schemas(false),
            SchemaSelect::default(),
            "postgres",
        );

        let config = dbd_core::config::read(&tmp.path().join("design.yaml")).unwrap();
        let schemas = config.schema_names();
        for kept in ["auth", "extensions", "cron", "net", "app"] {
            assert!(
                schemas.contains(&kept.to_string()),
                "`{kept}` must be kept: {schemas:?}"
            );
        }
        let extensions: Vec<&str> = config.target["postgres"].extensions.iter().map(|e| e.name()).collect();
        assert_eq!(extensions, vec!["pgcrypto"]);
        assert!(tmp.path().join("ddl/table/auth/users.ddl").exists());
    }

    /// A foreign key into a schema the project leaves out points at a table
    /// the project does not build. It is declared `external:`, as the
    /// Supabase scaffold declares `auth.users`, so the reference resolves —
    /// whether the schema was left out by `--exclude-schema` or by Supabase's
    /// platform denylist.
    #[test]
    fn init_declares_an_external_for_a_table_a_foreign_key_reaches_outside_the_project() {
        let excluded = tempfile::tempdir().unwrap();
        init_into(
            excluded.path(),
            platform_named_schemas(false),
            SchemaSelect {
                exclude: vec!["auth".into()],
                ..Default::default()
            },
            "postgres",
        );
        let supabase = tempfile::tempdir().unwrap();
        init_into(
            supabase.path(),
            platform_named_schemas(true),
            SchemaSelect::default(),
            "supabase",
        );

        for dir in [excluded.path(), supabase.path()] {
            let config = dbd_core::config::read(&dir.join("design.yaml")).unwrap();
            let externals: Vec<&str> = config.external.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(externals, vec!["auth.users"], "in {}", dir.display());
            assert!(!config.schema_names().contains(&"auth".to_string()));
        }
    }

    /// `init --from-dbml` on an empty dir scaffolds design.yaml + a baseline snapshot.
    #[test]
    fn init_from_dbml_creates_project() {
        let tmp = tempfile::tempdir().unwrap();
        let dbml = write_dbml(tmp.path());
        let cfg = tmp.path().join("design.yaml");
        cmd_init_from_dbml(
            tmp.path(),
            &dbml,
            "dev",
            &cfg,
            Some("proj"),
            "postgres",
            1,
            all_schemas(),
            false,
        )
        .unwrap();
        assert!(cfg.exists());
    }

    /// `--dry-run` previews and writes nothing.
    #[test]
    fn init_from_dbml_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dbml = write_dbml(tmp.path());
        let cfg = tmp.path().join("design.yaml");
        cmd_init_from_dbml(
            tmp.path(),
            &dbml,
            "dev",
            &cfg,
            Some("proj"),
            "postgres",
            1,
            all_schemas(),
            true,
        )
        .unwrap();
        assert!(!cfg.exists(), "dry-run must not write design.yaml");
    }

    /// `init` refuses when a project already exists (points at `merge`).
    #[test]
    fn init_from_dbml_refuses_when_project_exists() {
        let proj = testutil::copy_fixture_project();
        let dbml = write_dbml(proj.path());
        let cfg = proj.path().join("design.yaml");
        let err = cmd_init_from_dbml(
            proj.path(),
            &dbml,
            "dev",
            &cfg,
            None,
            "postgres",
            1,
            all_schemas(),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
    }

    /// `merge --from-dbml` refuses when there's no project (points at `init`).
    #[test]
    fn merge_from_dbml_refuses_without_project() {
        let tmp = tempfile::tempdir().unwrap();
        let dbml = write_dbml(tmp.path());
        let cfg = tmp.path().join("design.yaml"); // does not exist
        let err = cmd_merge_from_dbml(tmp.path(), &dbml, "dev", &cfg, all_schemas(), false).unwrap_err();
        assert!(err.to_string().contains("no design.yaml"), "got: {err}");
    }

    /// `merge --from-dbml --dry-run` previews the plan against an existing project.
    #[test]
    fn merge_from_dbml_dry_run_previews() {
        let proj = testutil::copy_fixture_project();
        let dbml = write_dbml(proj.path());
        let cfg = proj.path().join("design.yaml");
        cmd_merge_from_dbml(proj.path(), &dbml, "dev", &cfg, all_schemas(), true).unwrap();
    }

    /// `merge --from-dbml` applies (overwrite + snapshot) into an existing project.
    #[test]
    fn merge_from_dbml_applies_into_project() {
        let proj = testutil::copy_fixture_project();
        let dbml = write_dbml(proj.path());
        let cfg = proj.path().join("design.yaml");
        cmd_merge_from_dbml(proj.path(), &dbml, "dev", &cfg, all_schemas(), false).unwrap();
    }
}
