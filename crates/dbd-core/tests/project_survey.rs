//! `survey` — is this directory a dbd project, and what is in it?
//!
//! The cheap counterpart to [`Design::from_config`]: it reads the config and
//! walks the layout, but parses no SQL. That is what an external scanner wants
//! first — sensei walking a repo needs to know *whether* to parse a directory,
//! and with which parser, before paying to parse anything.
//!
//! Two properties carry the weight:
//!
//! - "not a dbd project" is an ordinary answer (`Ok(None)`), not an error. A
//!   scanner meets far more non-projects than projects, and a caller that has
//!   to distinguish "no design.yaml" from "broken design.yaml" by matching on
//!   error strings will get it wrong.
//! - what was *left out* is reported, not silently dropped. `migrations/` and
//!   `snapshots/` hold generated SQL, and a scanner that indexed them would
//!   report every historical version of a table as a live entity.

use dbd_core::parser::ParserChoice;
use dbd_core::project::{self, ExclusionReason};
use std::path::Path;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A project with one of everything the layout defines.
fn project_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    write(
        d,
        "design.yaml",
        "project:\n  name: shop\n  version: 3\n\nsource:\n  dialect: postgresql\n\n\
         target:\n  postgres:\n    url: $DATABASE_URL\n\nschemas:\n  - app\n  - config\n",
    );
    write(d, "ddl/table/app/users.ddl", "create table users (id int);\n");
    write(d, "ddl/view/app/active.sql", "create view active as select 1;\n");
    write(d, "policies/app/users.sql", "-- rls\n");
    write(d, "import/app/users.csv", "id\n1\n");
    // Generated — must not be offered as source files.
    write(d, "migrations/0001_init.sql", "create table users (id int);\n");
    write(d, "snapshots/v1.sql", "create table users (id int);\n");
    tmp
}

// ── Is this a dbd project? ──────────────────────────────────────────────────

#[test]
fn a_directory_without_a_design_yaml_is_not_a_project() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "ddl/table/app/users.ddl", "create table users (id int);\n");
    write(tmp.path(), "README.md", "# not dbd\n");

    assert!(
        project::survey(tmp.path()).expect("absence is not an error").is_none(),
        "a directory with DDL but no design.yaml is not a dbd project"
    );
}

/// The distinction a scanner cannot afford to lose: absent is `Ok(None)`,
/// broken is `Err`. Collapsing them means a malformed project is silently
/// skipped as "not dbd".
#[test]
fn a_broken_design_yaml_is_an_error_not_a_non_project() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "design.yaml", "project: [this is not a mapping\n");

    assert!(
        project::survey(tmp.path()).is_err(),
        "a design.yaml that cannot be read must not be reported as 'not a dbd project'"
    );
}

#[test]
fn a_project_is_identified_with_its_config_and_identity() {
    let tmp = project_fixture();
    let s = project::survey(tmp.path()).unwrap().expect("this is a dbd project");

    assert_eq!(s.project, "shop");
    assert_eq!(s.version, 3);
    assert_eq!(s.config_path, tmp.path().join("design.yaml"));
    assert_eq!(s.schemas, vec!["app".to_string(), "config".to_string()]);
}

// ── Dialect and parser ──────────────────────────────────────────────────────

#[test]
fn the_dialect_and_the_parser_it_selects_are_both_reported() {
    let tmp = project_fixture();
    let s = project::survey(tmp.path()).unwrap().unwrap();
    assert_eq!(s.dialect, "postgresql");
    assert_eq!(s.parser, ParserChoice::PgQuery);
}

/// The parser, not just the dialect string — a caller should not have to
/// re-derive the mapping and risk disagreeing with dbd about it.
#[test]
fn a_sqlite_project_reports_the_verbatim_parser() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: local\n  version: 1\n\nsource:\n  dialect: sqlite\n\n\
         target:\n  sqlite:\n    url: $DATABASE_URL\n\nschemas: []\n",
    );
    let s = project::survey(tmp.path()).unwrap().unwrap();
    assert_eq!(s.dialect, "sqlite");
    assert_eq!(s.parser, ParserChoice::Verbatim);
}

/// A project naming the retired parser must fail here for the same reason it
/// fails at load — a survey that quietly reported a different parser than the
/// one dbd would use is worse than no survey.
#[test]
fn a_retired_parser_value_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: old\n  version: 1\n\nsource:\n  dialect: postgresql\n  parser: sqlparser\n\n\
         target:\n  postgres:\n    url: $DATABASE_URL\n\nschemas: []\n",
    );
    assert!(project::survey(tmp.path()).is_err());
}

