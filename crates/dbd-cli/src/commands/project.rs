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

    let total_issues = config_issues.len() + stale_files.len();

    if total_issues == 0 {
        output::info(verbosity, "No issues found — project is up to date.");
        output::summary(0, 0, 0);
        return Ok(());
    }

    if !config_issues.is_empty() {
        output::always(&format!(
            "Found {} config issue{}:",
            config_issues.len(),
            if config_issues.len() != 1 { "s" } else { "" }
        ));
        for issue in &config_issues {
            output::always(&format!("  - {issue}"));
        }
    }

    if !stale_files.is_empty() {
        output::always(&format!(
            "\nFound {} stale file{} (now managed internally by dbd):",
            stale_files.len(),
            if stale_files.len() != 1 { "s" } else { "" }
        ));
        for f in &stale_files {
            output::always(&format!("  - {} — {}", f.path.display(), f.reason));
        }
    }

    if fix {
        let mut fixed = 0;

        if !config_issues.is_empty() {
            let migrated = dbd_core::doctor::migrate_config(&content)
                .context("Config migration failed")?;

            let _: dbd_core::config::DesignConfig = serde_yaml::from_str(&migrated)
                .context("Migrated config failed to parse — please report this as a bug")?;

            let backup = config.with_extension("yaml.bak");
            safe_copy(project_dir, config, &backup)?;
            output::info(verbosity, &format!("Backup saved to {}", backup.display()));

            safe_write(project_dir, config, &migrated)?;
            output::info(verbosity, &format!("Migrated {}", config.display()));
            fixed += config_issues.len();
        }

        if !stale_files.is_empty() {
            let results = dbd_core::doctor::remove_stale_files(&stale_files);
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
        }

        output::summary(0, 0, fixed);
    } else {
        output::always("\nRun with --fix to resolve automatically.");
        output::summary(total_issues, 0, 0);
    }

    Ok(())
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
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    output::info(verbosity, &format!("Deploying from source: {source}"));

    let project_dir = dbd_core::deploy::resolve_source(source)
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
