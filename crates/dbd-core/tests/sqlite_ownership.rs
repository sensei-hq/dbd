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

/// SQLite has no schemas: `auth.items` is the table `items`. Claimed under its
/// path's schema, one table had two keys — and a second project, its file
/// flat, took it, and its reset dropped the first's table and rows (#7).
#[tokio::test]
async fn a_sqlite_table_is_one_object_whatever_schema_its_path_names() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(ad.join("ddl/table/auth")).unwrap();
    std::fs::create_dir_all(&bd).unwrap();
    std::fs::write(
        ad.join("design.yaml"),
        "project:\n  name: lite_a\n\nsource:\n  dialect: sqlite\n\nschemas: []\n",
    )
    .unwrap();
    std::fs::write(
        ad.join("ddl/table/auth/items.ddl"),
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY);\n",
    )
    .unwrap();
    let a = connect(&file, "lite_a").await;
    Design::from_config_with_dir(&ad.join("design.yaml"), "dev", Some(&ad))
        .unwrap()
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");

    let b = connect(&file, "lite_b").await;
    let theirs = lite_project(&bd, "lite_b", &["items"]);
    let err = theirs
        .apply(&*b, None, false, None, Progress::none())
        .await
        .expect_err("items is lite_a's")
        .to_string();
    assert!(err.contains("items is owned by \"lite_a\""), "{err}");
    theirs
        .reset(&*b, "sqlite", true, false, false, None)
        .await
        .expect_err("a reset claims first");
    assert!(a.list_entities().await.unwrap().contains(&"items".to_string()));
}

/// SQLite matches names without regard to case, and keeps the case a table
/// was created with; a key from a path is folded. A mixed-case table read as
/// gone: no name-reuse guard, and its claim released once its file left the
/// design, for another project's reset to drop it (#7).
#[tokio::test]
async fn a_mixed_case_sqlite_table_stays_its_owners() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::create_dir_all(&bd).unwrap();
    let a = connect(&file, "lite_a").await;
    lite_project(&ad, "lite_a", &["orders"]);
    std::fs::write(
        ad.join("ddl/table/Users.ddl"),
        "CREATE TABLE IF NOT EXISTS Users (id INTEGER PRIMARY KEY);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&ad.join("design.yaml"), "dev", Some(&ad)).unwrap();
    design
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");
    let owned = a.owned_objects("lite_a").await.unwrap();
    assert!(owned.iter().any(|o| o.name.eq_ignore_ascii_case("users")), "{owned:?}");

    // Out of the design, still in the database: still lite_a's.
    std::fs::remove_file(ad.join("ddl/table/Users.ddl")).unwrap();
    Design::from_config_with_dir(&ad.join("design.yaml"), "dev", Some(&ad))
        .unwrap()
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply without Users");
    let b = connect(&file, "lite_b").await;
    lite_project(&bd, "lite_b", &["users"])
        .reset(&*b, "sqlite", true, false, false, None)
        .await
        .expect_err("Users is lite_a's");
    assert!(a.list_entities().await.unwrap().contains(&"Users".to_string()));
}

/// SQLite has no catalog of what depends on what, but owners record their
/// uses: a reset of a table another project's view reads is refused (#7).
#[tokio::test]
async fn a_sqlite_reset_is_refused_for_a_table_another_projects_view_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::create_dir_all(bd.join("ddl/view")).unwrap();
    let a = connect(&file, "lite_a").await;
    let design = lite_project(&ad, "lite_a", &["items"]);
    design
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");
    // Read as Postgres, so its view's reference is recorded.
    std::fs::write(
        bd.join("design.yaml"),
        "project:\n  name: lite_b\n\nexternal:\n  - name: items\n\nschemas: []\n",
    )
    .unwrap();
    std::fs::write(
        bd.join("ddl/view/v_items.ddl"),
        "CREATE VIEW v_items AS SELECT id FROM items;\n",
    )
    .unwrap();
    Design::from_config_with_dir(&bd.join("design.yaml"), "dev", Some(&bd))
        .unwrap()
        .apply(&*connect(&file, "lite_b").await, None, false, None, Progress::none())
        .await
        .expect("lite_b's apply");

    let err = design
        .reset(&*a, "sqlite", true, false, false, None)
        .await
        .expect_err("lite_b's view reads items")
        .to_string();
    assert!(err.contains("used by \"lite_b\""), "{err}");
    assert!(a.list_entities().await.unwrap().contains(&"items".to_string()));
}

/// A claim refused for one held table takes none of the others; a claim of a
/// table dropped and no longer declared is released; and a trigger is not a
/// table, whatever it is called (#7).
#[tokio::test]
async fn sqlite_claims_are_refused_whole_and_released_when_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shared.db");
    let (ad, bd) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&ad).unwrap();
    std::fs::create_dir_all(&bd).unwrap();
    let a = connect(&file, "lite_a").await;
    lite_project(&ad, "lite_a", &["items", "orders", "t"])
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply");

    let b = connect(&file, "lite_b").await;
    lite_project(&bd, "lite_b", &["items", "extras"])
        .reset(&*b, "sqlite", true, false, false, None)
        .await
        .expect_err("items is lite_a's");
    assert!(
        !b.registry().await.unwrap().claims.iter().any(|h| h.owner == "lite_b"),
        "a refused claim took nothing"
    );

    a.execute_script("DROP TABLE orders; CREATE TRIGGER orders AFTER INSERT ON t BEGIN SELECT 1; END;")
        .await
        .unwrap();
    std::fs::remove_file(ad.join("ddl/table/orders.ddl")).unwrap();
    Design::from_config_with_dir(&ad.join("design.yaml"), "dev", Some(&ad))
        .unwrap()
        .apply(&*a, None, false, None, Progress::none())
        .await
        .expect("lite_a's apply without orders");
    let owned: Vec<String> = a
        .owned_objects("lite_a")
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.name)
        .collect();
    assert!(!owned.contains(&"orders".to_string()), "{owned:?}");
    assert!(owned.contains(&"items".to_string()), "{owned:?}");
}

