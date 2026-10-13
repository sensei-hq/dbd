//! Entity ownership on SQLite (#7 phase 2, slice 8).
//!
//! SQLite takes the same model as Postgres in its one namespace: what each
//! project's design declares is claimed in `_dbd_objects`, what it uses
//! recorded in `_dbd_uses`. Two projects sharing one database file are kept
//! apart by it as two sharing a Postgres database are.

#![cfg(feature = "sqlite")]

use dbd_core::adapter::DatabaseAdapter;
use dbd_core::design::Progress;
use dbd_core::{Design, EntityType};
use std::path::Path;

/// A SQLite project `name` in `dir`, one table per name in `tables`.
fn lite_project(dir: &Path, name: &str, tables: &[&str]) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        format!("project:\n  name: {name}\n\nsource:\n  dialect: sqlite\n\nschemas: []\n"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("ddl/table")).unwrap();
    for table in tables {
        std::fs::write(
            dir.join(format!("ddl/table/{table}.ddl")),
            format!("CREATE TABLE IF NOT EXISTS {table} (id INTEGER PRIMARY KEY);\n"),
        )
        .unwrap();
    }
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

async fn connect(file: &Path, project: &str) -> Box<dyn DatabaseAdapter> {
    dbd_core::connect(&format!("sqlite://{}", file.display()), project)
        .await
        .expect("connect")
}

/// T18: a second project declaring a table the first owns is refused, and
/// builds nothing (#7).
#[tokio::test]
async fn a_second_sqlite_project_declaring_a_table_the_first_owns_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::create_dir_all(&bd).unwrap();
    lite_project(&ad, "lite_a", &["items"])
        .apply(&*connect(&file, "lite_a").await, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");

    let b = connect(&file, "lite_b").await;
    let err = lite_project(&bd, "lite_b", &["items", "extras"])
        .apply(&*b, None, false, None, Progress::none())
        .await
        .expect_err("items is lite_a's")
        .to_string();
    assert!(err.contains("items is owned by \"lite_a\""), "{err}");
    assert!(!b.list_entities().await.unwrap().contains(&"extras".to_string()));
}

/// What shares a SQLite database is read from its registry — and the
/// registry's own tables are dbd's, never an entity of the project's (#7).
#[tokio::test]
async fn a_shared_sqlite_database_names_each_project_and_what_it_owns() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::create_dir_all(&bd).unwrap();
    let a = connect(&file, "lite_a").await;
    let design = lite_project(&ad, "lite_a", &["items"]);
    design
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");
    lite_project(&bd, "lite_b", &["extras"])
        .apply(&*connect(&file, "lite_b").await, None, false, None, Progress::none())
        .await
        .expect("lite_b's apply");

    let shared = design.shared_database(&*a).await.expect("the report");
    let owns = |name: &str| -> Vec<String> {
        shared
            .projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.owns.iter().map(|o| o.name.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(owns("lite_a"), ["items"]);
    assert_eq!(owns("lite_b"), ["extras"]);
    assert!(shared.projects.iter().all(|p| p.recording), "{:?}", shared.projects);

    let listed = a.list_entities().await.unwrap();
    assert!(!listed.iter().any(|n| n.starts_with("_dbd_")), "{listed:?}");
    let introspected = a.introspect().await.unwrap();
    assert!(
        !introspected
            .iter()
            .any(|e| e.entity_type == EntityType::Table && e.name.starts_with("_dbd_")),
        "{:?}",
        introspected.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

/// A design under a SQLite project's name declaring none of its tables is
/// another project reusing the name; `--adopt-from` takes a table over (#7).
#[tokio::test]
async fn sqlite_name_reuse_is_refused_and_adoption_moves_a_table() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, nd, bd) = (tmp.path().join("a"), tmp.path().join("n"), tmp.path().join("b"));
    for d in [&ad, &nd, &bd] {
        std::fs::create_dir_all(d).unwrap();
    }
    let a = connect(&file, "lite_a").await;
    lite_project(&ad, "lite_a", &["items"])
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");

    let err = lite_project(&nd, "lite_a", &["other"])
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect_err("another project under lite_a's name")
        .to_string();
    assert!(err.contains("this design declares none of them"), "{err}");

    let b = connect(&file, "lite_b").await;
    let taker = lite_project(&bd, "lite_b", &["items"]).adopting_from("lite_a");
    taker
        .apply(&*b, None, false, None, Progress::none())
        .await
        .expect("lite_b adopts items");
    let shared = taker.shared_database(&*b).await.unwrap();
    let owner_of_items = shared
        .projects
        .iter()
        .find(|p| p.owns.iter().any(|o| o.name == "items"))
        .map(|p| p.name.as_str());
    assert_eq!(owner_of_items, Some("lite_b"));
}

/// A SQLite reset claims first: a design declaring another project's table
/// is refused before it drops anything (#7).
#[tokio::test]
async fn a_sqlite_reset_cannot_drop_another_projects_table() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::create_dir_all(&bd).unwrap();
    let a = connect(&file, "lite_a").await;
    lite_project(&ad, "lite_a", &["items"])
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");

    lite_project(&bd, "lite_b", &["items"])
        .reset(&*connect(&file, "lite_b").await, "sqlite", true, false, false, None)
        .await
        .expect_err("items is lite_a's");
    assert!(a.list_entities().await.unwrap().contains(&"items".to_string()));
}
