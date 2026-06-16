use std::path::Path;

use anyhow::{bail, Context, Result};
use dbd_core::config::FormatConfig;
use dbd_core::reverse::{self, SchemaSelect};

/// Returns schemas that appear in `written` but not in `declared`, sorted and deduped.
fn undeclared_schemas(written: &[String], declared: &[String]) -> Vec<String> {
    let declared_set: std::collections::HashSet<&String> = declared.iter().collect();
    let mut result: Vec<String> = written
        .iter()
        .filter(|s| !declared_set.contains(s))
        .cloned()
        .collect();
    result.sort();
    result.dedup();
    result
}

/// Resolve the connection string from the single pre-resolved candidate.
///
/// The caller is responsible for combining explicit flag values with the
/// global `-d`/`$DATABASE_URL` value (which clap already reads from the
/// environment). This function simply bails with an actionable error when
/// nothing was resolved.
pub(crate) fn resolve_conn(candidate: Option<&str>) -> Result<String> {
    match candidate {
        Some(c) if !c.is_empty() => Ok(c.to_string()),
        _ => anyhow::bail!(
            "no connection given — pass a connection URL via the positional argument, \
             --from-db <url>, -d <url>, or $DATABASE_URL"
        ),
    }
}

/// The version-safety decision for a `merge` against a (possibly managed) DB.
///
/// Every `merge` that proceeds ends in a snapshot, so foreign and managed databases
/// behave identically — there are only two outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeDecision {
    /// Managed DB behind the project (D < Y) — refuse; the project is ahead of a stale DB.
    Refuse { db: u32, project: u32 },
    /// Foreign DB (no `_dbd_meta`) OR managed DB at/ahead of the project (D ≥ Y) —
    /// overwrite the introspected DDL (no `.bak`) + auto-snapshot the delta as a new version.
    Snapshot,
}

/// Decide what a `merge` should do given the DB's managed version (`None` = foreign)
/// and the project's `design.yaml` version. This is the single source of truth for
/// the gate — the handler routes through it so the tested logic is the real logic.
///
/// Only a managed DB strictly behind the project (`D < Y`) is refused; everything
/// else (a foreign DB, or a managed DB at/ahead of the project) snapshots.
pub(crate) fn merge_decision(managed: Option<u32>, project_version: u32) -> MergeDecision {
    match managed {
        Some(d) if d < project_version => MergeDecision::Refuse { db: d, project: project_version },
        _ => MergeDecision::Snapshot,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_init_from_db(
    project_dir: &Path,
    // Explicit --from-db value (None = flag absent; Some("") = bare flag → fall back).
    from_db: Option<&str>,
    // Global -d / $DATABASE_URL, already resolved by clap.
    database_url: Option<&str>,
    env: &str,
    // The design.yaml path (already resolved by the dispatcher).
    config_path: &Path,
    name: Option<&str>,
    version: u32,
    sel: SchemaSelect,
    // Opt-in: also reverse-engineer cluster-global roles.
    roles: bool,
    dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!("design.yaml already exists here — use `dbd merge` to sync a DB into an existing project");
    }
    // Precedence: explicit --from-db URL > global -d/$DATABASE_URL.
    let candidate = match from_db {
        Some(s) if !s.is_empty() => Some(s),
        _ => database_url,
    };
    let conn = resolve_conn(candidate)?;

    // Resolve the project name used both for the connection (the per-project meta
    // read keys on it) and as the generated `design.yaml` `project.name`:
    // --name > db name parsed from the conn > "project".
    let project_name = name
        .map(String::from)
        .unwrap_or_else(|| db_name_from_conn(&conn).unwrap_or_else(|| "project".into()));

    // Version-safety gate: `init --from-db` is only for databases NOT managed by
    // dbd. Detect a `_dbd_meta` table in any schema and refuse if found.
    let adapter = dbd_core::connect(&conn, &project_name)
        .await
        .context("failed to connect to the database")?;
    if let Some(d) = adapter.reverse_managed_version().await? {
        bail!(
            "database is managed by dbd (version {d}); `dbd init --from-db` is only for \
             databases not managed by dbd — use `dbd merge` from the project's repository instead"
        );
    }

    // A fresh project has no design.yaml yet, so use the default format settings —
    // matching what `dbd format` would later apply with a freshly-scaffolded config.
    let mut entities = adapter.introspect().await.context("introspection failed")?;
    // Roles are cluster-global and gated behind the `--roles` opt-in: append them
    // only when asked (introspect_roles is NOT part of introspect()).
    if roles {
        entities.extend(
            adapter
                .introspect_roles()
                .await
                .context("role introspection failed")?,
        );
    }
    init_with_entities(
        project_dir,
        config_path,
        env,
        entities,
        &project_name,
        version,
        sel,
        dry_run,
        "init from database",
    )
}

/// Reverse-engineer a fresh project from a DBML file (no DB connection).
///
/// DBML is always a foreign source — there is no `_dbd_meta` to gate against, so
/// this skips the version-safety check entirely and otherwise shares the exact
/// init tail (write-plan + baseline snapshot) with `cmd_init_from_db` via
/// [`init_with_entities`].
#[allow(clippy::too_many_arguments)]
pub fn cmd_init_from_dbml(
    project_dir: &Path,
    dbml_path: &Path,
    env: &str,
    config_path: &Path,
    name: Option<&str>,
    version: u32,
    sel: SchemaSelect,
    dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!("design.yaml already exists here — use `dbd merge --from-dbml <file>` to sync a DBML file into an existing project");
    }
    let entities = parse_dbml_file(dbml_path)?;
    // The project name is derived from --name or the file stem (there is no DB to
    // read it from).
    let project_name = name
        .map(String::from)
        .unwrap_or_else(|| {
            dbml_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| "project".into())
        });
    init_with_entities(
        project_dir,
        config_path,
        env,
        entities,
        &project_name,
        version,
        sel,
        dry_run,
        "init from dbml",
    )
}

