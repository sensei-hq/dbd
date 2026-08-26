use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::SchemaDiff;
use dbd_core::design::Design;

use crate::output::{self, Verbosity};

use super::get_adapter;

/// Build the human-readable lines for a schema diff. Pure so it can be
/// unit-tested. Each changed entity gets one summary line and, for alters, the
/// generated SQL indented beneath; warnings, advisories, and materialized-view
/// drift follow.
pub(crate) fn diff_report_lines(d: &SchemaDiff) -> Vec<String> {
    use dbd_core::diff::{self, DiffAction};
    use dbd_core::schema_diff::MatviewDriftKind;
    if d.is_empty() {
        return vec!["Live database is in sync with the design — no differences.".to_string()];
    }
    let mut lines = Vec::new();
    for c in &d.changes {
        match &c.action {
            DiffAction::Add => lines.push(format!("  + create {}", c.entity_name)),
            DiffAction::Drop => lines.push(format!("  - drop   {}", c.entity_name)),
            DiffAction::Change(_) => {
                lines.push(format!("  ~ alter  {}", c.entity_name));
                let sql = diff::generate_migration_sql(std::slice::from_ref(c));
                for l in sql.lines().filter(|l| !l.trim().is_empty()) {
                    lines.push(format!("      {l}"));
                }
            }
        }
    }
    for w in &d.warnings {
        lines.push(format!("  ⚠ {w}"));
    }
    for a in &d.advisories {
        lines.push(format!("  ⚠ advisory: {a}"));
    }
    for m in &d.matview_drift {
        match m.kind {
            MatviewDriftKind::Missing => lines.push(format!("  + create materialized view {}", m.name)),
            MatviewDriftKind::Drifted => lines.push(format!(
                "  ~ matview  {} (definition differs — drop + re-apply to update)",
                m.name
            )),
            MatviewDriftKind::Unstamped => lines.push(format!(
                "  ⚠ materialized view {}: not stamped by dbd (definition unverifiable)",
                m.name
            )),
            MatviewDriftKind::Orphan => lines.push(format!("  - materialized view {} (only in database)", m.name)),
        }
    }
    lines
}

/// `dbd diff` — read-only full diff of the live database against the design.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_diff(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    json: bool,
    exit_code: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir)).context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let adapter = get_adapter(config, database_url).await?;

    let diff = design
        .diff_live(&*adapter, Some(&resolved))
        .await
        .context("Diff failed")?;

    if json {
        let doc = serde_json::to_string_pretty(&diff).context("Failed to serialize diff")?;
        output::always(&doc);
    } else {
        for line in diff_report_lines(&diff) {
            output::always(&line);
        }
    }

    if let Some(code) = diff_exit_code(diff.is_empty(), exit_code) {
        std::process::exit(code);
    }
    let _ = verbosity;
    Ok(())
}

