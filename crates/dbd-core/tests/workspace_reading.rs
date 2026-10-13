//! What reads a workspace as one design (#7 phase 3, slice 5): the schema
//! model, the graph, DBML and the combined and emitted scripts go over the
//! modules a run selects, each through its scope — every entity drawn and
//! written once, in the module declaring it, from that module's definition.

use dbd_core::design::{Reading, Workspace};
use dbd_core::parser::Dialect;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `shop`: `shared` holds currencies and the order status; `core` holds
/// orders, keyed to a currency and typed with the status, and a view reading
/// both; `lean` is `core` without the currencies.
fn shop(root: &Path) {
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n  note: The shop.\n\nschemas:\n  - app\n\n\
         modules:\n  shared:\n    note: Reference data.\n  core:\n    depends_on: [shared]\n\n\
         scopes:\n  lean:\n    modules: [core]\n    excludes: [app.currencies]\n",
    );
    write(
        root,
        "shared/ddl/table/app/currencies.ddl",
        "set search_path to app;\n\ncreate table if not exists currencies (\n  code text primary key\n, name text not null\n);\n",
    );
    write(
        root,
        "shared/ddl/enum/app/status.ddl",
        "set search_path to app;\n\ncreate type status as enum ('open', 'closed');\n",
    );
    write(
        root,
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\n\ncreate table if not exists orders (\n  id integer primary key\n\
         , currency text references currencies (code)\n, state status not null\n);\n",
    );
    write(
        root,
        "core/ddl/view/app/order_currencies.ddl",
        "set search_path to app;\n\ncreate or replace view order_currencies as\n\
         select o.id, c.name from orders o join currencies c on c.code = o.currency;\n",
    );
}

fn reading(root: &Path, module: Option<&str>, scope: Option<&str>) -> Reading {
    Workspace::load(&root.join("design.yaml"), "dev", Some(root))
        .expect("load")
        .reading(module, scope, None)
        .expect("reading")
}

#[test]
fn a_workspaces_model_names_its_modules_and_each_entitys_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let model = dbd_core::schema_model::build_reading(&reading(tmp.path(), None, None)).expect("model");

    assert_eq!(model.project.name, "shop");
    assert_eq!(model.project.note.as_deref(), Some("The shop."));
    let modules: Vec<(&str, &str, Vec<&str>, Option<&str>)> = model
        .modules
        .iter()
        .map(|m| {
            (
                m.name.as_str(),
                m.owner.as_str(),
                m.depends_on.iter().map(String::as_str).collect(),
                m.note.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        modules,
        [
            ("shared", "shop/shared", vec![], Some("Reference data.")),
            ("core", "shop/core", vec!["shared"], None),
        ]
    );

    let tables: Vec<(&str, Option<&str>)> = model
        .tables
        .iter()
        .map(|t| (t.name.as_str(), t.module.as_deref()))
        .collect();
    assert_eq!(tables, [("currencies", Some("shared")), ("orders", Some("core"))]);
    let entities: Vec<(&str, Option<&str>)> = model
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e.module.as_deref()))
        .collect();
    assert_eq!(entities, [("order_currencies", Some("core"))]);
    let enums: Vec<(&str, Option<&str>)> = model
        .enums
        .iter()
        .map(|e| (e.name.as_str(), e.module.as_deref()))
        .collect();
    assert_eq!(enums, [("status", Some("shared"))]);
    assert_eq!(model.schemas.len(), 1, "one schema, counted over the modules");
    assert_eq!((model.schemas[0].tables, model.schemas[0].enums), (2, 1));
}

/// A key, a type and a read into a dependency land on its entity — drawn
/// once, in its module — not on a stub or an unresolved name.
#[test]
fn what_a_module_uses_of_a_dependency_lands_on_its_entity() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let model = dbd_core::schema_model::build_reading(&reading(tmp.path(), None, None)).expect("model");

    assert!(model.stubs.is_empty(), "{:?}", model.stubs);
    assert!(
        model.refs.iter().any(|r| r.from.t == "orders" && r.to.t == "currencies"),
        "{:?}",
        model.refs
    );
    let orders = model.tables.iter().find(|t| t.name == "orders").unwrap();
    let state = orders.columns.iter().find(|c| c.name == "state").unwrap();
    assert!(state.en, "the status is shared's enum");
    let reads: Vec<_> = model
        .deps
        .iter()
        .filter(|d| d.from.n == "order_currencies")
        .map(|d| (d.to.n.as_str(), d.unresolved))
        .collect();
    assert!(reads.contains(&("currencies", false)), "{reads:?}");
}

/// A table a scope leaves out of a dependency is a stub, as in one design —
/// typed from the module declaring it, and named its.
#[test]
fn a_dependencys_table_a_scope_leaves_out_is_a_stub_from_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let model = dbd_core::schema_model::build_reading(&reading(tmp.path(), None, Some("lean"))).expect("model");

    let tables: Vec<&str> = model.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tables, ["orders"]);
    let [stub] = &model.stubs[..] else {
        panic!("one stub: {:?}", model.stubs)
    };
    assert_eq!(
        (stub.name.as_str(), stub.kind.as_str(), stub.module.as_deref()),
        ("currencies", "out_of_scope", Some("shared"))
    );
    let code: Vec<(&str, &str)> = stub.columns.iter().map(|c| (c.name.as_str(), c.ty.as_str())).collect();
    assert_eq!(code, [("code", "text")]);
}

