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

// ── reconcile and diff must not claim "in sync" on a verbatim project ───────
//
// A verbatim entity has `raw_ddl` and no `table_def`, and both snapshot
// builders keep only entities where `table_def.is_some()`. So desired and live
// both reduce to nothing, `plan_reconcile` compares empty against empty, and
// the plan comes back clean — against a database that may share not one table
// with the design. Observed: reconcile on an EMPTY target returned
// `added=0 altered=0 dropped=0`.
//
// "In sync" is the one answer that must never be wrong. Refusing is the honest
// reply, and there is precedent: reconcile already refuses a batch adapter with
// "no live SQL schema to diff".

#[tokio::test]
async fn reconcile_refuses_a_verbatim_project_instead_of_reporting_in_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let source = seeded_db("sqlite::memory:").await;
    let config = write_project(tmp.path(), &*source).await;
    let design = Design::from_config(&config, "dev").expect("load");

    // Deliberately empty: every table in the design is missing here, so any
    // answer other than "refused" or "massive drift" is false.
    let target = dbd_core::connect("sqlite::memory:", "roundtrip")
        .await
        .expect("connect");
    let scope = design.resolve_scope(None, None).expect("scope");

    let result = design
        .reconcile(
            &*target,
            true,
            false,
            false,
            Some(&scope),
            dbd_core::design::Progress::none(),
        )
        .await;

    let err = match result {
        Ok(plan) => panic!(
            "reconcile reported a clean plan against an empty database: \
             added={} altered={} dropped={}",
            plan.added.len(),
            plan.altered.len(),
            plan.dropped.len()
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("verbatim") || err.contains("structured"),
        "the refusal must say why — no structured model to diff: {err}"
    );
}

#[tokio::test]
async fn diff_refuses_a_verbatim_project_instead_of_reporting_in_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let source = seeded_db("sqlite::memory:").await;
    let config = write_project(tmp.path(), &*source).await;
    let design = Design::from_config(&config, "dev").expect("load");

    let target = dbd_core::connect("sqlite::memory:", "roundtrip")
        .await
        .expect("connect");
    let scope = design.resolve_scope(None, None).expect("scope");

    assert!(
        design.diff_live(&*target, Some(&scope)).await.is_err(),
        "diff must refuse a project it cannot structurally compare, not report no drift"
    );
}

/// `dbd reset` on SQLite ran PostgreSQL's drops (`"public"."authors" CASCADE`)
/// and failed on the first, so a SQLite database could never be reset. It
/// drops the project's views and tables — a child before the parent its
/// foreign key references, since SQLite enforces them and has no CASCADE —
/// and keeps dbd's own bookkeeping.
#[tokio::test]
async fn reset_drops_a_sqlite_projects_tables_and_views() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for (rel, sql) in [
        (
            "ddl/table/authors.ddl",
            "CREATE TABLE IF NOT EXISTS authors (id INTEGER PRIMARY KEY, name TEXT NOT NULL);\n",
        ),
        (
            "ddl/table/books.ddl",
            "CREATE TABLE IF NOT EXISTS books (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES authors(id));\n",
        ),
        (
            "ddl/view/named.ddl",
            "CREATE VIEW IF NOT EXISTS named AS SELECT id, name FROM authors;\n",
        ),
    ] {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, sql).unwrap();
    }
    // Unversioned, so the applied-migrations guard is not what is tested.
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: roundtrip\n\nsource:\n  dialect: sqlite\n\ntarget:\n  sqlite:\n    url: $DATABASE_URL\n\nschemas: []\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");

    let db = dbd_core::connect("sqlite::memory:", "roundtrip")
        .await
        .expect("connect");
    design
        .apply(&*db, None, false, None, dbd_core::design::Progress::none())
        .await
        .expect("apply");
    db.execute_script(
        "INSERT INTO authors (id, name) VALUES (1, 'a'); INSERT INTO books (id, author_id) VALUES (1, 1);",
    )
    .await
    .expect("rows a foreign key holds on to");

    design
        .reset(&*db, "sqlite", false, false, false, None)
        .await
        .expect("a SQLite reset");

    assert_eq!(db.list_entities().await.unwrap(), Vec::<String>::new());
    // The bookkeeping tables are dbd's, not the project's: they survive, while
    // a full reset forgets the project's row in them.
    db.execute_script("SELECT 1 FROM _dbd_meta LIMIT 0")
        .await
        .expect("dbd's bookkeeping is not the project's to drop");
    assert!(
        db.get_project_meta().await.unwrap().is_none(),
        "a full reset forgets the database's record of the project"
    );
}