/// terraform-`plan` style exit codes for `--exit-code`: `Some(0)` in sync,
/// `Some(2)` when differences exist, `None` when the flag is off (caller keeps
/// the normal `0`). Errors are handled separately by the caller (exit 1).
pub(crate) fn diff_exit_code(is_empty: bool, exit_code_flag: bool) -> Option<i32> {
    if !exit_code_flag {
        return None;
    }
    Some(if is_empty { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbd_core::diff::{DiffAction, MigrationDiff};
    use dbd_core::entity::EntityType;
    use dbd_core::schema_diff::{MatviewDrift, MatviewDriftKind};

    fn add(name: &str) -> MigrationDiff {
        MigrationDiff {
            entity_name: name.into(),
            entity_type: EntityType::Table,
            action: DiffAction::Add,
        }
    }

    fn mv(name: &str, kind: MatviewDriftKind) -> MatviewDrift {
        MatviewDrift {
            name: name.into(),
            kind,
        }
    }

    /// In-sync diff renders the friendly "no differences" line.
    #[test]
    fn renders_in_sync() {
        let d = SchemaDiff::default();
        let out = diff_report_lines(&d).join("\n");
        assert!(out.contains("in sync"), "got: {out}");
    }

    /// A create shows a `+ create` line and advisories are surfaced.
    #[test]
    fn renders_changes_and_advisories() {
        let d = SchemaDiff {
            changes: vec![add("public.audit_log")],
            warnings: vec![],
            advisories: vec!["CHECK ck on public.orders couldn't be normalized".into()],
            matview_drift: vec![],
        };
        let out = diff_report_lines(&d).join("\n");
        assert!(out.contains("+ create public.audit_log"), "got: {out}");
        assert!(out.contains("advisory"), "advisory must render; got: {out}");
    }

    /// Locks the top-level `--json` shape (keys tooling depends on) plus the
    /// per-change `entity_name`/`entity_type`/`action` discriminant so a
    /// rename or a switch away from serde's default enum representation
    /// trips this test instead of silently breaking downstream tooling.
    #[test]
    fn json_shape_is_stable() {
        let d = SchemaDiff {
            changes: vec![add("public.audit_log")],
            warnings: vec![],
            advisories: vec![],
            matview_drift: vec![],
        };
        let json = serde_json::to_string(&d).unwrap();
        // Top-level keys tooling depends on.
        assert!(json.contains("\"changes\""), "got: {json}");
        assert!(json.contains("\"warnings\""), "got: {json}");
        assert!(json.contains("\"advisories\""), "got: {json}");
        assert!(json.contains("public.audit_log"), "got: {json}");
        // Per-change fields and the `Add` unit-variant discriminant, so a
        // field rename or enum-representation change fails this test.
        assert!(json.contains("\"entity_name\":\"public.audit_log\""), "got: {json}");
        assert!(json.contains("\"entity_type\":\"table\""), "got: {json}");
        assert!(json.contains("\"action\":\"Add\""), "got: {json}");
    }

    /// A diff carrying ONLY matview drift is not "in sync": it renders the
    /// per-kind drift lines, never the in-sync message, and `--exit-code`
    /// reports drift (exit 2).
    #[test]
    fn renders_matview_drift_missing_and_drifted() {
        let d = SchemaDiff {
            changes: vec![],
            warnings: vec![],
            advisories: vec![],
            matview_drift: vec![
                mv("analytics.daily", MatviewDriftKind::Drifted),
                mv("analytics.new", MatviewDriftKind::Missing),
            ],
        };
        assert!(!d.is_empty(), "matview-drift-only diff must not be in sync");
        let out = diff_report_lines(&d).join("\n");
        assert!(!out.contains("in sync"), "must not print the in-sync line; got: {out}");
        assert!(out.contains("+ create materialized view analytics.new"), "got: {out}");
        assert!(out.contains("~ matview  analytics.daily"), "got: {out}");
        assert_eq!(diff_exit_code(d.is_empty(), true), Some(2), "drift → exit 2");
    }

    /// Unstamped and Orphan kinds render their own lines.
    #[test]
    fn renders_matview_drift_unstamped_and_orphan() {
        let d = SchemaDiff {
            changes: vec![],
            warnings: vec![],
            advisories: vec![],
            matview_drift: vec![
                mv("analytics.legacy", MatviewDriftKind::Unstamped),
                mv("analytics.stale", MatviewDriftKind::Orphan),
            ],
        };
        let out = diff_report_lines(&d).join("\n");
        assert!(
            out.contains("materialized view analytics.legacy: not stamped by dbd"),
            "got: {out}"
        );
        assert!(
            out.contains("- materialized view analytics.stale (only in database)"),
            "got: {out}"
        );
    }

    #[test]
    fn exit_code_semantics() {
        assert_eq!(diff_exit_code(true, false), None); // flag off → no forced exit
        assert_eq!(diff_exit_code(false, false), None); // flag off → no forced exit
        assert_eq!(diff_exit_code(true, true), Some(0)); // --exit-code, in sync
        assert_eq!(diff_exit_code(false, true), Some(2)); // --exit-code, drift
    }
}
