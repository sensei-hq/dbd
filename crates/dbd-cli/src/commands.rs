use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::adapter::DatabaseAdapter;
use dbd_core::Design;

use crate::cli::Commands;
use crate::output::{self, Verbosity};

pub async fn run(
    command: &Commands,
    config: &Path,
    env: &str,
    database_url: Option<&str>,
    project_dir: &Path,
    source: &str,
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

        Commands::Snapshot { list, name } => {
            if *list {
                cmd_snapshot_list(project_dir, verbosity);
                return Ok(());
            }
            cmd_snapshot_create(config, env, project_dir, name.as_deref(), verbosity)
        }

        Commands::Migrate { status } => {
            if *status {
                cmd_migrate_status(config, database_url, project_dir, verbosity).await
            } else {
                output::info(verbosity, "Use --status to check migration state. Use 'dbd apply' to run migrations.");
                Ok(())
            }
        }

        Commands::Deploy { dry_run } => {
            cmd_deploy(source, config, env, database_url, *dry_run, verbosity).await
        }

        Commands::Export { name, format } => {
            cmd_export(config, env, project_dir, database_url, name.as_deref(), format, verbosity).await
        }

        Commands::Dbml { file } => cmd_dbml(config, env, project_dir, file, verbosity),

        Commands::Doctor { fix } => cmd_doctor(config, *fix, verbosity),

        Commands::Init { name, target } => {
            let project_name = name.as_deref().unwrap_or_else(|| {
                project_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("my-project")
            });
            cmd_init(project_dir, project_name, target, verbosity)
        }
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

#[allow(clippy::collapsible_if)]
fn cmd_inspect(config: &Path, env: &str, project_dir: &Path, name: Option<&str>, verbosity: Verbosity) -> Result<()> {
    let mut design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let report = design.report(name);
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

#[allow(clippy::too_many_arguments)]
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

async fn cmd_migrate_status(
    config: &Path,
    database_url: Option<&str>,
    project_dir: &Path,
    verbosity: Verbosity,
) -> Result<()> {
    let adapter = get_adapter(config, database_url).await?;
    adapter.ensure_migrations_table().await?;
    let db_version = adapter.get_db_version().await?;

    let snapshots = dbd_core::snapshot::list_snapshots(project_dir);
    let latest_version = snapshots.last().map(|s| s.version).unwrap_or(0);

    output::always(&format!("Database version: {}", dbd_core::snapshot::pad_version(db_version)));
    output::always(&format!("Latest snapshot:  {}", dbd_core::snapshot::pad_version(latest_version)));

    let pending = dbd_core::snapshot::pending_migrations(db_version, project_dir);
    if pending.is_empty() {
        output::info(verbosity, "Database is up to date.");
    } else {
        output::always(&format!("{} pending migration(s):", pending.len()));
        for m in &pending {
            let detail = format!(
                "  v{} -> v{}  ({} added, {} altered, {} dropped)",
                dbd_core::snapshot::pad_version(m.from_version),
                dbd_core::snapshot::pad_version(m.to_version),
                m.added.len(),
                m.altered.len(),
                m.dropped.len(),
            );
            output::always(&detail);
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
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


}

fn cmd_snapshot_create(
    config: &Path,
    env: &str,
    project_dir: &Path,
    description: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let desc = description.unwrap_or("snapshot");
    let result = dbd_core::snapshot::create_snapshot(design.entities(), project_dir, config, desc)
        .context("Failed to create snapshot")?;

    // Single snapshot, no changes
    if result.snapshots.len() == 1 && result.snapshots[0].no_changes {
        output::info(verbosity, "No schema changes detected — snapshot skipped.");
        return Ok(());
    }

    let total_stages = result.snapshots.len();

    for (i, snap) in result.snapshots.iter().enumerate() {
        let version = dbd_core::snapshot::pad_version(snap.snapshot.version);

        if snap.is_baseline {
            output::info(verbosity, &format!("Baseline snapshot v{version} created."));
            continue;
        }

        if total_stages == 1 {
            let graph = snap.graph.as_ref();
            let added = graph.map(|g| g.added.len()).unwrap_or(0);
            let altered = graph.map(|g| g.altered.len()).unwrap_or(0);
            let dropped = graph.map(|g| g.dropped.len()).unwrap_or(0);
            output::info(verbosity, &format!(
                "Snapshot v{version} created — {added} added, {altered} altered, {dropped} dropped."
            ));
        } else {
            output::info(verbosity, &format!(
                "\nSnapshot v{version} created (stage {} of {total_stages})", i + 1
            ));
        }

        if !snap.migration_files.is_empty() {
            for mf in &snap.migration_files {
                output::detail(verbosity, &format!("  {}", mf.relative_path.display()));
            }
        }
    }

    // Print TODO items
    if !result.todos.is_empty() {
        output::always("\nAction required:");
        for todo in &result.todos {
            output::always(&format!("  {} — {}", todo.file.display(), todo.message));
        }
    }

    let final_version = result.snapshots.last().map(|s| s.snapshot.version).unwrap_or(0);
    output::info(verbosity, &format!("\ndesign.yaml version updated to {final_version}"));

    Ok(())
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

    output::always(&format!("Found {} config issue{}:", issues.len(), if issues.len() != 1 { "s" } else { "" }));
    for issue in &issues {
        output::always(&format!("  - {issue}"));
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

async fn cmd_export(
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

    // Build export list: either from config export entries, or all tables
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

    output::info(verbosity, &format!("Exporting {} table(s) as {format}...", tables.len()));

    for table in &tables {
        // Set format on a clone for the adapter
        let mut export_entity = (*table).clone();
        export_entity.format = Some(format.to_string());
        adapter.export_data(&export_entity).await
            .context(format!("Failed to export {}", table.name))?;
        output::detail(verbosity, &format!("  exported {}", table.name));
    }

    output::info(verbosity, "Export complete. Files written to export/");
    Ok(())
}

async fn cmd_deploy(
    source: &str,
    _config_name: &Path,
    env: &str,
    database_url: Option<&str>,
    dry_run: bool,
    verbosity: Verbosity,
) -> Result<()> {
    output::info(verbosity, &format!("Deploying from source: {source}"));

    // Resolve source to a local directory (downloads from GitHub if needed)
    let project_dir = dbd_core::deploy::resolve_source(source)
        .await
        .context("Failed to resolve source")?;

    let config_path = project_dir.join("design.yaml");
    if !config_path.exists() {
        anyhow::bail!("No design.yaml found in {}", project_dir.display());
    }

    // Load design from resolved source
    let mut design = Design::from_config_with_dir(&config_path, env, Some(&project_dir))
        .context("Failed to load design from source")?;

    if dry_run {
        let report = design.report(None);
        output::info(verbosity, &format!(
            "{} entities found, {} errors, {} warnings",
            design.entities().len(),
            report.issues.len(),
            report.warnings.len(),
        ));
        output::info(verbosity, "[dry-run] No changes applied.");
        return Ok(());
    }

    // Apply
    let adapter = get_adapter(&config_path, database_url).await?;
    output::info(verbosity, "Applying schema...");
    design.apply(&adapter, None, false).await
        .context("Apply failed")?;

    // Import
    let import_plan = design.import_plan(None);
    if !import_plan.is_empty() {
        output::info(verbosity, &format!("Importing {} data file(s)...", import_plan.len()));
        design.import_data(&adapter, None, false).await
            .context("Import failed")?;
    }

    output::info(verbosity, "Deploy complete.");
    Ok(())
}

fn cmd_init(project_dir: &Path, name: &str, target: &str, verbosity: Verbosity) -> Result<()> {
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
