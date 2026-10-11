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
use dbd_core::entity::{IdentityKind, IndexDef, SortOrder, TableConstraint, TableDef};
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

/// The table-level UNIQUE constraints of `td`, as `(name, columns,
/// nulls_not_distinct)`.
fn uniques(td: &TableDef) -> Vec<(Option<&str>, Vec<&str>, bool)> {
    td.constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::Unique {
                name,
                columns,
                nulls_not_distinct,
            } => Some((
                name.as_deref(),
                columns.iter().map(String::as_str).collect(),
                *nulls_not_distinct,
            )),
            _ => None,
        })
        .collect()
}

/// A table-level `UNIQUE` is a constraint the design states, and DBML has no
/// constraint syntax for it — only a unique index. Leaving it out of the
/// document meant the table `init --from-dbml` rebuilt accepted duplicate
/// order codes the original refused.
///
/// Read back as a unique *index*, it enforced the same thing and still read
/// as drift: `diff` against the database the original built asked to drop the
/// constraint and create the index. The document marks it as a constraint (in
/// the index note, DBML's one free-text slot), so it comes back as one, under
/// its own name.
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

    assert_eq!(
        uniques(td),
        vec![
            (Some("orders_ref_code_uq"), vec!["ref_code"], false),
            (Some("orders_customer_ref_uq"), vec!["customer_id", "ref_code"], false),
        ],
        "both come back as the constraints they were:\n{dbml}"
    );
    assert!(
        td.indexes.is_empty(),
        "and not as unique indexes beside them: {:#?}\n{dbml}",
        td.indexes
    );
}

/// An unnamed UNIQUE comes back unnamed, so Postgres names it the way it named
/// the original (`t_r_s_key`) — the name reconcile's `ADD UNIQUE` gives it too
/// — rather than the `t_idx` the unique index was given. `NULLS NOT DISTINCT`
/// rides along. A unique index the design declared as an index stays an index.
#[test]
fn an_unnamed_unique_constraint_comes_back_unnamed_and_a_unique_index_stays_an_index() {
    let t = parse(
        "ddl/table/app/t.ddl",
        "create table app.t (id int primary key, r text, s text, u text, unique (r, s), \
         unique nulls not distinct (u));\n\
         create unique index t_lower_r_idx on app.t (r) where s is not null;",
    );
    let dbml = document(&[t]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "app.t");

    assert_eq!(
        uniques(td),
        vec![(None, vec!["r", "s"], false), (None, vec!["u"], true)],
        "{dbml}"
    );
    let ix = index(td, "t_lower_r_idx");
    assert!(
        ix.unique && ix.predicate.is_some(),
        "the unique index stays one: {ix:#?}\n{dbml}"
    );
    assert_eq!(td.indexes.len(), 1, "{:#?}", td.indexes);
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
    assert!(
        uniques(td).contains(&(Some("orders_ref_code_uq"), vec!["ref_code"], true)),
        "the constraint comes back as one, NULLS NOT DISTINCT kept:\n{dbml}"
    );
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

fn checks(td: &TableDef) -> Vec<&str> {
    let mut expressions: Vec<&str> = td
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::Check { expression, .. } => Some(expression.as_str()),
            _ => None,
        })
        .collect();
    expressions.sort_unstable();
    expressions
}

/// DBML has carried CHECK constraints since @dbml/core v5 (`checks { … }`),
/// and dbd wrote none: every CHECK the design enforces was missing from the
/// document and from the table `init --from-dbml` rebuilt.
#[test]
fn a_check_constraint_survives_dbml_and_back() {
    let orders = parse(
        "ddl/table/shop/orders.ddl",
        "create table shop.orders (\n\
           id uuid primary key,\n\
           qty integer check (qty > 0),\n\
           total_cents integer,\n\
           status text,\n\
           constraint orders_total_positive check (total_cents >= 0),\n\
           constraint orders_status_known check (status in ('open', 'paid'))\n\
         );",
    );
    let original = orders.table_def.clone().unwrap();
    let dbml = document(&[orders]);
    let reversed = reverse(&dbml);

    assert_eq!(checks(&original).len(), 3, "the test needs all three checks parsed");
    assert_eq!(checks(table(&reversed, "shop.orders")), checks(&original), "\n{dbml}");
}

