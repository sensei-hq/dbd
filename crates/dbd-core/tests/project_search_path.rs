//! The fallback schema path is the project's to choose, not dbd's to hardcode.
//!
//! # Why
//!
//! Every dbd DDL file is expected to open with `SET search_path TO <schema>;`
//! — no emitter writes it, it is a convention the author maintains. Measured
//! across dbd's own fixtures, 43 of 44 schema-owning DDL files do (the one
//! exception qualifies every name instead).
//!
//! Which means the interesting case is the file that *forgets*. Today it
//! silently falls back to `public`, a constant compiled into dbd that has
//! nothing to do with the project. For a project whose schemas are `app` and
//! `shared`, `public` is simply the wrong answer, and nothing says so.
//!
//! `source.search_path` in design.yaml supplies the project's own answer. A
//! file that states a path still wins — this is a *fallback*, not an override.

use dbd_core::design::Design;
use dbd_core::entity::{PathEntry, PathSource};
use std::path::Path;

/// Build a project on disk and load it.
fn project(dir: &Path, source_block: &str, ddl: &[(&str, &str)]) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        format!("project:\n  name: T\n\n{source_block}\nschemas:\n  - app\n  - shared\n"),
    )
    .unwrap();
    for (rel, sql) in ddl {
        let path = dir.join("ddl").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, sql).unwrap();
    }
    match Design::from_config(&dir.join("design.yaml"), "dev") {
        Ok(d) => d,
        Err(e) => panic!("loads: {e}"),
    }
}

fn entity<'a>(d: &'a Design, name: &str) -> &'a dbd_core::entity::Entity {
    d.entities().iter().find(|e| e.name == name).expect("entity")
}

// ── The configured fallback ─────────────────────────────────────────────────

#[test]
fn a_file_that_states_no_path_takes_the_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n  search_path: [app, shared]\n",
        &[("table/app/t.ddl", "create table app.t (id int);")],
    );
    let e = entity(&d, "app.t");
    assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["app", "shared"]);
    assert_eq!(
        e.schema_path.source,
        PathSource::Project,
        "the project said so, not the file and not dbd"
    );
}

/// A fallback, not an override. The file is closer to the truth than the
/// config is.
#[test]
fn a_file_that_states_a_path_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n  search_path: [app, shared]\n",
        &[(
            "table/app/t.ddl",
            "set search_path to shared;\ncreate table app.t (id int);",
        )],
    );
    let e = entity(&d, "app.t");
    assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["shared"]);
    assert_eq!(e.schema_path.source, PathSource::File);
}

/// Unconfigured, dbd falls back to what Postgres itself would do, and says
/// the answer is nobody's but the session's.
#[test]
fn without_the_setting_the_session_default_stands() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n",
        &[("table/app/t.ddl", "create table app.t (id int);")],
    );
    let e = entity(&d, "app.t");
    assert_eq!(e.schema_path.source, PathSource::SessionDefault);
    assert_eq!(
        e.schema_path.entries,
        vec![PathEntry::CurrentUser, PathEntry::Schema("public".into())]
    );
}

// ── It changes what a bare reference resolves to ────────────────────────────

/// The payoff. A file that forgot its `SET search_path` and refers to a bare
/// name used to have that name qualified against `public` — a schema this
/// project does not even declare.
#[test]
fn a_bare_reference_resolves_against_the_projects_path() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n  search_path: [app]\n",
        &[
            ("table/app/parent.ddl", "create table app.parent (id uuid primary key);"),
            // No `SET search_path`, and a bare FK target.
            (
                "table/app/child.ddl",
                "create table app.child (pid uuid references parent (id));",
            ),
        ],
    );
    let child = entity(&d, "app.child");
    assert!(
        child.refers_to("app.parent"),
        "the bare `parent` should resolve in the project's own schema, got {:?}",
        child.refers().collect::<Vec<_>>()
    );
}

// ── Configuration is validated ──────────────────────────────────────────────

/// An empty list is a mistake, not "no schemas" — it would silently leave the
/// project with nothing to resolve against.
#[test]
fn an_empty_search_path_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("design.yaml"),
        "project:\n  name: T\n\nsource:\n  dialect: postgresql\n  search_path: []\n",
    )
    .unwrap();
    let err = match Design::from_config(&tmp.path().join("design.yaml"), "dev") {
        Err(e) => e,
        Ok(_) => panic!("an empty search_path must be refused"),
    };
    assert!(
        err.to_string().contains("search_path"),
        "the message must name the setting: {err}"
    );
}

/// `$user` is meaningful in this setting too, and must survive as the
/// placeholder rather than becoming a schema called `$user`.
#[test]
fn the_configured_path_may_name_the_current_user() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n  search_path: [\"$user\", app]\n",
        &[("table/app/t.ddl", "create table app.t (id int);")],
    );
    let e = entity(&d, "app.t");
    assert_eq!(
        e.schema_path.entries,
        vec![PathEntry::CurrentUser, PathEntry::Schema("app".into())]
    );
    assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["app"]);
}

// ── And it is never silent ──────────────────────────────────────────────────

/// The fallback must be reported, not applied quietly. A forgotten
/// `SET search_path` is a real authoring mistake, and resolving it against
/// *anything* without saying so is how a reference ends up aimed at a schema
/// nobody chose.
#[test]
fn a_file_with_no_search_path_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n  search_path: [app]\n",
        &[
            (
                "table/app/quiet.ddl",
                "set search_path to app;\ncreate table app.quiet (id int);",
            ),
            ("table/app/loud.ddl", "create table app.loud (id int);"),
        ],
    );
    let warned: Vec<&String> = d.entities().iter().flat_map(|e| &e.warnings).collect();
    assert!(
        warned
            .iter()
            .any(|w| w.contains("loud.ddl") && w.contains("search_path")),
        "the file that forgot must be named: {warned:?}"
    );
    assert!(
        !warned.iter().any(|w| w.contains("quiet.ddl")),
        "the file that stated one must not be: {warned:?}"
    );
    assert!(
        warned.iter().any(|w| w.contains("source.search_path")),
        "and the report must say what it was resolved against: {warned:?}"
    );
}

/// Unconfigured, the report says so and points at the setting.
#[test]
fn without_the_setting_the_report_names_it_as_the_remedy() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n",
        &[("table/app/t.ddl", "create table app.t (id int);")],
    );
    let w = d
        .entities()
        .iter()
        .flat_map(|e| &e.warnings)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        w.iter()
            .any(|w| w.contains("session default") && w.contains("source.search_path")),
        "{w:?}"
    );
}

/// A role has no unqualified names to resolve, so a missing path is not a
/// defect there and must not be reported as one.
#[test]
fn a_schemaless_entity_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let d = project(
        tmp.path(),
        "source:\n  dialect: postgresql\n",
        &[("role/basic.ddl", "create role basic;")],
    );
    let w: Vec<&String> = d.entities().iter().flat_map(|e| &e.warnings).collect();
    assert!(!w.iter().any(|x| x.contains("basic.ddl")), "{w:?}");
}
