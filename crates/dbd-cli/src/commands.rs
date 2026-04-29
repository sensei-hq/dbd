use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use crate::cli::Commands;
use crate::output::{self, Verbosity};

pub async fn run(
    command: &Commands,
    config: &Path,
    env: &str,
    database_url: Option<&str>,
    project_dir: &Path,
    verbosity: Verbosity,
) -> Result<()> {
    match command {
        Commands::Inspect { name } => cmd_inspect(config, env, project_dir, name.as_deref(), verbosity),

        Commands::Combine { file } => cmd_combine(config, env, project_dir, file, verbosity),

        Commands::Graph { name } => cmd_graph(config, env, project_dir, name.as_deref(), verbosity),

        Commands::Apply { name, dry_run } => {
            cmd_apply(config, env, project_dir, database_url, name.as_deref(), *dry_run, verbosity).await
        }

        Commands::Import { name, dry_run } => {
            if *dry_run {
                cmd_import_dry_run(config, env, project_dir, name.as_deref(), verbosity)
            } else {
                cmd_import(config, env, project_dir, database_url, name.as_deref(), verbosity).await
            }
        }

        Commands::Reset { target, dry_run, force } => {
            cmd_reset(config, env, project_dir, database_url, target, *dry_run, *force, verbosity).await
        }

        Commands::Snapshot { list, .. } => {
            if *list {
                cmd_snapshot_list(project_dir, verbosity);
                return Ok(());
            }
            output::info(verbosity, "Snapshot creation not yet implemented in Rust CLI");
            Ok(())
        }

        Commands::Migrate { status, apply: _, to: _, dry_run: _ } => {
            if *status {
                output::info(verbosity, "Migrate status not yet implemented in Rust CLI");
            } else {
                output::info(verbosity, "Migrate apply not yet implemented in Rust CLI");
            }
            Ok(())
        }

        Commands::Deploy { dry_run: _ } => {
            output::info(verbosity, "Deploy not yet implemented in Rust CLI");
            Ok(())
        }

        Commands::Dbml { file } => cmd_dbml(config, env, project_dir, file, verbosity),

        Commands::Doctor { fix } => cmd_doctor(config, *fix, verbosity),
    }
}

/// Get or create a database adapter from the URL.

async fn get_adapter(
    config: &Path,
    database_url: Option<&str>,
) -> Result<dbd_core::adapter::postgres::PostgresAdapter> {
    let design_config = dbd_core::config::read(config)
        .context("Failed to read config")?;

    // Resolve database URL: CLI flag > config > error
    let url = match database_url {
        Some(u) => u.to_string(),
        None => {
            let target = design_config.get_target(None)
                .context("No target configured")?;
            target.url.clone()
                .map(|u| resolve_env_vars(&u))
                .context("No database URL — set DATABASE_URL or configure target.url in design.yaml")?
        }
    };

    let project = &design_config.project.name;
    dbd_core::adapter::postgres::PostgresAdapter::new(&url, project)
        .await
        .context("Failed to connect to database")
}


/// Expand $ENV_VAR references in a string.
fn resolve_env_vars(s: &str) -> String {
    if let Some(var) = s.strip_prefix('$') {
        std::env::var(var).unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    }
}

// ── Command implementations ─────────────────────────────

fn cmd_inspect(config: &Path, env: &str, project_dir: &Path, name: Option<&str>, verbosity: Verbosity) -> Result<()> {
    let mut design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let report = design.report(name);
    let total_entities = design.entities().len();

    if verbosity.is_verbose() {
        if let Some(entity) = &report.entity {
            output::always(&serde_json::to_string_pretty(entity)?);
        }
    }

    if !verbosity.is_silent() {
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

    output::summary(report.issues.len(), report.warnings.len(), total_entities);
    Ok(())
}

fn cmd_combine(config: &Path, env: &str, project_dir: &Path, file: &Path, verbosity: Verbosity) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    design.combine(file)?;
    output::info(verbosity, &format!("Generated {}", file.display()));
    Ok(())
}

fn cmd_graph(config: &Path, env: &str, project_dir: &Path, name: Option<&str>, verbosity: Verbosity) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let graph = design.graph(name);

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

