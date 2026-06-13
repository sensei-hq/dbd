mod data;
mod diagram;
mod migration;
mod project;
mod schema;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dbd_core::design::{ApplyComplete, ApplyStrategy, ImportComplete};

use crate::cli::Commands;
use crate::output::{self, Verbosity};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    command: &Commands,
    config: &Path,
    env: &str,
    database_url: Option<&str>,
    project_dir: &Path,
    source: &str,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    match command {
        Commands::Inspect { name, fix, from_db } => {
            schema::cmd_inspect(config, env, project_dir, database_url, name.as_deref(), *fix, *from_db, scope, deps, verbosity).await
        }

        Commands::Combine { file } => {
            schema::cmd_combine(config, env, project_dir, file, scope, deps, verbosity)
        }

        Commands::Graph { name } => {
            project::cmd_graph(config, env, project_dir, name.as_deref(), scope, deps, verbosity)
        }

        Commands::Apply { name, dry_run, with_policies } => {
            schema::cmd_apply(config, env, project_dir, database_url, name.as_deref(), *dry_run, *with_policies, scope, deps, verbosity).await
        }

        Commands::Import { name, dry_run } => {
            if *dry_run {
                data::cmd_import_dry_run(config, env, project_dir, name.as_deref(), scope, deps, verbosity)
            } else {
                data::cmd_import(config, env, project_dir, database_url, name.as_deref(), scope, deps, verbosity).await
            }
        }

        Commands::Reset { target, dry_run, force } => {
            migration::cmd_reset(config, env, project_dir, database_url, target, *dry_run, *force, scope, deps, verbosity).await
        }

        Commands::Snapshot { list, name } => {
            if *list {
                migration::cmd_snapshot_list(project_dir, verbosity);
                return Ok(());
            }
            migration::cmd_snapshot_create(config, env, project_dir, name.as_deref(), verbosity)
        }

        Commands::Migrate { status } => {
            if *status {
                migration::cmd_migrate_status(config, database_url, project_dir, verbosity).await
            } else {
                output::info(verbosity, "Use --status to check migration state. Use 'dbd apply' to run migrations.");
                Ok(())
            }
        }

        Commands::Deploy { dry_run } => {
            project::cmd_deploy(source, config, env, database_url, *dry_run, scope, deps, verbosity).await
        }

        Commands::Export { name, format } => {
            data::cmd_export(config, env, project_dir, database_url, name.as_deref(), format, scope, deps, verbosity).await
        }

        Commands::Dbml { file } => {
            project::cmd_dbml(config, env, project_dir, file, scope, deps, verbosity)
        }

        Commands::Diagram { file, json } => {
            diagram::cmd_diagram(config, env, project_dir, file, *json, scope, deps, verbosity)
        }

        Commands::Doctor { fix } => project::cmd_doctor(config, *fix, verbosity),

        Commands::Init { name, target } => {
            let project_name = name.as_deref().unwrap_or_else(|| {
                project_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("my-project")
            });
            project::cmd_init(project_dir, project_name, target, verbosity)
        }

        Commands::Format { check } => schema::cmd_format(config, project_dir, *check, verbosity),

        Commands::Policies { dry_run } => {
            schema::cmd_policies(config, project_dir, database_url, *dry_run, verbosity).await
        }
    }
}

// ── Summary formatting helpers ────────────────────────────────────────────────

pub(super) fn format_apply_summary(s: &ApplyComplete) -> String {
    match s.strategy {
        ApplyStrategy::Fresh => {
            format!("Fresh install at v{} — {} entities applied.", s.to_version, s.applied)
        }
        ApplyStrategy::Current => {
            format!("Already up to date (v{}) — {} entities applied.", s.from_version, s.applied)
        }
        ApplyStrategy::Migrate => {
            format!(
                "Migrated v{} → v{} — {} applied, {} migrated, {} created, {} dropped.",
                s.from_version, s.to_version, s.applied, s.migrated, s.created, s.dropped
            )
        }
    }
}

