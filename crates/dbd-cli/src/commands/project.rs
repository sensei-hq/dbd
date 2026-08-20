use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::Progress;
use dbd_core::Design;

use super::{format_deploy_summary, get_adapter, safe_copy, safe_read, safe_write};
use crate::output::{self, Verbosity};

#[allow(clippy::too_many_arguments)]
pub fn cmd_graph(
    config: &Path,
    env: &str,
    project_dir: &Path,
    name: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps)?;
    let graph = design.graph(name, Some(&resolved))?;

    let json = serde_json::json!({
        "nodes": graph.nodes.iter().map(|n| serde_json::json!({
            "name": n.name, "type": n.entity_type, "schema": n.schema,
        })).collect::<Vec<_>>(),
        "edges": graph.edges.iter().map(|e| serde_json::json!({
            "from": e.from, "to": e.to,
        })).collect::<Vec<_>>(),
        "layers": graph.layers,
    });

    output::always(&serde_json::to_string_pretty(&json)?);
    output::detail(
        verbosity,
        &format!("{} nodes, {} edges, {} layers", graph.nodes.len(), graph.edges.len(), graph.layers.len()),
    );
    Ok(())
}

pub fn cmd_dbml(
    config: &Path,
    env: &str,
    project_dir: &Path,
    file: &Path,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    // Filter to the scope's working set so the generated DBML documents only the
    // entities that deploy under this scope. The all-scope keeps everything.
    let resolved = design.resolve_scope(scope, deps)?;
    let entities = design.scoped_entities(&resolved)?;

    let docs = dbd_core::dbml::generate_all(&dbd_core::dbml::DbmlMultiParams {
        entities: &entities,
        project_name: &design.config().project.name,
        database_type: &design.config().source.dialect,
        project_note: design.config().project.note.as_deref(),
        docs: &design.config().dbml,
    });

    // Single document → use the user-supplied path verbatim. Multiple
    // documents → write each to `<parent_of(file)>/<doc.file_name>`,
    // preserving the user's directory choice while honoring each doc's
    // configured filename.
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let written = match docs.len() {
        0 => 0,
        1 => {
            safe_write(project_dir, file, &docs[0].content)?;
            output::info(verbosity, &format!("Generated DBML in {}", file.display()));
            1
        }
        _ => {
            for doc in &docs {
                let path = dir.join(&doc.file_name);
                safe_write(project_dir, &path, &doc.content)?;
                output::info(verbosity, &format!("Generated DBML in {}", path.display()));
            }
            docs.len()
        }
    };
    output::detail(verbosity, &format!("Wrote {written} DBML document(s)"));
    Ok(())
}

pub fn cmd_doctor(config: &Path, fix: bool, verbosity: Verbosity) -> Result<()> {
    if !config.exists() {
        anyhow::bail!("Config file not found: {}", config.display());
    }

    let project_dir = config.parent().unwrap_or(Path::new("."));
    let content = safe_read(project_dir, config)?;

    let config_issues = dbd_core::doctor::detect_old_format(&content);
    let stale_files = dbd_core::doctor::detect_stale_files(project_dir);
    let plural_dirs = dbd_core::doctor::detect_plural_ddl_dirs(project_dir);
    // Misfiled view/matview DDL — detection only; the fix is a manual move.
    let mismatches = dbd_core::doctor::detect_ddl_type_mismatches(project_dir);

    let auto_fixable = config_issues.len() + stale_files.len() + plural_dirs.len();
    let total_issues = auto_fixable + mismatches.len();

    if total_issues == 0 {
        output::info(verbosity, "No issues found — project is up to date.");
        output::summary(0, 0, 0);
        return Ok(());
    }

    report_doctor_issues(&config_issues, &stale_files, &plural_dirs, &mismatches);

    if !fix {
        if auto_fixable > 0 {
            output::always("\nRun with --fix to resolve automatically.");
        }
        if !mismatches.is_empty() {
            output::always("Misfiled DDL files must be moved manually (see above).");
        }
        output::summary(total_issues, 0, 0);
        return Ok(());
    }

    let mut fixed = 0;
    if !config_issues.is_empty() {
        fix_config_migration(config, project_dir, &content, verbosity)?;
        fixed += config_issues.len();
    }
    if !stale_files.is_empty() {
        fixed += fix_stale_files(&stale_files, verbosity);
    }
    if !plural_dirs.is_empty() {
        fixed += fix_plural_dirs(&plural_dirs, verbosity);
    }
    // Misfiled DDL is not auto-fixed: report it as remaining so the summary is
    // honest, and reset stays broken until the file is moved.
    if !mismatches.is_empty() {
        output::always(&format!(
            "\n{} misfiled DDL file{} left unchanged — move manually:",
            mismatches.len(),
            if mismatches.len() != 1 { "s" } else { "" }
        ));
        for m in &mismatches {
            output::always(&format!("  - {} → {}", m.path.display(), m.suggested_path.display()));
        }
    }
    output::summary(mismatches.len(), 0, fixed);

    Ok(())
}