/// DBML has no generated-column syntax, so `total_eur` came back from
/// `init --from-dbml` as a plain writable column: the computed value was gone
/// and nothing stopped a write from contradicting `total_cents`.
#[test]
fn a_generated_column_survives_dbml_and_back() {
    let orders = parse(
        "ddl/table/shop/orders.ddl",
        "create table shop.orders (\n\
           id uuid primary key,\n\
           total_cents integer not null,\n\
           total_eur numeric generated always as (total_cents / 100.0) stored,\n\
           label text generated always as (coalesce(note, 'n/a')) stored,\n\
           note text\n\
         );",
    );
    let original = orders.table_def.clone().unwrap();
    let dbml = document(&[orders]);
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.orders");
    let generated = |td: &TableDef, name: &str| td.columns.iter().find(|c| c.name == name).unwrap().generated.clone();

    for name in ["total_eur", "label"] {
        assert!(
            generated(&original, name).is_some(),
            "{name} must be generated for this test to mean anything"
        );
        assert_eq!(generated(td, name), generated(&original, name), "{name}:\n{dbml}");
    }
}

/// `[increment]` cannot say ALWAYS, so an ALWAYS identity came back BY
/// DEFAULT — a column that silently started accepting hand-written ids.
#[test]
fn an_always_identity_stays_always_through_dbml_and_back() {
    let customers = parse(
        "ddl/table/shop/customers.ddl",
        "create table shop.customers (id bigint generated always as identity primary key, email text);",
    );
    let original = customers.table_def.clone().unwrap();
    let dbml = document(&[customers]);
    let reversed = reverse(&dbml);

    assert_eq!(original.columns[0].identity, Some(IdentityKind::Always));
    assert_eq!(
        table(&reversed, "shop.customers").columns[0].identity,
        Some(IdentityKind::Always),
        "\n{dbml}"
    );
}

/// A default that is one of SQL's value keywords came back from
/// `init --from-dbml` as a string literal: `DEFAULT 'current_date'`, which
/// Postgres refuses on a date column, and `DEFAULT 'current_user'` — the text,
/// not the user — on a text column. Every keyword default reads back as itself.
#[test]
fn a_keyword_default_survives_dbml_and_back_as_the_keyword() {
    let columns = [
        ("a", "date", "current_date"),
        ("b", "time with time zone", "current_time"),
        ("c", "timestamptz", "current_timestamp"),
        ("d", "time", "localtime"),
        ("e", "timestamp", "localtimestamp"),
        ("f", "text", "current_user"),
        ("g", "text", "session_user"),
        ("h", "text", "user"),
        ("i", "text", "current_role"),
        ("j", "text", "current_catalog"),
        ("k", "text", "current_schema"),
        ("l", "text", "'current_user'"),
        ("m", "text", "'open'"),
    ];
    let body: Vec<String> = columns
        .iter()
        .map(|(name, ty, default)| format!("{name} {ty} default {default}"))
        .collect();
    let original = parse(
        "ddl/table/app/t.ddl",
        &format!("create table app.t (id int primary key, {});", body.join(", ")),
    );
    let dbml = document(std::slice::from_ref(&original));
    let reversed = reverse(&dbml);
    let td = table(&reversed, "app.t");
    let before = original.table_def.as_ref().unwrap();
    for (name, _, _) in columns {
        let default = |td: &TableDef| {
            td.columns
                .iter()
                .find(|c| c.name == name)
                .and_then(|c| c.default_value.clone())
        };
        assert_eq!(default(td), default(before), "`{name}`'s default:\n{dbml}");
    }
}

/// The foreign keys of `td`, as `(name, columns)`.
fn foreign_keys(td: &TableDef) -> Vec<(Option<&str>, Vec<&str>)> {
    td.constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::ForeignKey(fk) => {
                Some((fk.name.as_deref(), fk.columns.iter().map(String::as_str).collect()))
            }
            _ => None,
        })
        .collect()
}

