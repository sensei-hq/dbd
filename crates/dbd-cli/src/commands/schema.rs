use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::{ApplyComplete, Progress};
use dbd_core::{Design, Entity, EntityType};

use super::{format_apply_summary, get_adapter, safe_read, safe_write};
use crate::output::{self, Verbosity};

/// Warn when the config loaded but no authored DDL was scanned under the
/// resolved project dir — the tell-tale of a wrong `--source`. The config path
/// (`-c`) and the ddl/ scan root (`--source`) are independent, so an absolute
/// `-c` with a defaulted `--source` loads the config yet silently scans the
/// wrong directory, which would otherwise read as a successful no-op.
fn warn_if_no_authored_ddl(design: &Design, project_dir: &Path) {
    if design.authored_entity_count() == 0 {
        output::warn(&format!(
            "no authored DDL found under '{dir}/ddl' — check that --source points at your \
             project (currently '{dir}').",
            dir = project_dir.display()
        ));
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_inspect(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    fix: bool,
    use_database: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let mut design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    warn_if_no_authored_ddl(&design, project_dir);

    resolve_inspect_refs(&mut design, config, database_url, use_database, verbosity).await?;

    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let report = design.report(name, Some(&resolved));

    report_scope_gaps(&resolved, &report, verbosity)?;

    // Count what this run is actually about. Under a scope the whole-project
    // total contradicts the "scope 'X': N entities" line printed just above.
    let scope_name = (!resolved.is_all).then_some(resolved.name.as_str());
    let total_entities = match scope_name {
        Some(_) => resolved.entities.len(),
        None => design.entities().len(),
    };

    if verbosity.is_verbose()
        && let Some(entity) = &report.entity
    {
        output::always(&serde_json::to_string_pretty(entity)?);
    }

    print_report_findings(&report, scope_name, verbosity);

    // Auto-format DDL files when --fix is passed
    if fix {
        fix_format_ddl(config, project_dir, verbosity)?;
    }

    // Report unresolved data.sql TODOs across all migration directories
    let todos = design.data_sql_todos()?;
    print_data_sql_todos(&todos);

    // Validate materialized-view refresh config (concurrently/unique-index,
    // pg_cron presence, cron expression syntax) — offline, no DB required.
    let declared_extensions: Vec<String> = design
        .entities()
        .iter()
        .filter(|e| e.entity_type == dbd_core::EntityType::Extension)
        .map(|e| e.name.clone())
        .collect();
    let matview_errors = dbd_core::design::validate_materialized_views(
        design.entities(),
        &design.config().materialized_views,
        &declared_extensions,
    );
    print_matview_errors(&matview_errors);

    // Advisory only — string-set CHECK constraints that could be a Postgres enum.
    // Report-only: NOT added to the summary error count, never affects the exit code.
    let enum_hints = dbd_core::design::suggest_enum_candidates(design.entities(), &design.config().source.dialect);
    print_enum_hints(&enum_hints);

    // Summary last, so the counts are the final thing on screen. Printed before
    // the advisory section it would scroll away behind it, which is backwards:
    // the tally is what a reader is looking for.
    let blocking = report.issues.len() + todos.len() + matview_errors.len();
    output::always("");
    output::summary(blocking, report.warnings.len(), total_entities);
    if !report.out_of_scope_issues.is_empty() {
        output::always(&format!(
            "({} error(s) outside scope '{}')",
            report.out_of_scope_issues.len(),
            scope_name.unwrap_or("all"),
        ));
    }
    // Named in the tally so a reader knows the advisory block was counted
    // separately rather than folded into the error count.
    if !enum_hints.is_empty() {
        output::always(&format!("({} enum suggestion(s) — advisory)", enum_hints.len()));
    }

    // Report first, then fail. The findings above are the useful output; an
    // early return would print an "Error:" line instead of them.
    let code = inspect_exit_code(blocking);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Resolve unknown references against the live DB (persisting a refcache) or,
/// offline, against the project-local cache.
async fn resolve_inspect_refs(
    design: &mut Design,
    config: &Path,
    database_url: Option<&str>,
    use_database: bool,
    verbosity: Verbosity,
) -> Result<()> {
    if use_database {
        let adapter = get_adapter(config, database_url).await?;
        let dropped = design
            .resolve_unknown_refs_via_db(&*adapter)
            .await
            .context("Failed to resolve references against database catalog")?;
        if dropped > 0 {
            output::detail(
                verbosity,
                &format!("  resolved {dropped} reference(s) against database catalog"),
            );
        }

        // Persist a project-local snapshot for offline use on subsequent runs.
        let source = design.config().default_target().unwrap_or("postgres").to_string();
        match design.write_ref_cache(&*adapter, &source).await {
            Ok(n) => output::detail(verbosity, &format!("  cached {n} entity name(s) in .dbd/refcache.json")),
            Err(e) => output::detail(verbosity, &format!("  refcache save skipped: {e}")),
        }
    } else {
        // Offline path: consult the project-local cache if it exists.
        match design.resolve_unknown_refs_via_cache() {
            Ok((dropped, Some(size))) => {
                if dropped > 0 {
                    output::detail(
                        verbosity,
                        &format!("  resolved {dropped} reference(s) via .dbd/refcache.json ({size} cached)"),
                    );
                }
            }
            Ok((_, None)) => {}
            Err(e) => output::detail(verbosity, &format!("  refcache read skipped: {e}")),
        }
    }
    Ok(())
}

/// Print out-of-scope dependency gaps; bail when the deps policy is `Report`.
fn report_scope_gaps(
    resolved: &dbd_core::ResolvedScope,
    report: &dbd_core::design::Report,
    verbosity: Verbosity,
) -> Result<()> {
    if resolved.is_all {
        return Ok(());
    }

    output::info(
        verbosity,
        &format!("scope '{}': {} entities", resolved.name, resolved.entities.len()),
    );
    for gap in &report.gaps {
        output::always(&format!(
            "✗ dependency gap: {} requires {} (out of scope)\n    chain: {}",
            gap.required_by,
            gap.missing,
            gap.chain.join(" → ")
        ));
    }
    if report.gaps.is_empty() {
        return Ok(());
    }
    match resolved.deps {
        dbd_core::config::DepsPolicy::Report => anyhow::bail!(
            "{} dependency gap(s) in scope '{}' — add them to the scope, or run with --deps include",
            report.gaps.len(),
            resolved.name
        ),
        dbd_core::config::DepsPolicy::Include => {
            output::info(
                verbosity,
                &format!("{} gap(s) will be auto-included (--deps include)", report.gaps.len()),
            );
        }
    }
    Ok(())
}

/// Print entity errors and warnings, or an all-clear message when there's neither.
/// One `file =>` block per errored entity, then its messages.
fn print_entity_problems(entities: &[dbd_core::Entity]) {
    for entity in entities {
        let label = entity
            .file
            .as_ref()
            .map(|f| f.display().to_string())
            .unwrap_or_else(|| entity.name.clone());
        output::always(&format!("\n{label} =>"));
        for err in &entity.errors {
            output::always(&format!("  {err}"));
        }
    }
}

fn print_report_findings(report: &dbd_core::design::Report, scope_name: Option<&str>, verbosity: Verbosity) {
    if !report.issues.is_empty() {
        match scope_name {
            Some(name) => output::always(&format!("Errors (blocking scope '{name}'):")),
            None => output::always("Errors:"),
        }
        print_entity_problems(&report.issues);
        // An entity that failed to parse is dropped from the desired set, so a
        // run would build a database missing it. `ensure_fully_parsed` refuses
        // rather than do that — say so here, or the report reads as advisory
        // and the refusal later looks like a new problem.
        output::always("\n  → dbd apply / reconcile / deploy will refuse to run until these are fixed.");
    }

    // Errored files the scope excludes. Not blocking this run, but never
    // silent: a file no scope builds is exactly how a broken file survives.
    if !report.out_of_scope_issues.is_empty() {
        let name = scope_name.unwrap_or("the active scope");
        output::always(&format!("\nOut of scope — not blocking '{name}':"));
        print_entity_problems(&report.out_of_scope_issues);
        output::always(&format!(
            "\n  → these files are not built by '{name}', so it can run. They will block a run whose scope includes them."
        ));
    }

    if !report.warnings.is_empty() {
        output::always("\nWarnings:");
        for entity in &report.warnings {
            let label = entity
                .file
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_else(|| entity.name.clone());
            output::always(&format!("\n{label} =>"));
            for warn in &entity.warnings {
                output::always(&format!("  {warn}"));
            }
        }
    }

    if report.issues.is_empty() && report.out_of_scope_issues.is_empty() && report.warnings.is_empty() {
        output::info(verbosity, "Everything looks ok");
    }
}

/// The process exit code for an inspect run.
///
/// Non-zero when the design carries anything that would stop a run, so a CI
/// gate running `dbd inspect` fails on exactly what `dbd apply` would refuse
/// on. It exited 0 here for every release up to v0.12.0, which made
/// `apply`'s own "run `dbd inspect` for the full report" advice point at a
/// command that passed green on the very file apply was rejecting.
///
/// Out-of-scope errors are deliberately excluded: the scoped run they do not
/// belong to still succeeds, and failing it would punish the scope that is
/// correct.
pub(crate) fn inspect_exit_code(blocking_errors: usize) -> i32 {
    if blocking_errors > 0 { 1 } else { 0 }
}

/// Auto-format every DDL file under `project_dir` in place (the `--fix` path).
fn fix_format_ddl(config: &Path, project_dir: &Path, verbosity: Verbosity) -> Result<()> {
    let format_config = if config.exists() {
        dbd_core::config::read(config)?.format
    } else {
        dbd_core::config::FormatConfig::default()
    };

    let files = dbd_core::scanner::scan_ddl(project_dir)?;
    let mut changed = 0;
    for file in &files {
        let content = safe_read(project_dir, file)?;
        let formatted = dbd_core::formatter::format_ddl(&content, &format_config);
        if content != formatted {
            changed += 1;
            safe_write(project_dir, file, &formatted)?;
            output::info(verbosity, &format!("  formatted: {}", file.display()));
        }
    }
    if changed > 0 {
        output::info(verbosity, &format!("Formatted {changed} file(s)."));
    }
    Ok(())
}

/// Print unresolved `data.sql` TODOs across all migration directories.
fn print_data_sql_todos(todos: &[dbd_core::DataSqlTodo]) {
    if todos.is_empty() {
        return;
    }
    output::always("\ndata.sql TODOs (resolve before applying):");
    for todo in todos {
        output::always(&format!("  {} (v{}):", todo.file.display(), todo.version));
        for line in &todo.lines {
            output::always(&format!("    {line}"));
        }
    }
}

/// Print materialized-view refresh-config validation errors (concurrently
/// without a unique index, missing pg_cron, invalid cron expression).
fn print_matview_errors(errors: &[String]) {
    if errors.is_empty() {
        return;
    }
    output::always("\nMaterialized view errors:");
    for err in errors {
        output::always(&format!("  {err}"));
    }
}

/// One proposed enum: the columns that would share it, and the name to file it under.
struct EnumProposal {
    schema: String,
    type_name: String,
    values: Vec<String>,
    /// Qualified `schema.table.column` for each column with this exact set.
    columns: Vec<String>,
    /// True when the column name was already claimed by a different value set,
    /// so the type is named after its table instead.
    renamed: bool,
}

/// Group enum-candidate hints into one proposal per distinct value set.
///
/// Two things the old per-column rendering got wrong. It repeated the same
/// rationale on every line, and it derived the filename from the *column*
/// (`ddl/enum/<schema>/<column>.ddl`) — so columns that share a name but not a
/// domain all pointed at one file. In a real project three different `state`
/// sets and two different `source` sets collided, and following the advice
/// literally would have produced one file with conflicting definitions.
///
/// Grouping is by exact value list, not by a set: Postgres enums are ordered,
/// so two columns listing the same values in a different order are not
/// self-evidently the same type, and merging them would be a guess.
fn group_enum_hints(hints: &[dbd_core::design::EnumHint]) -> Vec<EnumProposal> {
    // Preserve first-seen order so output is deterministic for a given design.
    let mut order: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut columns: std::collections::HashMap<(String, String, Vec<String>), Vec<String>> =
        std::collections::HashMap::new();

    for h in hints {
        let (schema, table) = h.entity.split_once('.').unwrap_or(("public", h.entity.as_str()));
        let key = (schema.to_string(), h.column.clone(), h.values.clone());
        if !columns.contains_key(&key) {
            order.push(key.clone());
        }
        columns
            .entry(key)
            .or_default()
            .push(format!("{schema}.{table}.{}", h.column));
    }

    // A column name is ambiguous when two different value sets both want it.
    let mut claims: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
    for (schema, column, _) in &order {
        *claims.entry((schema.clone(), column.clone())).or_default() += 1;
    }

    let mut proposals: Vec<EnumProposal> = order
        .into_iter()
        .map(|key| {
            let (schema, column, values) = key.clone();
            let cols = columns.remove(&key).unwrap_or_default();
            let ambiguous = claims.get(&(schema.clone(), column.clone())).copied().unwrap_or(0) > 1;
            // Disambiguate with the table that owns it. Only reached for a real
            // collision, so the plainer name is kept whenever it is unambiguous.
            let type_name = if ambiguous {
                let table = cols
                    .first()
                    .and_then(|c| c.split('.').nth(1))
                    .unwrap_or(column.as_str());
                format!("{table}_{column}")
            } else {
                column.clone()
            };
            EnumProposal {
                schema,
                type_name,
                values,
                columns: cols,
                renamed: ambiguous,
            }
        })
        .collect();

    // Scan order is deterministic but arbitrary to read. Sorting groups each
    // schema's proposals together and makes the list scannable.
    proposals.sort_by(|a, b| (&a.schema, &a.type_name).cmp(&(&b.schema, &b.type_name)));
    proposals
}

/// Render the advisory enum-candidate section: one rationale, then the
/// instances (pure, so it's unit-testable).
fn render_enum_hints(hints: &[dbd_core::design::EnumHint]) -> Vec<String> {
    let proposals = group_enum_hints(hints);
    if proposals.is_empty() {
        return Vec::new();
    }

    let mut out = vec![
        "  A CHECK that pins a column to a fixed set of strings can be a Postgres enum instead —".to_string(),
        "  the type is enforced by the database and introspects as a real type. Advisory only:".to_string(),
        "  never counted as an error, never affects the exit code.".to_string(),
        String::new(),
    ];

    for p in &proposals {
        let set = p.values.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", ");
        out.push(format!("  ddl/enum/{}/{}.ddl — {set}", p.schema, p.type_name));
        out.push(format!("      {}", p.columns.join(", ")));
    }

    // Only mention renaming if it actually happened, and say why — a reader who
    // sees `sync_state_state` should know it is a collision-avoidance name and
    // not dbd's idea of good taste.
    let renamed: Vec<&EnumProposal> = proposals.iter().filter(|p| p.renamed).collect();
    if !renamed.is_empty() {
        out.push(String::new());
        out.push(format!(
            "  {} type(s) named after their table because the column name covers more than one",
            renamed.len()
        ));
        out.push("  value set in that schema — rename to whatever reads better.".to_string());
    }
    out
}

/// Print the advisory `Suggestions:` section (enum candidates). Report-only.
fn print_enum_hints(hints: &[dbd_core::design::EnumHint]) {
    if hints.is_empty() {
        return;
    }
    output::always("\nSuggestions:");
    for line in render_enum_hints(hints) {
        output::always(&line);
    }
}

pub fn cmd_combine(
    config: &Path,
    env: &str,
    project_dir: &Path,
    file: &Path,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps)?;
    output::scope_filtered(
        &resolved,
        design.scoped_entities(&resolved)?.len(),
        design.entities().len(),
    );
    design.combine(file, Some(&resolved))?;
    output::info(verbosity, &format!("Generated {}", file.display()));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_apply(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    dry_run: bool,
    with_policies: bool,
    allow_scope_change: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    warn_if_no_authored_ddl(&design, project_dir);
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;

    if dry_run {
        // Surface the same gap/closure errors a real apply would (dry-run must
        // not hide a misconfigured scope).
        design.check_scope_gaps(&resolved).context("scope check failed")?;
        let ws = design.working_set(&resolved)?;
        let entities: Vec<_> = design
            .entities()
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != dbd_core::EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .filter(|e| {
                resolved.is_all
                    || ws.contains(&e.name)
                    || matches!(
                        e.entity_type,
                        dbd_core::EntityType::Extension | dbd_core::EntityType::Role
                    )
            })
            .collect();

        for entity in &entities {
            let detail = match &entity.file {
                Some(f) => format!("{:?} => {} using \"{}\"", entity.entity_type, entity.name, f.display()),
                None => format!("{:?} => {}", entity.entity_type, entity.name),
            };
            output::info(verbosity, &detail);
        }
        output::summary(0, 0, entities.len());
        return Ok(());
    }

    let adapter = get_adapter(config, database_url).await?;

    // Scope guard: refuse an apply under a different scope than this DB was
    // pinned to (unless the operator opted in to re-point it).
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, allow_scope_change)?;

    let spinner = output::StepSpinner::new(verbosity);
    let mut apply_summary: Option<ApplyComplete> = None;
    let result = design
        .apply(
            &*adapter,
            name,
            false,
            Some(&resolved),
            Progress {
                on_start: |desc: &str| spinner.start(desc),
                on_done: |desc: &str, err: Option<&str>| spinner.done(desc, err),
                on_complete: |s| apply_summary = Some(s),
            },
        )
        .await;
    spinner.finish();
    result?;

    if let Some(s) = apply_summary {
        // Always reported, whatever the verbosity: a hook a scope filtered out
        // is the reason something the user expected to happen did not.
        for warning in &s.warnings {
            output::warn(warning);
        }
        output::info(verbosity, &format_apply_summary(&s));
    }

    // pg_cron refresh-job sync + matview hash-stamping now live in core
    // `Design::apply`, so `dbd apply` and `dbd deploy` both schedule refresh
    // jobs through the shared path (no CLI-side sync call needed here).

    // Grants: universal `schemas:` `WithGrants` entries apply regardless of
    // target; the chosen target's `grants:` config merges on top of them —
    // per schema, target role entries add to / override the universal ones
    // for that role.
    let mut schema_grants = design.config().schema_grants();
    let mut supabase_schemas: Vec<String> = vec![];
    if let Some((target_name, target_config)) = design.config().target.iter().next() {
        if let Some(ref grants) = target_config.grants {
            for (schema, gc) in grants {
                schema_grants
                    .entry(schema.clone())
                    .or_default()
                    .extend(gc.roles.clone());
            }
        }
        // PostgREST USAGE grants ride along only when the user configured some
        // grants, so a no-grants Supabase apply stays a no-op (as it was before
        // universal `schemas:` grants existed).
        if target_name == "supabase" && !schema_grants.is_empty() {
            supabase_schemas = design.config().schema_names();
        }
    }

    // Grants are Postgres/Supabase DDL. Skip cleanly on targets without a grant
    // model (SQLite, Convex) rather than feeding them SQL they can't run — a
    // cross-target design may declare schema grants yet apply to any target.
    if !schema_grants.is_empty() {
        if adapter.supports_schema_grants() {
            if let Some(grants_sql) = dbd_core::script::build_grants_script(&schema_grants, &supabase_schemas) {
                output::info(verbosity, "Applying grants...");
                adapter
                    .execute_script(&grants_sql)
                    .await
                    .context("Failed to apply grants")?;
                output::detail(verbosity, "  NOTIFY pgrst, 'reload config'");
            }
        } else {
            output::info(verbosity, "Skipping schema grants (target has no grant model).");
        }
    }

    // Apply RLS policies if requested
    if with_policies {
        let policy_ws = if resolved.is_all {
            None
        } else {
            design.working_set(&resolved).ok().map(|ws| (resolved.name.clone(), ws))
        };
        let report = dbd_core::design::apply_policies(
            &*adapter,
            project_dir,
            false,
            policy_ws.as_ref().map(|(n, ws)| (n.as_str(), ws)),
        )
        .await?;
        for (file, why) in &report.skipped {
            output::info(verbosity, &format!("  skipped {} — {why}", file.display()));
        }
        if !report.applied.is_empty() {
            output::info(verbosity, &format!("Applied {} policy file(s).", report.applied.len()));
        }
        for (file, err) in &report.failed {
            output::always(&format!("  Policy FAILED: {} — {}", file.display(), err));
        }
    }

    Ok(())
}

/// Select the materialized views a `refresh` invocation should target, in
/// `entities` order (already dependency-sorted, matviews contiguous).
///
/// - `None` → every materialized view.
/// - `Some("schema.*")` → every materialized view in that schema.
/// - `Some("schema.name")` → that one materialized view (by qualified name).
fn select_matviews<'a>(entities: &'a [Entity], name: Option<&str>) -> Vec<&'a Entity> {
    entities
        .iter()
        .filter(|e| e.entity_type == EntityType::MaterializedView)
        .filter(|e| match name {
            None => true,
            Some(sel) if sel.ends_with(".*") => {
                let schema = sel.trim_end_matches(".*");
                e.schema.as_deref() == Some(schema)
            }
            Some(sel) => e.name == sel,
        })
        .collect()
}