/// Read + parse a DBML file into entities, with file-level error context.
///
/// `dbml_path` is an operator-supplied CLI argument (the `--from-dbml <FILE>`
/// value), intentionally an arbitrary path — the user may keep their `.dbml`
/// anywhere — so it is not constrained to the project root, mirroring how
/// `--from-db <CONN>` accepts an arbitrary connection string.
fn parse_dbml_file(dbml_path: &Path) -> Result<Vec<dbd_core::Entity>> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path — operator-supplied CLI path, not external input
    let text = std::fs::read_to_string(dbml_path)
        .with_context(|| format!("failed to read DBML file {}", dbml_path.display()))?;
    dbd_core::dbml_parse::parse_dbml(&text)
        .with_context(|| format!("failed to parse DBML file {}", dbml_path.display()))
}

/// Shared `init` tail for both the DB and DBML sources: build the write-plan,
/// write `design.yaml`, apply by overwriting, then emit a baseline snapshot at
/// `version`. The caller owns acquiring `entities` (introspection or DBML parse)
/// and any source-specific safety gating before calling this.
#[allow(clippy::too_many_arguments)]
fn init_with_entities(
    project_dir: &Path,
    config_path: &Path,
    env: &str,
    entities: Vec<dbd_core::Entity>,
    project_name: &str,
    version: u32,
    sel: SchemaSelect,
    dry_run: bool,
    snapshot_label: &str,
) -> Result<()> {
    run_plan(
        project_dir,
        config_path,
        entities,
        Some(project_name),
        Some(version),
        sel,
        dry_run,
        true,
        &FormatConfig::default(),
    )?;

    // Emit a baseline snapshot at `--version` so the new project is version-tracked
    // from the start (end state: snapshots/{version}.json + design.yaml
    // project.version = version). Skip entirely on --dry-run.
    if dry_run {
        println!("[dry-run] would create baseline snapshot v{version}");
        return Ok(());
    }

    // Reload from the freshly-written DDL so the snapshot reflects exactly what landed
    // on disk (and what `dbd apply` would later read), then write the baseline.
    let design = dbd_core::Design::from_config_with_dir(config_path, env, Some(project_dir))
        .context("failed to reload project after writing DDL")?;
    dbd_core::snapshot::create_baseline_snapshot(
        design.entities(), project_dir, config_path, snapshot_label, version,
    )
    .context("failed to create baseline snapshot after writing DDL")?;
    println!("baseline snapshot v{version} created");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_merge(
    project_dir: &Path,
    // Explicit positional connection argument.
    conn: Option<&str>,
    // Global -d / $DATABASE_URL, already resolved by clap.
    database_url: Option<&str>,
    env: &str,
    // The design.yaml path (already resolved by the dispatcher).
    config_path: &Path,
    sel: SchemaSelect,
    // Opt-in: also reverse-engineer cluster-global roles.
    roles: bool,
    dry_run: bool,
) -> Result<()> {
    if !config_path.exists() {
        bail!("no design.yaml here — use `dbd init --from-db <conn>` to start a new project");
    }
    // Load the existing project's config once: its `project.name` keys the per-project
    // meta read AND the live connection; its `format` settings drive emit-side
    // formatting; its declared schemas drive the undeclared-schema warning below.
    let config = dbd_core::config::read(config_path)
        .context("failed to read design.yaml")?;
    let project_name = config.project.name.clone();
    // Precedence: explicit positional conn > global -d/$DATABASE_URL.
    let candidate = conn.or(database_url);
    let conn = resolve_conn(candidate)?;

    // Connect with the REAL project name (not the literal "reverse") so the
    // `_dbd_meta` lookup keys on the right (project, env).
    let adapter = dbd_core::connect(&conn, &project_name)
        .await
        .context("failed to connect to the database")?;

    // Version-safety gate. Every merge that proceeds ends in a snapshot, so foreign
    // and managed databases take the same overwrite+snapshot path; only a managed DB
    // strictly behind the project is refused.
    let managed = adapter.reverse_managed_version().await?;
    let project_version = config.project.version.unwrap_or(0);
    match merge_decision(managed, project_version) {
        MergeDecision::Refuse { db, project } => {
            bail!(
                "refusing to merge: database is at version {db} but this project is at version \
                 {project} — the project is ahead of a stale database. Bring the database up to \
                 date (`dbd apply`), or revert the project to v{db} via version control if you \
                 mean to discard those changes."
            );
        }
        MergeDecision::Snapshot => {
            let mut entities = adapter.introspect().await.context("introspection failed")?;
            // Roles are cluster-global and gated behind the `--roles` opt-in: append
            // them only when asked (introspect_roles is NOT part of introspect()).
            if roles {
                entities.extend(
                    adapter
                        .introspect_roles()
                        .await
                        .context("role introspection failed")?,
                );
            }
            merge_snapshot(
                project_dir, config_path, env, &entities, sel, dry_run, &config,
            )
        }
    }
}

/// Sync a DBML file into the current project (no DB connection).
///
/// DBML is always a foreign source — there is no `_dbd_meta` to gate against, so
/// this skips the version-safety decision entirely and goes straight to the
/// shared snapshot path with the parsed entities. Schema selection, orphan
/// reporting, the undeclared-schema warning, and `--dry-run` all behave exactly
/// as the DB merge path because both call [`merge_snapshot`].
pub fn cmd_merge_from_dbml(
    project_dir: &Path,
    dbml_path: &Path,
    env: &str,
    config_path: &Path,
    sel: SchemaSelect,
    dry_run: bool,
) -> Result<()> {
    if !config_path.exists() {
        bail!("no design.yaml here — use `dbd init --from-dbml <file>` to start a new project");
    }
    let config = dbd_core::config::read(config_path)
        .context("failed to read design.yaml")?;
    let entities = parse_dbml_file(dbml_path)?;
    merge_snapshot(project_dir, config_path, env, &entities, sel, dry_run, &config)
}

/// The unified `merge` apply path (foreign DB, or managed DB at/ahead of the project):
/// write the introspected DDL into the project, **overwriting drift with NO `.bak`**,
/// then auto-snapshot the delta as a new version. `--dry-run` previews the plan + the
/// snapshot version and writes nothing.
///
/// Reports orphans (never deleted), unsafe-path skips, and warns about schemas written
/// but not declared in `design.yaml` — the same surfacing the foreign path used.
///
/// # Non-transactionality (accepted limitation)
/// `apply_overwrite` writes all DDL files before the project is reloaded and
/// `create_snapshot` is called. If the reload or snapshot step fails after files
/// have been written, the project directory is in a partial-apply state: files are
/// already overwritten with no `.bak` and no snapshot exists. Recover via version
/// control (`git checkout -- .` or equivalent). This mirrors the non-transactionality
/// note on `apply_overwrite`.
fn merge_snapshot(
    project_dir: &Path,
    config_path: &Path,
    env: &str,
    entities: &[dbd_core::Entity],
    sel: SchemaSelect,
    dry_run: bool,
    config: &dbd_core::config::DesignConfig,
) -> Result<()> {
    let format = &config.format;
    // Select schemas + keep only entities under a selected schema (same logic as
    // the normal path).
    let (selected, kept) = select_and_keep(entities, &sel);
    if selected.is_empty() {
        println!("No user schemas to reverse-engineer (after filtering). Nothing to do.");
        return Ok(());
    }

    // Warn about schemas written but not declared in design.yaml (spec line 67).
    // `merge` never edits config — it surfaces the gap so the user adds the schema.
    let declared = config.schema_names();
    for schema in undeclared_schemas(&selected, &declared) {
        eprintln!(
            "  warning: schema `{schema}` written but not listed in design.yaml; add it to include those files"
        );
    }

    let plan = reverse::plan_from_entities(project_dir, &kept, &selected, format);

    if dry_run {
        // Count straight off the plan — a dry-run writes nothing, so there's no
        // apply step to perform; the overwrite path has no conflict gate.
        let created = plan.items.iter().filter(|i| i.action == reverse::FileAction::Create).count();
        let unchanged = plan.items.iter().filter(|i| i.action == reverse::FileAction::Skip).count();
        let conflicts = plan.items.iter().filter(|i| i.action == reverse::FileAction::Conflict).count();
        println!(
            "[dry-run] {created} created · {unchanged} unchanged · {conflicts} conflict(s) (would overwrite, no .bak)"
        );
        for it in plan.items.iter().filter(|i| i.action == reverse::FileAction::Conflict) {
            println!("  conflict (differs from DB): {}", it.path.display());
        }
        for o in &plan.orphans {
            println!("  orphan (no DB entity): {}", o.display());
        }
        for label in &plan.skipped_unsafe {
            eprintln!("  warning: skipped unsafe path segment: {label}");
        }
        // Only promise a snapshot when there is actually something to write.
        // If every item is Skip the real run would print "already in sync — no
        // snapshot created", so the dry-run preview must match.
        //
        // Note: this is a *byte-level* check (the write-plan). The real run's
        // `create_snapshot` decides no-changes by a *semantic* diff of the parsed
        // entity model vs the latest snapshot, so a file that differs only
        // cosmetically (a byte Conflict that parses to an identical entity) is
        // previewed here as "would snapshot" yet the real run reports in-sync. The
        // overwrite still happens correctly either way — this is preview accuracy
        // only, not a data-safety gap.
        let has_changes = plan
            .items
            .iter()
            .any(|i| i.action != reverse::FileAction::Skip);
        if has_changes {
            let n = dbd_core::snapshot::next_version(project_dir);
            println!("[dry-run] would capture changes as snapshot v{n}");
        } else {
            println!("[dry-run] already in sync — no snapshot needed");
        }
        return Ok(());
    }

    // Apply: overwrite Create + Conflict in place (no `.bak`), skip Skip.
    let report = reverse::apply_overwrite(project_dir, &plan)?;
    for o in &plan.orphans {
        println!("  orphan (no DB entity): {}", o.display());
    }
    for label in &plan.skipped_unsafe {
        eprintln!("  warning: skipped unsafe path segment: {label}");
    }
    let files_written = report.created + report.overwritten;

    // Reload the project from the freshly-written DDL, then snapshot the delta.
    // `create_snapshot` bumps design.yaml's version and writes snapshot+migration
    // files itself — do not duplicate that here.
    //
    // NOTE: both calls below happen *after* apply_overwrite has written all DDL
    // files. A failure here leaves the project in a partial-apply state (files
    // overwritten, no snapshot). Recover via version control.
    let design = dbd_core::Design::from_config_with_dir(config_path, env, Some(project_dir))
        .context("failed to reload project after writing DDL — recover via version control if files were partially written")?;
    let snap = dbd_core::snapshot::create_snapshot(
        design.entities(), project_dir, config_path, "merge from database",
    )
    .context("failed to create snapshot after writing DDL — recover via version control if files were partially written")?;

    // `create_snapshot` handles the no-previous-snapshot (baseline) case, so a
    // project without prior snapshots still snapshots cleanly. A single result
    // flagged `no_changes` means the DDL was already in sync with the latest snapshot.
    if snap.snapshots.len() == 1 && snap.snapshots[0].no_changes {
        println!(
            "{files_written} file(s) written — already in sync — no snapshot created"
        );
    } else {
        let version = snap.snapshots.last().map(|s| s.snapshot.version).unwrap_or(0);
        println!("{files_written} file(s) written · snapshot v{version} created");
    }
    Ok(())
}

/// Select schemas from the introspected entities and keep only the entities under
/// a selected schema.
///
/// A schema entity "belongs to" itself (its own name), so it is filtered by name —
/// otherwise a denylisted/excluded schema (e.g. Supabase `extensions`) would still
/// get a stray `ddl/schema/<name>.ddl` file even though it's left out of
/// design.yaml. Truly schema-less entities (e.g. roles) have no owning schema and
/// are always kept.
fn select_and_keep(
    entities: &[dbd_core::Entity],
    sel: &SchemaSelect,
) -> (Vec<String>, Vec<dbd_core::Entity>) {
    let db_schemas: Vec<String> = {
        let mut s: Vec<String> = entities.iter().filter_map(|e| e.schema.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    let selected = reverse::select_schemas(&db_schemas, sel);
    let kept: Vec<_> = entities
        .iter()
        .filter(|e| {
            let owning = if e.entity_type == dbd_core::EntityType::Schema {
                Some(e.name.as_str())
            } else {
                e.schema.as_deref()
            };
            owning.is_none_or(|s| selected.iter().any(|sel| sel == s))
        })
        .cloned()
        .collect();
    (selected, kept)
}

/// Build the write-plan from already-introspected entities, write `design.yaml`
/// (init only), apply by **overwriting** in place (no `.bak`), and report. The caller
/// owns the connection + introspection (so the version-safety gate can act on the
/// adapter before any writes).
///
/// This is the `init --from-db` path. A fresh project directory has no managed files
/// yet, so every item is a `Create`; `apply_overwrite` handles that the same as the
/// unified `merge` path (and harmlessly clobbers any pre-existing drift, since the
/// baseline snapshot + version control are the record).
#[allow(clippy::too_many_arguments)]
fn run_plan(
    project_dir: &Path,
    config_path: &Path,
    entities: Vec<dbd_core::Entity>,
    name: Option<&str>,
    version: Option<u32>,
    sel: SchemaSelect,
    dry_run: bool,
    write_config: bool,
    format: &FormatConfig,
) -> Result<Vec<String>> {
    // 1. select schemas + keep only entities under a selected schema
    let (selected, kept) = select_and_keep(&entities, &sel);
    if selected.is_empty() {
        println!("No user schemas to reverse-engineer (after filtering). Nothing to do.");
        return Ok(vec![]);
    }

    // 2. build write-plan (emitted DDL is formatted so it matches `dbd format`)
    let plan = reverse::plan_from_entities(project_dir, &kept, &selected, format);

    // 3. write design.yaml (init only; only if absent; skip on dry-run)
    if write_config && !dry_run {
        let project = name
            .map(String::from)
            .unwrap_or_else(|| "project".into());
        let yaml = reverse::design_yaml(&project, "postgres", &selected, version.unwrap_or(1));
        // Write to the resolved config path (project_dir/<--config>), matching what the
        // baseline-snapshot reload reads — not a hardcoded "design.yaml" (which would
        // diverge under `--config custom.yaml`).
        std::fs::write(config_path, yaml)
            .context("failed to write design.yaml")?;
    }

    // 4. apply + report
    if dry_run {
        let created = plan.items.iter().filter(|i| i.action == reverse::FileAction::Create).count();
        let unchanged = plan.items.iter().filter(|i| i.action == reverse::FileAction::Skip).count();
        println!(
            "[dry-run] {created} created · {unchanged} unchanged · {} orphan(s) left as-is",
            plan.orphans.len()
        );
    } else {
        let report = reverse::apply_overwrite(project_dir, &plan)?;
        let files_written = report.created + report.overwritten;
        println!(
            "{files_written} file(s) written · {} unchanged · {} orphan(s) left as-is",
            report.unchanged, report.orphans
        );
    }
    for o in &plan.orphans {
        println!("  orphan (no DB entity): {}", o.display());
    }
    for label in &plan.skipped_unsafe {
        eprintln!("  warning: skipped unsafe path segment: {label}");
    }
    Ok(selected)
}

/// Parse the database name out of a connection string for the default project name.
/// Best-effort heuristic: a URL with a trailing slash and no db name (e.g. `postgres://host/`)
/// yields `None` and falls back to `"project"`; `--name` always overrides this entirely.
fn db_name_from_conn(conn: &str) -> Option<String> {
    let after = conn.rsplit('/').next()?;
    let db = after.split(['?', '#']).next()?;
    if db.is_empty() {
        None
    } else {
        Some(db.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// All written schemas are declared — no warnings.
    #[test]
    fn undeclared_schemas_all_declared_returns_empty() {
        let written = strs(&["app", "config"]);
        let declared = strs(&["app", "config", "staging"]);
        assert_eq!(undeclared_schemas(&written, &declared), Vec::<String>::new());
    }

    /// One written schema is missing from declared — returned.
    #[test]
    fn undeclared_schemas_one_missing() {
        let written = strs(&["app", "new_schema"]);
        let declared = strs(&["app"]);
        assert_eq!(undeclared_schemas(&written, &declared), strs(&["new_schema"]));
    }

    /// Duplicates in `written` are deduped in the result.
    #[test]
    fn undeclared_schemas_deduplicates() {
        let written = strs(&["app", "app", "new_schema", "new_schema"]);
        let declared = strs(&["app"]);
        assert_eq!(undeclared_schemas(&written, &declared), strs(&["new_schema"]));
    }

    /// Result is sorted alphabetically.
    #[test]
    fn undeclared_schemas_sorted() {
        let written = strs(&["zzz", "aaa", "mmm"]);
        let declared = strs(&[]);
        assert_eq!(undeclared_schemas(&written, &declared), strs(&["aaa", "mmm", "zzz"]));
    }

    /// Empty written — empty result regardless of declared.
    #[test]
    fn undeclared_schemas_empty_written() {
        let written = strs(&[]);
        let declared = strs(&["app"]);
        assert_eq!(undeclared_schemas(&written, &declared), Vec::<String>::new());
    }

    // ── merge_decision ────────────────────────────────────────────────────────

    /// Foreign DB (no `_dbd_meta`) → snapshot (unified overwrite + auto-snapshot path).
    #[test]
    fn merge_decision_foreign_snapshots() {
        assert_eq!(merge_decision(None, 5), MergeDecision::Snapshot);
    }

    /// Managed DB behind the project (D < Y) → refuse.
    #[test]
    fn merge_decision_refuses_when_db_behind() {
        assert_eq!(merge_decision(Some(2), 5), MergeDecision::Refuse { db: 2, project: 5 });
    }

    /// Managed DB at the project version (D == Y) → snapshot.
    #[test]
    fn merge_decision_snapshots_when_equal() {
        assert_eq!(merge_decision(Some(5), 5), MergeDecision::Snapshot);
    }

    /// Managed DB ahead of the project (D > Y) → snapshot.
    #[test]
    fn merge_decision_snapshots_when_db_ahead() {
        assert_eq!(merge_decision(Some(7), 5), MergeDecision::Snapshot);
    }

    /// Both at zero (unset project version) → snapshot.
    #[test]
    fn merge_decision_snapshots_at_zero() {
        assert_eq!(merge_decision(Some(0), 0), MergeDecision::Snapshot);
    }

    // ── resolve_conn ──────────────────────────────────────────────────────────

    /// Explicit non-empty value is returned as-is.
    #[test]
    fn resolve_conn_explicit_wins() {
        let got = resolve_conn(Some("postgres://explicit/db")).unwrap();
        assert_eq!(got, "postgres://explicit/db");
    }

    /// None candidate bails with an actionable message.
    #[test]
    fn resolve_conn_none_bails() {
        let err = resolve_conn(None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("-d") || msg.contains("$DATABASE_URL"), "message should mention -d or $DATABASE_URL: {msg}");
    }

    /// Empty string candidate bails (same as None — sentinel for bare --from-db with no fallback).
    #[test]
    fn resolve_conn_empty_bails() {
        let err = resolve_conn(Some("")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("-d") || msg.contains("$DATABASE_URL"), "message should mention -d or $DATABASE_URL: {msg}");
    }

    // ── db_name_from_conn ─────────────────────────────────────────────────────

    /// Standard postgres URL → database name extracted.
    #[test]
    fn db_name_from_conn_standard_url() {
        assert_eq!(db_name_from_conn("postgres://user:pass@host/mydb"), Some("mydb".into()));
    }

    /// URL with query string → query stripped.
    #[test]
    fn db_name_from_conn_strips_query() {
        assert_eq!(db_name_from_conn("postgres://host/mydb?sslmode=require"), Some("mydb".into()));
    }

    /// URL with trailing slash and no db name → None (falls back to "project" at call site).
    #[test]
    fn db_name_from_conn_trailing_slash_returns_none() {
        assert_eq!(db_name_from_conn("postgres://host/"), None);
    }
}
