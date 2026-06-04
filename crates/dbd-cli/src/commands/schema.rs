use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::ApplyComplete;
use dbd_core::Design;

use super::{format_apply_summary, get_adapter, safe_read, safe_write};
use crate::output::{self, Verbosity};

#[allow(clippy::collapsible_if)]
#[allow(clippy::too_many_arguments)]
pub async fn cmd_inspect(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    fix: bool,
    use_database: bool,
    verbosity: Verbosity,
) -> Result<()> {
    let mut design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;

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

    let report = design.report(name, None);
    let total_entities = design.entities().len();

    if verbosity.is_verbose() {
        if let Some(entity) = &report.entity {
            output::always(&serde_json::to_string_pretty(entity)?);
        }
    }

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

    // Auto-format DDL files when --fix is passed
    if fix {
        let format_config = if config.exists() {
            let design_config = dbd_core::config::read(config)?;
            design_config.format
        } else {
            dbd_core::config::FormatConfig::default()
        };

        let files = dbd_core::scanner::scan_ddl(project_dir);
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
    }

    // Report unresolved data.sql TODOs across all migration directories
    let todos = design.data_sql_todos();
    if !todos.is_empty() {
        output::always("\ndata.sql TODOs (resolve before applying):");
        for todo in &todos {
            output::always(&format!("  {} (v{}):", todo.file.display(), todo.version));
            for line in &todo.lines {
                output::always(&format!("    {line}"));
            }
        }
    }

    output::summary(report.issues.len() + todos.len(), report.warnings.len(), total_entities);
    Ok(())
}

pub fn cmd_combine(config: &Path, env: &str, project_dir: &Path, file: &Path, verbosity: Verbosity) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    design.combine(file)?;
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
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;

    if dry_run {
        let entities: Vec<_> = design
            .entities()
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != dbd_core::EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
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

    let spinner = output::StepSpinner::new(verbosity);
    let mut apply_summary: Option<ApplyComplete> = None;
    let result = design
        .apply(
            &*adapter,
            name,
            false,
            None,
            |desc| spinner.start(desc),
            |desc, err| spinner.done(desc, err),
            |s| apply_summary = Some(s),
        )
        .await;
    spinner.finish();
    result?;

    if let Some(s) = apply_summary {
        output::info(verbosity, &format_apply_summary(&s));
    }

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

pub async fn cmd_policies(
    config: &Path,
    project_dir: &Path,
    database_url: Option<&str>,
    dry_run: bool,
    verbosity: Verbosity,
) -> Result<()> {
    if dry_run {
        let files = dbd_core::scanner::scan_policies(project_dir);
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

    let files = dbd_core::scanner::scan_ddl(project_dir);
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