/// The workspace transfer rule on SQLite (#7 phase 3, slice 6): a project
/// split into modules hands each its tables, a table moved from one module
/// to another changes hands, and the project's record is retired once it
/// holds nothing.
#[tokio::test]
async fn sqlite_modules_take_over_from_their_project_and_from_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shop.db");
    let root = tmp.path().join("shop");
    let table = |name: &str| format!("CREATE TABLE IF NOT EXISTS {name} (id INTEGER PRIMARY KEY);\n");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "design.yaml",
        "project:\n  name: shop\n\nsource:\n  dialect: sqlite\n\nschemas: []\n",
    );
    write("ddl/table/items.ddl", &table("items"));
    write("ddl/table/orders.ddl", &table("orders"));
    let project = Design::from_config_with_dir(&root.join("design.yaml"), "dev", Some(&root)).expect("load");
    let shop = connect(&file, "shop").await;
    project
        .apply(&*shop, None, false, None, Progress::none())
        .await
        .expect("shop's apply");
    shop.execute_script("INSERT INTO items (id) VALUES (7)").await.unwrap();

    std::fs::remove_file(root.join("ddl/table/items.ddl")).unwrap();
    std::fs::remove_file(root.join("ddl/table/orders.ddl")).unwrap();
    write("shared/ddl/table/items.ddl", &table("items"));
    write("core/ddl/table/orders.ddl", &table("orders"));
    write(
        "design.yaml",
        "project:\n  name: shop\n\nsource:\n  dialect: sqlite\n\nschemas: []\n\n\
         modules:\n  shared: {}\n  core:\n    depends_on: [shared]\n",
    );
    let apply_all = || async {
        let modules = dbd_core::design::Workspace::load(&root.join("design.yaml"), "dev", Some(&root))
            .expect("load")
            .select(None)
            .expect("select");
        for module in &modules {
            module
                .design()
                .apply(
                    &*connect(&file, module.owner()).await,
                    None,
                    false,
                    None,
                    Progress::none(),
                )
                .await
                .unwrap_or_else(|e| panic!("{}'s apply: {e}", module.owner()));
        }
        modules
    };
    let modules = apply_all().await;
    let owners = |shared: dbd_core::design::SharedDatabase| -> Vec<(String, Vec<String>)> {
        shared
            .projects
            .into_iter()
            .map(|p| (p.name, p.owns.into_iter().map(|o| o.name).collect()))
            .collect()
    };
    let registry = modules[0].design().shared_database(&*shop).await.unwrap();
    assert_eq!(
        owners(registry),
        [
            ("shop/core".to_string(), vec!["orders".to_string()]),
            ("shop/shared".to_string(), vec!["items".to_string()]),
        ],
        "shop is retired once its tables are the modules'"
    );

    // items moves to core.
    std::fs::rename(
        root.join("shared/ddl/table/items.ddl"),
        root.join("core/ddl/table/items.ddl"),
    )
    .unwrap();
    write("shared/ddl/table/notes.ddl", &table("notes"));
    let modules = apply_all().await;
    let registry = modules[0].design().shared_database(&*shop).await.unwrap();
    let core = owners(registry)
        .into_iter()
        .find(|(name, _)| name == "shop/core")
        .expect("core");
    assert!(core.1.contains(&"items".to_string()), "{core:?}");
    assert!(
        shop.list_entities().await.unwrap().contains(&"items".to_string()),
        "items is kept"
    );
}

/// On SQLite as on Postgres, the project keeps its record — and its claim —
/// while it holds a table its modules have not taken over yet.
#[tokio::test]
async fn sqlite_keeps_the_project_until_its_last_table_is_taken_over() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("shop.db");
    let root = tmp.path().join("shop");
    let table = |name: &str| format!("CREATE TABLE IF NOT EXISTS {name} (id INTEGER PRIMARY KEY);\n");
    let write = |rel: &str, text: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "design.yaml",
        "project:\n  name: shop\n\nsource:\n  dialect: sqlite\n\nschemas: []\n",
    );
    write("ddl/table/items.ddl", &table("items"));
    write("ddl/table/orders.ddl", &table("orders"));
    let shop = connect(&file, "shop").await;
    Design::from_config_with_dir(&root.join("design.yaml"), "dev", Some(&root))
        .unwrap()
        .apply(&*shop, None, false, None, Progress::none())
        .await
        .expect("shop's apply");
    std::fs::remove_file(root.join("ddl/table/items.ddl")).unwrap();
    std::fs::remove_file(root.join("ddl/table/orders.ddl")).unwrap();
    write("shared/ddl/table/items.ddl", &table("items"));
    write("core/ddl/table/orders.ddl", &table("orders"));
    write(
        "design.yaml",
        "project:\n  name: shop\n\nsource:\n  dialect: sqlite\n\nschemas: []\n\n\
         modules:\n  shared: {}\n  core:\n    depends_on: [shared]\n",
    );
    let modules = dbd_core::design::Workspace::load(&root.join("design.yaml"), "dev", Some(&root))
        .unwrap()
        .select(None)
        .unwrap();
    modules[0]
        .design()
        .apply(
            &*connect(&file, "shop/shared").await,
            None,
            false,
            None,
            Progress::none(),
        )
        .await
        .expect("shared's apply");
    assert!(!modules[0].design().retired_predecessor(), "shop still holds orders");
    let owned: Vec<String> = shop
        .owned_objects("shop")
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.name)
        .collect();
    assert_eq!(owned, ["orders"]);
}