/// Print the config / stale-file / plural-folder issues doctor detected.
fn report_doctor_issues(
    config_issues: &[String],
    stale_files: &[dbd_core::doctor::StaleFile],
    plural_dirs: &[dbd_core::doctor::PluralDdlDir],
    mismatches: &[dbd_core::doctor::DdlTypeMismatch],
) {
    if !config_issues.is_empty() {
        output::always(&format!(
            "Found {} config issue{}:",
            config_issues.len(),
            if config_issues.len() != 1 { "s" } else { "" }
        ));
        for issue in config_issues {
            output::always(&format!("  - {issue}"));
        }
    }

    if !stale_files.is_empty() {
        output::always(&format!(
            "\nFound {} stale file{} (now managed internally by dbd):",
            stale_files.len(),
            if stale_files.len() != 1 { "s" } else { "" }
        ));
        for f in stale_files {
            output::always(&format!("  - {} — {}", f.path.display(), f.reason));
        }
    }

    if !plural_dirs.is_empty() {
        output::always(&format!(
            "\nFound {} plural DDL folder{} (singular is canonical):",
            plural_dirs.len(),
            if plural_dirs.len() != 1 { "s" } else { "" }
        ));
        for d in plural_dirs {
            output::always(&format!("  - {} → {}", d.plural.display(), d.singular.display()));
        }
    }

    if !mismatches.is_empty() {
        output::always(&format!(
            "\nFound {} misfiled DDL file{} (folder disagrees with the CREATE statement):",
            mismatches.len(),
            if mismatches.len() != 1 { "s" } else { "" }
        ));
        for m in mismatches {
            output::always(&format!(
                "  - {} declares CREATE {} but lives in ddl/{}/",
                m.path.display(),
                m.declared.replace('_', " ").to_uppercase(),
                m.folder
            ));
            output::always(&format!("      → move to {}", m.suggested_path.display()));
        }
    }
}

/// Migrate an old-format config in place, validating the result and backing up
/// the original to a `.yaml.bak` sibling first.
fn fix_config_migration(
    config: &Path,
    project_dir: &Path,
    content: &str,
    verbosity: Verbosity,
) -> Result<()> {
    let migrated = dbd_core::doctor::migrate_config(content).context("Config migration failed")?;

    let _: dbd_core::config::DesignConfig = serde_yaml::from_str(&migrated)
        .context("Migrated config failed to parse — please report this as a bug")?;

    let backup = config.with_extension("yaml.bak");
    safe_copy(project_dir, config, &backup)?;
    output::info(verbosity, &format!("Backup saved to {}", backup.display()));

    safe_write(project_dir, config, &migrated)?;
    output::info(verbosity, &format!("Migrated {}", config.display()));
    Ok(())
}

/// Remove stale internally-managed files, returning how many were removed.
fn fix_stale_files(stale_files: &[dbd_core::doctor::StaleFile], verbosity: Verbosity) -> usize {
    let mut fixed = 0;
    let results = dbd_core::doctor::remove_stale_files(stale_files);
    for (path, err) in &results {
        match err {
            None => {
                output::info(verbosity, &format!("Removed {}", path.display()));
                fixed += 1;
            }
            Some(e) => {
                output::always(&format!("Failed to remove {}: {e}", path.display()));
            }
        }
    }
    fixed
}

