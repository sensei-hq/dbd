//! Integration tests for dbd-core.
//!
//! These test the full pipeline using fixture files — from config parsing
//! through entity discovery, parsing, dependency resolution, and output generation.
//! No database connection required.

use std::path::PathBuf;

use dbd_core::design::Progress;
use dbd_core::entity::EntityType;
use dbd_core::Design;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
}

fn design() -> Design {
    Design::from_config(&fixture_dir().join("design.yaml"), "dev").unwrap()
}

// ── Scenario: Config loading ────────────────────────────

#[test]
fn loads_config_with_all_sections() {
    let d = design();
    assert_eq!(d.config().project.name, "example");
    assert_eq!(d.config().source.dialect, "postgresql");
    assert!(d.config().default_target().is_some());
    assert!(!d.config().schema_names().is_empty());
}

#[test]
fn config_has_target_with_extensions_and_roles() {
    let d = design();
    let target = d.config().get_target(None).unwrap();
    assert!(!target.extensions.is_empty());
    assert!(!target.roles.is_empty());
}

#[test]
fn config_has_import_settings() {
    let d = design();
    assert!(!d.config().import.staging.is_empty());
    assert!(d.config().import.options.truncate);
}

// ── Scenario: Entity discovery ──────────────────────────

#[test]
fn discovers_entities_from_ddl_folder() {
    let d = design();
    assert!(!d.entities().is_empty());
    // Should have at least tables, views, procedures from fixtures
    let types: Vec<EntityType> = d.entities().iter().map(|e| e.entity_type).collect();
    assert!(types.contains(&EntityType::Table));
}

#[test]
fn entities_include_schemas_from_config() {
    let d = design();
    let schemas: Vec<&str> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Schema)
        .map(|e| e.name.as_str())
        .collect();
    assert!(schemas.contains(&"config"));
    assert!(schemas.contains(&"staging"));
}

#[test]
fn entities_include_extensions_from_target() {
    let d = design();
    let exts: Vec<&str> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Extension)
        .map(|e| e.name.as_str())
        .collect();
    assert!(exts.contains(&"uuid-ossp"));
}

#[test]
fn entities_include_roles_from_target() {
    let d = design();
    let roles: Vec<&str> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Role)
        .map(|e| e.name.as_str())
        .collect();
    assert!(roles.contains(&"basic"));
    assert!(roles.contains(&"advanced"));
}

#[test]
fn auto_discovers_schemas_from_entity_paths() {
    let d = design();
    let schemas: Vec<&str> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Schema)
        .map(|e| e.name.as_str())
        .collect();
    // config schema should be auto-discovered from ddl/table/config/lookups.ddl
    assert!(schemas.contains(&"config"));
}

// ── Scenario: Materialized views (end-to-end) ───────────

#[test]
fn discovers_and_emits_materialized_view_from_fixture() {
    let d = design();

    // Discovered from ddl/materialized_view/config/genders_mv.ddl with the
    // MaterializedView type inferred from its folder.
    let mv = d
        .entities()
        .iter()
        .find(|e| e.name == "config.genders_mv")
        .expect("matview discovered from ddl/materialized_view/");
    assert_eq!(mv.entity_type, EntityType::MaterializedView);

    // Its SELECT source is a real fixture entity, so dependency resolution is
    // realistic (the matview reads from the config.genders view).
    assert!(
        mv.refers.iter().any(|r| r == "config.genders"),
        "matview should depend on its source view, got: {:?}",
        mv.refers
    );

    // Emits a CREATE MATERIALIZED VIEW statement carrying its unique index.
    let sql = dbd_core::emit::emit_entity(mv).expect("matview emits DDL");
    assert!(sql.contains("CREATE MATERIALIZED VIEW"));
    assert!(sql.contains("CREATE UNIQUE INDEX"), "index carried through emit: {sql}");

    // Config resolution honours the per-view override and inherits global options.
    let r = d.config().materialized_views.resolve("config.genders_mv");
    assert_eq!(r.refresh.as_deref(), Some("*/15 * * * *")); // per-view override
    assert!(r.concurrently); // inherited from global options
}

// ── Scenario: Entity ordering ───────────────────────────

