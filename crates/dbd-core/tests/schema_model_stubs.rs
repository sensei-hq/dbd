//! Every foreign key in the model leads somewhere the viewer can draw.
//!
//! `refs` used to keep only the foreign keys whose target table was itself in
//! `tables`, so an FK to an `external:` table, to a table the scope leaves out,
//! or to a table the project never defines was dropped — while its column kept
//! `fk: true`. The viewer showed a foreign key that led nowhere.
//!
//! Now the ref stays, and the table it lands on is carried as a *stub* in
//! `stubs`: a table-shaped node holding only the columns the references land
//! on, whose `kind` says why it is not a full table — `external`,
//! `out_of_scope` or `unresolved`. A separate array rather than more entries in
//! `tables`, because `tables` means "this model's tables" to every consumer
//! that counts or lists them.

use dbd_core::Design;
use dbd_core::schema_model::build;
use serde_json::{Value, json};
use std::path::Path;

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// `app.orders` → `app.people` (in the project), → `auth.users` (external),
/// → `app.ghosts` (nowhere).
fn project(dir: &Path) -> Design {
    write(
        dir,
        "design.yaml",
        "project:\n  name: stubs\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\n\
         schemas:\n  - app\n\n\
         external:\n  - name: auth.users\n    note: Supabase accounts\n    columns:\n      - id: uuid\n      - email: text\n\n\
         scopes:\n  orders_only:\n    includes:\n      - app.orders\n",
    );
    write(
        dir,
        "ddl/table/app/people.ddl",
        "set search_path to app;\n\
         create table if not exists people (\n  id integer primary key\n, name text not null\n);\n\
         comment on table people is 'Everyone';",
    );
    write(
        dir,
        "ddl/table/app/orders.ddl",
        "set search_path to app;\n\
         create table if not exists orders (\n  \
           id        integer primary key\n, \
           person_id integer not null references app.people (id)\n, \
           user_id   uuid references auth.users (id)\n, \
           ghost_id  integer references app.ghosts (id)\n\
         );",
    );
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

fn model(design: &Design, scope: Option<&str>) -> Value {
    let resolved = scope.map(|s| design.resolve_scope(Some(s), None).unwrap());
    serde_json::to_value(build(design, resolved.as_ref()).unwrap()).unwrap()
}

fn ref_to(m: &Value, schema: &str, table: &str) -> Option<Value> {
    m["refs"]
        .as_array()?
        .iter()
        .find(|r| r["to"]["s"] == schema && r["to"]["t"] == table)
        .cloned()
}

fn stub(m: &Value, schema: &str, name: &str) -> Option<Value> {
    m["stubs"]
        .as_array()?
        .iter()
        .find(|s| s["schema"] == schema && s["name"] == name)
        .cloned()
}

#[test]
fn a_foreign_key_to_an_external_table_keeps_its_ref_and_lands_on_an_external_stub() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(&project(tmp.path()), None);
    assert_eq!(
        ref_to(&m, "auth", "users"),
        Some(
            json!({ "from": { "s": "app", "t": "orders", "c": "user_id" }, "to": { "s": "auth", "t": "users", "c": "id" } })
        )
    );
    // Typed from the design.yaml declaration, and only the column the ref lands on.
    assert_eq!(
        stub(&m, "auth", "users"),
        Some(
            json!({ "schema": "auth", "name": "users", "kind": "external", "note": "Supabase accounts",
                     "noteMd": "Supabase accounts", "columns": [{ "name": "id", "type": "uuid" }] })
        )
    );
}

#[test]
fn a_foreign_key_out_of_the_scope_keeps_its_ref_and_lands_on_an_out_of_scope_stub() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(&project(tmp.path()), Some("orders_only"));
    let tables: Vec<&Value> = m["tables"].as_array().unwrap().iter().map(|t| &t["name"]).collect();
    assert_eq!(tables, vec!["orders"], "precondition: people is out of scope");
    assert!(ref_to(&m, "app", "people").is_some(), "the FK to people is kept");
    // The table is in the project, so its referenced column is described in full.
    assert_eq!(
        stub(&m, "app", "people"),
        Some(
            json!({ "schema": "app", "name": "people", "kind": "out_of_scope", "note": "Everyone",
                     "noteMd": "Everyone", "columns": [{ "name": "id", "type": "int", "pk": true, "nn": true }] })
        )
    );
}

#[test]
fn a_foreign_key_to_a_table_defined_nowhere_keeps_its_ref_and_lands_on_an_unresolved_stub() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(&project(tmp.path()), None);
    assert!(ref_to(&m, "app", "ghosts").is_some());
    assert_eq!(
        stub(&m, "app", "ghosts"),
        Some(json!({ "schema": "app", "name": "ghosts", "kind": "unresolved",
                     "columns": [{ "name": "id", "type": "" }] }))
    );
}

/// The invariant the viewer relies on: a column that says it is a foreign key
/// has a ref, and every ref lands on a column of a node the model carries.
#[test]
fn every_foreign_key_column_has_a_ref_that_lands_on_a_drawable_column() {
    let tmp = tempfile::tempdir().unwrap();
    let design = project(tmp.path());
    for scope in [None, Some("orders_only")] {
        let m = model(&design, scope);
        let nodes: Vec<&Value> = m["tables"]
            .as_array()
            .unwrap()
            .iter()
            .chain(m["stubs"].as_array().into_iter().flatten())
            .collect();
        for t in m["tables"].as_array().unwrap() {
            for c in t["columns"].as_array().unwrap().iter().filter(|c| c["fk"] == true) {
                assert!(
                    m["refs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|r| r["from"]["s"] == t["schema"]
                            && r["from"]["t"] == t["name"]
                            && r["from"]["c"] == c["name"]),
                    "{scope:?}: {}.{} says fk but has no ref",
                    t["name"],
                    c["name"]
                );
            }
        }
        for r in m["refs"].as_array().unwrap() {
            let target = nodes
                .iter()
                .find(|n| n["schema"] == r["to"]["s"] && n["name"] == r["to"]["t"])
                .unwrap_or_else(|| panic!("{scope:?}: ref {r} lands on no node"));
            assert!(
                target["columns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["name"] == r["to"]["c"]),
                "{scope:?}: ref {r} lands on no column"
            );
        }
    }
}

/// A stub is not one of the model's tables: it is not listed there, and it
/// does not put its schema into `schemas`.
#[test]
fn a_stub_is_neither_a_table_nor_a_schema_of_the_model() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(&project(tmp.path()), None);
    assert!(m["tables"].as_array().unwrap().iter().all(|t| t["kind"] == "table"));
    assert!(m["schemas"].as_array().unwrap().iter().all(|s| s["name"] != "auth"));
}

#[test]
fn a_model_whose_foreign_keys_all_land_inside_it_has_no_stubs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    drop(project(dir));
    write(
        dir,
        "ddl/table/app/orders.ddl",
        "set search_path to app;\n\
         create table if not exists orders (id integer primary key, person_id integer references app.people (id));",
    );
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
    let m = model(&design, None);
    assert!(ref_to(&m, "app", "people").is_some());
    assert!(m.get("stubs").is_none(), "absent, not []");
}
