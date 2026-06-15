use std::path::Path;

use anyhow::{bail, Context, Result};
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

/// Resolve the connection string: explicit arg, else $DATABASE_URL.
pub(crate) fn resolve_conn(arg: Option<&str>) -> Result<String> {
    if let Some(c) = arg {
        return Ok(c.to_string());
    }
    std::env::var("DATABASE_URL")
        .context("no connection given: pass it as an argument or set $DATABASE_URL")
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_init_from_db(
    project_dir: &Path,
    conn: Option<&str>,
    name: Option<&str>,
    version: u32,
    sel: SchemaSelect,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!("design.yaml already exists here — use `dbd merge` to sync a DB into an existing project");
    }
    let conn = resolve_conn(conn)?;
    run(project_dir, &conn, name, Some(version), sel, force, dry_run, true)
        .await
        .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_merge(
    project_dir: &Path,
    conn: Option<&str>,
    sel: SchemaSelect,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let design_yaml_path = project_dir.join("design.yaml");
    if !design_yaml_path.exists() {
        bail!("no design.yaml here — use `dbd init --from-db <conn>` to start a new project");
    }
    let conn = resolve_conn(conn)?;
    let covered = run(project_dir, &conn, None, None, sel, force, dry_run, false).await?;

    // Warn about schemas written but not declared in design.yaml (spec line 67).
    let config = dbd_core::config::read(&design_yaml_path)
        .context("failed to re-read design.yaml for schema validation")?;
    let declared = config.schema_names();
    for schema in undeclared_schemas(&covered, &declared) {
        eprintln!(
            "  warning: schema `{schema}` written but not listed in design.yaml; add it to include those files"
        );
    }

    Ok(())
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
) -> Result<Vec<String>> {
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
        return Ok(vec![]);
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
    Ok(selected)
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
}