#[test]
fn schemas_come_before_extensions() {
    let d = design();
    let first_ext = d
        .entities()
        .iter()
        .position(|e| e.entity_type == EntityType::Extension);
    let last_schema = d
        .entities()
        .iter()
        .rposition(|e| e.entity_type == EntityType::Schema);

    if let (Some(last_s), Some(first_e)) = (last_schema, first_ext) {
        assert!(last_s < first_e, "Schemas must come before extensions");
    }
}

#[test]
fn extensions_come_before_tables() {
    let d = design();
    let first_table = d
        .entities()
        .iter()
        .position(|e| e.entity_type == EntityType::Table);
    let last_ext = d
        .entities()
        .iter()
        .rposition(|e| e.entity_type == EntityType::Extension);

    if let (Some(last_e), Some(first_t)) = (last_ext, first_table) {
        assert!(last_e < first_t, "Extensions must come before tables");
    }
}

#[test]
fn tables_come_before_views() {
    let d = design();
    let first_view = d
        .entities()
        .iter()
        .position(|e| e.entity_type == EntityType::View);
    let last_table = d
        .entities()
        .iter()
        .rposition(|e| e.entity_type == EntityType::Table);

    if let (Some(last_t), Some(first_v)) = (last_table, first_view) {
        assert!(last_t < first_v, "Tables must come before views");
    }
}

#[test]
fn tables_come_before_functions() {
    let d = design();
    let first_func = d
        .entities()
        .iter()
        .position(|e| e.entity_type == EntityType::Function || e.entity_type == EntityType::Procedure);
    let last_table = d
        .entities()
        .iter()
        .rposition(|e| e.entity_type == EntityType::Table);

    if let (Some(last_t), Some(first_f)) = (last_table, first_func) {
        assert!(last_t < first_f, "Tables must come before functions/procedures");
    }
}

#[test]
fn roles_sorted_by_dependency() {
    let d = design();
    let roles: Vec<&str> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Role)
        .map(|e| e.name.as_str())
        .collect();
    // basic must come before advanced (advanced refers to basic)
    if let (Some(basic_pos), Some(adv_pos)) = (
        roles.iter().position(|&r| r == "basic"),
        roles.iter().position(|&r| r == "advanced"),
    ) {
        assert!(basic_pos < adv_pos, "basic must come before advanced");
    }
}

// ── Scenario: SQL parsing ───────────────────────────────

#[test]
fn table_entities_have_table_def() {
    let d = design();
    let tables: Vec<_> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Table)
        .collect();
    assert!(!tables.is_empty());
    for table in &tables {
        assert!(
            table.table_def.is_some(),
            "Table {} should have a TableDef",
            table.name
        );
    }
}

#[test]
fn table_columns_are_extracted() {
    let d = design();
    let lookups = d
        .entities()
        .iter()
        .find(|e| e.name == "config.lookups")
        .unwrap();
    let table_def = lookups.table_def.as_ref().unwrap();
    assert!(table_def.columns.len() >= 8);

    let id_col = table_def.columns.iter().find(|c| c.name == "id").unwrap();
    assert!(id_col.is_pk);
    assert!(!id_col.nullable);
}

#[test]
fn fk_references_extracted() {
    let d = design();
    let lookup_values = d
        .entities()
        .iter()
        .find(|e| e.name == "config.lookup_values")
        .unwrap();
    assert!(
        !lookup_values.refers.is_empty(),
        "lookup_values should reference other tables via FK"
    );
}

#[test]
fn enum_values_extracted() {
    let d = design();
    let status = d
        .entities()
        .iter()
        .find(|e| e.entity_type == EntityType::Enum);
    if let Some(enum_entity) = status {
        assert!(!enum_entity.enum_values.is_empty());
    }
}

#[test]
fn search_paths_extracted() {
    let d = design();
    let lookups = d
        .entities()
        .iter()
        .find(|e| e.name == "config.lookups")
        .unwrap();
    assert!(
        !lookups.search_paths.is_empty(),
        "Should have search_paths from SET search_path"
    );
}

#[test]
fn procedure_reads_writes_extracted() {
    let d = design();
    let procs: Vec<_> = d
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Procedure)
        .collect();
    if !procs.is_empty() {
        let has_reads = procs.iter().any(|p| !p.reads.is_empty());
        let has_writes = procs.iter().any(|p| !p.writes.is_empty());
        assert!(has_reads || has_writes, "At least one procedure should have reads or writes");
    }
}

