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

    // core's, with shared's it depends on.
    let graph = json(&ok(&tmp.path().join("core"), &["graph"]));
    let mut found = nodes(&graph);
    found.sort();
    assert!(found.contains(&("app.currencies".into(), "shared".into())), "{found:?}");
    assert!(found.contains(&("app.orders".into(), "core".into())), "{found:?}");
}

/// `-n` is the entity and what it needs, from its module.
#[test]
fn graph_of_an_entity_is_it_and_what_it_needs() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let graph = json(&ok(tmp.path(), &["graph", "-n", "app.currencies"]));
    assert_eq!(nodes(&graph), [("app.currencies".to_string(), "shared".to_string())]);
}

/// A scope selecting a module is that module and what it depends on.
#[test]
fn graph_under_a_scope_selecting_a_module_is_that_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let manifest = fs::read_to_string(tmp.path().join("design.yaml")).unwrap();
    write(
        tmp.path(),
        "design.yaml",
        &format!("{manifest}\nscopes:\n  ref:\n    modules: [shared]\n"),
    );
    let graph = json(&ok(tmp.path(), &["graph", "--scope", "ref"]));
    let names: Vec<String> = nodes(&graph).into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"app.currencies".to_string()), "{names:?}");
    assert!(!names.contains(&"app.orders".to_string()), "{names:?}");
}

/// dbml, the diagram and emit run in a module's folder are that module's.
#[test]
fn every_artifact_in_a_module_folder_is_that_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let shared = tmp.path().join("shared");

    ok(&shared, &["dbml", "-f", "shared.dbml"]);
    let dbml = fs::read_to_string(tmp.path().join("shared.dbml")).unwrap();
    assert!(
        dbml.contains("\"currencies\"") && !dbml.contains("\"orders\""),
        "{dbml}"
    );

    ok(&shared, &["diagram", "--json", "-f", "shared.json"]);
    let model: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("shared.json")).unwrap()).unwrap();
    let tables: Vec<&str> = model["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tables, ["currencies"]);

    ok(&shared, &["emit", "--dialect", "sqlite", "-f", "shared.sql"]);
    let sql = fs::read_to_string(tmp.path().join("shared.sql")).unwrap();
    assert!(sql.contains("currencies") && !sql.contains("orders"), "{sql}");
}

/// A later module's policies are reported as not carried, as one design's.
#[test]
fn emit_reports_each_modules_policies() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(
        tmp.path(),
        "core/policies/app/orders.sql",
        "alter table app.orders enable row level security;\n",
    );
    ok(
        tmp.path(),
        &[
            "emit",
            "--dialect",
            "sqlite",
            "-f",
            "shop.sql",
            "--report",
            "report.json",
        ],
    );
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(tmp.path().join("report.json")).unwrap()).unwrap();
    assert!(
        report.as_array().unwrap().iter().any(|d| d["entity"] == "app.orders"
            && d["to"] == "nothing"
            && d["from"].as_str().unwrap().contains("policies/app/orders.sql")),
        "{report}"
    );
}

/// Each configured document without groups of its own gets one per module
/// it holds tables of; one with its own groups keeps just them.
#[test]
fn every_dbml_document_groups_by_module_unless_it_groups_its_own_way() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let manifest = fs::read_to_string(tmp.path().join("design.yaml")).unwrap();
    write(
        tmp.path(),
        "design.yaml",
        &format!(
            "{manifest}\ndbml:\n  shop: {{}}\n  ref:\n    include:\n      tables: [app.currencies]\n  \
             own:\n    groups:\n      - name: mine\n        tables: [app.orders]\n"
        ),
    );
    ok(tmp.path(), &["dbml", "-f", "x.dbml"]);
    let groups = |doc: &str| -> Vec<String> {
        fs::read_to_string(tmp.path().join(doc))
            .unwrap()
            .lines()
            .filter_map(|l| l.strip_prefix("TableGroup "))
            .map(|g| g.trim_end_matches(" {").to_string())
            .collect()
    };
    assert_eq!(groups("shop.dbml"), ["\"shared\"", "\"core\""]);
    assert_eq!(groups("ref.dbml"), ["\"shared\""]);
    assert_eq!(groups("own.dbml"), ["\"mine\""]);
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

/// The scope line counts what the run writes — the manifest's schema once,
/// as it is written once.
#[test]
fn the_scope_line_counts_each_entity_once() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let manifest = fs::read_to_string(tmp.path().join("design.yaml")).unwrap();
    write(
        tmp.path(),
        "design.yaml",
        &format!("{manifest}\nscopes:\n  lean:\n    modules: [core]\n    excludes: [app.currencies]\n"),
    );
    let out = ok(tmp.path(), &["combine", "-f", "lean.sql", "--scope", "lean"]);
    // app and orders, of app, currencies and orders.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("scope 'lean': 2 of 3 entities"),
        "{}",
        text(&out)
    );
}

/// `-n` of an entity `--module` leaves out names the module declaring it.
#[test]
fn graph_of_an_entity_outside_the_module_names_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd(tmp.path(), &["graph", "--module", "shared", "-n", "app.orders"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("app.orders") && text.contains("\"core\""), "{text}");
}