// ── Which files ─────────────────────────────────────────────────────────────

#[test]
fn ddl_files_are_listed_and_generated_sql_is_not() {
    let tmp = project_fixture();
    let s = project::survey(tmp.path()).unwrap().unwrap();

    let rel: Vec<String> = s
        .ddl_files
        .iter()
        .map(|p| p.strip_prefix(tmp.path()).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(rel, vec!["ddl/table/app/users.ddl", "ddl/view/app/active.sql"]);

    for p in &s.ddl_files {
        let t = p.to_string_lossy();
        assert!(
            !t.contains("/migrations/") && !t.contains("/snapshots/"),
            "generated SQL must never be offered as a source file: {t}"
        );
    }
}

/// Policies are SQL but they are not entity definitions, so they are a separate
/// list rather than folded in — a scanner that parsed them as DDL would invent
/// entities that do not exist.
#[test]
fn policies_and_imports_are_reported_separately_from_ddl() {
    let tmp = project_fixture();
    let s = project::survey(tmp.path()).unwrap().unwrap();

    assert_eq!(s.policy_files.len(), 1, "one policy file");
    assert!(s.policy_files[0].ends_with("policies/app/users.sql"));
    assert_eq!(s.import_files.len(), 1, "one import data file");
    assert!(s.import_files[0].ends_with("import/app/users.csv"));

    assert!(
        !s.ddl_files.iter().any(|p| p.to_string_lossy().contains("/policies/")),
        "a policy file is not an entity definition"
    );
}

// ── What was left out, and why ──────────────────────────────────────────────

#[test]
fn generated_directories_are_reported_as_exclusions_with_a_reason() {
    let tmp = project_fixture();
    let s = project::survey(tmp.path()).unwrap().unwrap();

    let reasons: Vec<&ExclusionReason> = s.excluded.iter().map(|e| &e.reason).collect();
    assert!(
        reasons.iter().any(|r| **r == ExclusionReason::Generated),
        "migrations/ and snapshots/ must be reported as excluded, not silently dropped: {:?}",
        s.excluded
    );

    let paths: Vec<String> = s
        .excluded
        .iter()
        .map(|e| e.path.to_string_lossy().into_owned())
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("migrations")),
        "migrations/ missing from exclusions: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("snapshots")),
        "snapshots/ missing from exclusions: {paths:?}"
    );
}

/// Nothing is reported that is not there. An exclusion list naming directories
/// a project does not have reads as "dbd skipped your files".
#[test]
fn a_project_without_generated_directories_reports_no_exclusions_for_them() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: fresh\n  version: 1\n\ntarget:\n  postgres:\n    url: $DATABASE_URL\n\nschemas: []\n",
    );
    write(tmp.path(), "ddl/table/public/t.ddl", "create table t (id int);\n");

    let s = project::survey(tmp.path()).unwrap().unwrap();
    assert!(
        s.excluded.is_empty(),
        "a project with no generated output has nothing to exclude: {:?}",
        s.excluded
    );
    assert_eq!(s.ddl_files.len(), 1);
}

/// A file dbd used to scaffold and now manages itself is not the user's entity.
/// Indexing it would attribute dbd's own plumbing to the project.
#[test]
fn an_internally_managed_file_is_excluded_from_the_ddl_list() {
    let tmp = project_fixture();
    write(
        tmp.path(),
        "ddl/procedure/staging/import_jsonb_to_table.ddl",
        "create procedure import_jsonb_to_table() language sql as $$ select 1 $$;\n",
    );

    let s = project::survey(tmp.path()).unwrap().unwrap();
    assert!(
        !s.ddl_files.iter().any(|p| p.ends_with("import_jsonb_to_table.ddl")),
        "dbd's own internally-managed procedure must not be offered as a project entity"
    );
    assert!(
        s.excluded
            .iter()
            .any(|e| e.reason == ExclusionReason::ManagedByDbd && e.path.ends_with("import_jsonb_to_table.ddl")),
        "and it must say why: {:?}",
        s.excluded
    );
}

/// The whole point for an embedder: survey, then parse with what it reports.
#[test]
fn the_reported_parser_reads_the_reported_files() {
    let tmp = project_fixture();
    let s = project::survey(tmp.path()).unwrap().unwrap();

    for file in &s.ddl_files {
        let sql = std::fs::read_to_string(file).unwrap();
        let entity = dbd_core::parser::parse_entity_with(s.parser, file, &sql).unwrap();
        assert!(
            entity.errors.is_empty(),
            "{}: survey offered a file its own parser cannot read: {:?}",
            file.display(),
            entity.errors
        );
    }
}
