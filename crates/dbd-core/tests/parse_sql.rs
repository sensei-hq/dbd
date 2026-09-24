//! `parse_sql` — the statement-level entry point for external embedders
//! (issue #19).
//!
//! `parse_entity` derives identity from the **path** (`ddl/<type>/<schema>/
//! <name>.ddl`), which is correct inside dbd's layout contract and wrong
//! outside it: an embedder with its own scanner got `EntityType::Table` named
//! after a directory, with no signal beyond `entity.errors` that the answer was
//! a fallback. Measured on a foreign corpus, `parse_entity` "succeeded" on
//! every file — reporting a stored procedure as `Table Users.sp_NewMCRIssue`.
//!
//! `parse_sql` asks the SQL what it declares instead. These tests pin that
//! contract: identity off the statement, one entity per declaration, and the
//! subordinate statements (indexes, comments) attached to the entity they
//! belong to rather than surfacing as entities of their own.

use dbd_core::entity::EntityType;
use dbd_core::parser::parse_sql;

fn one(sql: &str) -> dbd_core::Entity {
    let parsed = parse_sql(sql).expect("parse_sql must not fail on valid SQL");
    assert!(
        parsed.errors.is_empty(),
        "unexpected file-level errors: {:?}",
        parsed.errors
    );
    assert_eq!(
        parsed.entities.len(),
        1,
        "expected exactly one entity, got {:?}",
        parsed.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    parsed.entities.into_iter().next().unwrap()
}

// ── Identity from the statement ─────────────────────────────────────────────

#[test]
fn table_identity_comes_from_the_statement() {
    let e = one("create table app.users (id int primary key, name text not null);");
    assert_eq!(e.entity_type, EntityType::Table);
    assert_eq!(e.schema.as_deref(), Some("app"));
    assert_eq!(e.name, "app.users");
    assert!(e.table_def.is_some(), "a table must carry its structure");
}

/// The failure this whole ticket is about: a stored procedure read as a table
/// named after a path component.
#[test]
fn a_procedure_is_not_misread_as_a_table() {
    let e = one("create procedure app.sp_new_issue() language plpgsql as $$ begin end $$;");
    assert_eq!(e.entity_type, EntityType::Procedure);
    assert_eq!(e.name, "app.sp_new_issue");
}

#[test]
fn a_function_is_distinguished_from_a_procedure() {
    let e = one("create function app.f() returns int language sql as $$ select 1 $$;");
    assert_eq!(e.entity_type, EntityType::Function);
    assert_eq!(e.name, "app.f");
}

#[test]
fn a_matview_is_distinguished_from_a_view() {
    let view = one("create view app.v as select 1 as n;");
    assert_eq!(view.entity_type, EntityType::View);
    assert_eq!(view.name, "app.v");

    let mv = one("create materialized view app.mv as select 1 as n;");
    assert_eq!(
        mv.entity_type,
        EntityType::MaterializedView,
        "matview must not read as a view"
    );
    assert_eq!(mv.name, "app.mv");
}

#[test]
fn enum_sequence_and_role_are_identified() {
    let en = one("create type app.status as enum ('active', 'archived');");
    assert_eq!(en.entity_type, EntityType::Enum);
    assert_eq!(en.name, "app.status");
    let labels: Vec<&str> = en.enum_values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(labels, vec!["active", "archived"]);

    let seq = one("create sequence app.s start with 5;");
    assert_eq!(seq.entity_type, EntityType::Sequence);
    assert_eq!(seq.name, "app.s");

    // A role is not schema-qualified, so its name must stay bare.
    let role = one("create role app_ro;");
    assert_eq!(role.entity_type, EntityType::Role);
    assert_eq!(role.name, "app_ro");
    assert_eq!(role.schema, None, "a role has no schema");
}

/// An unqualified name is what Postgres resolves against `search_path`, so the
/// file's own `SET` decides — not `public`, and not a directory name.
#[test]
fn an_unqualified_name_is_qualified_by_the_search_path() {
    let e = one("set search_path to app;\ncreate table users (id int primary key);");
    assert_eq!(e.name, "app.users");
    assert_eq!(e.schema.as_deref(), Some("app"));
}

#[test]
fn an_unqualified_name_falls_back_to_public() {
    let e = one("create table users (id int primary key);");
    assert_eq!(e.name, "public.users");
}

// ── One entity per declaration ──────────────────────────────────────────────

#[test]
fn several_declarations_in_one_file_become_several_entities() {
    let parsed = parse_sql(
        "set search_path to app;\n\
         create table users (id int primary key);\n\
         create table orders (id int primary key, uid int references users(id));\n\
         create view recent as select * from orders;",
    )
    .unwrap();

    let names: Vec<&str> = parsed.entities.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["app.users", "app.orders", "app.recent"]);

    let types: Vec<EntityType> = parsed.entities.iter().map(|e| e.entity_type).collect();
    assert_eq!(types, vec![EntityType::Table, EntityType::Table, EntityType::View]);
}

