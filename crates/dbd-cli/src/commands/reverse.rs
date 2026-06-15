use std::path::Path;

use anyhow::{bail, Context, Result};
use dbd_core::reverse::{self, SchemaSelect};

/// Resolve the connection string: explicit arg, else $DATABASE_URL.
fn resolve_conn(arg: Option<&str>) -> Result<String> {
    if let Some(c) = arg {
        return Ok(c.to_string());
    }
    std::env::var("DATABASE_URL")
        .context("no connection given: pass it as an argument or set $DATABASE_URL")
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_init_from_db(
    project_dir: &Path,
    conn: &str,
    name: Option<&str>,
    version: u32,
    sel: SchemaSelect,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!("design.yaml already exists here — use `dbd merge` to sync a DB into an existing project");
    }
    run(project_dir, conn, name, Some(version), sel, force, dry_run, true).await
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_merge(
    project_dir: &Path,
    conn: Option<&str>,
    sel: SchemaSelect,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    if !project_dir.join("design.yaml").exists() {
        bail!("no design.yaml here — use `dbd init --from-db <conn>` to start a new project");
    }
    let conn = resolve_conn(conn)?;
    run(project_dir, &conn, None, None, sel, force, dry_run, false).await
}

#[allow(clippy::too_many_arguments)]
async fn run(
    project_dir: &Path,
    conn: &str,
    name: Option<&str>,
    version: Option<u32>,
    sel: SchemaSelect,
    force: bool,
    dry_run: bool,
    write_config: bool,
) -> Result<()> {
    // 1. connect + introspect
    let adapter = dbd_core::connect(conn, "reverse")
        .await
        .context("failed to connect to the database")?;
    let entities = adapter.introspect().await.context("introspection failed")?;

    // 2. select schemas (derived from schemas present on the introspected entities)
    let db_schemas: Vec<String> = {
        let mut s: Vec<String> = entities.iter().filter_map(|e| e.schema.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    let selected = reverse::select_schemas(&db_schemas, &sel);
    if selected.is_empty() {
        println!("No user schemas to reverse-engineer (after filtering). Nothing to do.");
        return Ok(());
    }

    // Keep only entities whose schema is in the selected set (or schema-less entities).
    let kept: Vec<_> = entities
        .into_iter()
        .filter(|e| e.schema.as_ref().is_none_or(|s| selected.contains(s)))
        .collect();

    // 3. build write-plan
    let plan = reverse::plan_from_entities(project_dir, &kept, &selected);

    // 4. write design.yaml (init only; only if absent; skip on dry-run)
    if write_config && !dry_run {
        let project = name
            .map(String::from)
            .unwrap_or_else(|| db_name_from_conn(conn).unwrap_or_else(|| "project".into()));
        let yaml = reverse::design_yaml(&project, "postgresql", &selected, version.unwrap_or(1));
        std::fs::write(project_dir.join("design.yaml"), yaml)
            .context("failed to write design.yaml")?;
    }

    // 5. apply + report
    let report = reverse::apply_plan(project_dir, &plan, force, dry_run)?;
    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "{}{} created · {} unchanged · {} overwritten (.bak) · {} orphan(s) left as-is",
        prefix, report.created, report.unchanged, report.overwritten, report.orphans
    );
    for o in &plan.orphans {
        println!("  orphan (no DB entity): {}", o.display());
    }
    for label in &plan.skipped_unsafe {
        eprintln!("  warning: skipped unsafe path segment: {label}");
    }
    Ok(())
}

/// Parse the database name out of a connection string for the default project name.
fn db_name_from_conn(conn: &str) -> Option<String> {
    let after = conn.rsplit('/').next()?;
    let db = after.split(['?', '#']).next()?;
    if db.is_empty() {
        None
    } else {
        Some(db.to_string())
    }
}