async fn cmd_apply(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    dry_run: bool,
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

    let valid: Vec<_> = design
        .entities()
        .iter()
        .filter(|e| e.errors.is_empty() && e.entity_type != dbd_core::EntityType::External)
        .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
        .collect();

    if verbosity.is_verbose() {
        output::always("Apply order:");
        for entity in &valid {
            let detail = match &entity.file {
                Some(f) => format!("  {:?} => {} using \"{}\"", entity.entity_type, entity.name, f.display()),
                None => format!("  {:?} => {}", entity.entity_type, entity.name),
            };
            output::always(&detail);
        }
        output::always("");
    }

    let adapter = get_adapter(config, database_url).await?;
    output::info(verbosity, "Applying...");
    design.apply(&adapter, name, false).await?;
    output::info(verbosity, &format!("Applied {} entities.", valid.len()));
    Ok(())
}

fn cmd_import_dry_run(
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

async fn cmd_import(
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
    design.import_data(&adapter, name, false).await?;

    let count = design.import_tables().len();
    output::info(verbosity, &format!("Imported {count} tables."));
    Ok(())
}

async fn cmd_reset(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    target: &str,
    dry_run: bool,
    force: bool,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;

    if dry_run {
        // Show all schemas (config-declared + auto-discovered from DDL paths)
        let schemas: Vec<&str> = design
            .entities()
            .iter()
            .filter(|e| e.entity_type == dbd_core::EntityType::Schema)
            .map(|e| e.name.as_str())
            .collect();
        output::info(verbosity, "[dry-run] Would drop schemas:");
        for schema in &schemas {
            output::info(verbosity, &format!("  {schema}"));
        }
        return Ok(());
    }

    let adapter = get_adapter(config, database_url).await?;
    design.reset(&adapter, target, force).await?;
    Ok(())
}

fn cmd_snapshot_list(project_dir: &Path, verbosity: Verbosity) {
    let dir = project_dir;
    let snapshots = dbd_core::snapshot::list_snapshots(dir);

    if snapshots.is_empty() {
        output::info(verbosity, "No snapshots found.");
        return;
    }

    for s in &snapshots {
        let ts = if s.timestamp.len() >= 10 { &s.timestamp[..10] } else { &s.timestamp };
        let desc = if s.description.is_empty() { "(no description)" } else { &s.description };
        output::info(
            verbosity,
            &format!("  {}  {}  {}", dbd_core::snapshot::pad_version(s.version), ts, desc),
        );
    }

    if verbosity.is_silent() {
        output::always(&format!("{} snapshots", snapshots.len()));
    }
}

fn cmd_dbml(config: &Path, env: &str, project_dir: &Path, file: &Path, verbosity: Verbosity) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    let doc = dbd_core::dbml::generate_dbml(&dbd_core::dbml::DbmlParams {
        entities: design.entities(),
        project_name: &design.config().project.name,
        database_type: &design.config().source.dialect,
        project_note: design.config().project.note.as_deref(),
    });

    std::fs::write(file, &doc.content)?;
    output::info(verbosity, &format!("Generated DBML in {}", file.display()));
    Ok(())
}

fn cmd_doctor(config: &Path, fix: bool, verbosity: Verbosity) -> Result<()> {
    if !config.exists() {
        anyhow::bail!("Config file not found: {}", config.display());
    }

    let content = std::fs::read_to_string(config)
        .context("Failed to read config")?;

    let issues = dbd_core::doctor::detect_old_format(&content);

    if issues.is_empty() {
        output::info(verbosity, "design.yaml is in the current format — no migration needed.");
        output::summary(0, 0, 0);
        return Ok(());
    }

    if !verbosity.is_silent() {
        output::always(&format!("Found {} config issue{}:", issues.len(), if issues.len() != 1 { "s" } else { "" }));
        for issue in &issues {
            output::always(&format!("  - {issue}"));
        }
    }

    if fix {
        let migrated = dbd_core::doctor::migrate_config(&content)
            .context("Config migration failed")?;

        // Verify the migrated config parses
        let _: dbd_core::config::DesignConfig = serde_yaml::from_str(&migrated)
            .context("Migrated config failed to parse — please report this as a bug")?;

        // Write backup
        let backup = config.with_extension("yaml.bak");
        std::fs::copy(config, &backup)?;
        output::info(verbosity, &format!("Backup saved to {}", backup.display()));

        // Write migrated
        std::fs::write(config, &migrated)?;
        output::info(verbosity, &format!("Migrated {}", config.display()));

        output::summary(0, 0, issues.len());
    } else {
        output::always("\nRun with --fix to migrate automatically.");
        output::summary(issues.len(), 0, 0);
    }

    Ok(())
}
