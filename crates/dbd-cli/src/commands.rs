use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use crate::cli::Commands;
use crate::output::{self, Verbosity};

pub async fn run(command: &Commands, config: &Path, env: &str, verbosity: Verbosity) -> Result<()> {
    match command {
        Commands::Inspect { name } => cmd_inspect(config, env, name.as_deref(), verbosity),

        Commands::Combine { file } => cmd_combine(config, env, file, verbosity),

        Commands::Graph { name } => cmd_graph(config, env, name.as_deref(), verbosity),

        Commands::Apply { name, dry_run } => {
            cmd_apply(config, env, name.as_deref(), *dry_run, verbosity).await
        }

        Commands::Snapshot { list, .. } => {
            if *list {
                cmd_snapshot_list(config, verbosity);
                return Ok(());
            }
            output::info(verbosity, "Snapshot creation requires a database connection (adapter not yet wired)");
            Ok(())
        }

        Commands::Migrate { status, .. } => {
            if *status {
                output::info(verbosity, "Migrate status requires a database connection (adapter not yet wired)");
            } else {
                output::info(verbosity, "Migrate requires a database connection (adapter not yet wired)");
            }
            Ok(())
        }

        Commands::Import { dry_run, .. } => {
            if !dry_run {
                anyhow::bail!("import requires a database connection (adapter not yet wired)");
            }
            output::info(verbosity, "Import dry-run not yet implemented");
            Ok(())
        }

        Commands::Deploy { .. } => {
            anyhow::bail!("deploy requires --source and a database connection (not yet wired)");
        }

        Commands::Reset { .. } => {
            anyhow::bail!("reset requires a database connection (adapter not yet wired)");
        }

        Commands::Doctor { .. } => {
            output::info(verbosity, "Doctor not yet implemented");
            Ok(())
        }
    }
}

fn cmd_inspect(config: &Path, env: &str, name: Option<&str>, verbosity: Verbosity) -> Result<()> {
    let mut design = Design::from_config(config, env).context("Failed to load design")?;
    let report = design.report(name);

    let total_entities = design.entities().len();

    // Verbose: show entity details
    if verbosity.is_verbose() {
        if let Some(entity) = &report.entity {
            output::always(&serde_json::to_string_pretty(entity)?);
        }
    }

    // Normal + Verbose: show errors
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

    // Always show summary (even in silent mode)
    output::summary(report.issues.len(), report.warnings.len(), total_entities);

    Ok(())
}

fn cmd_combine(config: &Path, env: &str, file: &Path, verbosity: Verbosity) -> Result<()> {
    let design = Design::from_config(config, env).context("Failed to load design")?;
    design.combine(file)?;
    output::info(verbosity, &format!("Generated {}", file.display()));
    Ok(())
}

fn cmd_graph(config: &Path, env: &str, name: Option<&str>, verbosity: Verbosity) -> Result<()> {
    let design = Design::from_config(config, env).context("Failed to load design")?;
    let graph = design.graph(name);

    let json = serde_json::json!({
        "nodes": graph.nodes.iter().map(|n| serde_json::json!({
            "name": n.name,
            "type": n.entity_type,
            "schema": n.schema,
        })).collect::<Vec<_>>(),
        "edges": graph.edges.iter().map(|e| serde_json::json!({
            "from": e.from,
            "to": e.to,
        })).collect::<Vec<_>>(),
        "layers": graph.layers,
    });

    output::always(&serde_json::to_string_pretty(&json)?);

    if !verbosity.is_silent() {
        output::detail(
            verbosity,
            &format!(
                "{} nodes, {} edges, {} layers",
                graph.nodes.len(),
                graph.edges.len(),
                graph.layers.len()
            ),
        );
    }

    Ok(())
}

async fn cmd_apply(
    config: &Path,
    env: &str,
    name: Option<&str>,
    dry_run: bool,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config(config, env).context("Failed to load design")?;

    if !dry_run {
        anyhow::bail!("apply requires a database connection (adapter not yet wired)");
    }

    // Dry-run: list entities in apply order
    let entities: Vec<_> = design
        .entities()
        .iter()
        .filter(|e| e.errors.is_empty())
        .filter(|e| e.entity_type != dbd_core::EntityType::External)
        .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
        .collect();

    for entity in &entities {
        let detail = match &entity.file {
            Some(f) => format!(
                "{:?} => {} using \"{}\"",
                entity.entity_type,
                entity.name,
                f.display()
            ),
            None => format!("{:?} => {}", entity.entity_type, entity.name),
        };
        output::info(verbosity, &detail);
    }

    output::summary(0, 0, entities.len());

    Ok(())
}

fn cmd_snapshot_list(config: &Path, verbosity: Verbosity) {
    let dir = config.parent().unwrap_or(Path::new("."));
    let snapshots = dbd_core::snapshot::list_snapshots(dir);

    if snapshots.is_empty() {
        output::info(verbosity, "No snapshots found.");
        return;
    }

    for s in &snapshots {
        let ts = if s.timestamp.len() >= 10 {
            &s.timestamp[..10]
        } else {
            &s.timestamp
        };
        let desc = if s.description.is_empty() {
            "(no description)"
        } else {
            &s.description
        };
        output::info(
            verbosity,
            &format!("  {}  {}  {}", dbd_core::snapshot::pad_version(s.version), ts, desc),
        );
    }

    if verbosity.is_silent() {
        output::always(&format!("{} snapshots", snapshots.len()));
    }
}