/// An index and a comment are not entities — Postgres has no way to nest either
/// inside `CREATE TABLE`, so they arrive as siblings and must be folded into
/// the entity they name.
#[test]
fn indexes_and_comments_attach_to_their_table() {
    let parsed = parse_sql(
        "create table app.users (id int primary key, email text);\n\
         create unique index users_email_uidx on app.users (email);\n\
         comment on table app.users is 'people';",
    )
    .unwrap();

    assert_eq!(
        parsed.entities.len(),
        1,
        "index and comment must not become entities: {:?}",
        parsed.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    let td = parsed.entities[0].table_def.as_ref().expect("table_def");
    assert_eq!(td.indexes.len(), 1, "the index must land on its table");
    assert!(td.indexes[0].unique);
    assert_eq!(td.comments.table.as_deref(), Some("people"));
}

/// Statements are attributed to the table they name, not to whichever
/// declaration happens to precede them.
#[test]
fn an_index_lands_on_the_table_it_names_not_the_nearest_one() {
    let parsed = parse_sql(
        "create table app.a (id int primary key, x text);\n\
         create table app.b (id int primary key, y text);\n\
         create index a_x_idx on app.a (x);",
    )
    .unwrap();

    let a = parsed.entities.iter().find(|e| e.name == "app.a").expect("app.a");
    let b = parsed.entities.iter().find(|e| e.name == "app.b").expect("app.b");
    assert_eq!(a.table_def.as_ref().unwrap().indexes.len(), 1, "index belongs to app.a");
    assert_eq!(b.table_def.as_ref().unwrap().indexes.len(), 0, "app.b has no index");
}

// ── Relations ───────────────────────────────────────────────────────────────

#[test]
fn a_foreign_key_becomes_a_reference() {
    let parsed = parse_sql(
        "create table app.users (id int primary key);\n\
         create table app.orders (id int primary key, uid int references app.users(id));",
    )
    .unwrap();

    let orders = parsed.entities.iter().find(|e| e.name == "app.orders").unwrap();
    assert!(
        orders.refers.contains(&"app.users".to_string()),
        "FK must become an edge, got {:?}",
        orders.refers
    );
}

/// The read/write split is the thing an embedder cannot get from any other
/// language's indexer, so it has to survive this entry point.
#[test]
fn a_routine_keeps_its_reads_and_writes_separated() {
    let e = one("set search_path to app;\n\
         create procedure sync() language plpgsql as $$ begin \
         insert into target select * from source; end $$;");
    assert!(e.reads.iter().any(|r| r == "app.source"), "reads: {:?}", e.reads);
    assert!(e.writes.iter().any(|w| w == "app.target"), "writes: {:?}", e.writes);
}

// ── Failure is reported, not fabricated ─────────────────────────────────────

#[test]
fn unparseable_sql_reports_an_error_and_declares_nothing() {
    let parsed = parse_sql("THIS IS NOT SQL AT ALL ;;;").unwrap();
    assert!(parsed.entities.is_empty(), "nothing was declared");
    assert!(!parsed.errors.is_empty(), "a parse failure must be reported");
}

/// Valid SQL that declares no entity is not an error — it is an empty answer.
/// An embedder scanning a repo hits these constantly (migrations that only
/// `INSERT`, `GRANT` scripts, plain `SELECT`s).
#[test]
fn sql_that_declares_nothing_is_empty_not_an_error() {
    let parsed = parse_sql("insert into app.t (id) values (1);").unwrap();
    assert!(parsed.entities.is_empty());
    assert!(parsed.errors.is_empty(), "got: {:?}", parsed.errors);
}

#[test]
fn the_files_search_path_is_reported() {
    let parsed = parse_sql("set search_path to app, shared;\ncreate table t (id int);").unwrap();
    assert_eq!(parsed.search_paths, vec!["app".to_string(), "shared".to_string()]);
}
