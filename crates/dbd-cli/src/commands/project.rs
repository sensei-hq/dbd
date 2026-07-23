use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::{ApplyComplete, ApplyStrategy, ImportComplete};
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

    let total_issues = config_issues.len() + stale_files.len() + plural_dirs.len();

    if total_issues == 0 {
        output::info(verbosity, "No issues found — project is up to date.");
        output::summary(0, 0, 0);
        return Ok(());
    }

    report_doctor_issues(&config_issues, &stale_files, &plural_dirs);

    if !fix {
        output::always("\nRun with --fix to resolve automatically.");
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
    output::summary(0, 0, fixed);

    Ok(())
}

/// Print the config / stale-file / plural-folder issues doctor detected.
fn report_doctor_issues(
    config_issues: &[String],
    stale_files: &[dbd_core::doctor::StaleFile],
    plural_dirs: &[dbd_core::doctor::PluralDdlDir],
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
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

    let adapter = get_adapter(&config_path, database_url).await?;
    output::info(verbosity, "Applying schema...");
    let mut apply_summary: Option<ApplyComplete> = None;
    {
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .apply(
                &*adapter,
                None,
                false,
                Some(&resolved),
                |desc| spinner.start(desc),
                |desc, err| spinner.done(desc, err),
                |s| apply_summary = Some(s),
            )
            .await;
        spinner.finish();
        result.context("Apply failed")?;
    }

    let mut import_summary: Option<ImportComplete> = None;
    let ws = design.working_set(&resolved)?;
    let import_plan: Vec<_> = design
        .import_plan(None)
        .into_iter()
        .filter(|e| dbd_core::design::import_entry_in_scope(e, &ws, resolved.is_all))
        .collect();
    if !import_plan.is_empty() {
        output::info(verbosity, &format!("Importing {} data file(s)...", import_plan.len()));
        let spinner = output::StepSpinner::new(verbosity);
        let result = design
            .import_data(
                &*adapter,
                None,
                false,
                Some(&resolved),
                |desc| spinner.start(desc),
                |desc, err| spinner.done(desc, err),
                |s| import_summary = Some(s),
            )
            .await;
        spinner.finish();
        result.context("Import failed")?;
    }

    let summary = dbd_core::design::DeployComplete {
        apply: apply_summary.unwrap_or(dbd_core::design::ApplyComplete {
            strategy: ApplyStrategy::Current,
            from_version: 0,
            to_version: 0,
            applied: 0,
            migrated: 0,
            created: 0,
            dropped: 0,
        }),
        import: import_summary.unwrap_or(ImportComplete {
            tables: 0,
            procedures: 0,
            after_scripts: 0,
        }),
    };
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
            .reconcile(&*adapter, true, allow_destructive, prune, Some(&resolved), |_| {}, |_, _| {}, |_| {})
            .await
            .context("Reconcile planning failed")?;
        print_reconcile_plan(&plan, prune, verbosity);
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

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
                |desc| spinner.start(desc),
                |desc, err| spinner.done(desc, err),
                |s| summary = Some(s),
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
    if plan.is_empty() && plan.dropped.is_empty() {
        output::info(verbosity, "Already in sync — no changes.");
        return;
    }
    for name in &plan.added {
        output::always(&format!("  + create {name}"));
    }
    for s in &plan.altered {
        output::always(&format!("  ~ alter  {}", s.entity_name));
    }
    for s in &plan.dropped {
        if prune {
            output::always(&format!("  - prune  {}", s.entity_name));
        } else {
            output::always(&format!("  · orphan {} (use --prune to drop)", s.entity_name));
        }
    }
    for w in &plan.warnings {
        output::always(&format!("  ⚠ {w}"));
    }
    if plan.destructive && !plan.altered.is_empty() {
        output::always("This plan drops columns/constraints — re-run with --allow-destructive to apply.");
    }
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
            /*dry_run*/ true, /*no_cache*/ true, /*clear_cache*/ false, None, None, Verbosity::Normal,
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
}