/// Migrate plural DDL folders to their singular canonical form, returning how
/// many moves succeeded.
fn fix_plural_dirs(plural_dirs: &[dbd_core::doctor::PluralDdlDir], verbosity: Verbosity) -> usize {
    use dbd_core::doctor::DdlMoveOutcome;
    let mut fixed = 0;
    for d in plural_dirs {
        for outcome in dbd_core::doctor::migrate_plural_ddl_dir(d) {
            match outcome {
                DdlMoveOutcome::RenamedDir { from, to } => {
                    output::info(verbosity, &format!("Renamed {} → {}", from.display(), to.display()));
                    fixed += 1;
                }
                DdlMoveOutcome::MovedFile { from, to } => {
                    output::info(verbosity, &format!("Moved {} → {}", from.display(), to.display()));
                    fixed += 1;
                }
                DdlMoveOutcome::BackedUp { winner, loser, final_path, backup } => {
                    output::always(&format!(
                        "Collision at {}: kept newer (from {}), backed up older (from {}) → {}",
                        final_path.display(),
                        winner.display(),
                        loser.display(),
                        backup.display()
                    ));
                    fixed += 1;
                }
                DdlMoveOutcome::Error { path, error } => {
                    output::always(&format!("Failed to migrate {}: {error}", path.display()));
                }
            }
        }
    }
    fixed
}

pub fn cmd_init(project_dir: &Path, name: &str, target: &str, verbosity: Verbosity) -> Result<()> {
    let files = dbd_core::init::create_project(project_dir, name, target)
        .context("Failed to initialize project")?;

    output::info(verbosity, &format!("Initialized dbd project '{name}' ({target})"));
    for file in &files {
        if !file.content.is_empty() {
            output::detail(verbosity, &format!("  created {}", file.path.display()));
        }
    }
    output::info(verbosity, "\nNext steps:");
    output::info(verbosity, "  1. Set DATABASE_URL or edit design.yaml target.url");
    output::info(verbosity, "  2. Add DDL files to ddl/table/<schema>/<name>.ddl");
    output::info(verbosity, "  3. Run 'dbd inspect' to validate");
    output::info(verbosity, "  4. Run 'dbd apply' to create schema in database");

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_deploy(
    source: &str,
    _config_name: &Path,
    env: &str,
    database_url: Option<&str>,
    dry_run: bool,
    no_cache: bool,
    clear_cache: bool,
    allow_scope_change: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    output::info(verbosity, &format!("Deploying from source: {source}"));

    if clear_cache {
        dbd_core::deploy::clear_cache().context("Failed to clear cache")?;
        output::info(verbosity, "Cleared download cache");
    }

    let project_dir = dbd_core::deploy::resolve_source(source, no_cache)
        .await
        .context("Failed to resolve source")?;

    let config_path = project_dir.join("design.yaml");
    if !config_path.exists() {
        anyhow::bail!("No design.yaml found in {}", project_dir.display());
    }

    let mut design = Design::from_config_with_dir(&config_path, env, Some(&project_dir))
        .context("Failed to load design from source")?;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;

    if dry_run {
        // Surface the same gap/closure errors a real deploy would.
        design.check_scope_gaps(&resolved).context("scope check failed")?;
        let report = design.report(None, Some(&resolved));
        if !resolved.is_all {
            for gap in &report.gaps {
                output::always(&format!(
                    "✗ dependency gap: {} requires {} (out of scope)\n    chain: {}",
                    gap.required_by,
                    gap.missing,
                    gap.chain.join(" → ")
                ));
            }
        }
        output::info(verbosity, &format!(
            "{} entities found, {} errors, {} warnings",
            design.entities().len(),
            report.issues.len(),
            report.warnings.len(),
        ));

        // Preview the import the same way the real run reports it: always state
        // the file count — including zero — and why anything was left out.
        let ws = design.working_set(&resolved)?;
        let import_plan: Vec<_> = design
            .import_plan(None)
            .into_iter()
            .filter(|e| dbd_core::design::import_entry_in_scope(e, &ws, resolved.is_all))
            .collect();
        output::info(
            verbosity,
            &format!("{} data file(s) would be imported.", import_plan.len()),
        );
        for warning in design.import_warnings(None) {
            output::warn(&warning);
        }

        let policy_files = dbd_core::scanner::scan_policies(&project_dir)?;
        output::info(
            verbosity,
            &format!("{} policy file(s) would be applied.", policy_files.len()),
        );
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

    let adapter = get_adapter(&config_path, database_url).await?;
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, allow_scope_change)?;

    // Delegate to the library's `Design::deploy` — apply + import + policies —
    // so `dbd deploy` and an embedder calling the crate run the same pipeline.
    output::info(verbosity, "Applying schema...");
    let mut summary: Option<dbd_core::design::DeployComplete> = None;
    {
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .deploy_with_progress(
                &*adapter,
                false,
                Some(&resolved),
                Progress {
                    on_start: |desc: &str| spinner.start(desc),
                    on_done: |desc: &str, err: Option<&str>| spinner.done(desc, err),
                    on_complete: |s| summary = Some(s),
                },
            )
            .await;
        spinner.finish();
        result.context("Deploy failed")?;
    }

    let summary = summary.unwrap_or_default();
    if !summary.policies.applied.is_empty() {
        output::info(verbosity, &format!("Applied {} policy file(s).", summary.policies.applied.len()));
    }
    // Non-fatal diagnostics — skipped imports and failed policy files — are
    // always surfaced, regardless of verbosity. A deploy that quietly loaded
    // nothing is the failure mode this reporting exists to prevent.
    for warning in summary.warnings() {
        output::warn(&warning);
    }
    output::info(verbosity, &format_deploy_summary(&summary));
    Ok(())
}

/// Reconcile the live database to the design in place (pre-release workflow).
///
/// Gated on `project.released`: once a project is released, schema changes must
/// go through `dbd snapshot` + `dbd apply` instead.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_reconcile(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    dry_run: bool,
    allow_destructive: bool,
    prune: bool,
    allow_scope_change: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    if design.config().project.released {
        anyhow::bail!(
            "Project is released — `reconcile` is disabled. \
             Capture changes with `dbd snapshot`, then migrate with `dbd apply`."
        );
    }

    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let adapter = get_adapter(config, database_url).await?;

    if dry_run {
        let plan = design
            .reconcile(&*adapter, true, allow_destructive, prune, Some(&resolved), Progress::none())
            .await
            .context("Reconcile planning failed")?;
        print_reconcile_plan(&plan, prune, verbosity);
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, allow_scope_change)?;

    output::info(verbosity, "Reconciling schema to design...");
    let mut summary = None;
    let plan = {
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .reconcile(
                &*adapter,
                false,
                allow_destructive,
                prune,
                Some(&resolved),
                Progress {
                    on_start: |desc: &str| spinner.start(desc),
                    on_done: |desc: &str, err: Option<&str>| spinner.done(desc, err),
                    on_complete: |s| summary = Some(s),
                },
            )
            .await;
        spinner.finish();
        result.context("Reconcile failed")?
    };

    for w in &plan.warnings {
        output::always(&format!("⚠ {w}"));
    }
    // Orphaned tables left untouched (only pruned with --prune).
    if !prune && !plan.dropped.is_empty() {
        output::always(&format!(
            "{} orphaned table(s) not in the design were left untouched (re-run with --prune to drop):",
            plan.dropped.len()
        ));
        for s in &plan.dropped {
            output::always(&format!("    {}", s.entity_name));
        }
    }
    if let Some(s) = summary {
        output::info(
            verbosity,
            &format!(
                "Reconciled — {} created, {} altered, {} re-applied, {} pruned.",
                s.created, s.altered, s.reapplied, s.dropped
            ),
        );
    }
    Ok(())
}