pub(super) fn format_import_summary(s: &ImportComplete) -> String {
    format!(
        "Import complete — {} table(s) loaded, {} procedure(s) called, {} after script(s) run.",
        s.tables, s.procedures, s.after_scripts
    )
}

pub(super) fn format_deploy_summary(s: &dbd_core::design::DeployComplete) -> String {
    let apply_line = format_apply_summary(&s.apply);
    let import_line = format_import_summary(&s.import);
    format!("{apply_line} {import_line}")
}

// ── Database adapter ──────────────────────────────────────────────────────────

pub(super) async fn get_adapter(
    config: &Path,
    database_url: Option<&str>,
) -> Result<Box<dyn dbd_core::DatabaseAdapter>> {
    let design_config = dbd_core::config::read(config)
        .context("Failed to read config")?;

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
    dbd_core::connect(&url, project)
        .await
        .context("Failed to connect to database")
}

fn resolve_env_vars(s: &str) -> String {
    if let Some(var) = s.strip_prefix('$') {
        std::env::var(var).unwrap_or_else(|_| s.to_string())
    } else {
        s.to_string()
    }
}

// ── Safe file helpers ─────────────────────────────────────────────────────────

pub(super) fn safe_canonicalize_within(root: &Path, file: &Path) -> Result<PathBuf> {
    let canon_root = root.canonicalize()
        .with_context(|| format!("Cannot resolve project root: {}", root.display()))?;
    // Resolve the file. If it already exists, canonicalize it directly. If it
    // doesn't (a new output file like `schema.json`), canonicalize its parent
    // directory — which must exist — and re-attach the file name, so commands
    // can create new files rather than erroring on a missing path.
    let canon_file = match file.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            let file_name = file
                .file_name()
                .with_context(|| format!("Invalid output path (no file name): {}", file.display()))?;
            let parent = match file.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => root,
            };
            let canon_parent = parent.canonicalize()
                .with_context(|| format!("Cannot resolve directory for: {}", file.display()))?;
            canon_parent.join(file_name)
        }
    };
    anyhow::ensure!(
        canon_file.starts_with(&canon_root),
        "path traversal rejected: {} is outside project root {}",
        file.display(),
        root.display()
    );
    Ok(canon_file)
}

pub(super) fn safe_read(root: &Path, file: &Path) -> Result<String> {
    let canon = safe_canonicalize_within(root, file)?;
    std::fs::read_to_string(&canon) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to read {}", file.display()))
}

pub(super) fn safe_write(root: &Path, file: &Path, contents: &str) -> Result<()> {
    let canon = safe_canonicalize_within(root, file)?;
    std::fs::write(&canon, contents) // nosemgrep: path validated by safe_canonicalize_within
        .with_context(|| format!("Failed to write {}", file.display()))
}

pub(super) fn safe_copy(root: &Path, src: &Path, dst: &Path) -> Result<()> {
    let canon_src = safe_canonicalize_within(root, src)?;
    let parent = dst.parent().unwrap_or(root);
    let canon_parent = parent.canonicalize()
        .with_context(|| format!("Cannot resolve backup directory: {}", parent.display()))?;
    anyhow::ensure!(
        canon_parent.starts_with(root.canonicalize()?),
        "path traversal rejected: backup destination {} is outside project root",
        dst.display()
    );
    std::fs::copy(&canon_src, dst) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to copy {} → {}", src.display(), dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_write_creates_a_new_file() {
        // A brand-new file (doesn't exist yet) must be writable — `safe_write`
        // resolves via the parent directory rather than canonicalizing the
        // (non-existent) file itself.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("schema.json");
        assert!(!file.exists());
        safe_write(dir.path(), &file, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{}");
    }

    #[test]
    fn safe_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.sql");
        std::fs::write(&file, "old").unwrap();
        safe_write(dir.path(), &file, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
    }

    #[test]
    fn safe_write_rejects_new_file_outside_root() {
        // A new file outside the project root is still rejected.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir(&root).unwrap();
        let outside = tmp.path().join("evil.json"); // sibling of root, doesn't exist
        let err = safe_write(&root, &outside, "x").unwrap_err();
        assert!(err.to_string().contains("path traversal"), "got: {err}");
    }
}
