//! `dbml`, `diagram`, `graph`, `combine` and `emit` over a workspace (#7
//! phase 3, slice 5): each goes over the modules a run selects as one design,
//! every entity written once, in the module declaring it.
//!
//! None of these need a database. Every run clears `DATABASE_URL`.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn dbd(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .env_remove("DATABASE_URL")
        .arg("-s")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = dbd(dir, args);
    assert!(out.status.success(), "{args:?}: {}", text(&out));
    out
}

/// `shop`: `shared` holds currencies, `core` holds orders keyed to them.
fn shop(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\n\
         modules:\n  shared: {}\n  core:\n    depends_on: [shared]\n",
    );
    write(
        dir,
        "shared/ddl/table/app/currencies.ddl",
        "set search_path to app;\n\ncreate table if not exists currencies (\n  code text primary key\n);\n",
    );
    write(
        dir,
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\n\ncreate table if not exists orders (\n  id integer primary key\n\
         , currency text references currencies (code)\n);\n",
    );
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(out)))
}

fn nodes(graph: &serde_json::Value) -> Vec<(String, String)> {
    graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["name"].as_str().unwrap().to_string(),
                n["module"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect()
}

#[test]
fn graph_goes_over_the_workspace_naming_each_nodes_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let graph = json(&ok(tmp.path(), &["graph"]));
    let mut found = nodes(&graph);
    found.sort();
    assert!(found.contains(&("app.currencies".into(), "shared".into())), "{found:?}");
    assert!(found.contains(&("app.orders".into(), "core".into())), "{found:?}");
    assert!(
        graph["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["from"] == "app.orders" && e["to"] == "app.currencies"),
        "{graph}"
    );

    let graph = json(&ok(tmp.path(), &["graph", "--module", "shared"]));
    let names: Vec<String> = nodes(&graph).into_iter().map(|(n, _)| n).collect();
    assert!(!names.contains(&"app.orders".to_string()), "{names:?}");
    assert!(names.contains(&"app.currencies".to_string()), "{names:?}");

    let graph = json(&ok(tmp.path(), &["graph", "-n", "app.orders"]));
    let names: Vec<String> = nodes(&graph).into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"app.currencies".to_string()), "{names:?}");
}

/// Run in a module's folder, it is that module's — with its dependencies.
#[test]
fn graph_in_a_module_folder_is_that_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let graph = json(&ok(&tmp.path().join("shared"), &["graph"]));
    let names: Vec<String> = nodes(&graph).into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"app.currencies".to_string()), "{names:?}");
    assert!(!names.contains(&"app.orders".to_string()), "{names:?}");
}

#[test]
fn dbml_documents_the_workspace_a_group_per_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    ok(tmp.path(), &["dbml", "-f", "shop.dbml"]);
    let dbml = fs::read_to_string(tmp.path().join("shop.dbml")).unwrap();
    for wanted in [
        "Project \"shop\"",
        "Table \"app\".\"currencies\"",
        "Table \"app\".\"orders\"",
        "TableGroup \"shared\"",
        "TableGroup \"core\"",
    ] {
        assert!(dbml.contains(wanted), "{wanted}: {dbml}");
    }

    // A document grouping its own way is grouped its way: DBML puts a table in
    // one group.
    let manifest = fs::read_to_string(tmp.path().join("design.yaml")).unwrap();
    write(
        tmp.path(),
        "design.yaml",
        &format!("{manifest}\ndbml:\n  shop:\n    auto_group_by_schema: true\n"),
    );
    ok(tmp.path(), &["dbml", "-f", "shop.dbml"]);
    let dbml = fs::read_to_string(tmp.path().join("shop.dbml")).unwrap();
    assert!(dbml.contains("TableGroup \"app\""), "{dbml}");
    assert!(!dbml.contains("TableGroup \"shared\""), "{dbml}");
}

#[test]
fn combine_writes_one_script_a_section_per_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    ok(tmp.path(), &["combine", "-f", "all.sql"]);
    let sql = fs::read_to_string(tmp.path().join("all.sql")).unwrap();
    let shared = sql.find("-- module shared (shop/shared)").expect(&sql);
    let core = sql.find("-- module core (shop/core)").expect(&sql);
    let currencies = sql.find("currencies (").expect(&sql);
    let orders = sql.find("orders (").expect(&sql);
    assert!(shared < currencies && currencies < core && core < orders, "{sql}");

    ok(tmp.path(), &["combine", "-f", "shared.sql", "--module", "shared"]);
    let sql = fs::read_to_string(tmp.path().join("shared.sql")).unwrap();
    assert!(sql.contains("currencies (") && !sql.contains("orders ("), "{sql}");
}

#[test]
fn emit_translates_the_workspace_keeping_keys_between_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = ok(tmp.path(), &["emit", "--dialect", "sqlite", "-f", "shop.sql"]);
    let text = text(&out);
    assert!(!text.contains("foreign key"), "a key into shared is kept: {text}");
    let sql = fs::read_to_string(tmp.path().join("shop.sql")).unwrap();
    assert!(sql.contains("currencies") && sql.contains("orders"), "{sql}");
    assert!(sql.contains("REFERENCES"), "{sql}");
}

#[test]
fn the_diagram_model_names_the_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    ok(tmp.path(), &["diagram", "--json", "-f", "model.json"]);
    let model: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("model.json")).unwrap()).unwrap();
    assert_eq!(model["project"]["name"], "shop");
    let modules: Vec<&str> = model["modules"]
        .as_array()
        .expect("modules")
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(modules, ["shared", "core"]);
    let tables: Vec<(&str, &str)> = model["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t["module"].as_str().unwrap()))
        .collect();
    assert_eq!(tables, [("currencies", "shared"), ("orders", "core")]);
    assert!(model.get("stubs").is_none(), "the key lands on shared's table: {model}");
}