/// The primary key of `td`, as `(name, columns)`.
fn primary_key(td: &TableDef) -> Option<(Option<&str>, Vec<&str>)> {
    td.constraints.iter().find_map(|c| match c {
        TableConstraint::PrimaryKey { name, columns } => {
            Some((name.as_deref(), columns.iter().map(String::as_str).collect()))
        }
        _ => None,
    })
}

/// The DBML writer dropped an index's access method, its `INCLUDE` columns,
/// its operator classes and storage parameters: `init --from-dbml` rebuilt a
/// GIN index as a btree, a covering index without its payload. DBML's own
/// `type:` setting carries the method; the rest goes in the index note, as a
/// partial index's predicate does, and all of it comes back.
#[test]
fn an_index_keeps_its_method_include_opclass_and_storage_through_dbml_and_back() {
    let docs = parse(
        "ddl/table/shop/docs.ddl",
        "create table shop.docs (\n\
           id uuid primary key,\n\
           name text,\n\
           body jsonb,\n\
           owner_id uuid,\n\
           stamp timestamptz\n\
         );\n\
         create index docs_body_idx on shop.docs using gin (body);\n\
         create index docs_owner_cover_idx on shop.docs (owner_id) include (name, stamp);\n\
         create index docs_name_pattern_idx on shop.docs (name text_pattern_ops, owner_id) with (fillfactor = 70);",
    );
    let original = docs.table_def.clone().unwrap();
    let dbml = document(&[docs]);
    assert!(
        dbml.contains("type: gin"),
        "DBML's own setting carries the method:\n{dbml}"
    );
    let reversed = reverse(&dbml);
    let td = table(&reversed, "shop.docs");
    for name in ["docs_body_idx", "docs_owner_cover_idx", "docs_name_pattern_idx"] {
        assert_eq!(index(td, name), index(&original, name), "{name}:\n{dbml}");
    }
    let gin = index(&original, "docs_body_idx");
    assert_eq!(
        gin.index_type,
        Some(dbd_core::entity::IndexType::Gin),
        "the test means something"
    );
}

/// A primary key's and a foreign key's own names were lost: DBML has no
/// constraint syntax for them, and the writer wrote neither. A named key is
/// a `pk` index entry with `name:`, a named foreign key a named `Ref` — and a
/// name Postgres would give the key anyway is left out, so the column keeps
/// its `[pk]` and the document stays as it was.
#[test]
fn primary_and_foreign_key_names_survive_dbml_and_back() {
    let owners = parse(
        "ddl/table/shop/owners.ddl",
        "create table shop.owners (id uuid, region text, constraint owners_key primary key (id, region));",
    );
    let pets = parse(
        "ddl/table/shop/pets.ddl",
        "create table shop.pets (\n\
           id uuid,\n\
           owner_id uuid,\n\
           region text,\n\
           vet_id uuid,\n\
           constraint pets_pkey primary key (id),\n\
           constraint pets_owner_fk foreign key (owner_id, region) references shop.owners (id, region),\n\
           constraint pets_vet_id_fkey foreign key (vet_id) references shop.vets (id)\n\
         );",
    );
    let vets = parse(
        "ddl/table/shop/vets.ddl",
        "create table shop.vets (id uuid primary key);",
    );
    let dbml = document(&[owners, pets, vets]);
    assert!(
        dbml.contains("[pk, name: 'owners_key']"),
        "a named key is a named pk entry:\n{dbml}"
    );
    assert!(
        !dbml.contains("pets_pkey") && !dbml.contains("pets_vet_id_fkey"),
        "default names are left out:\n{dbml}"
    );
    let reversed = reverse(&dbml);

    assert_eq!(
        primary_key(table(&reversed, "shop.owners")),
        Some((Some("owners_key"), vec!["id", "region"])),
        "\n{dbml}"
    );
    let pets = table(&reversed, "shop.pets");
    assert_eq!(primary_key(pets), Some((None, vec!["id"])), "\n{dbml}");
    let mut fks = foreign_keys(pets);
    fks.sort();
    assert_eq!(
        fks,
        vec![
            (None, vec!["vet_id"]),
            (Some("pets_owner_fk"), vec!["owner_id", "region"])
        ],
        "\n{dbml}"
    );
}