/// `--module` is the module and what it depends on — and the model names
/// just those.
#[test]
fn a_module_views_itself_and_its_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let model = dbd_core::schema_model::build_reading(&reading(tmp.path(), Some("shared"), None)).expect("model");
    let modules: Vec<&str> = model.modules.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(modules, ["shared"]);
    let tables: Vec<&str> = model.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tables, ["currencies"]);
}

/// A project's model is as it was: no modules, no module on anything.
#[test]
fn a_projects_model_names_no_module() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", "project:\n  name: shop\n\nschemas:\n  - app\n");
    write(
        root,
        "ddl/table/app/orders.ddl",
        "set search_path to app;\n\ncreate table if not exists orders (id integer primary key);\n",
    );
    let model = dbd_core::schema_model::build_reading(&reading(root, None, None)).expect("model");
    assert!(model.modules.is_empty());
    assert!(model.tables.iter().all(|t| t.module.is_none()));
    let json = serde_json::to_string(&model).unwrap();
    assert!(!json.contains("\"module"), "{json}");
}

/// The graph spans the modules in view: `-n` follows an entity into its
/// dependencies' modules, and one no module declares is refused.
#[test]
fn the_graph_spans_the_modules_read() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let reading = reading(tmp.path(), None, None);
    assert_eq!(reading.module_of("app.currencies"), Some("shared"));
    assert_eq!(reading.module_of("app.orders"), Some("core"));

    let graph = reading.graph(Some("app.orders")).expect("graph");
    let nodes: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(nodes.contains(&"app.currencies"), "{nodes:?}");
    assert!(
        graph
            .edges
            .iter()
            .any(|e| e.from == "app.orders" && e.to == "app.currencies"),
        "{:?}",
        graph.edges
    );
    let err = match reading.graph(Some("app.nothing")) {
        Ok(_) => panic!("no module declares app.nothing"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("app.nothing"), "{err}");
}

/// One script: each module's statements after a comment naming it, in
/// `depends_on` order.
#[test]
fn the_combined_script_heads_each_modules_statements() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let script = reading(tmp.path(), None, None).combined().expect("combine");
    let shared = script.find("-- module shared (shop/shared)").expect(&script);
    let currencies = script.find("create table if not exists currencies").expect(&script);
    let core = script.find("-- module core (shop/core)").expect(&script);
    let orders = script.find("create table if not exists orders").expect(&script);
    assert!(shared < currencies && currencies < core && core < orders, "{script}");
}

/// An emitted script is the workspace's: a key into a dependency's table is
/// kept, since the script creates that table too.
#[test]
fn an_emitted_script_keeps_a_key_into_a_dependency() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let (sql, downgrades) =
        dbd_core::emit_dialect::emit_reading(&reading(tmp.path(), None, None), Dialect::Sqlite).expect("emit");
    assert!(sql.contains("module shared (shop/shared)"), "{sql}");
    assert!(sql.contains("module core (shop/core)"), "{sql}");
    assert!(
        !downgrades.iter().any(|d| d.from.contains("foreign key")),
        "{downgrades:?}"
    );
    let start = sql
        .match_indices("CREATE TABLE")
        .map(|(at, _)| at)
        .find(|&at| sql[at..].lines().next().unwrap().contains("orders"))
        .expect(&sql);
    let orders = &sql[start..start + sql[start..].find(';').unwrap()];
    assert!(orders.contains("REFERENCES") && orders.contains("currencies"), "{sql}");
}

/// DBML documents the workspace: a document with no groups of its own gets
/// one `TableGroup` per module.
#[test]
fn dbml_groups_each_modules_tables() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let docs = dbd_core::dbml::generate_reading(&reading(tmp.path(), None, None)).expect("dbml");
    let [doc] = &docs[..] else { panic!("one document") };
    let shared = doc.content.find("TableGroup \"shared\"").expect(&doc.content);
    let core = doc.content.find("TableGroup \"core\"").expect(&doc.content);
    let group = |at: usize| &doc.content[at..at + doc.content[at..].find('}').unwrap()];
    assert!(group(shared).contains("\"currencies\"") && !group(shared).contains("\"orders\""));
    assert!(group(core).contains("\"orders\"") && !group(core).contains("\"currencies\""));
    assert!(doc.content.contains("Project \"shop\""), "{}", doc.content);
}

/// Each module's history is its own, named for it, the modules' one after
/// another in `depends_on` order.
#[test]
fn each_history_entry_names_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    let workspace = Workspace::load(&root.join("design.yaml"), "dev", Some(root)).expect("load");
    for module in workspace.modules() {
        dbd_core::snapshot::create_snapshot_of(
            module.design().entities(),
            module.design().project_dir(),
            &root.join("design.yaml"),
            module.name(),
            "first",
        )
        .expect("snapshot");
    }
    let history = reading(root, None, None).history().expect("history");
    let entries: Vec<(Option<&str>, u32)> = history.iter().map(|h| (h.module.as_deref(), h.version)).collect();
    assert_eq!(entries, [(Some("shared"), 1), (Some("core"), 1)]);
}