/// Print a reconcile plan (used by `--dry-run`).
fn print_reconcile_plan(plan: &dbd_core::ReconcilePlan, prune: bool, verbosity: Verbosity) {
    for line in reconcile_plan_lines(plan, prune, verbosity) {
        output::always(&line);
    }
}

/// Build the display lines for a reconcile plan. Pure so it can be unit-tested.
///
/// In verbose mode each altered entity's ALTER SQL is emitted (indented) beneath
/// its summary line; normal mode shows only the one-line-per-entity summary,
/// matching the CLI convention that SQL is a `-v` detail.
fn reconcile_plan_lines(plan: &dbd_core::ReconcilePlan, prune: bool, verbosity: Verbosity) -> Vec<String> {
    // `is_empty()` already covers added/altered/dropped/matview_creates, but
    // warnings live in their own channel — a plan whose ONLY content is a matview
    // drift warning must still print it rather than falsely claiming "in sync".
    if plan.is_empty() && plan.dropped.is_empty() && plan.warnings.is_empty() {
        return vec!["Already in sync — no changes.".to_string()];
    }
    let mut lines = Vec::new();
    for name in &plan.added {
        lines.push(format!("  + create {name}"));
    }
    for name in &plan.matview_creates {
        lines.push(format!("  + create materialized view {name}"));
    }
    for s in &plan.altered {
        lines.push(format!("  ~ alter  {}", s.entity_name));
        // Verbose surfaces the column-level ALTER SQL beneath the summary line.
        if verbosity.is_verbose() {
            for sql_line in s.sql.lines() {
                lines.push(format!("      {sql_line}"));
            }
        }
    }
    for s in &plan.dropped {
        if prune {
            lines.push(format!("  - prune  {}", s.entity_name));
        } else {
            lines.push(format!("  · orphan {} (use --prune to drop)", s.entity_name));
        }
    }
    for w in &plan.warnings {
        lines.push(format!("  ⚠ {w}"));
    }
    if plan.destructive && !plan.altered.is_empty() {
        lines.push("This plan drops columns/constraints — re-run with --allow-destructive to apply.".to_string());
    }
    lines
}

