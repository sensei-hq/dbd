//! `dbd dbml` → `dbd init --from-dbml` must give back the schema it documented.
//!
//! Each test drives the chain a user actually runs: DDL files are parsed the
//! way a project scan parses them, `dbml::generate_dbml` documents them,
//! `dbml_parse::parse_dbml` reads the document back, `emit::emit_entity` writes
//! the DDL `init --from-dbml` puts in `ddl/`, and that DDL is parsed again. What
//! a feature looks like at the end of the chain is what the user gets, so that
//! is what these tests assert on — not the intermediate DBML text alone, which
//! can look right while the parser on the other side drops it.

use std::path::Path;

use dbd_core::dbml::{DbmlParams, generate_dbml};
use dbd_core::entity::{IndexDef, TableDef};
use dbd_core::{Entity, EntityType};

/// Parse one DDL file exactly as a project scan does — identity from the path.
fn parse(path: &str, sql: &str) -> Entity {
    dbd_core::parser::parse_entity(Path::new(path), sql).unwrap_or_else(|e| panic!("{path} must parse: {e}\n{sql}"))
}

/// Document `entities` as DBML with no filters.
fn document(entities: &[Entity]) -> String {
    generate_dbml(&DbmlParams {
        entities,
        project_name: "roundtrip",
        database_type: "PostgreSQL",
        project_note: None,
        include_schemas: vec![],
        exclude_schemas: vec![],
        include_tables: vec![],
        exclude_tables: vec![],
        groups: vec![],
        auto_group_by_schema: false,
    })
    .content
}

/// What `init --from-dbml` hands back for a DBML document: every table and
/// enum, written out as the DDL the reverse engine emits and parsed again.
fn reverse(dbml: &str) -> Vec<Entity> {
    let parsed = dbd_core::dbml_parse::parse_dbml(dbml).unwrap_or_else(|e| panic!("DBML must parse: {e}\n{dbml}"));
    parsed
        .iter()
        .filter(|e| matches!(e.entity_type, EntityType::Table | EntityType::Enum))
        .map(|e| {
            let sql = dbd_core::emit::emit_entity(e).expect("tables and enums always emit");
            let schema = e.schema.as_deref().unwrap_or("public");
            let name = e.name.rsplit('.').next().unwrap_or(&e.name);
            let path = format!("ddl/{}/{schema}/{name}.ddl", e.entity_type.folder_name());
            parse(&path, &sql)
        })
        .collect()
}

/// The table named `name` in a reversed project, by its `TableDef`.
fn table<'a>(entities: &'a [Entity], name: &str) -> &'a TableDef {
    entities
        .iter()
        .find(|e| e.entity_type == EntityType::Table && e.name == name)
        .and_then(|e| e.table_def.as_ref())
        .unwrap_or_else(|| panic!("table {name} missing after the round trip"))
}

/// The index named `name` on a table, or a panic that shows what is there.
fn index<'a>(td: &'a TableDef, name: &str) -> &'a IndexDef {
    td.indexes
        .iter()
        .find(|ix| ix.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("index {name} missing; indexes are {:#?}", td.indexes))
}

fn key_names(ix: &IndexDef) -> Vec<&str> {
    ix.columns.iter().map(|c| c.name.as_str()).collect()
}

/// A table-level `UNIQUE` is a constraint the design states, and DBML has no
/// constraint syntax for it — only a unique index. Leaving it out of the
/// document meant the table `init --from-dbml` rebuilt accepted duplicate
/// order codes the original refused.
#[test]
fn a_table_level_unique_constraint_survives_dbml_and_back() {
    let orders = parse(
        "ddl/table/shop/orders.ddl",
        "create table shop.orders (\n\
           id uuid primary key,\n\
           customer_id uuid not null,\n\
           ref_code text not null,\n\
           constraint orders_ref_code_uq unique (ref_code),\n\
           constraint orders_customer_ref_uq unique (customer_id, ref_code)\n\
         );",
    );
    let dbml = document(&[orders]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.orders");

    let single = index(td, "orders_ref_code_uq");
    assert!(single.unique, "uniqueness is the whole point:\n{dbml}");
    assert_eq!(key_names(single), vec!["ref_code"]);

    let composite = index(td, "orders_customer_ref_uq");
    assert!(composite.unique, "uniqueness is the whole point:\n{dbml}");
    assert_eq!(key_names(composite), vec!["customer_id", "ref_code"]);
}
