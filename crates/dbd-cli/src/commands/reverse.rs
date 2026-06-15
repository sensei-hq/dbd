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

#[allow(clippy::too_many_arguments)]
pub async fn cmd_init_from_db(
    project_dir: &Path,
    // Explicit --from-db value (None = flag absent; Some("") = bare flag → fall back).
    from_db: Option<&str>,
    // Global -d / $DATABASE_URL, already resolved by clap.
    database_url: Option<&str>,
    name: Option<&str>,
    version: u32,
    sel: SchemaSelect,
    force: bool,
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
    run(project_dir, &conn, name, Some(version), sel, force, dry_run, true)
        .await
        .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_merge(
    project_dir: &Path,
    // Explicit positional connection argument.
    conn: Option<&str>,
    // Global -d / $DATABASE_URL, already resolved by clap.
    database_url: Option<&str>,
    sel: SchemaSelect,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let design_yaml_path = project_dir.join("design.yaml");
    if !design_yaml_path.exists() {
        bail!("no design.yaml here — use `dbd init --from-db <conn>` to start a new project");
    }
    // Precedence: explicit positional conn > global -d/$DATABASE_URL.
    let candidate = conn.or(database_url);
    let conn = resolve_conn(candidate)?;
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

    // Keep only entities whose owning schema is in the selected set. A schema entity
    // "belongs to" itself (its own name), so it must be filtered by name — otherwise a
    // denylisted/excluded schema (e.g. Supabase `extensions`) would still get a stray
    // ddl/schema/<name>.ddl file even though it's left out of design.yaml. Truly
    // schema-less entities (e.g. roles) have no owning schema and are always kept.
    let kept: Vec<_> = entities
        .into_iter()
        .filter(|e| {
            let owning = if e.entity_type == dbd_core::EntityType::Schema {
                Some(e.name.as_str())
            } else {
                e.schema.as_deref()
            };
            owning.is_none_or(|s| selected.iter().any(|sel| sel == s))
        })
        .collect();

    // 3. build write-plan
    let plan = reverse::plan_from_entities(project_dir, &kept, &selected);

    // 4. write design.yaml (init only; only if absent; skip on dry-run)
    if write_config && !dry_run {
        let project = name
            .map(String::from)
            .unwrap_or_else(|| db_name_from_conn(conn).unwrap_or_else(|| "project".into()));
        let yaml = reverse::design_yaml(&project, "postgres", &selected, version.unwrap_or(1));
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
