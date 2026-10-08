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

/// The targets `reset` has a script for — what `--target` accepts.
const RESET_TARGETS: &[&str] = &["postgres", "supabase", "sqlite"];

/// The platform reset runs for: `--target` when given, otherwise the design's
/// target (its first `target:` key), otherwise plain `postgres`. It decides
/// the dialect of the drops and which schemas reset must leave alone.
/// Defaulting to `postgres` regardless of the design let a Supabase project's
/// `reset --schemas` drop `public`, and ran PostgreSQL's drops on SQLite.
fn reset_target<'a>(flag: Option<&'a str>, design: &'a Design) -> &'a str {
    flag.or_else(|| design.config().default_target()).unwrap_or("postgres")
}

/// Refuse, before connecting, a target reset has no script for. clap refuses
/// such a `--target`; this catches the design's own (a `convex:` target).
fn check_reset_target(target: &str) -> Result<()> {
    if !RESET_TARGETS.contains(&target) {
        anyhow::bail!(
            "reset supports the {} targets; this design's target is `{target}`",
            RESET_TARGETS.join(", ")
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_reset(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    target: Option<&str>,
    opts: ResetOptions,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let ResetOptions {
        dry_run,
        force,
        drop_schemas,
        drop_extensions,
        allow_scope_change,
    } = opts;
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps)?;
    let (kept, total) = design.scope_counts(&resolved)?;
    output::scope_filtered(&resolved, kept, total);
    let target = reset_target(target, &design);
    check_reset_target(target)?;
    if target == "sqlite" && (drop_schemas || drop_extensions) {
        output::warn("SQLite has no schemas or extensions: --schemas, --extensions and --clean change nothing here.");
    }

    if dry_run {
        // A preview of a reset the real run would refuse is a false promise, so
        // with a database to ask, ask it first. With none configured the plan
        // is all there is to show, and the dry run stays database-free.
        // The roles the real run keeps are asked of the same database, so the
        // preview does not list a `DROP ROLE` the run will not perform.
        let mut kept = Vec::new();
        if let Some(adapter) = super::dry_run_adapter(config, database_url).await? {
            check_reset_guards(&*adapter, &resolved, force, allow_scope_change).await?;
            kept = design.roles_kept_by_reset(&*adapter, target, Some(&resolved)).await?;
        }
        let keep: Vec<String> = kept.iter().map(|k| k.role.clone()).collect();
        let sql = design
            .reset_script_keeping(target, drop_schemas, drop_extensions, Some(&resolved), &keep)?
            .unwrap_or_else(|| "-- nothing to drop".to_string());
        output::info(verbosity, "[dry-run] Reset would run:");
        output::always(&sql);
        let preview = dbd_core::ResetComplete {
            roles_kept: kept,
            ..Default::default()
        };
        for line in kept_role_lines(&preview) {
            output::info(verbosity, &format!("[dry-run] {line}"));
        }
        return Ok(());
    }

    let adapter = get_adapter(config, database_url).await?;
    let summary = reset_with_adapter(
        &*adapter,
        &design,
        target,
        force,
        drop_schemas,
        drop_extensions,
        &resolved,
        allow_scope_change,
    )
    .await?;
    for line in format_reset_summary(&summary) {
        output::info(verbosity, &line);
    }
    Ok(())
}

/// What a reset did, as `dbd reset` prints it: what it dropped, counted by
/// kind — and the roles by name, since a role outlives any one database — or
/// that it found nothing to drop. It printed nothing at all, so a reset that
/// emptied the schema and one that found nothing looked the same.
pub(crate) fn format_reset_summary(summary: &dbd_core::ResetComplete) -> Vec<String> {
    use dbd_core::EntityType;
    if summary.dropped.is_empty() {
        let mut lines = vec!["Reset dropped nothing — none of the project's objects were in the database.".to_string()];
        lines.extend(kept_role_lines(summary));
        return lines;
    }
    let counted = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts: Vec<String> = [
        (EntityType::Function, "function", "functions"),
        (EntityType::Procedure, "procedure", "procedures"),
        (EntityType::MaterializedView, "materialized view", "materialized views"),
        (EntityType::View, "view", "views"),
        (EntityType::Table, "table", "tables"),
        (EntityType::Sequence, "sequence", "sequences"),
        (EntityType::Enum, "enum", "enums"),
        (EntityType::Extension, "extension", "extensions"),
        (EntityType::Schema, "schema", "schemas"),
    ]
    .into_iter()
    .filter_map(|(kind, one, many)| match summary.count(kind) {
        0 => None,
        n => Some(counted(n, one, many)),
    })
    .collect();
    let roles: Vec<&str> = summary
        .dropped
        .iter()
        .filter(|(kind, _)| *kind == EntityType::Role)
        .map(|(_, name)| name.as_str())
        .collect();
    if !roles.is_empty() {
        parts.push(format!(
            "{} ({})",
            counted(roles.len(), "role", "roles"),
            roles.join(", ")
        ));
    }
    let mut lines = vec![format!("Reset dropped {}.", parts.join(", "))];
    lines.extend(kept_role_lines(summary));
    lines
}

/// One line per role the reset kept: which, and the databases that hold it.
fn kept_role_lines(summary: &dbd_core::ResetComplete) -> Vec<String> {
    summary
        .roles_kept
        .iter()
        .map(|k| {
            format!(
                "Role {} kept: it holds privileges or objects in database {} — roles are server-wide, \
                 and dropping it would break that database.",
                k.role,
                k.databases.join(", ")
            )
        })
        .collect()
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
) -> Result<dbd_core::ResetComplete> {
    check_reset_guards(adapter, resolved, force, allow_scope_change).await?;
    Ok(design
        .reset(adapter, target, force, drop_schemas, drop_extensions, Some(resolved))
        .await?)
}

/// Every guard a reset checks before it drops anything, read-only: the scope
/// pin (cleared by `--force` or `--allow-scope-change`), then prod and applied
/// migrations (cleared by `--force` alone).
///
/// One function for both runs, so `--dry-run` refuses exactly where the real
/// reset does. [`Design::reset`] checks the second guard again after it heals
/// the bookkeeping; this read needs no heal, since meta is read under either
/// layout.
pub(crate) async fn check_reset_guards(
    adapter: &dyn dbd_core::DatabaseAdapter,
    resolved: &dbd_core::ResolvedScope,
    force: bool,
    allow_scope_change: bool,
) -> Result<()> {
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, force || allow_scope_change)?;
    if !force {
        Design::check_reset_guard(meta.as_ref())?;
    }
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

    let pad = dbd_core::snapshot::pad_version;
    // Version 0 is no version: a database built before the project had one.
    let recorded = if db_version == 0 {
        "none recorded".to_string()
    } else {
        pad(db_version)
    };
    output::always(&format!("Database version: {recorded}"));
    output::always(&format!("Latest snapshot:  {}", pad(latest_version)));

    // "Up to date" only when the versions agree. With no migration pending it
    // was printed beside 000 and 001 alike, which is the one case where the
    // next apply still has something to record.
    let pending = dbd_core::snapshot::pending_migrations(db_version, project_dir);
    if pending.is_empty() {
        let state = match db_version.cmp(&latest_version) {
            std::cmp::Ordering::Equal => "Database is up to date.".to_string(),
            std::cmp::Ordering::Greater => format!(
                "The database is at v{}, ahead of the latest snapshot v{} — it was applied from a newer design.",
                pad(db_version),
                pad(latest_version)
            ),
            std::cmp::Ordering::Less if db_version == 0 => format!(
                "No migration is pending — the next `dbd apply` applies the current DDL and records v{}.",
                pad(latest_version)
            ),
            std::cmp::Ordering::Less => format!(
                "No migration from v{} to v{} is under snapshots/ — `dbd apply` re-applies the current DDL \
                 and leaves the recorded version at v{}.",
                pad(db_version),
                pad(latest_version),
                pad(db_version)
            ),
        };
        output::info(verbosity, &state);
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
}

pub fn cmd_snapshot_create(
    config: &Path,
    env: &str,
    project_dir: &Path,
    description: Option<&str>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
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
            output::info(
                verbosity,
                &format!("Snapshot v{version} created — {added} added, {altered} altered, {dropped} dropped."),
            );
        } else {
            output::info(
                verbosity,
                &format!("\nSnapshot v{version} created (stage {} of {total_stages})", i + 1),
            );
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

    /// A Supabase project resets as a Supabase project without being told so:
    /// the design already names its target. Before, `--target` defaulted to
    /// `postgres`, and `reset --schemas` dropped `public` CASCADE.
    #[test]
    fn reset_protects_the_designs_platform_unless_told_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("design.yaml");
        std::fs::write(
            &config,
            "project:\n  name: t\ntarget:\n  supabase:\n    url: $DATABASE_URL\nschemas:\n  - public\n  - app\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/items.ddl"),
            "create table if not exists app.items (id int primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&config, "dev", Some(tmp.path())).unwrap();

        let target = reset_target(None, &design);
        assert_eq!(target, "supabase");
        let sql = design.reset_script(target, true, false, None).unwrap().unwrap();
        assert!(
            !sql.contains("DROP SCHEMA IF EXISTS \"public\""),
            "public must survive: {sql}"
        );
        assert!(
            sql.contains("DROP SCHEMA IF EXISTS \"app\""),
            "the project's own schema still drops: {sql}"
        );

        // The flag still wins, and a design with no target falls back to postgres.
        assert_eq!(reset_target(Some("postgres"), &design), "postgres");
        std::fs::write(&config, "project:\n  name: t\n").unwrap();
        let bare = Design::from_config_with_dir(&config, "dev", Some(tmp.path())).unwrap();
        assert_eq!(reset_target(None, &bare), "postgres");
    }

    /// With no database configured — no `-d`, and a target with no `url` — the
    /// dry run builds the reset script without a connection. The project names
    /// no URL rather than `$DATABASE_URL`, so a variable set in the shell running
    /// the tests cannot point it at a database.
    #[tokio::test]
    async fn reset_dry_run_needs_no_database() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("design.yaml");
        std::fs::write(
            &config,
            "project:\n  name: t\ntarget:\n  postgres: {}\nschemas:\n  - app\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/items.ddl"),
            "create table if not exists app.items (id int primary key);\n",
        )
        .unwrap();
        cmd_reset(
            &config,
            "dev",
            tmp.path(),
            None,
            None,
            ResetOptions {
                dry_run: true,
                force: false,
                drop_schemas: false,
                drop_extensions: false,
                allow_scope_change: false,
            },
            None,
            None,
            Verbosity::Normal,
        )
        .await
        .unwrap();
    }

    /// A target `url: $VAR` whose variable is unset names no database, so the
    /// dry run stays the offline preview it always was. Most projects write
    /// `url: $DATABASE_URL`; were the unresolved `$VAR` taken for a URL, every
    /// dry run on a machine without the variable would fail to connect to it.
    #[tokio::test]
    async fn reset_dry_run_with_an_unset_url_variable_needs_no_database() {
        const VAR: &str = "DBD_TEST_RESET_DRY_RUN_NEVER_SET";
        assert!(std::env::var_os(VAR).is_none(), "{VAR} must be unset for this test");
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("design.yaml");
        std::fs::write(
            &config,
            format!("project:\n  name: t\ntarget:\n  postgres:\n    url: ${VAR}\nschemas:\n  - app\n"),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/items.ddl"),
            "create table if not exists app.items (id int primary key);\n",
        )
        .unwrap();
        cmd_reset(
            &config,
            "dev",
            tmp.path(),
            None,
            None,
            ResetOptions {
                dry_run: true,
                force: false,
                drop_schemas: false,
                drop_extensions: false,
                allow_scope_change: false,
            },
            None,
            None,
            Verbosity::Normal,
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
        let design =
            Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures())).unwrap();
        let resolved = design.resolve_scope(None, None).unwrap();

        let mock = MockAdapter::new().with_version(0).with_scope("a_different_scope");

        let err = reset_with_adapter(&mock, &design, "dev", false, false, false, &resolved, false)
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
        let design =
            Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures())).unwrap();
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
        let design =
            Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures())).unwrap();
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
