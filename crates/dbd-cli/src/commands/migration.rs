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
    reset_with_adapter(&*adapter, &design, target, force, drop_schemas, drop_extensions, &resolved, allow_scope_change).await
}

/// The body of a non-dry-run `dbd reset`, with the adapter supplied rather than
/// connected.
///
/// Split out so the scope guard — which is what stops a reset pinned to one
/// scope from wiping another — is testable without a live database.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reset_with_adapter(
    adapter: &dyn dbd_core::DatabaseAdapter,
    design: &Design,
    target: &str,
    force: bool,
    drop_schemas: bool,
    drop_extensions: bool,
    resolved: &dbd_core::ResolvedScope,
    allow_scope_change: bool,
) -> Result<()> {
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, force || allow_scope_change)?;
    design.reset(adapter, target, force, drop_schemas, drop_extensions, Some(resolved)).await?;
    Ok(())
}

pub async fn cmd_migrate_status(
    config: &Path,
    database_url: Option<&str>,
    project_dir: &Path,
    verbosity: Verbosity,
) -> Result<()> {
    let adapter = get_adapter(config, database_url).await?;
    migrate_status_with_adapter(&*adapter, project_dir, verbosity).await
}

/// The body of `dbd migrate --status`, with the adapter supplied rather than
/// connected. Split out so the pending-migration reporting is testable without a
/// live database.
pub(crate) async fn migrate_status_with_adapter(
    adapter: &dyn dbd_core::DatabaseAdapter,
    project_dir: &Path,
    verbosity: Verbosity,
) -> Result<()> {
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

    /// A second snapshot with nothing changed takes the no-changes branch and
    /// must not write another migration directory.
    #[test]
    fn snapshot_create_twice_reports_no_changes() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("first"), Verbosity::Normal).unwrap();
        let after_first = dbd_core::snapshot::list_snapshots(proj.path()).len();

        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("second"), Verbosity::Normal).unwrap();
        let after_second = dbd_core::snapshot::list_snapshots(proj.path()).len();

        assert_eq!(
            after_first, after_second,
            "an unchanged schema must not produce another snapshot"
        );
    }

    /// A real schema change after a baseline produces a further snapshot, which
    /// is the branch that reports added/altered/dropped counts.
    #[test]
    fn snapshot_create_after_a_change_adds_a_snapshot() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("baseline"), Verbosity::Normal).unwrap();
        let before = dbd_core::snapshot::list_snapshots(proj.path()).len();

        std::fs::write(
            proj.path().join("ddl/table/config/added_later.ddl"),
            "set search_path to config;\n\
             create table if not exists added_later (id int primary key);\n",
        )
        .unwrap();

        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("after change"), Verbosity::Verbose).unwrap();
        let after = dbd_core::snapshot::list_snapshots(proj.path()).len();

        assert!(
            after > before,
            "a new table must produce a snapshot ({before} → {after})"
        );
    }

    /// Listing a project that HAS snapshots exercises the print-each-entry path,
    /// which an empty fixture never reaches.
    #[test]
    fn snapshot_list_prints_existing_snapshots() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("listed"), Verbosity::Normal).unwrap();
        assert!(
            !dbd_core::snapshot::list_snapshots(proj.path()).is_empty(),
            "precondition: the project must have a snapshot to list"
        );
        cmd_snapshot_list(proj.path(), Verbosity::Verbose);
    }

    // ── Adapter-backed paths, via the mock ────────────────────────────────────

    use dbd_core::adapter::mock::MockAdapter;

    /// `migrate --status` on a database behind the latest snapshot reports the
    /// gap as pending.
    ///
    /// Note a *baseline* snapshot is not a pending migration — it records the
    /// starting state — so this needs a real schema change on top of one to
    /// produce something pending at all.
    #[tokio::test]
    async fn migrate_status_reports_pending_when_db_behind() {
        let proj = testutil::copy_fixture_project();
        let cfg = proj.path().join("design.yaml");
        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("baseline"), Verbosity::Normal).unwrap();
        std::fs::write(
            proj.path().join("ddl/table/config/pending_probe.ddl"),
            "set search_path to config;\n\
             create table if not exists pending_probe (id int primary key);\n",
        )
        .unwrap();
        cmd_snapshot_create(&cfg, "dev", proj.path(), Some("change"), Verbosity::Normal).unwrap();

        // Precondition: a database at v1 has the v1→v2 migration outstanding.
        let pending = dbd_core::snapshot::pending_migrations(1, proj.path());
        assert!(
            !pending.is_empty(),
            "a change snapshot on top of a baseline must be pending from v1"
        );

        // Mock reports v1, so the status path takes the "pending" branch.
        let mock = MockAdapter::new().with_version(1);
        migrate_status_with_adapter(&mock, proj.path(), Verbosity::Verbose)
            .await
            .unwrap();
    }

    /// An up-to-date database takes the "up to date" branch instead.
    #[tokio::test]
    async fn migrate_status_reports_up_to_date_on_empty_project() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = MockAdapter::new();
        assert!(
            dbd_core::snapshot::pending_migrations(0, tmp.path()).is_empty(),
            "precondition: no snapshots means nothing pending"
        );
        migrate_status_with_adapter(&mock, tmp.path(), Verbosity::Verbose)
            .await
            .unwrap();
    }

    /// The scope guard blocks a reset when the database is pinned to a different
    /// scope — the check that stops a scoped reset wiping another scope.
    #[tokio::test]
    async fn reset_is_blocked_when_pinned_to_another_scope() {
        let design = Design::from_config_with_dir(
            &testutil::fixture_config(), "dev", Some(&testutil::fixtures()),
        )
        .unwrap();
        let resolved = design.resolve_scope(None, None).unwrap();

        let mock = MockAdapter::new().with_version(0).with_scope("a_different_scope");

        let err = reset_with_adapter(
            &mock, &design, "dev", false, false, false, &resolved, false,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("a_different_scope"),
            "the guard must name the pinned scope: {err}"
        );
        assert!(
            mock.scripts.lock().unwrap().is_empty(),
            "a blocked reset must not execute any SQL"
        );
    }

    /// `--allow-scope-change` is the documented override for the scope guard, so
    /// the same mismatch then proceeds and the reset runs.
    ///
    /// The pinned version stays 0 on purpose: a database with applied migrations
    /// trips a *separate* safety guard that only `--force` clears, which would
    /// mask whether the scope override worked.
    #[tokio::test]
    async fn reset_proceeds_when_scope_change_allowed() {
        let design = Design::from_config_with_dir(
            &testutil::fixture_config(), "dev", Some(&testutil::fixtures()),
        )
        .unwrap();
        let resolved = design.resolve_scope(None, None).unwrap();

        let mock = MockAdapter::new().with_version(0).with_scope("a_different_scope");

        reset_with_adapter(&mock, &design, "dev", false, false, false, &resolved, true)
            .await
            .unwrap();
        assert!(
            !mock.scripts.lock().unwrap().is_empty(),
            "an allowed reset must actually execute the drop script"
        );
    }

    /// The applied-migrations guard is independent of the scope guard: a
    /// database at v1 is blocked even when the scope matches, and only `--force`
    /// clears it.
    #[tokio::test]
    async fn reset_is_blocked_when_db_has_applied_migrations() {
        let design = Design::from_config_with_dir(
            &testutil::fixture_config(), "dev", Some(&testutil::fixtures()),
        )
        .unwrap();
        let resolved = design.resolve_scope(None, None).unwrap();

        // Scope matches, so only the applied-migrations guard can fire.
        let blocked = MockAdapter::new().with_version(1).with_scope(&resolved.name);
        let err = reset_with_adapter(&blocked, &design, "dev", false, false, false, &resolved, false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("--force"),
            "the guard must point at the override: {err}"
        );
        assert!(
            blocked.scripts.lock().unwrap().is_empty(),
            "a blocked reset must not execute any SQL"
        );

        let forced = MockAdapter::new().with_version(1).with_scope(&resolved.name);
        reset_with_adapter(&forced, &design, "dev", true, false, false, &resolved, false)
            .await
            .unwrap();
        assert!(
            !forced.scripts.lock().unwrap().is_empty(),
            "--force must let the reset through"
        );
    }
}