/// Release the current version: write a baseline snapshot and set the released
/// flag, locking the project into the snapshot/migration workflow.
pub fn cmd_release(
    config: &Path,
    env: &str,
    project_dir: &Path,
    name: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    if design.config().project.released {
        anyhow::bail!("Project is already released.");
    }
    if dbd_core::snapshot::has_snapshots(project_dir) {
        anyhow::bail!(
            "Snapshots already exist — the project is already on the migration track. \
             Use `dbd snapshot` for subsequent versions."
        );
    }

    let error_count = design.entities().iter().filter(|e| !e.errors.is_empty()).count();
    if error_count > 0 {
        anyhow::bail!(
            "Design has {error_count} entity error(s); fix them before releasing (run `dbd inspect`)."
        );
    }

    let version = design.config().project.version.unwrap_or(1);
    let desc = name.unwrap_or("baseline release");
    dbd_core::snapshot::create_baseline_snapshot(design.entities(), project_dir, config, desc, version)
        .context("Failed to create baseline snapshot")?;
    dbd_core::config::set_released(config, true).context("Failed to set released flag")?;

    output::always(&format!(
        "✓ Released v{version} — baseline snapshot written; `reconcile` is now disabled."
    ));
    output::info(
        verbosity,
        "Next changes: edit the design → `dbd snapshot` → `dbd apply`.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil;
    use dbd_core::reconcile::ReconcileStatement;

    /// A plan with one altered table carrying column-level ALTER SQL.
    fn altered_plan() -> dbd_core::ReconcilePlan {
        dbd_core::ReconcilePlan {
            altered: vec![ReconcileStatement {
                entity_name: "public.users".to_string(),
                sql: "ALTER TABLE public.users ADD COLUMN email text;".to_string(),
            }],
            ..Default::default()
        }
    }

    /// Verbose `--dry-run` surfaces the column-level ALTER SQL under each altered
    /// entity — the detail a plain summary omits.
    #[test]
    fn reconcile_plan_verbose_emits_alter_sql() {
        let out = reconcile_plan_lines(&altered_plan(), false, Verbosity::Verbose).join("\n");
        assert!(out.contains("~ alter") && out.contains("public.users"), "summary line missing:\n{out}");
        assert!(
            out.contains("ADD COLUMN email text"),
            "verbose output must include the ALTER SQL; got:\n{out}"
        );
    }

    /// Normal `--dry-run` stays a terse summary: entity names, no SQL.
    #[test]
    fn reconcile_plan_normal_hides_alter_sql() {
        let out = reconcile_plan_lines(&altered_plan(), false, Verbosity::Normal).join("\n");
        assert!(out.contains("~ alter") && out.contains("public.users"), "summary line missing:\n{out}");
        assert!(!out.contains("ADD COLUMN"), "normal output must NOT include ALTER SQL; got:\n{out}");
    }

    /// `graph` loads the fixture design and emits the node/edge/layer JSON.
    #[test]
    fn graph_emits_json_from_fixture() {
        cmd_graph(&testutil::fixture_config(), "dev", &testutil::fixtures(), None, None, None, Verbosity::Verbose).unwrap();
    }

    /// `dbml` writes generated document(s) under the project — run against a
    /// copy so the repo fixture is never touched.
    #[test]
    fn dbml_writes_into_temp_project() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        let out = proj.path().join("schema.dbml");
        cmd_dbml(&cfg, "dev", proj.path(), &out, None, None, Verbosity::Normal).unwrap();
    }

    /// A missing config bails with an actionable "not found" error.
    #[test]
    fn doctor_bails_when_config_missing() {
        let missing = std::path::Path::new("/definitely/not/here/design.yaml");
        let err = cmd_doctor(missing, false, Verbosity::Normal).unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    /// Doctor is read-only without `--fix` and returns Ok on the fixture.
    #[test]
    fn doctor_runs_read_only_on_fixture() {
        cmd_doctor(&testutil::fixture_config(), false, Verbosity::Normal).unwrap();
    }

    /// `init` scaffolds a project into an empty directory.
    #[test]
    fn init_creates_project_files() {
        let tmp = tempfile::tempdir().unwrap();
        cmd_init(tmp.path(), "demo", "postgres", Verbosity::Normal).unwrap();
        assert!(tmp.path().join("design.yaml").exists());
    }

    /// `deploy --dry-run` resolves a local source and reports without applying,
    /// so it never opens a DB connection.
    #[tokio::test]
    async fn deploy_dry_run_from_local_source() {
        let src = testutil::fixtures();
        cmd_deploy(
            src.to_str().unwrap(), &testutil::fixture_config(), "dev", None,
            /*dry_run*/ true, /*no_cache*/ true, /*clear_cache*/ false, /*allow_scope_change*/ false,
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Deploy treats `policies/` as part of the deploy: a project with a policy
    /// file makes `deploy --dry-run` walk the policy step (scan + report) without
    /// error. Guards against the regression where deploy silently skipped RLS.
    #[tokio::test]
    async fn deploy_dry_run_includes_policies() {
        let proj = testutil::copy_fixture_project();
        std::fs::create_dir_all(proj.path().join("policies")).unwrap();
        std::fs::write(proj.path().join("policies").join("secrets.sql"), "-- rls policy\n").unwrap();
        let cfg = proj.path().join("design.yaml");
        cmd_deploy(
            proj.path().to_str().unwrap(), &cfg, "dev", None,
            /*dry_run*/ true, /*no_cache*/ true, /*clear_cache*/ false, /*allow_scope_change*/ false,
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Releasing writes a baseline snapshot and flips the released flag; a second
    /// release is then refused. Runs against a throwaway copy.
    #[test]
    fn release_writes_baseline_then_refuses_second() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_release(&cfg, "dev", proj.path(), Some("v1"), Verbosity::Normal).unwrap();
        let err = cmd_release(&cfg, "dev", proj.path(), None, Verbosity::Normal).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("already released") || msg.contains("Snapshots already exist"),
            "expected a released/snapshot guard, got: {msg}"
        );
    }

    /// A project with a `snapshots/` dir but no `released` flag set is already
    /// on the migration track — `release` refuses even before checking for
    /// entity errors.
    #[test]
    fn release_refuses_when_snapshots_exist_without_released_flag() {
        let proj = testutil::copy_fixture_project();
        std::fs::create_dir_all(proj.path().join("snapshots")).unwrap();
        std::fs::write(proj.path().join("snapshots").join("001.json"), "{}").unwrap();
        let cfg = proj.path().join("design.yaml");

        let err = cmd_release(&cfg, "dev", proj.path(), None, Verbosity::Normal).unwrap_err();
        assert!(err.to_string().contains("Snapshots already exist"), "got: {err}");
    }

    /// A design with an entity parse error refuses to release — releasing a
    /// broken design would bake the error into the baseline snapshot.
    #[test]
    fn release_refuses_when_an_entity_has_errors() {
        let proj = testutil::copy_fixture_project();
        std::fs::write(
            proj.path().join("ddl/table/config/broken.ddl"),
            "create table config.broken (!!! not valid sql !!!);\n",
        )
        .unwrap();
        let cfg = proj.path().join("design.yaml");

        let err = cmd_release(&cfg, "dev", proj.path(), None, Verbosity::Normal).unwrap_err();
        assert!(err.to_string().contains("entity error"), "got: {err}");
    }

    /// `reconcile` is disabled once a project is released — this must be
    /// refused before ever touching a DB adapter.
    #[tokio::test]
    async fn reconcile_refuses_when_project_released() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_release(&cfg, "dev", proj.path(), Some("v1"), Verbosity::Normal).unwrap();

        let err = cmd_reconcile(
            &cfg, "dev", proj.path(), None, /*dry_run*/ true, /*allow_destructive*/ false,
            /*prune*/ false, /*allow_scope_change*/ false, None, None, Verbosity::Normal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("released"), "got: {err}");
    }

    /// `deploy --clear-cache` clears the local download cache before the rest
    /// of the (local, no-DB) dry-run flow proceeds.
    #[tokio::test]
    async fn deploy_clears_cache_before_dry_run() {
        let src = testutil::fixtures();
        cmd_deploy(
            src.to_str().unwrap(), &testutil::fixture_config(), "dev", None,
            /*dry_run*/ true, /*no_cache*/ true, /*clear_cache*/ true, /*allow_scope_change*/ false,
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// A source directory that exists but has no `design.yaml` is a clear,
    /// actionable error — resolved entirely from the local filesystem.
    #[tokio::test]
    async fn deploy_bails_when_source_has_no_design_yaml() {
        let empty = tempfile::tempdir().unwrap();
        let err = cmd_deploy(
            empty.path().to_str().unwrap(), &testutil::fixture_config(), "dev", None,
            /*dry_run*/ true, /*no_cache*/ true, /*clear_cache*/ false, /*allow_scope_change*/ false,
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("No design.yaml found"), "got: {err}");
    }

    /// A scope with `deps: include` doesn't block on its dependency gap (that's
    /// the whole point of `include`), but `deploy --dry-run` still surfaces the
    /// gap as an FYI — exercising the gap-reporting loop that a `Report`-policy
    /// gap never reaches (it bails earlier, in `check_scope_gaps`).
    #[tokio::test]
    async fn deploy_dry_run_reports_gap_for_scope_with_include_deps() {
        let src = testutil::fixtures();
        cmd_deploy(
            src.to_str().unwrap(), &testutil::fixture_config(), "dev", None,
            /*dry_run*/ true, /*no_cache*/ true, /*clear_cache*/ false, /*allow_scope_change*/ false,
            Some("incomplete_auto"), None, Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// A design.yaml with old-format markers, a stale internally-managed file,
    /// and a plural DDL folder all get reported (not touched) without `--fix`.
    fn write_legacy_project(dir: &std::path::Path) -> std::path::PathBuf {
        let config = dir.join("design.yaml");
        std::fs::write(
            &config,
            "project:\n  name: legacy\n  database: postgresql\nroles:\n  - name: admin\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("ddl/procedure/staging")).unwrap();
        std::fs::write(dir.join("ddl/procedure/staging/import_jsonb_to_table.ddl"), "-- stale\n").unwrap();
        std::fs::create_dir_all(dir.join("ddl/tables/public")).unwrap();
        std::fs::write(dir.join("ddl/tables/public/thing.ddl"), "create table public.thing (id int);\n").unwrap();
        config
    }

    #[test]
    fn doctor_reports_issues_without_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let config = write_legacy_project(tmp.path());
        let original = std::fs::read_to_string(&config).unwrap();

        cmd_doctor(&config, /*fix*/ false, Verbosity::Normal).unwrap();

        // Report-only: nothing on disk changes.
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        assert!(tmp.path().join("ddl/procedure/staging/import_jsonb_to_table.ddl").exists());
        assert!(tmp.path().join("ddl/tables").exists());
    }

    #[test]
    fn doctor_fix_migrates_backs_up_and_cleans_up() {
        let tmp = tempfile::tempdir().unwrap();
        let config = write_legacy_project(tmp.path());
        let original = std::fs::read_to_string(&config).unwrap();

        cmd_doctor(&config, /*fix*/ true, Verbosity::Normal).unwrap();

        // Backup preserves the original content.
        let backup = tmp.path().join("design.yaml.bak");
        assert!(backup.exists(), "expected a .yaml.bak backup");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);

        // The migrated config is valid new-format and parses cleanly.
        dbd_core::config::read(&config).expect("migrated config should parse as DesignConfig");

        // The stale file is gone.
        assert!(!tmp.path().join("ddl/procedure/staging/import_jsonb_to_table.ddl").exists());

        // The plural folder was renamed to its singular form.
        assert!(!tmp.path().join("ddl/tables").exists());
        assert!(tmp.path().join("ddl/table/public/thing.ddl").exists());
    }

    /// `dbml` writes one file per configured document when more than one is
    /// declared, using each doc's own filter/output settings.
    #[test]
    fn dbml_writes_one_file_per_configured_document() {
        let proj = testutil::copy_fixture_project();
        let cfg_path = proj.path().join("design.yaml");
        let original = std::fs::read_to_string(&cfg_path).unwrap();
        let needle = "dbml:\n  base:\n    exclude:\n      schemas:\n        - staging\n        - extensions\n";
        assert!(original.contains(needle), "fixture dbml block shape changed — update this test's patch");
        let patched = original.replace(
            needle,
            "dbml:\n  base:\n    exclude:\n      schemas:\n        - staging\n        - extensions\n  extra:\n    output: extra.dbml\n    include:\n      schemas:\n        - config\n",
        );
        std::fs::write(&cfg_path, patched).unwrap();

        let out = proj.path().join("schema.dbml"); // filename ignored in the multi-doc branch; only its dir matters
        cmd_dbml(&cfg_path, "dev", proj.path(), &out, None, None, Verbosity::Normal).unwrap();

        assert!(proj.path().join("base.dbml").exists());
        let extra_path = proj.path().join("extra.dbml");
        assert!(extra_path.exists());
        let extra_content = std::fs::read_to_string(&extra_path).unwrap();
        assert!(extra_content.contains("config"), "extra.dbml should contain the config-schema tables");
        assert!(!extra_content.contains("staging"), "extra.dbml should be filtered to the config schema only");
    }

    /// `reconcile_plan_lines` on an empty plan reports the in-sync message —
    /// the early-return branch the other reconcile tests never take.
    #[test]
    fn reconcile_plan_lines_empty_plan_reports_in_sync() {
        let plan = dbd_core::ReconcilePlan::default();
        let out = reconcile_plan_lines(&plan, false, Verbosity::Normal).join("\n");
        assert_eq!(out, "Already in sync — no changes.");
    }

    /// Added/dropped entities, warnings, and the destructive-change notice all
    /// render — dropped entities render differently depending on `--prune`.
    #[test]
    fn reconcile_plan_lines_covers_added_dropped_warnings_and_destructive() {
        let plan = dbd_core::ReconcilePlan {
            added: vec!["public.new_table".to_string()],
            matview_creates: vec!["analytics.daily_sales".to_string()],
            altered: vec![ReconcileStatement {
                entity_name: "public.users".to_string(),
                sql: "ALTER TABLE public.users ADD COLUMN email text;".to_string(),
            }],
            dropped: vec![ReconcileStatement {
                entity_name: "public.orphan".to_string(),
                sql: "DROP TABLE public.orphan CASCADE;".to_string(),
            }],
            warnings: vec!["enum value dropped".to_string()],
            destructive: true,
        };

        let pruned = reconcile_plan_lines(&plan, true, Verbosity::Normal).join("\n");
        assert!(pruned.contains("+ create public.new_table"), "got: {pruned}");
        assert!(
            pruned.contains("+ create materialized view analytics.daily_sales"),
            "got: {pruned}"
        );
        assert!(pruned.contains("~ alter  public.users"), "got: {pruned}");
        assert!(pruned.contains("- prune  public.orphan"), "got: {pruned}");
        assert!(pruned.contains("⚠ enum value dropped"), "got: {pruned}");
        assert!(pruned.contains("--allow-destructive"), "got: {pruned}");

        let unpruned = reconcile_plan_lines(&plan, false, Verbosity::Normal).join("\n");
        assert!(
            unpruned.contains("· orphan public.orphan (use --prune to drop)"),
            "got: {unpruned}"
        );
    }

    /// A plan whose ONLY content is a matview drift warning must NOT report
    /// "Already in sync" — it renders the `⚠` line so real drift is surfaced
    /// rather than hidden (warnings live outside `is_empty()`).
    #[test]
    fn reconcile_plan_lines_warning_only_is_not_in_sync() {
        let plan = dbd_core::ReconcilePlan {
            warnings: vec!["materialized view app.mv: definition differs from the deployed object".to_string()],
            ..Default::default()
        };
        let out = reconcile_plan_lines(&plan, false, Verbosity::Normal).join("\n");
        assert!(
            !out.contains("Already in sync"),
            "a drift-only plan must not claim in-sync; got: {out}"
        );
        assert!(
            out.contains("⚠ materialized view app.mv: definition differs"),
            "the warning must render; got: {out}"
        );
    }

    /// `print_reconcile_plan` is the thin `output::always`-per-line wrapper
    /// around `reconcile_plan_lines` — just needs to run without panicking.
    #[test]
    fn print_reconcile_plan_runs_for_a_populated_plan() {
        print_reconcile_plan(&altered_plan(), false, Verbosity::Normal);
    }
}
