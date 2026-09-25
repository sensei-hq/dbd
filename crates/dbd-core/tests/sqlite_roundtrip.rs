//! `dbd init --from-db sqlite://` → `dbd apply` must round-trip (issue #20).
//!
//! It does not today, and the failure is total rather than partial. `init
//! --from-db` writes SQLite DDL into `ddl/` verbatim — `AUTOINCREMENT`,
//! `WITHOUT ROWID` and `STRICT` included, which the adapter documents as
//! deliberate and lossless. But `reverse::design_yaml` emits no `source:` block
//! at all, so the project loads under the `postgresql` default and libpg_query
//! rejects all three. `ensure_fully_parsed` then refuses every write.
//!
//! The fix is symmetry, not a second structured parser. SQLite's `introspect`
//! already models a table as `raw_ddl` and nothing else — no `table_def`, no
//! columns — because `sqlite_master.sql` *is* the schema. The read side has to
//! match: for a SQLite project a DDL file is its own model, taken verbatim.
//!
//! These tests drive the real journey against a real database, in memory, so
//! "it round-trips" is checked against the schema that actually lands rather
//! than against the code path that was supposed to produce it.

#![cfg(feature = "sqlite")]

use dbd_core::adapter::DatabaseAdapter;
use dbd_core::{Design, EntityType, reverse};
use std::path::{Path, PathBuf};

/// The SQLite-isms that make this more than a formality: each one is rejected
/// outright by libpg_query, so any of them alone breaks the whole project.
const SOURCE_SCHEMA: &[&str] = &[
    "CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    "CREATE TABLE settings (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID",
    "CREATE TABLE strict_t (a INTEGER, b TEXT) STRICT",
    "CREATE TABLE books (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES authors(id), title TEXT)",
    "CREATE INDEX books_title_idx ON books (title)",
    "CREATE VIEW long_titles AS SELECT id, title FROM books WHERE length(title) > 10",
];

async fn seeded_db(url: &str) -> Box<dyn DatabaseAdapter> {
    let adapter = dbd_core::connect(url, "roundtrip").await.expect("connect");
    for stmt in SOURCE_SCHEMA {
        adapter.execute_script(stmt).await.expect("seed");
    }
    adapter
}

/// Lay out a project exactly as `dbd init --from-db sqlite://` does: flat
/// `ddl/<type>/<name>.ddl` (SQLite has no schemas), plus a generated
/// `design.yaml`.
async fn write_project(dir: &Path, source: &dyn DatabaseAdapter) -> PathBuf {
    let entities = source.introspect().await.expect("introspect");
    assert!(!entities.is_empty(), "the source database must have a schema to export");

    for e in &entities {
        let folder = e.entity_type.folder_name();
        let ddl_dir = dir.join("ddl").join(&folder);
        std::fs::create_dir_all(&ddl_dir).expect("mkdir");
        let body = e
            .raw_ddl
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no raw_ddl — SQLite introspection is verbatim", e.name));
        std::fs::write(ddl_dir.join(format!("{}.ddl", e.name)), format!("{body};\n")).expect("write ddl");
    }

    let config = dir.join("design.yaml");
    std::fs::write(&config, reverse::design_yaml("roundtrip", "sqlite", &[], 1)).expect("write design.yaml");
    config
}

/// The generated project must declare what it is. Without this the dialect is
/// never set and every other fix is unreachable.
#[test]
fn init_from_a_sqlite_source_records_the_sqlite_dialect() {
    let yaml = reverse::design_yaml("localdb", "sqlite", &[], 1);
    assert!(
        yaml.contains("dialect: sqlite"),
        "a SQLite project must declare its dialect, got:\n{yaml}"
    );

    // A Postgres source must not acquire one it does not need — `postgresql` is
    // already the default, and writing it changes every generated project.
    let pg = reverse::design_yaml("shopdb", "postgres", &["public".to_string()], 1);
    assert!(
        !pg.contains("dialect: sqlite"),
        "a Postgres project must not be labelled sqlite:\n{pg}"
    );
}

/// The failure as a user meets it: files dbd itself wrote, which dbd then
/// refuses to read.
#[tokio::test]
async fn a_project_exported_from_sqlite_loads_without_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let source = seeded_db("sqlite::memory:").await;
    let config = write_project(tmp.path(), &*source).await;

    let design = Design::from_config(&config, "dev").expect("a project dbd generated must load");

    let bad: Vec<(&str, &Vec<String>)> = design
        .entities()
        .iter()
        .filter(|e| !e.errors.is_empty())
        .map(|e| (e.name.as_str(), &e.errors))
        .collect();
    assert!(bad.is_empty(), "SQLite DDL dbd wrote must parse: {bad:?}");

    // Every table and view is present, and each carries its own DDL — the model
    // for a SQLite entity is the text, exactly as `introspect` produces it.
    for name in ["authors", "settings", "strict_t", "books", "long_titles"] {
        let e = design
            .entities()
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} missing from the design"));
        assert!(
            e.raw_ddl.is_some(),
            "{name} must carry its verbatim DDL, as the introspected entity does"
        );
    }

    let strict = design.entities().iter().find(|e| e.name == "strict_t").unwrap();
    assert!(
        strict.raw_ddl.as_deref().unwrap().contains("STRICT"),
        "the SQLite-only clause must survive the round-trip verbatim"
    );
}

