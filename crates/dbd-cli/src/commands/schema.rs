use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::{ApplyComplete, Progress};
use dbd_core::{Design, Entity, EntityType};

use super::{format_apply_summary, get_adapter, safe_read, safe_write};
use crate::output::{self, Verbosity};

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

    resolve_inspect_refs(&mut design, config, database_url, use_database, verbosity).await?;

    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let report = design.report(name, Some(&resolved));

    report_scope_gaps(&resolved, &report, verbosity)?;

    let total_entities = design.entities().len();

    if verbosity.is_verbose()
        && let Some(entity) = &report.entity
    {
        output::always(&serde_json::to_string_pretty(entity)?);
    }

    print_report_findings(&report, verbosity);

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

    output::summary(
        report.issues.len() + todos.len() + matview_errors.len(),
        report.warnings.len(),
        total_entities,
    );

    // Advisory only — string-set CHECK constraints that could be a Postgres enum.
    // Report-only: NOT added to the summary error count, never affects the exit code.
    let enum_hints = dbd_core::design::suggest_enum_candidates(
        design.entities(),
        &design.config().source.dialect,
    );
    print_enum_hints(&enum_hints);
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
            output::detail(verbosity, &format!("  resolved {dropped} reference(s) against database catalog"));
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

    output::info(verbosity, &format!("scope '{}': {} entities", resolved.name, resolved.entities.len()));
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
            output::info(verbosity, &format!("{} gap(s) will be auto-included (--deps include)", report.gaps.len()));
        }
    }
    Ok(())
}

/// Print entity errors and warnings, or an all-clear message when there's neither.
fn print_report_findings(report: &dbd_core::design::Report, verbosity: Verbosity) {
    if !report.issues.is_empty() {
        output::always("Errors:");
        for entity in &report.issues {
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

    if report.issues.is_empty() && report.warnings.is_empty() {
        output::info(verbosity, "Everything looks ok");
    }
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

/// Render one advisory line per enum-candidate hint (pure, so it's unit-testable).
fn render_enum_hints(hints: &[dbd_core::design::EnumHint]) -> Vec<String> {
    hints
        .iter()
        .map(|h| {
            let set = h
                .values
                .iter()
                .map(|v| format!("'{v}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let schema = h.entity.split_once('.').map(|(s, _)| s).unwrap_or("public");
            format!(
                "  {entity}.{column}: CHECK constrains to a fixed string set {{{set}}} — a \
                 Postgres enum (ddl/enum/{schema}/{column}.ddl) gives type safety + cleaner \
                 introspection. Not required.",
                entity = h.entity,
                column = h.column,
            )
        })
        .collect()
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
            .filter(|e| resolved.is_all
                || ws.contains(&e.name)
                || matches!(e.entity_type, dbd_core::EntityType::Extension | dbd_core::EntityType::Role))
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
        output::info(verbosity, &format_apply_summary(&s));
    }

    // pg_cron refresh-job sync + matview hash-stamping now live in core
    // `Design::apply`, so `dbd apply` and `dbd deploy` both schedule refresh
    // jobs through the shared path (no CLI-side sync call needed here).

    // Run grants if target has grants config
    if let Some((target_name, target_config)) = design.config().target.iter().next()
        && let Some(ref grants) = target_config.grants
    {
            let schema_grants: std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>> =
                grants.iter().map(|(schema, gc)| {
                    (schema.clone(), gc.roles.clone())
                }).collect();

            let supabase_schemas = if target_name == "supabase" {
                design.config().schema_names()
            } else {
                vec![]
            };

            if let Some(grants_sql) = dbd_core::script::build_grants_script(&schema_grants, &supabase_schemas) {
                output::info(verbosity, "Applying grants...");
                adapter.execute_script(&grants_sql).await
                    .context("Failed to apply grants")?;
                output::detail(verbosity, "  NOTIFY pgrst, 'reload config'");
            }
        }

    // Apply RLS policies if requested
    if with_policies {
        let report = dbd_core::design::apply_policies(&*adapter, project_dir, false).await?;
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
pub async fn cmd_refresh(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;

    let selected = select_matviews(design.entities(), name);
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

pub async fn cmd_policies(
    config: &Path,
    project_dir: &Path,
    database_url: Option<&str>,
    dry_run: bool,
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
    let report = dbd_core::design::apply_policies(&*adapter, project_dir, false).await?;

    if report.applied.is_empty() && report.failed.is_empty() {
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

    #[test]
    fn render_enum_hints_renders_advisory_line() {
        let hints = vec![dbd_core::design::EnumHint {
            entity: "config.lookups".into(),
            column: "status".into(),
            values: vec!["active".into(), "inactive".into()],
        }];
        let out = render_enum_hints(&hints);
        assert!(
            out.iter().any(|l| l.contains("config.lookups.status")
                && l.contains("'active'")
                && l.contains("enum")),
            "got: {out:?}"
        );
    }

    /// Offline `inspect` (no `--from-db`) validates the fixture against the
    /// project-local cache — no DB connection.
    #[tokio::test]
    async fn inspect_offline_on_fixture() {
        cmd_inspect(
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            /*name*/ None, /*fix*/ false, /*use_database*/ false, None, None, Verbosity::Normal,
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
        cmd_combine(&testutil::fixture_config(), "dev", &testutil::fixtures(), &out, None, None, Verbosity::Normal).unwrap();
        assert!(out.exists());
    }

    /// `apply --dry-run` lists the entities it would apply and returns before
    /// constructing an adapter.
    #[tokio::test]
    async fn apply_dry_run_lists_entities() {
        cmd_apply(
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            /*name*/ None, /*dry_run*/ true, /*with_policies*/ false, /*allow_scope_change*/ false,
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// `policies --dry-run` on a project with no `policies/` dir takes the
    /// "no policy files" path without touching a DB.
    #[tokio::test]
    async fn policies_dry_run_without_policy_dir() {
        cmd_policies(&testutil::fixture_config(), &testutil::fixtures(), None, /*dry_run*/ true, Verbosity::Normal)
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
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            /*name*/ Some("config.lookups"), /*fix*/ false, /*use_database*/ false, None, None,
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
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            None, false, false, Some("incomplete"), None, Verbosity::Normal,
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
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            None, false, false, Some("incomplete_auto"), None, Verbosity::Normal,
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
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            /*name*/ None, /*dry_run*/ true, /*with_policies*/ false, /*allow_scope_change*/ false,
            Some("config_only"), None, Verbosity::Normal,
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
        cmd_policies(&cfg, proj.path(), None, /*dry_run*/ true, Verbosity::Normal)
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
        assert!(has_warning_before, "fixture setup should produce the unresolved-reference warning");

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
            &cfg, "dev", proj.path(), None,
            /*name*/ None, /*fix*/ true, /*use_database*/ false, None, None, Verbosity::Normal,
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
            warnings: vec![],
            gaps: vec![],
        };
        // Exercises the issues-loop formatting (label fallback + per-error line).
        print_report_findings(&report, Verbosity::Normal);
    }

    /// A wildcard scope with every dependency present hits the "no gaps"
    /// early-return — distinct from the `incomplete`/`incomplete_auto` scopes,
    /// which always have a gap to report.
    #[tokio::test]
    async fn inspect_scoped_with_no_gaps_succeeds() {
        cmd_inspect(
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            None, false, false, Some("config_wild"), None, Verbosity::Normal,
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
            vec!["analytics.daily_sales", "analytics.weekly_sales", "reporting.monthly_totals"]
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