// ── Scenario: Validation ────────────────────────────────

#[test]
fn validate_reports_no_errors_on_fixture() {
    let mut d = design();
    let report = d.report(None, None);
    // Fixture project should be clean (or have only unresolved external refs)
    for entity in &report.issues {
        // Only allow "File not found" for entities with relative paths
        // that don't exist outside the fixture dir
        assert!(
            entity.errors.iter().all(|e| e.contains("File not found") || e.contains("Parse error")),
            "Unexpected error on {}: {:?}",
            entity.name,
            entity.errors
        );
    }
}

#[test]
fn validate_scoped_to_entity() {
    let mut d = design();
    let report = d.report(Some("config.lookups"), None);
    // Should only include the requested entity
    if let Some(entity) = &report.entity {
        assert_eq!(entity.name, "config.lookups");
    }
}

// ── Scenario: Dependency graph ──────────────────────────

#[test]
fn graph_has_nodes_and_layers() {
    let d = design();
    let graph = d.graph(None, None).unwrap();
    assert!(!graph.nodes.is_empty());
    assert!(!graph.layers.is_empty());
}

#[test]
fn graph_scoped_to_entity() {
    let d = design();
    let full = d.graph(None, None).unwrap();
    let scoped = d.graph(Some("config.lookup_values"), None).unwrap();
    assert!(scoped.nodes.len() <= full.nodes.len());
}

#[test]
fn graph_filtered_by_design_scope() {
    let d = design();
    let scope = d.resolve_scope(Some("config_only"), None).unwrap();
    let graph = d.graph(None, Some(&scope)).unwrap();
    assert!(!graph.nodes.is_empty());
    assert!(graph.nodes.iter().all(|n| !n.name.starts_with("staging.")));
}

// ── Scenario: Combine ───────────────────────────────────

#[test]
fn combine_generates_sql() {
    let d = design();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("init.sql");
    d.combine(&out, None).unwrap();

    let content = std::fs::read_to_string(&out).unwrap();
    assert!(content.contains("CREATE SCHEMA"));
    assert!(content.contains("CREATE EXTENSION"));
}

#[test]
fn combine_filtered_by_scope() {
    let d = design();
    let scope = d.resolve_scope(Some("config_only"), None).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("hub.sql");
    d.combine(&out, Some(&scope)).unwrap();

    let content = std::fs::read_to_string(&out).unwrap();
    assert!(content.contains("config"));
    assert!(!content.contains("staging"));
}

// ── Scenario: Import plan ───────────────────────────────

#[test]
fn import_plan_pairs_tables_with_procedures() {
    let d = design();
    let plan = d.import_plan(None);
    // If we have import tables, some should match procedures
    if !plan.is_empty() {
        let has_proc = plan.iter().any(|e| e.procedure.is_some());
        // Only assert if there are actually procedures in the project
        let has_procedures = d
            .entities()
            .iter()
            .any(|e| e.entity_type == EntityType::Procedure);
        if has_procedures {
            assert!(
                has_proc,
                "Import plan should match at least one procedure"
            );
        }
    }
}

// ── Scenario: DBML generation ───────────────────────────

#[test]
fn dbml_generates_valid_output() {
    let d = design();
    let doc = dbd_core::dbml::generate_dbml(&dbd_core::dbml::DbmlParams {
        entities: d.entities(),
        project_name: &d.config().project.name,
        database_type: &d.config().source.dialect,
        project_note: d.config().project.note.as_deref(),
        include_schemas: vec![],
        exclude_schemas: vec![],
        include_tables: vec![],
        exclude_tables: vec![],
        groups: vec![],
        auto_group_by_schema: false,
    });

    assert!(!doc.content.is_empty());
    assert!(doc.content.contains("Project"));
    assert!(doc.content.contains("Table"));
}

#[test]
fn dbml_includes_refs() {
    let d = design();
    let doc = dbd_core::dbml::generate_dbml(&dbd_core::dbml::DbmlParams {
        entities: d.entities(),
        project_name: "test",
        database_type: "postgresql",
        project_note: None,
        include_schemas: vec![],
        exclude_schemas: vec![],
        include_tables: vec![],
        exclude_tables: vec![],
        groups: vec![],
        auto_group_by_schema: false,
    });

    assert!(doc.content.contains("Ref:"), "DBML should contain FK refs");
}