/// The acceptance criterion, checked against the database rather than the plan:
/// apply the exported project to an empty database and compare the schema it
/// produces with the one it came from.
#[tokio::test]
async fn applying_an_exported_sqlite_project_reproduces_the_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let source = seeded_db("sqlite::memory:").await;
    let config = write_project(tmp.path(), &*source).await;

    let design = Design::from_config(&config, "dev").expect("load");
    let target = dbd_core::connect("sqlite::memory:", "roundtrip")
        .await
        .expect("connect");
    let scope = design.resolve_scope(None, None).expect("scope");

    design
        .apply(&*target, None, false, Some(&scope), dbd_core::design::Progress::none())
        .await
        .expect("apply must succeed against SQLite");

    let mut before: Vec<String> = source
        .introspect()
        .await
        .unwrap()
        .iter()
        .map(|e| format!("{:?} {}", e.entity_type, e.name))
        .collect();
    let mut after: Vec<String> = target
        .introspect()
        .await
        .unwrap()
        .iter()
        .map(|e| format!("{:?} {}", e.entity_type, e.name))
        .collect();
    before.sort();
    after.sort();

    assert_eq!(after, before, "the applied schema must match the one exported");

    // Names alone would pass on an empty table, so compare the definitions too —
    // that is where AUTOINCREMENT / WITHOUT ROWID / STRICT actually live.
    let ddl_of = |es: &[dbd_core::Entity], name: &str| {
        es.iter()
            .find(|e| e.name == name)
            .and_then(|e| e.raw_ddl.clone())
            .unwrap_or_default()
            .replace(char::is_whitespace, "")
    };
    let src = source.introspect().await.unwrap();
    let tgt = target.introspect().await.unwrap();
    for name in ["authors", "settings", "strict_t", "books"] {
        assert_eq!(
            ddl_of(&tgt, name),
            ddl_of(&src, name),
            "{name}: the applied definition differs from the exported one"
        );
    }
}

/// A table type SQLite does not have must still be refused, so this does not
/// become a path that accepts anything and applies nothing.
#[tokio::test]
async fn a_postgres_only_entity_still_fails_on_sqlite() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("ddl/enum")).unwrap();
    std::fs::write(
        tmp.path().join("ddl/enum/status.ddl"),
        "CREATE TYPE status AS ENUM ('a', 'b');\n",
    )
    .unwrap();
    let config = tmp.path().join("design.yaml");
    std::fs::write(&config, reverse::design_yaml("roundtrip", "sqlite", &[], 1)).unwrap();

    let design = Design::from_config(&config, "dev").expect("load");
    let target = dbd_core::connect("sqlite::memory:", "roundtrip")
        .await
        .expect("connect");
    let scope = design.resolve_scope(None, None).expect("scope");

    let result = design
        .apply(&*target, None, false, Some(&scope), dbd_core::design::Progress::none())
        .await;
    assert!(
        result.is_err(),
        "SQLite has no enums — applying one must fail, not silently no-op"
    );
    assert_eq!(
        design
            .entities()
            .iter()
            .filter(|e| e.entity_type == EntityType::Enum)
            .count(),
        1,
        "the enum should still be modelled; it is the apply that must refuse"
    );
}
