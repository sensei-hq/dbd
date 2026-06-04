use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::design::ImportComplete;
use dbd_core::Design;

use super::{format_import_summary, get_adapter};
use crate::output::{self, Verbosity};

pub fn cmd_import_dry_run(
    config: &Path,
    env: &str,
    project_dir: &Path,
    name: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    let plan = design.import_plan(name);

    // Step 1: Data loading
    for entry in &plan {
        let file = entry.table.file.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
        let fmt = entry.table.format.as_deref().unwrap_or("csv");
        output::info(verbosity, &format!("  import {} ({}) ← {}", entry.table.name, fmt, file));
    }

    // Step 2: Import procedures
    let has_procedures = plan.iter().any(|e| e.procedure.is_some());
    if has_procedures {
        output::info(verbosity, "");
    }
    for entry in &plan {
        if let Some(ref proc_name) = entry.procedure {
            output::info(verbosity, &format!("  call {proc_name}()"));
        }
    }

    // Step 3: After scripts
    if !design.config().import.after.is_empty() {
        output::info(verbosity, "");
        for after_file in &design.config().import.after {
            output::info(verbosity, &format!("  run {after_file}"));
        }
    }

    output::summary(0, 0, plan.len());
    Ok(())
}

pub async fn cmd_import(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    let adapter = get_adapter(config, database_url).await?;

    let spinner = output::StepSpinner::new(verbosity);
    let mut import_summary: Option<ImportComplete> = None;
    let result = design
        .import_data(
            &*adapter,
            name,
            false,
            None,
            |desc| spinner.start(desc),
            |desc, err| spinner.done(desc, err),
            |s| import_summary = Some(s),
        )
        .await;
    spinner.finish();
    result?;

    if let Some(s) = import_summary {
        output::info(verbosity, &format_import_summary(&s));
    }
    Ok(())
}

pub async fn cmd_export(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    format: &str,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    let adapter = get_adapter(config, database_url).await?;

    // Build a name→format map from config export entries (per-table overrides).
    let config_format_map: std::collections::HashMap<String, String> = design
        .config()
        .export
        .iter()
        .filter_map(|e| e.format().map(|f| (e.name(), f.to_string())))
        .collect();

    // Build export list: either from config export entries, or all tables.
    let tables: Vec<&dbd_core::Entity> = if !design.config().export.is_empty() {
        let export_names: Vec<String> = design.config().export.iter().map(|e| e.name()).collect();
        design.entities().iter()
            .filter(|e| e.entity_type == dbd_core::EntityType::Table)
            .filter(|e| export_names.contains(&e.name))
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .collect()
    } else {
        design.entities().iter()
            .filter(|e| e.entity_type == dbd_core::EntityType::Table)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .collect()
    };

    if tables.is_empty() {
        output::info(verbosity, "No tables to export.");
        return Ok(());
    }

    output::info(verbosity, &format!("Exporting {} table(s)...", tables.len()));

    for table in &tables {
        // Precedence: per-table config override → CLI --format flag → "csv".
        let effective_format = config_format_map
            .get(&table.name)
            .map(|s| s.as_str())
            .unwrap_or(format);

        let mut export_entity = (*table).clone();
        export_entity.format = Some(effective_format.to_string());
        adapter.export_data(&export_entity).await
            .context(format!("Failed to export {}", table.name))?;
        output::detail(verbosity, &format!("  exported {} ({})", table.name, effective_format));
    }

    output::info(verbosity, "Export complete. Files written to export/");
    Ok(())
}