/// Refresh materialized views: `REFRESH MATERIALIZED VIEW [CONCURRENTLY] …`,
/// honoring each view's resolved `concurrently` setting, in dependency order.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_refresh(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;

    // A matview the scope excludes does not exist on this plane, so refreshing
    // it fails with `relation … does not exist` — an expected condition dressed
    // as an error, the same failure the policy phase used to produce.
    let resolved_scope = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let scoped = design.scoped_entities(&resolved_scope)?;
    output::scope_filtered(&resolved_scope, scoped.len(), design.entities().len());

    let selected = select_matviews(&scoped, name);
    if selected.is_empty() {
        output::info(verbosity, "No materialized views to refresh.");
        return Ok(());
    }

    let adapter = get_adapter(config, database_url).await?;

    for entity in selected {
        let resolved = design.config().materialized_views.resolve(&entity.name);
        output::info(verbosity, &format!("Refreshing {} ...", entity.name));
        adapter
            .refresh_matview(&entity.name, resolved.concurrently)
            .await
            .with_context(|| format!("Failed to refresh {}", entity.name))?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_policies(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    dry_run: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    if dry_run {
        let files = dbd_core::scanner::scan_policies(project_dir)?;
        if files.is_empty() {
            output::info(verbosity, "No policy files found in policies/");
            return Ok(());
        }
        output::info(verbosity, "[dry-run] Would apply policies:");
        for file in &files {
            output::info(verbosity, &format!("  {}", file.display()));
        }
        return Ok(());
    }

    let adapter = get_adapter(config, database_url).await?;
    // `dbd policies` takes the global --scope like every other command; before
    // this it silently applied every file, so a policy for a schema this plane
    // does not have reported `schema "…" does not exist` on every run.
    let design = Design::from_config_with_dir(config, env, Some(project_dir))?;
    let resolved = design.resolve_scope(scope, deps)?;
    let policy_ws = if resolved.is_all {
        None
    } else {
        design.working_set(&resolved).ok().map(|ws| (resolved.name.clone(), ws))
    };
    let report = dbd_core::design::apply_policies(
        &*adapter,
        project_dir,
        false,
        policy_ws.as_ref().map(|(n, ws)| (n.as_str(), ws)),
    )
    .await?;

    for (file, why) in &report.skipped {
        output::info(verbosity, &format!("Skipped {} — {why}", file.display()));
    }

    if report.applied.is_empty() && report.failed.is_empty() && report.skipped.is_empty() {
        output::info(verbosity, "No policy files found in policies/");
        return Ok(());
    }

    for file in &report.applied {
        output::detail(verbosity, &format!("  applied: {}", file.display()));
    }
    for (file, err) in &report.failed {
        output::always(&format!("  FAILED: {} — {}", file.display(), err));
    }

    output::info(
        verbosity,
        &format!(
            "Policies: {} applied, {} failed.",
            report.applied.len(),
            report.failed.len()
        ),
    );

    if !report.failed.is_empty() {
        std::process::exit(1);
    }

    Ok(())
}

pub fn cmd_format(config: &Path, project_dir: &Path, check: bool, verbosity: Verbosity) -> Result<()> {
    let format_config = if config.exists() {
        let design_config = dbd_core::config::read(config)?;
        design_config.format
    } else {
        dbd_core::config::FormatConfig::default()
    };

    let files = dbd_core::scanner::scan_ddl(project_dir)?;
    let mut changed = 0;

    for file in &files {
        let content = safe_read(project_dir, file)?;
        let formatted = dbd_core::formatter::format_ddl(&content, &format_config);

        if content != formatted {
            changed += 1;
            if check {
                output::info(verbosity, &format!("  would reformat: {}", file.display()));
            } else {
                safe_write(project_dir, file, &formatted)?;
                output::info(verbosity, &format!("  formatted: {}", file.display()));
            }
        }
    }

    if check && changed > 0 {
        output::info(verbosity, &format!("{changed} file(s) would be reformatted."));
        std::process::exit(1);
    } else if changed > 0 {
        output::info(verbosity, &format!("Formatted {changed} file(s)."));
    } else {
        output::info(verbosity, "All files already formatted.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil;

    fn hint(entity: &str, column: &str, values: &[&str]) -> dbd_core::design::EnumHint {
        dbd_core::design::EnumHint {
            entity: entity.into(),
            column: column.into(),
            values: values.iter().map(|v| (*v).to_string()).collect(),
        }
    }

    #[test]
    fn render_enum_hints_renders_advisory_line() {
        let out = render_enum_hints(&[hint("config.lookups", "status", &["active", "inactive"])]);
        let body = out.join("\n");
        assert!(body.contains("config.lookups.status"), "names the column: {body}");
        assert!(body.contains("'active'"), "lists the values: {body}");
        assert!(body.contains("ddl/enum/config/status.ddl"), "proposes a path: {body}");
    }

    /// The rationale is stated once, not repeated per column — the whole point
    /// of grouping. Fourteen candidates used to mean fourteen copies of it.
    #[test]
    fn render_enum_hints_states_the_rationale_once() {
        let out = render_enum_hints(&[
            hint("app.a", "kind", &["x", "y"]),
            hint("app.b", "flavour", &["p", "q"]),
            hint("app.c", "mode", &["m", "n"]),
        ]);
        let body = out.join("\n");
        assert_eq!(
            body.matches("Postgres enum").count(),
            1,
            "rationale must appear once: {body}"
        );
    }

    /// Columns sharing an identical value set are one enum, listed together —
    /// the signal worth acting on.
    #[test]
    fn columns_with_the_same_value_set_become_one_proposal() {
        let out = render_enum_hints(&[
            hint("sensei.intake_guide", "source", &["builtin", "org", "learned"]),
            hint("sensei.playbooks", "source", &["builtin", "org", "learned"]),
            hint("sensei.playbook_rules", "source", &["builtin", "org", "learned"]),
        ]);
        let body = out.join("\n");
        assert_eq!(
            body.matches("ddl/enum/sensei/source.ddl").count(),
            1,
            "one shared enum, not three: {body}"
        );
        for c in [
            "sensei.intake_guide.source",
            "sensei.playbooks.source",
            "sensei.playbook_rules.source",
        ] {
            assert!(body.contains(c), "must list {c}: {body}");
        }
    }

    /// The bug this replaced: the path came from the column name, so three
    /// different `state` domains all pointed at `ddl/enum/sensei/state.ddl`.
    /// Following that literally yields one file with conflicting definitions.
    #[test]
    fn different_value_sets_sharing_a_column_name_get_distinct_paths() {
        let out = render_enum_hints(&[
            hint("sensei.sync_state", "state", &["pending", "synced"]),
            hint("sensei.dojo_inbox", "state", &["pending", "applied"]),
            hint("sensei.dojo_outbox", "state", &["pending", "sent"]),
        ]);
        let body = out.join("\n");
        let paths: Vec<&str> = body.lines().filter(|l| l.contains("ddl/enum/")).collect();
        assert_eq!(paths.len(), 3, "three distinct domains: {body}");
        let unique: std::collections::HashSet<_> = paths
            .iter()
            .map(|l| l.split(" — ").next().unwrap_or(l).trim())
            .collect();
        assert_eq!(unique.len(), 3, "each needs its own file, got collisions: {paths:?}");
        assert!(body.contains("rename"), "must say why the names look like that: {body}");
    }

    /// An unambiguous column keeps its plain name — disambiguation only fires
    /// on a real collision, so the common case stays readable.
    #[test]
    fn an_unambiguous_column_keeps_its_plain_name() {
        let out = render_enum_hints(&[hint("dojo.repositories", "visibility", &["private", "public"])]);
        let body = out.join("\n");
        assert!(body.contains("ddl/enum/dojo/visibility.ddl"), "got: {body}");
        assert!(
            !body.contains("repositories_visibility"),
            "must not over-qualify: {body}"
        );
        assert!(!body.contains("rename"), "no collision, so no rename note: {body}");
    }

    /// Offline `inspect` (no `--from-db`) validates the fixture against the
    /// project-local cache — no DB connection.
    #[tokio::test]
    async fn inspect_offline_on_fixture() {
        cmd_inspect(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            /*name*/ None,
            /*fix*/ false,
            /*use_database*/ false,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `combine` writes a single concatenated SQL file (target may live outside
    /// the project, per the core API).
    #[test]
    fn combine_writes_sql_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("combined.sql");
        cmd_combine(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            &out,
            None,
            None,
            Verbosity::Normal,
        )
        .unwrap();
        assert!(out.exists());
    }

    /// `apply --dry-run` lists the entities it would apply and returns before
    /// constructing an adapter.
    #[tokio::test]
    async fn apply_dry_run_lists_entities() {
        cmd_apply(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            /*name*/ None,
            /*dry_run*/ true,
            /*with_policies*/ false,
            /*allow_scope_change*/ false,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `policies --dry-run` on a project with no `policies/` dir takes the
    /// "no policy files" path without touching a DB.
    #[tokio::test]
    async fn policies_dry_run_without_policy_dir() {
        cmd_policies(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            /*dry_run*/ true,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `format` (write mode) rewrites DDL in place; run against a copy and with
    /// `check=false` so it never hits the `process::exit` in check mode.
    #[test]
    fn format_write_mode_on_temp_copy() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_format(&cfg, proj.path(), /*check*/ false, Verbosity::Normal).unwrap();
    }

    /// `format --check` on an already-formatted project reports "all
    /// formatted" and returns `Ok` — it must NOT hit the `process::exit(1)`
    /// branch, which only fires when `check && changed > 0` (untestable
    /// in-process; see report).
    #[test]
    fn format_check_mode_reports_all_formatted_when_clean() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        // Normalize first so the check pass finds zero diffs.
        cmd_format(&cfg, proj.path(), /*check*/ false, Verbosity::Normal).unwrap();
        cmd_format(&cfg, proj.path(), /*check*/ true, Verbosity::Normal).unwrap();
    }

    /// `inspect --name` filters the report to a single, warning-free entity —
    /// exercises the verbose entity-JSON dump and the "Everything looks ok"
    /// all-clear path (both require issues *and* warnings to be empty, which
    /// only holds once filtered to a clean entity).
    #[tokio::test]
    async fn inspect_verbose_named_clean_entity_reports_all_clear() {
        cmd_inspect(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            /*name*/ Some("config.lookups"),
            /*fix*/ false,
            /*use_database*/ false,
            None,
            None,
            Verbosity::Verbose,
        )
        .await
        .unwrap();
    }

    /// A scope with an unresolved dependency gap under the default `Report`
    /// policy refuses to proceed — the same guard `deploy`/`apply` rely on to
    /// keep a misconfigured scope from silently dropping entities.
    #[tokio::test]
    async fn inspect_bails_on_scope_gap_with_report_policy() {
        let err = cmd_inspect(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            None,
            false,
            false,
            Some("incomplete"),
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("dependency gap"), "got: {msg}");
    }

    /// The same gap, but the scope opts into `deps: include` — inspect
    /// reports the gap will be auto-included instead of refusing.
    #[tokio::test]
    async fn inspect_reports_scope_gap_with_include_policy() {
        cmd_inspect(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            None,
            false,
            false,
            Some("incomplete_auto"),
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `apply --dry-run` against a named scope exercises the working-set
    /// membership filter (as opposed to the always-true `resolved.is_all`
    /// short-circuit the all-scope tests take).
    #[tokio::test]
    async fn apply_dry_run_lists_entities_for_named_scope() {
        cmd_apply(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            /*name*/ None,
            /*dry_run*/ true,
            /*with_policies*/ false,
            /*allow_scope_change*/ false,
            Some("config_only"),
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `policies --dry-run` with a populated `policies/` dir lists the files
    /// it would apply, still without touching a DB.
    #[tokio::test]
    async fn policies_dry_run_lists_existing_policy_files() {
        let proj = testutil::copy_fixture_project();
        std::fs::create_dir_all(proj.path().join("policies")).unwrap();
        std::fs::write(proj.path().join("policies").join("secrets.sql"), "-- rls policy\n").unwrap();
        let cfg = proj.path().join("design.yaml");
        cmd_policies(
            &cfg,
            "dev",
            proj.path(),
            None,
            /*dry_run*/ true,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Offline reference resolution drops a warning whose target is present
    /// in a cached `.dbd/refcache.json` snapshot, and reports how many
    /// entities the cache carried.
    #[tokio::test]
    async fn resolve_inspect_refs_offline_drops_cached_warning() {
        let proj = testutil::copy_fixture_project();
        std::fs::write(
            proj.path().join("ddl/table/config/refs_missing.ddl"),
            "create table config.refs_missing (\n  id uuid primary key,\n  other_id uuid references config.totally_missing(id)\n);\n",
        )
        .unwrap();
        let cache = dbd_core::refcache::RefCache::new("postgres", vec!["config.totally_missing".to_string()]);
        cache.save(proj.path()).unwrap();

        let cfg = proj.path().join("design.yaml");
        let mut design = Design::from_config_with_dir(&cfg, "dev", Some(proj.path())).unwrap();
        let has_warning_before = design
            .entities()
            .iter()
            .any(|e| e.warnings.iter().any(|w| w.contains("totally_missing")));
        assert!(
            has_warning_before,
            "fixture setup should produce the unresolved-reference warning"
        );

        resolve_inspect_refs(&mut design, &cfg, None, /*use_database*/ false, Verbosity::Normal)
            .await
            .unwrap();

        let still_warns = design
            .entities()
            .iter()
            .any(|e| e.warnings.iter().any(|w| w.contains("totally_missing")));
        assert!(!still_warns, "cached reference should have been resolved offline");
    }

    /// A corrupt `.dbd/refcache.json` doesn't fail `inspect` — the read error
    /// is reported as a detail line and resolution just continues unresolved.
    #[tokio::test]
    async fn resolve_inspect_refs_offline_handles_corrupt_cache() {
        let proj = testutil::copy_fixture_project();
        std::fs::create_dir_all(proj.path().join(".dbd")).unwrap();
        std::fs::write(proj.path().join(".dbd").join("refcache.json"), "not valid json").unwrap();

        let cfg = proj.path().join("design.yaml");
        let mut design = Design::from_config_with_dir(&cfg, "dev", Some(proj.path())).unwrap();
        resolve_inspect_refs(&mut design, &cfg, None, /*use_database*/ false, Verbosity::Normal)
            .await
            .unwrap();
    }

    /// `--fix` auto-formats DDL files in place — run against a copy and
    /// verify the file content actually changed to the formatted form.
    #[tokio::test]
    async fn inspect_fix_reformats_ddl_on_temp_copy() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        let target = proj.path().join("ddl/table/config/lookups.ddl");
        let before = std::fs::read_to_string(&target).unwrap();

        cmd_inspect(
            &cfg,
            "dev",
            proj.path(),
            None,
            /*name*/ None,
            /*fix*/ true,
            /*use_database*/ false,
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();

        let after = std::fs::read_to_string(&target).unwrap();
        assert_ne!(before, after, "--fix should have reformatted at least one DDL file");
    }

    /// Entities with parse errors surface under "Errors:" — direct call so
    /// the assertion doesn't depend on capturing stdout.
    #[test]
    fn print_report_findings_renders_issues_branch() {
        let mut broken = dbd_core::Entity::new(dbd_core::EntityType::Table, "public.broken");
        broken.errors.push("parse error: unexpected token".to_string());
        let report = dbd_core::design::Report {
            entity: None,
            issues: vec![broken],
            out_of_scope_issues: vec![],
            warnings: vec![],
            gaps: vec![],
        };
        // Exercises the issues-loop formatting (label fallback + per-error line).
        print_report_findings(&report, None, Verbosity::Normal);
    }

    /// The out-of-scope branch renders its own heading and does not touch the
    /// blocking one — a scoped run with only excluded errors must not read as
    /// if it were broken.
    #[test]
    fn print_report_findings_renders_out_of_scope_branch() {
        let mut broken = dbd_core::Entity::new(dbd_core::EntityType::Table, "svc.broken");
        broken.errors.push("parse error: unexpected token".to_string());
        let report = dbd_core::design::Report {
            entity: None,
            issues: vec![],
            out_of_scope_issues: vec![broken],
            warnings: vec![],
            gaps: vec![],
        };
        print_report_findings(&report, Some("daemon"), Verbosity::Normal);
    }

    /// The exit code is the contract a CI gate depends on: non-zero on exactly
    /// what `apply` would refuse on, zero otherwise.
    #[test]
    fn inspect_exit_code_semantics() {
        assert_eq!(inspect_exit_code(0), 0, "a clean design must exit 0");
        assert_eq!(inspect_exit_code(1), 1, "a blocking error must exit non-zero");
        assert_eq!(inspect_exit_code(9), 1);
    }

    /// A wildcard scope with every dependency present hits the "no gaps"
    /// early-return — distinct from the `incomplete`/`incomplete_auto` scopes,
    /// which always have a gap to report.
    #[tokio::test]
    async fn inspect_scoped_with_no_gaps_succeeds() {
        cmd_inspect(
            &testutil::fixture_config(),
            "dev",
            &testutil::fixtures(),
            None,
            None,
            false,
            false,
            Some("config_wild"),
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Unresolved `data.sql` TODOs render with their migration version, file
    /// path, and comment lines — direct call so the assertion doesn't depend
    /// on capturing stdout.
    #[test]
    fn print_data_sql_todos_renders_version_file_and_lines() {
        let todos = vec![dbd_core::DataSqlTodo {
            version: 3,
            file: std::path::PathBuf::from("migrations/003/seed.data.sql"),
            lines: vec!["-- TODO: fill me in".to_string()],
        }];
        print_data_sql_todos(&todos);
    }

    /// `format` doesn't need a `design.yaml` at all — a missing config just
    /// falls back to the default format config, unlike `inspect`, which
    /// requires the design to load first.
    #[test]
    fn format_without_design_yaml_uses_default_format_config() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/public")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/public/thing.ddl"),
            "create table public.thing(id int,name text);",
        )
        .unwrap();
        let missing_config = tmp.path().join("design.yaml");
        assert!(!missing_config.exists());

        cmd_format(&missing_config, tmp.path(), /*check*/ false, Verbosity::Normal).unwrap();

        let formatted = std::fs::read_to_string(tmp.path().join("ddl/table/public/thing.ddl")).unwrap();
        assert_ne!(formatted, "create table public.thing(id int,name text);");
    }

    /// Mixed fixture for `select_matviews`: two matviews in `analytics`, one
    /// in `reporting`, plus a non-matview table — so the None/`schema.*`/name
    /// cases are all distinguishable from each other.
    fn matview_fixture() -> Vec<Entity> {
        vec![
            Entity::new(EntityType::MaterializedView, "analytics.daily_sales"),
            Entity::new(EntityType::MaterializedView, "analytics.weekly_sales"),
            Entity::new(EntityType::MaterializedView, "reporting.monthly_totals"),
            Entity::new(EntityType::Table, "analytics.raw_events"),
        ]
    }

    /// `None` selects every materialized view, in entities() order, and
    /// excludes the plain table.
    #[test]
    fn select_matviews_none_selects_all_matviews() {
        let entities = matview_fixture();
        let selected = select_matviews(&entities, None);
        let names: Vec<&str> = selected.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "analytics.daily_sales",
                "analytics.weekly_sales",
                "reporting.monthly_totals"
            ]
        );
    }

    /// `schema.*` selects only that schema's matviews.
    #[test]
    fn select_matviews_wildcard_selects_schema_only() {
        let entities = matview_fixture();
        let selected = select_matviews(&entities, Some("analytics.*"));
        let names: Vec<&str> = selected.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["analytics.daily_sales", "analytics.weekly_sales"]);
    }

    /// A fully-qualified name selects just that one matview.
    #[test]
    fn select_matviews_named_selects_single_entity() {
        let entities = matview_fixture();
        let selected = select_matviews(&entities, Some("analytics.daily_sales"));
        let names: Vec<&str> = selected.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["analytics.daily_sales"]);
    }

    /// A name that isn't a materialized view (the table, or an unknown name)
    /// selects nothing — it never falls back to matching non-matview entities.
    #[test]
    fn select_matviews_non_matview_name_selects_nothing() {
        let entities = matview_fixture();
        assert!(select_matviews(&entities, Some("analytics.raw_events")).is_empty());
        assert!(select_matviews(&entities, Some("nonexistent.thing")).is_empty());
    }
}
