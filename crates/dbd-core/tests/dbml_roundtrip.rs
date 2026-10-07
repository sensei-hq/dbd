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
use dbd_core::entity::{IdentityKind, IndexDef, SortOrder, TableDef};
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

fn keys(ix: &IndexDef) -> Vec<(&str, bool)> {
    ix.columns.iter().map(|c| (c.name.as_str(), c.is_expression)).collect()
}

/// An expression key written bare (`lower(email)`) reads back as a column
/// literally named `lower(email)`, so `init --from-dbml` wrote
/// `("lower(email)")` and apply failed. DBML spells an expression in backticks.
#[test]
fn an_expression_index_survives_dbml_and_back() {
    let customers = parse(
        "ddl/table/shop/customers.ddl",
        "create table shop.customers (id bigint primary key, tenant_id bigint, email text, ctx jsonb);\n\
         create index customers_email_lower_idx on shop.customers (lower(email));\n\
         create index customers_tenant_email_idx on shop.customers (tenant_id, lower(email));\n\
         create index customers_module_idx on shop.customers ((ctx ->> 'module'), coalesce(email, 'x, y'));",
    );
    let original = customers.table_def.clone().unwrap();
    let dbml = document(&[customers]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.customers");

    for name in [
        "customers_email_lower_idx",
        "customers_tenant_email_idx",
        "customers_module_idx",
    ] {
        let before = keys(index(&original, name));
        assert!(
            before.iter().any(|(_, is_expression)| *is_expression),
            "{name} must have an expression key for this test to mean anything: {before:?}"
        );
        assert_eq!(keys(index(td, name)), before, "{name}:\n{dbml}");
    }
}

/// DBML has no partial-index syntax, so the `WHERE` was dropped — and a
/// partial UNIQUE index came back as a plain one. `one open order per
/// customer` became `one order per customer, ever`.
#[test]
fn a_partial_index_keeps_its_predicate_through_dbml_and_back() {
    let orders = parse(
        "ddl/table/shop/orders.ddl",
        r"create table shop.orders (id uuid primary key, customer_id bigint, status text, ref_code text);
          create unique index orders_one_open_per_customer on shop.orders (customer_id) where status = 'open';
          create index orders_numeric_ref_idx on shop.orders (ref_code) where ref_code ~ '^\d+$';",
    );
    let original = orders.table_def.clone().unwrap();
    let dbml = document(&[orders]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.orders");

    for name in ["orders_one_open_per_customer", "orders_numeric_ref_idx"] {
        let before = index(&original, name);
        assert!(
            before.predicate.is_some(),
            "{name} must be partial for this test to mean anything"
        );
        let after = index(td, name);
        assert_eq!(after.predicate, before.predicate, "{name}:\n{dbml}");
        assert_eq!(after.unique, before.unique, "{name}:\n{dbml}");
    }
}

/// DBML's index keys carry no sort order and DBML has no `NULLS NOT
/// DISTINCT`; both change what the rebuilt index is, and the second changes
/// what a unique one accepts.
#[test]
fn an_index_keeps_its_key_order_and_null_handling_through_dbml_and_back() {
    let orders = parse(
        "ddl/table/shop/orders.ddl",
        "create table shop.orders (\n\
           id uuid primary key,\n\
           customer_id bigint,\n\
           created_at timestamptz,\n\
           ref_code text,\n\
           ext_ref text,\n\
           constraint orders_ref_code_uq unique nulls not distinct (ref_code)\n\
         );\n\
         create index orders_recent_idx on shop.orders (customer_id, created_at desc nulls last);\n\
         create index orders_oldest_idx on shop.orders (created_at nulls first);\n\
         create unique index orders_ext_ref_uq on shop.orders (ext_ref) nulls not distinct;",
    );
    let original = orders.table_def.clone().unwrap();
    let dbml = document(&[orders]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.orders");

    let order = |ix: &IndexDef| -> Vec<(Option<SortOrder>, Option<bool>)> {
        ix.columns.iter().map(|c| (c.order, c.nulls_first)).collect()
    };
    for name in ["orders_recent_idx", "orders_oldest_idx"] {
        let before = index(&original, name);
        assert!(
            before
                .columns
                .iter()
                .any(|c| c.order.is_some() || c.nulls_first.is_some()),
            "{name} must order a key for this test to mean anything"
        );
        assert_eq!(order(index(td, name)), order(before), "{name}:\n{dbml}");
    }

    assert!(index(td, "orders_ext_ref_uq").nulls_not_distinct, "\n{dbml}");
    let constraint = index(td, "orders_ref_code_uq");
    assert!(constraint.unique && constraint.nulls_not_distinct, "\n{dbml}");
}

/// `[unique]` and `[increment]` were read and then lost: the parser threw
/// `increment` away for any type but a serial one, and the reverse emitter
/// never wrote a column's own UNIQUE. The rebuilt table accepted duplicate
/// emails and wanted every id supplied by hand.
#[test]
fn a_column_keeps_its_unique_and_identity_through_dbml_and_back() {
    let customers = parse(
        "ddl/table/shop/customers.ddl",
        "create table shop.customers (\n\
           id bigint generated by default as identity primary key,\n\
           email text not null unique,\n\
           code text unique\n\
         );",
    );
    let original = customers.table_def.clone().unwrap();
    let dbml = document(&[customers]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.customers");
    let column = |td: &TableDef, name: &str| td.columns.iter().find(|c| c.name == name).cloned().unwrap();

    assert_eq!(column(&original, "id").identity, Some(IdentityKind::ByDefault));
    assert_eq!(column(td, "id").identity, Some(IdentityKind::ByDefault), "\n{dbml}");
    for name in ["email", "code"] {
        assert!(column(&original, name).is_unique);
        assert!(column(td, name).is_unique, "{name}:\n{dbml}");
    }
}