#[test]
fn dbml_respects_scope_filtering() {
    use std::collections::HashSet;
    let d = design();
    // Select only config.lookups — config.lookup_values must drop out of the
    // generated DBML, proving `dbml` documents the scope's working set.
    let scope = dbd_core::ResolvedScope {
        name: "just_lookups".to_string(),
        entities: HashSet::from(["config.lookups".to_string()]),
        excluded: HashSet::new(),
        deps: dbd_core::config::DepsPolicy::Report,
        is_all: false,
        extensions: None,
    };
    let entities = d.scoped_entities(&scope).unwrap();

    let docs = dbd_core::dbml::generate_all(&dbd_core::dbml::DbmlMultiParams {
        entities: &entities,
        project_name: &d.config().project.name,
        database_type: &d.config().source.dialect,
        project_note: d.config().project.note.as_deref(),
        docs: &d.config().dbml,
    });
    let combined: String = docs.iter().map(|doc| doc.content.as_str()).collect();

    assert!(combined.contains("lookups"), "in-scope table should appear");
    assert!(
        !combined.contains("lookup_values"),
        "out-of-scope table must not appear in DBML"
    );
}

// ── Scenario: Doctor / config migration ─────────────────

#[test]
fn new_format_detected_as_clean() {
    let content = std::fs::read_to_string(fixture_dir().join("design.yaml")).unwrap();
    let issues = dbd_core::doctor::detect_old_format(&content);
    assert!(issues.is_empty(), "New format should have no issues");
}

#[test]
fn old_format_detected_and_migrated() {
    let old = r#"
project:
  name: test
  database: PostgreSQL
  staging: [staging]
extensions:
  - uuid-ossp
schemas:
  - public
"#;
    let issues = dbd_core::doctor::detect_old_format(old);
    assert!(!issues.is_empty());

    let migrated = dbd_core::doctor::migrate_config(old).unwrap();
    let _config: dbd_core::config::DesignConfig = serde_yaml::from_str(&migrated).unwrap();
}

// ── Scenario: GitHub source parsing ─────────────────────

#[test]
fn github_source_parsing() {
    use dbd_core::github::{is_github_source, parse_github_source};

    assert!(is_github_source("sensei-hq/daemon/database"));
    assert!(!is_github_source("."));
    assert!(!is_github_source("/local/path"));

    let src = parse_github_source("sensei-hq/daemon/database@v2.1").unwrap();
    assert_eq!(src.owner, "sensei-hq");
    assert_eq!(src.repo, "daemon");
    assert_eq!(src.subpath, Some("database".to_string()));
    assert_eq!(src.git_ref, "v2.1");
}

// ── Scenario: Snapshot I/O ──────────────────────────────

#[test]
fn snapshot_listing_on_empty_project() {
    let tmp = tempfile::tempdir().unwrap();
    let snapshots = dbd_core::snapshot::list_snapshots(tmp.path());
    assert!(snapshots.is_empty());
    assert!(!dbd_core::snapshot::has_snapshots(tmp.path()));
    assert_eq!(dbd_core::snapshot::next_version(tmp.path()), 1);
}

// ── Scenario: Reset safety guard ────────────────────────

#[tokio::test]
async fn reset_blocked_in_prod() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new().with_meta("prod", 0);
    let result = d.reset(&mock, "postgres", false, false, false, None).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("prod"));
}

#[tokio::test]
async fn reset_blocked_after_v1_in_dev() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new().with_meta("dev", 1);
    let result = d.reset(&mock, "postgres", false, false, false, None).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("migrations"));
}

#[tokio::test]
async fn reset_allowed_dev_pre_v1() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new().with_meta("dev", 0);
    let result = d.reset(&mock, "postgres", false, false, false, None).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn reset_force_overrides_guard() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new().with_meta("prod", 5);
    let result = d.reset(&mock, "postgres", true, false, false, None).await;
    assert!(result.is_ok());
}

// ── Scenario: Apply with mock adapter ───────────────────

#[tokio::test]
async fn apply_dry_run_does_not_execute() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new();
    d.apply(&mock, None, true, None, Progress::none()).await.unwrap();
    assert!(mock.applied_names().is_empty());
}

