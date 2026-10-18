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
    module: Option<&str>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let targets = super::targets(config, env, project_dir, scope, deps, module, None, super::Order::Build)?;
    let mut diffs: Vec<(Option<String>, String, SchemaDiff)> = Vec::new();
    for (i, target) in targets.into_iter().enumerate() {
        let module = target.module.clone();
        // Under --json stdout is the document: which module is next goes to
        // stderr, as the scope line does.
        let design = match (json, target.label()) {
            (true, Some(label)) => {
                eprintln!("{label}");
                target.design
            }
            _ => target.enter(i == 0).0,
        };
        let owner = design.owner().to_string();
        let diff = diff_design(design, config, database_url, json, scope, deps).await?;
        diffs.push((module, owner, diff));
    }

    if json {
        // A project's diff is its document; a workspace's, each module's under
        // its name — one document either way, for a pipeline to parse.
        let doc = match &diffs[..] {
            [(None, _, diff)] => serde_json::to_value(diff).context("Failed to serialize diff")?,
            _ => {
                let mut modules = Vec::new();
                for (module, owner, diff) in &diffs {
                    let diff = serde_json::to_value(diff).context("Failed to serialize diff")?;
                    modules.push(serde_json::json!({ "module": module, "owner": owner, "diff": diff }));
                }
                serde_json::json!({ "modules": modules })
            }
        };
        output::always(&serde_json::to_string_pretty(&doc).context("Failed to serialize diff")?);
    }
    // Every module answers: the run drifts if any module does.
    if let Some(code) = diff_exit_code(diffs.iter().all(|(_, _, d)| d.is_empty()), exit_code) {
        std::process::exit(code);
    }
    let _ = verbosity;
    Ok(())
}

/// [`cmd_diff`] for one design: the project's, or one module's — its report
/// said, unless the run's is one JSON document.
async fn diff_design(
    design: Design,
    config: &Path,
    database_url: Option<&str>,
    json: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
) -> Result<SchemaDiff> {
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    // On stderr under --json: stdout is the document, and a pipeline parses it.
    let (kept, total) = design.scope_counts(&resolved)?;
    match output::scope_line(&resolved, kept, total) {
        Some(line) if json => eprintln!("{line}"),
        Some(line) => output::always(&line),
        None => {}
    }
    let adapter = get_adapter(config, database_url, design.owner()).await?;
    if let Some((pinned, by)) = super::other_scope_pin(&*adapter, &design, &resolved).await? {
        let note = format!(
            "note: this database is pinned to scope '{pinned}'{by}; this diff compares scope '{}', so what \
             '{pinned}' does not build reads as missing — `--scope {pinned}` compares what it was built with.",
            resolved.name
        );
        if json {
            eprintln!("{note}");
        } else {
            output::always(&note);
        }
    }

    let diff = design
        .diff_live(&*adapter, Some(&resolved))
        .await
        .context("Diff failed")?;

    if !json {
        for line in diff_report_lines(&diff) {
            output::always(&line);
        }
    }
    Ok(diff)
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

    /// A key change a foreign key holds is shown as reconcile runs it: the
    /// holder's drop above the table's own SQL, the kept one's re-add below;
    /// a refusal is said, naming what holds the key.
    #[test]
    fn a_key_change_shows_the_foreign_keys_around_it() {
        use dbd_core::diff::{ChangeAction, DiffAction, FieldChange, FieldDetail, FieldType, MigrationDiff};
        use dbd_core::entity::TableConstraint;
        use dbd_core::schema_diff::KeyHolderSteps;
        let d = SchemaDiff {
            changes: vec![MigrationDiff {
                entity_name: "app.parent".to_string(),
                entity_type: dbd_core::EntityType::Table,
                action: DiffAction::Change(vec![FieldChange {
                    field_name: "pk:id".to_string(),
                    field_type: FieldType::Constraint,
                    action: ChangeAction::Add(Box::new(FieldDetail::Constraint(TableConstraint::PrimaryKey {
                        name: None,
                        columns: vec!["id".to_string()],
                    }))),
                }]),
            }],
            key_holders: vec![
                KeyHolderSteps {
                    table: "app.parent".to_string(),
                    drops: vec!["ALTER TABLE app.child DROP CONSTRAINT IF EXISTS child_fk;".to_string()],
                    adds: vec!["ALTER TABLE app.child ADD FOREIGN KEY (pid) REFERENCES app.parent(id);".to_string()],
                    refused: None,
                },
                KeyHolderSteps {
                    table: "app.other".to_string(),
                    refused: Some("app.other's primary key — held by ext.audit".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let lines = diff_report_lines(&d);
        let at = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} not in {lines:#?}"))
        };
        assert!(at("~ alter  app.parent") < at("DROP CONSTRAINT IF EXISTS child_fk"));
        assert!(at("DROP CONSTRAINT IF EXISTS child_fk") < at("ADD PRIMARY KEY (id)"));
        assert!(at("ADD PRIMARY KEY (id)") < at("ADD FOREIGN KEY (pid)"));
        assert!(
            lines[at("held by ext.audit")].contains("reconcile refuses"),
            "{lines:#?}"
        );
    }
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
            key_holders: Vec::new(),
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
            key_holders: Vec::new(),
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
            key_holders: Vec::new(),
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
            key_holders: Vec::new(),
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
