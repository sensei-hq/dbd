use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use super::get_adapter;
use crate::output::{self, Verbosity};

/// The boolean flags that drive `dbd reset`, grouped so callers construct them
/// by name. This prevents a transposition of, e.g., `allow_scope_change` (which
/// would silently disable the scope guard) into an adjacent positional bool slot.
pub(crate) struct ResetOptions {
    pub dry_run: bool,
    pub force: bool,
    pub drop_schemas: bool,
    pub drop_extensions: bool,
    pub allow_scope_change: bool,
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_reset(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    target: &str,
    opts: ResetOptions,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let ResetOptions { dry_run, force, drop_schemas, drop_extensions, allow_scope_change } = opts;
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps)?;

    if dry_run {
        let sql = design
            .reset_script(target, drop_schemas, drop_extensions, Some(&resolved))?
            .unwrap_or_else(|| "-- nothing to drop".to_string());
        output::info(verbosity, "[dry-run] Reset would run:");
        output::always(&sql);
        return Ok(());
    }

    let adapter = get_adapter(config, database_url).await?;
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, force || allow_scope_change)?;
    design.reset(&*adapter, target, force, drop_schemas, drop_extensions, Some(&resolved)).await?;
    Ok(())
}

pub async fn cmd_migrate_status(
    config: &Path,
    database_url: Option<&str>,
    project_dir: &Path,
    verbosity: Verbosity,
) -> Result<()> {
    let adapter = get_adapter(config, database_url).await?;
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
pub fn cmd_snapshot_list(project_dir: &Path, verbosity: Verbosity) {
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

pub fn cmd_snapshot_create(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testutil;

    /// An empty project has no snapshots — the listing takes the early return.
    #[test]
    fn snapshot_list_empty_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        cmd_snapshot_list(tmp.path(), Verbosity::Normal);
    }

    /// Listing against the real fixture exercises the print-each-snapshot path.
    #[test]
    fn snapshot_list_on_fixture_project() {
        cmd_snapshot_list(&testutil::fixtures(), Verbosity::Verbose);
    }

    /// `--dry-run` builds the reset script and returns before any DB adapter is
    /// constructed, so it runs without a live connection.
    #[tokio::test]
    async fn reset_dry_run_needs_no_database() {
        cmd_reset(
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None, "dev",
            ResetOptions {
                dry_run: true,
                force: false,
                drop_schemas: false,
                drop_extensions: false,
                allow_scope_change: false,
            },
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// Creating the first snapshot against a fresh copy writes a baseline and
    /// bumps the config version — run against a throwaway copy, never the repo.
    #[test]
    fn snapshot_create_writes_into_temp_project() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("test snapshot"), Verbosity::Normal).unwrap();
    }
}