#[tokio::test]
async fn apply_executes_all_entities() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new();
    d.apply(&mock, None, false, None, Progress::none()).await.unwrap();
    assert!(!mock.applied_names().is_empty());
}

#[tokio::test]
async fn apply_single_entity_by_name() {
    let d = design();
    let mock = dbd_core::adapter::mock::MockAdapter::new();
    d.apply(&mock, Some("config.lookups"), false, None, Progress::none()).await.unwrap();
    let applied = mock.applied_names();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0], "config.lookups");
}

// ── Scenario: SchemaModel ───────────────────────────────

#[test]
fn diagram_model_json_round_trips() {
    let d = design(); // existing helper in this file
    let model = dbd_core::schema_model::build(&d, None);
    let json = serde_json::to_string(&model).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["project"]["name"].is_string());
    assert!(v["schemas"].as_array().unwrap().iter().any(|s| s["name"] == "config"));
    assert!(v["tables"].as_array().unwrap().iter().any(|t| t["schema"] == "config" && t["name"] == "lookups"));
    assert!(v["refs"].is_array());
}

// ── Scenario: Scope resolution ──────────────────────────

#[test]
fn scope_complete_has_no_gaps() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let mut design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("config_only"), None).unwrap();
    let report = design.report(None, Some(&scope));
    assert!(report.gaps.is_empty());
    assert!(scope.entities.contains("config.lookups"));
    assert!(scope.entities.contains("config.lookup_values"));
}

#[test]
fn scope_wildcard_include_matches_schema_token() {
    // `config.*` (wildcard) resolves to the same working set as bare `config`.
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let wild = design.resolve_scope(Some("config_wild"), None).unwrap();
    assert!(wild.entities.contains("config.lookups"));
    assert!(wild.entities.contains("config.lookup_values"));
    assert!(wild.entities.contains("config")); // schema re-added
    assert!(!wild.entities.iter().any(|n| n.starts_with("staging.")));
}

#[test]
fn scope_wildcard_exclude_keeps_schema() {
    // `excludes: [staging.*]` drops staging's entities but keeps the schema.
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("drop_staging"), None).unwrap();
    assert!(!scope.entities.iter().any(|n| n.starts_with("staging.")));
    assert!(scope.entities.contains("staging")); // CREATE SCHEMA entity preserved
    assert!(scope.entities.contains("config.lookups")); // other schemas untouched
}

#[test]
fn scope_incomplete_reports_gap() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let mut design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("incomplete"), None).unwrap();
    let report = design.report(None, Some(&scope));
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].missing, "config.lookups");
    assert_eq!(report.gaps[0].required_by, "config.lookup_values");
}

#[test]
fn scope_include_policy_closes_gap() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("incomplete_auto"), None).unwrap();
    let ws = design.working_set(&scope).unwrap();
    assert!(ws.contains("config.lookups")); // pulled in by include policy
}

#[test]
fn no_scope_is_full_set() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(None, None).unwrap();
    assert!(scope.is_all);
    // resolved set spans config + staging entities (full project)
    assert!(scope.entities.iter().any(|n| n.starts_with("staging.")));
    assert!(scope.entities.iter().any(|n| n.starts_with("config.")));
}

// The CLI dry-run paths (apply/import/deploy --dry-run --scope) call
// check_scope_gaps so they surface the same error a real run would. Lock in
// that gate's behavior on the fixture scopes.
#[test]
fn check_scope_gaps_gates_report_but_not_include() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();

    // report policy with a gap → Err
    let incomplete = design.resolve_scope(Some("incomplete"), None).unwrap();
    let err = design.check_scope_gaps(&incomplete).unwrap_err();
    assert!(err.to_string().contains("dependency gap"));

    // include policy → no error (closure auto-resolves)
    let auto = design.resolve_scope(Some("incomplete_auto"), None).unwrap();
    assert!(design.check_scope_gaps(&auto).is_ok());

    // complete scope and all-scope → no error
    let complete = design.resolve_scope(Some("config_only"), None).unwrap();
    assert!(design.check_scope_gaps(&complete).is_ok());
    let all = design.resolve_scope(None, None).unwrap();
    assert!(design.check_scope_gaps(&all).is_ok());
}
