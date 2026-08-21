use std::path::Path;

use anyhow::{bail, Context, Result};
use dbd_core::design::{ImportComplete, Progress};
use dbd_core::{Design, Entity, EntityType};

use super::{format_import_summary, get_adapter};
use crate::output::{self, Verbosity};

/// Infer the import/export data format from a file extension.
/// `.jsonl` → "jsonl", `.tsv` → "tsv", everything else → "csv".
fn format_from_ext(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("jsonl") => "jsonl",
        Some(ext) if ext.eq_ignore_ascii_case("tsv") => "tsv",
        _ => "csv",
    }
}

/// Resolve the schema-qualified name of a `Table` entity matching `name`.
/// Falls back to `name` verbatim when no matching table is found (so an
/// ad-hoc COPY target can still be addressed by its bare name).
fn resolve_table_name(design: &Design, name: &str) -> String {
    design
        .entities()
        .iter()
        .find(|e| e.entity_type == EntityType::Table && (e.name == name || e.name.ends_with(&format!(".{name}"))))
        .map(|e| e.name.clone())
        .unwrap_or_else(|| name.to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_import_dry_run(
    config: &Path,
    env: &str,
    project_dir: &Path,
    name: Option<&str>,
    file: Option<&Path>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    // Ad-hoc single-file import plan: print the one line and write nothing.
    if let Some(path) = file {
        let name = match name {
            Some(n) => n,
            None => bail!("--file requires --name <entity>"),
        };
        let qualified = resolve_table_name(&design, name);
        output::info(verbosity, &format!("import {qualified} ← {}", path.display()));
        output::summary(0, 0, 1);
        return Ok(());
    }

    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;

    // Surface the same gap/closure errors a real import would (dry-run must
    // not hide a misconfigured scope).
    design.check_scope_gaps(&resolved).context("scope check failed")?;
    let plan = design.import_plan(name);
    let ws = design.working_set(&resolved)?;
    let plan: Vec<_> = plan
        .into_iter()
        .filter(|e| dbd_core::design::import_entry_in_scope(e, &ws, resolved.is_all))
        .collect();

    // Anything the scan or plan left out, before listing what remains — so a
    // preview that shows no steps still explains itself.
    for warning in design.import_warnings(name) {
        output::warn(&warning);
    }

    // Step 1: Data loading
    for entry in &plan {
        let file = entry.table.file.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
        let fmt = entry.table.format.as_deref().unwrap_or("csv");
        output::info(verbosity, &format!("  import {} ({}) ← {}", entry.table.name, fmt, file));
    }

    // Step 2: Import procedures
    let has_procedures = plan.iter().any(|e| e.procedure.is_some());
    if has_procedures {
        output::info(verbosity, "");
    }
    for entry in &plan {
        if let Some(ref proc_name) = entry.procedure {
            output::info(verbosity, &format!("  call {proc_name}()"));
        }
    }

    // Step 3: After scripts
    if !design.config().import.after.is_empty() {
        output::info(verbosity, "");
        for after_file in &design.config().import.after {
            output::info(verbosity, &format!("  run {after_file}"));
        }
    }

    output::summary(0, 0, plan.len());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_import(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    file: Option<&Path>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    // `--file` without `--name` is rejected before a connection is attempted, so
    // the error is about the flags rather than the database.
    if file.is_some() && name.is_none() {
        bail!("--file requires --name <entity>");
    }

    let adapter = get_adapter(config, database_url).await?;
    import_with_adapter(&*adapter, &design, name, file, scope, deps, verbosity).await
}

/// The body of `dbd import`, with the adapter supplied rather than connected.
///
/// Split out so the ad-hoc path's decisions — resolving a bare table name to its
/// schema-qualified form, and inferring the format from the file extension — are
/// testable against a recording adapter without a live database.
pub(crate) async fn import_with_adapter(
    adapter: &dyn dbd_core::DatabaseAdapter,
    design: &Design,
    name: Option<&str>,
    file: Option<&Path>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    // Ad-hoc single-file import: `dbd import -n <entity> -f <path>`.
    if let Some(path) = file {
        let name = match name {
            Some(n) => n,
            None => bail!("--file requires --name <entity>"),
        };
        let qualified = resolve_table_name(design, name);
        let format = format_from_ext(path);

        let mut entity = Entity::new(EntityType::Table, &qualified);
        entity.file = Some(path.to_path_buf());
        entity.format = Some(format.to_string());
        adapter
            .import_data(&entity, design.config().import.table_null_value(&qualified), false)
            .await
            .context(format!("Failed to import {qualified} ← {}", path.display()))?;

        output::info(verbosity, &format!("import {qualified} ← {}", path.display()));
        return Ok(());
    }

    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;

    let spinner = output::StepSpinner::new(verbosity);
    let mut import_summary: Option<ImportComplete> = None;
    let result = design
        .import_data(
            adapter,
            name,
            false,
            Some(&resolved),
            Progress {
                on_start: |desc: &str| spinner.start(desc),
                on_done: |desc: &str, err: Option<&str>| spinner.done(desc, err),
                on_complete: |s| import_summary = Some(s),
            },
        )
        .await;
    spinner.finish();
    result?;

    let summary = import_summary.unwrap_or_default();
    // Always reported, whatever the verbosity: these are the reasons a data file
    // did not load, and a silent skip is what makes an empty import look fine.
    for warning in &summary.warnings {
        output::warn(warning);
    }
    output::info(verbosity, &format_import_summary(&summary));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_export(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    format: &str,
    output: Option<&Path>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let adapter = get_adapter(config, database_url).await?;
    export_with_adapter(&*adapter, config, env, project_dir, name, format, output, scope, deps, verbosity).await
}

/// The body of `dbd export`, with the adapter supplied rather than connected.
///
/// Split out so the decisions here — which tables are in scope, and which format
/// each one exports as — are testable against a recording adapter without a
/// live database.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn export_with_adapter(
    adapter: &dyn dbd_core::DatabaseAdapter,
    config: &Path,
    env: &str,
    project_dir: &Path,
    name: Option<&str>,
    format: &str,
    output: Option<&Path>,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;

    // Restrict to the scope's working set (all entities for the all-scope).
    let resolved = design.resolve_scope(scope, deps)?;
    let in_scope: std::collections::HashSet<String> = design
        .scoped_entities(&resolved)?
        .into_iter()
        .map(|e| e.name)
        .collect();

    // Build a name→format map from config export entries (per-table overrides).
    let config_format_map: std::collections::HashMap<String, String> = design
        .config()
        .export
        .iter()
        .filter_map(|e| e.format().map(|f| (e.name(), f.to_string())))
        .collect();

    // Build export list: either from config export entries, or all tables.
    // The scope's working set filters either branch.
    let tables: Vec<&dbd_core::Entity> = if !design.config().export.is_empty() {
        let export_names: Vec<String> = design.config().export.iter().map(|e| e.name()).collect();
        design.entities().iter()
            .filter(|e| e.entity_type == dbd_core::EntityType::Table)
            .filter(|e| export_names.contains(&e.name))
            .filter(|e| in_scope.contains(&e.name))
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .collect()
    } else {
        design.entities().iter()
            .filter(|e| e.entity_type == dbd_core::EntityType::Table)
            .filter(|e| in_scope.contains(&e.name))
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .collect()
    };

    if tables.is_empty() {
        output::info(verbosity, "No tables to export.");
        return Ok(());
    }

    output::info(verbosity, &format!("Exporting {} table(s)...", tables.len()));

    for table in &tables {
        // Precedence: per-table config override → CLI --format flag → "csv".
        let effective_format = config_format_map
            .get(&table.name)
            .map(|s| s.as_str())
            .unwrap_or(format);

        let mut export_entity = (*table).clone();
        export_entity.format = Some(effective_format.to_string());
        adapter.export_data(&export_entity, output).await
            .context(format!("Failed to export {}", table.name))?;
        output::detail(verbosity, &format!("  exported {} ({})", table.name, effective_format));
    }

    let dest = output.map(|p| p.display().to_string()).unwrap_or_else(|| "export/".to_string());
    output::info(verbosity, &format!("Export complete. Files written to {dest}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn format_from_ext_maps_known_extensions() {
        assert_eq!(format_from_ext(&PathBuf::from("data/users.jsonl")), "jsonl");
        assert_eq!(format_from_ext(&PathBuf::from("data/users.tsv")), "tsv");
        assert_eq!(format_from_ext(&PathBuf::from("data/users.csv")), "csv");
    }

    #[test]
    fn format_from_ext_unknown_and_missing_default_to_csv() {
        // Unknown extension → csv.
        assert_eq!(format_from_ext(&PathBuf::from("dump.txt")), "csv");
        // No extension → csv.
        assert_eq!(format_from_ext(&PathBuf::from("dump")), "csv");
    }

    #[test]
    fn format_from_ext_is_case_insensitive() {
        assert_eq!(format_from_ext(&PathBuf::from("USERS.JSONL")), "jsonl");
        assert_eq!(format_from_ext(&PathBuf::from("Users.Tsv")), "tsv");
    }

    use crate::commands::testutil;
    use std::path::Path;

    /// A bare table name resolves to its schema-qualified form; an unknown name
    /// falls back verbatim.
    #[test]
    fn resolve_table_name_matches_and_falls_back() {
        let design = Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures())).unwrap();
        assert!(resolve_table_name(&design, "lookups").ends_with("lookups"));
        assert_eq!(resolve_table_name(&design, "nope_not_here"), "nope_not_here");
    }

    /// Full-plan dry-run against the fixture: resolves scope, builds the import
    /// plan, prints it, writes nothing.
    #[test]
    fn import_dry_run_full_plan_on_fixture() {
        cmd_import_dry_run(&testutil::fixture_config(), "dev", &testutil::fixtures(), None, None, None, None, Verbosity::Verbose).unwrap();
    }

    /// Ad-hoc single-file dry-run prints one line for the resolved target.
    #[test]
    fn import_dry_run_adhoc_file() {
        cmd_import_dry_run(
            &testutil::fixture_config(), "dev", &testutil::fixtures(),
            Some("lookups"), Some(Path::new("data/x.csv")), None, None, Verbosity::Normal,
        )
        .unwrap();
    }

    /// `--file` without `--name` is an actionable error (both dry-run and real).
    #[test]
    fn import_dry_run_file_without_name_bails() {
        let err = cmd_import_dry_run(
            &testutil::fixture_config(), "dev", &testutil::fixtures(),
            None, Some(Path::new("data/x.csv")), None, None, Verbosity::Normal,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--file requires --name"), "got: {err}");
    }

    #[tokio::test]
    async fn import_file_without_name_bails_before_db() {
        let err = cmd_import(
            &testutil::fixture_config(), "dev", &testutil::fixtures(), None,
            None, Some(Path::new("data/x.csv")), None, None, Verbosity::Normal,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--file requires --name"), "got: {err}");
    }

    // ── Adapter-backed paths, via the recording mock ──────────────────────────
    //
    // These assert what the command decided to send the adapter — the table set,
    // the per-table format, the resolved entity name — not merely that it
    // returned `Ok`. A test that only checked Ok-ness would pass with the
    // selection logic entirely broken.

    use dbd_core::adapter::mock::MockAdapter;

    fn fixture_design() -> Design {
        Design::from_config_with_dir(&testutil::fixture_config(), "dev", Some(&testutil::fixtures()))
            .unwrap()
    }

    /// An ad-hoc `--file` import resolves the bare name to its schema-qualified
    /// form before handing it to the adapter.
    #[tokio::test]
    async fn import_adhoc_sends_qualified_name_to_adapter() {
        let mock = MockAdapter::new();
        let design = fixture_design();
        import_with_adapter(
            &mock, &design, Some("lookups"), Some(Path::new("data/lookups.csv")),
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();

        let imported = mock.imported.lock().unwrap().clone();
        assert_eq!(imported.len(), 1, "expected exactly one import: {imported:?}");
        assert!(
            imported[0].contains('.') && imported[0].ends_with("lookups"),
            "expected a schema-qualified name, got {:?}",
            imported[0]
        );
    }

    /// An unknown name is passed through verbatim rather than silently dropped,
    /// so an ad-hoc COPY target can still be addressed by a bare name.
    #[tokio::test]
    async fn import_adhoc_unknown_name_passes_through() {
        let mock = MockAdapter::new();
        let design = fixture_design();
        import_with_adapter(
            &mock, &design, Some("not_a_fixture_table"), Some(Path::new("data/x.jsonl")),
            None, None, Verbosity::Normal,
        )
        .await
        .unwrap();

        assert_eq!(*mock.imported.lock().unwrap(), vec!["not_a_fixture_table".to_string()]);
    }

    /// Format precedence: a per-table `format:` in `design.yaml`'s `export:`
    /// block beats the CLI `--format` flag; a table without one uses the flag.
    ///
    /// The fixture declares `config.lookups` (no override) and
    /// `config.lookup_values` (`format: jsonl`), so one run pins both halves of
    /// the rule — and would catch the precedence being inverted.
    #[tokio::test]
    async fn export_format_precedence_config_override_beats_cli_flag() {
        let mock = MockAdapter::new();
        export_with_adapter(
            &mock, &testutil::fixture_config(), "dev", &testutil::fixtures(),
            None, "tsv", None, None, None, Verbosity::Normal,
        )
        .await
        .unwrap();

        let exported = mock.exported.lock().unwrap().clone();
        assert!(
            exported.contains(&"config.lookups (tsv)".to_string()),
            "a table with no override must use the CLI --format: {exported:?}"
        );
        assert!(
            exported.contains(&"config.lookup_values (jsonl)".to_string()),
            "a per-table config format must beat the CLI --format: {exported:?}"
        );
    }

    /// The `export:` block also restricts *which* tables are exported. Needs a
    /// table that exists but is NOT listed, so it runs against a copy of the
    /// fixture with one added — the fixture itself lists every table it has, so
    /// asserting this against it would prove nothing.
    #[tokio::test]
    async fn export_config_block_restricts_the_table_set() {
        let proj = testutil::copy_fixture_project();
        let extra = proj.path().join("ddl/table/config/unlisted.ddl");
        std::fs::write(
            &extra,
            "set search_path to config;\n\
             create table if not exists unlisted (id int primary key);\n",
        )
        .unwrap();
        let cfg = proj.path().join("design.yaml");

        // Precondition: the new table really is part of the design.
        let design = Design::from_config_with_dir(&cfg, "dev", Some(proj.path())).unwrap();
        assert!(
            design.entities().iter().any(|e| e.name == "config.unlisted"),
            "added table should be discovered"
        );

        let mock = MockAdapter::new();
        export_with_adapter(
            &mock, &cfg, "dev", proj.path(),
            None, "csv", None, None, None, Verbosity::Normal,
        )
        .await
        .unwrap();

        let exported = mock.exported.lock().unwrap().clone();
        assert!(
            !exported.iter().any(|e| e.starts_with("config.unlisted")),
            "a table absent from `export:` must not be exported: {exported:?}"
        );
        assert!(
            exported.iter().any(|e| e.starts_with("config.lookups")),
            "listed tables must still export: {exported:?}"
        );
    }

    /// `--name` narrows the export to that one table.
    #[tokio::test]
    async fn export_name_filter_selects_one_table() {
        let mock = MockAdapter::new();
        let design = fixture_design();
        let target = design
            .entities()
            .iter()
            .find(|e| e.entity_type == EntityType::Table)
            .map(|e| e.name.clone())
            .expect("fixture has at least one table");

        export_with_adapter(
            &mock, &testutil::fixture_config(), "dev", &testutil::fixtures(),
            Some(&target), "csv", None, None, None, Verbosity::Normal,
        )
        .await
        .unwrap();

        let exported = mock.exported.lock().unwrap().clone();
        assert_eq!(
            exported,
            vec![format!("{target} (csv)")],
            "only the named table should be exported"
        );
    }

    /// A name matching no table exports nothing and takes the early return —
    /// it must not silently export everything.
    #[tokio::test]
    async fn export_unknown_name_exports_nothing() {
        let mock = MockAdapter::new();
        export_with_adapter(
            &mock, &testutil::fixture_config(), "dev", &testutil::fixtures(),
            Some("no_such_table"), "csv", None, None, None, Verbosity::Normal,
        )
        .await
        .unwrap();

        assert!(
            mock.exported.lock().unwrap().is_empty(),
            "an unmatched --name must export nothing, not everything"
        );
    }

    /// A full (non-ad-hoc) import runs the design's import plan through the
    /// adapter and loads the fixture's data files.
    #[tokio::test]
    async fn import_full_plan_loads_fixture_tables() {
        let mock = MockAdapter::new();
        let design = fixture_design();
        import_with_adapter(&mock, &design, None, None, None, None, Verbosity::Normal)
            .await
            .unwrap();

        assert!(
            !mock.imported.lock().unwrap().is_empty(),
            "the fixture project has import data, so the plan must load something"
        );
    }
}
