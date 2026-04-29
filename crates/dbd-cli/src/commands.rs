use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use crate::cli::Commands;

pub async fn run(command: &Commands, config: &Path, _env: &str) -> Result<()> {
    match command {
        Commands::Inspect { name, verbose } => {
            let mut design = Design::from_config(config, _env)
                .context("Failed to load design")?;
            let report = design.report(name.as_deref());

            if let Some(entity) = &report.entity {
                if *verbose {
                    println!("{}", serde_json::to_string_pretty(entity)?);
                }
            }

            if !report.issues.is_empty() {
                println!("Errors:");
                for entity in &report.issues {
                    println!("\n{} =>", entity.file.as_ref().map(|f| f.display().to_string()).unwrap_or(entity.name.clone()));
                    for err in &entity.errors {
                        println!("  {err}");
                    }
                }
            }

            if !report.warnings.is_empty() {
                println!("\nWarnings:");
                for entity in &report.warnings {
                    println!("\n{} =>", entity.file.as_ref().map(|f| f.display().to_string()).unwrap_or(entity.name.clone()));
                    for warn in &entity.warnings {
                        println!("  {warn}");
                    }
                }
            }

            if report.issues.is_empty() && report.warnings.is_empty() {
                println!("Everything looks ok");
            }
            Ok(())
        }

        Commands::Combine { file } => {
            let design = Design::from_config(config, _env)
                .context("Failed to load design")?;
            design.combine(file)?;
            println!("Generated {}", file.display());
            Ok(())
        }

        Commands::Graph { name } => {
            let design = Design::from_config(config, _env)
                .context("Failed to load design")?;
            let graph = design.graph(name.as_deref());
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
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
            }))?);
            Ok(())
        }

        Commands::Apply { name, dry_run } => {
            let design = Design::from_config(config, _env)
                .context("Failed to load design")?;
            // Without a real adapter, only dry-run works for now
            if !dry_run {
                anyhow::bail!("apply requires a database connection (adapter not yet wired)");
            }
            let mock = dbd_core::adapter::mock::MockAdapter::new();
            design.apply(&mock, name.as_deref(), *dry_run).await?;
            Ok(())
        }

        Commands::Snapshot { list, .. } => {
            if *list {
                let dir = config.parent().unwrap_or(Path::new("."));
                let snapshots = dbd_core::snapshot::list_snapshots(dir);
                if snapshots.is_empty() {
                    println!("No snapshots found.");
                } else {
                    for s in &snapshots {
                        println!(
                            "  {}  {}  {}",
                            dbd_core::snapshot::pad_version(s.version),
                            &s.timestamp[..10.min(s.timestamp.len())],
                            if s.description.is_empty() { "(no description)" } else { &s.description }
                        );
                    }
                }
                return Ok(());
            }
            println!("Snapshot creation requires a database connection (adapter not yet wired)");
            Ok(())
        }

        Commands::Migrate { status, .. } => {
            if *status {
                println!("Migrate status requires a database connection (adapter not yet wired)");
            } else {
                println!("Migrate requires a database connection (adapter not yet wired)");
            }
            Ok(())
        }

        Commands::Import { dry_run, .. } => {
            if !dry_run {
                anyhow::bail!("import requires a database connection (adapter not yet wired)");
            }
            println!("Import dry-run not yet implemented");
            Ok(())
        }

        Commands::Deploy { .. } => {
            anyhow::bail!("deploy requires --source and a database connection (not yet wired)");
        }

        Commands::Reset { .. } => {
            anyhow::bail!("reset requires a database connection (adapter not yet wired)");
        }

        Commands::Doctor { .. } => {
            println!("Doctor not yet implemented");
            Ok(())
        }
    }
}