/// A SQLite project's migration is SQLite, which libpg_query cannot read —
/// `AUTOINCREMENT`, `INSERT OR IGNORE` — and the uses guard, Postgres's,
/// refused it as unreadable: no SQLite migration could run (#7).
#[tokio::test]
async fn a_sqlite_migration_in_sqlite_syntax_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let yaml = |version: u32| {
        format!(
            "project:\n  name: lite\n  version: {version}\n\nsource:\n  dialect: sqlite\n\n\
             target:\n  sqlite:\n    url: $DATABASE_URL\n\nschemas: []\n"
        )
    };
    std::fs::create_dir_all(dir.join("ddl/table")).unwrap();
    std::fs::write(
        dir.join("ddl/table/items.ddl"),
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);\n",
    )
    .unwrap();
    std::fs::write(dir.join("design.yaml"), yaml(1)).unwrap();
    let db_path = dir.join("lite.db");
    let db = dbd_core::connect(&format!("sqlite://{}", db_path.display()), "lite")
        .await
        .expect("connect");
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load v1")
        .apply(&*db, None, false, None, dbd_core::design::Progress::none())
        .await
        .expect("v1 applies");

    std::fs::create_dir_all(dir.join("migrations/002")).unwrap();
    std::fs::write(
        dir.join("migrations/002/graph.json"),
        r#"{"fromVersion":1,"toVersion":2,"added":[],"altered":["items"],"dropped":[]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("migrations/002/items.sql"),
        "CREATE TABLE items_new (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, note TEXT);\n\
         INSERT OR IGNORE INTO items_new (id, name) SELECT id, name FROM items;\n\
         DROP TABLE items;\n\
         ALTER TABLE items_new RENAME TO items;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/items.ddl"),
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, note TEXT);\n",
    )
    .unwrap();
    std::fs::write(dir.join("design.yaml"), yaml(2)).unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load v2")
        .apply(&*db, None, false, None, dbd_core::design::Progress::none())
        .await
        .expect("v2's SQLite migration runs");
    assert_eq!(db.get_db_version().await.unwrap(), 2);
}

/// A scope names a SQLite table bare, or as `main.<table>` (#87), and a run
/// under it builds that table alone.
#[tokio::test]
async fn a_scope_builds_the_sqlite_table_it_names() {
    for item in ["items", "main.items"] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            format!(
                "project:\n  name: lite\nsource:\n  dialect: sqlite\nschemas: []\n\
                 scopes:\n  one:\n    includes: [{item}]\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/items.ddl"),
            "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/notes.ddl"),
            "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY);\n",
        )
        .unwrap();
        let db = dbd_core::connect(&format!("sqlite://{}", dir.join("lite.db").display()), "lite")
            .await
            .expect("connect");
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
        let scope = design.resolve_scope(Some("one"), None).expect(item);
        design
            .apply(&*db, None, false, Some(&scope), dbd_core::design::Progress::none())
            .await
            .expect("apply under the scope");
        let tables = db.list_entities().await.unwrap();
        assert!(tables.iter().any(|t| t.ends_with("items")), "{item}: {tables:?}");
        assert!(!tables.iter().any(|t| t.ends_with("notes")), "{item}: {tables:?}");
    }
}
