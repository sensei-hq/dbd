//! `SchemaModel` carries every entity kind and the dependency graph (#24).
//!
//! # What v1 was
//!
//! Tables only. `build` filtered `entity_type == Table`, and `TableNode.kind`
//! was hardcoded `"table"` with a doc comment calling itself "an extension
//! point for view/function/procedure later". Views, matviews, functions and
//! procedures were absent, so anything rendering from this JSON could show an
//! ER diagram and nothing else.
//!
//! # Additive, deliberately
//!
//! `tables` and `refs` keep their exact v1 shape and contents. The new kinds
//! go in `entities`, and the dependency graph in `deps`, rather than being
//! folded into the existing arrays.
//!
//! Folding would have been tidier — one array, disambiguated by `kind` — and
//! it would have broken every consumer already reading `tables` as tables, at
//! the moment the viewer is being extracted into a shared package. Additive
//! costs a second array and invalidates nobody's work. `version` exists so the
//! next change does not have to be guessed at.
//!
//! # The data was already parsed
//!
//! `deps` is a projection of `Entity::refs` — `RefKind::{Reads, Writes, Calls,
//! Member}`, already resolved and deduplicated. No new parsing.

use dbd_core::Design;
use dbd_core::schema_model::build;
use std::path::Path;

fn project(dir: &Path) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: m2\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = dir.join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("orders.ddl"),
        "set search_path to app;\n\
         create table if not exists orders (\n  \
           id       integer primary key\n, \
           code     varchar(20) not null unique\n, \
           owner_id integer not null references app.people (id)\n\
         );\n\
         comment on table orders is 'Customer orders';\n\
         comment on column orders.code is 'Human-facing code';\n",
    )
    .unwrap();
    std::fs::write(
        t.join("people.ddl"),
        "set search_path to app;\ncreate table if not exists people (id integer primary key);",
    )
    .unwrap();
    let v = dir.join("ddl/view/app");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("recent.ddl"),
        "set search_path to app;\ncreate or replace view recent as select id, code from orders;",
    )
    .unwrap();
    let f = dir.join("ddl/function/app");
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(
        f.join("total.ddl"),
        "set search_path to app;\n\
         create or replace function total() returns bigint language sql as $$\n\
           select count(*) from orders;\n\
         $$;",
    )
    .unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

fn model(dir: &Path) -> dbd_core::schema_model::SchemaModel {
    build(&project(dir), None)
}

// ── v1 is untouched ─────────────────────────────────────────────────────────

/// The extraction session is working against the v1 shape. `tables` must keep
/// meaning tables, and `refs` must keep meaning foreign keys.
#[test]
fn tables_and_refs_keep_their_v1_meaning() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());

    let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["orders", "people"], "tables, and only tables");
    assert!(
        m.tables.iter().all(|t| t.kind == "table"),
        "a view must not appear here"
    );
    assert!(
        m.refs.iter().all(|r| !r.from.t.is_empty() && !r.to.t.is_empty()),
        "refs stays the FK edge list"
    );
}

/// Comments still reach the model — the entity description table is built
/// from them.
#[test]
fn table_and_column_comments_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    let orders = m.tables.iter().find(|t| t.name == "orders").expect("orders");
    assert_eq!(orders.note.as_deref(), Some("Customer orders"));
    let code = orders.columns.iter().find(|c| c.name == "code").expect("code");
    assert_eq!(code.note.as_deref(), Some("Human-facing code"));
}

// ── The version, so the next change is not a guess ──────────────────────────

#[test]
fn the_model_states_its_version() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(model(tmp.path()).version, 2);
}

#[test]
fn the_version_is_serialized() {
    let tmp = tempfile::tempdir().unwrap();
    let json = serde_json::to_string(&model(tmp.path())).unwrap();
    assert!(json.contains("\"version\":2"), "a consumer must be able to read it");
}

// ── Non-table entities ──────────────────────────────────────────────────────

#[test]
fn views_and_routines_appear_with_their_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    let kind_of = |n: &str| m.entities.iter().find(|e| e.name == n).map(|e| e.kind.clone());
    assert_eq!(kind_of("recent").as_deref(), Some("view"));
    assert_eq!(kind_of("total").as_deref(), Some("function"));
}

/// Tables are not duplicated into `entities` — a consumer walking both must
/// not see `orders` twice.
#[test]
fn tables_are_not_repeated_in_entities() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    assert!(
        !m.entities.iter().any(|e| e.kind == "table"),
        "tables live in `tables`: {:?}",
        m.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

// ── The dependency graph ────────────────────────────────────────────────────

/// The piece the ER diagram could never show: what reads and calls what.
#[test]
fn a_view_reading_a_table_is_a_dependency_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    assert!(
        m.deps
            .iter()
            .any(|d| d.from.n == "recent" && d.to.n == "orders" && d.kind == "reads"),
        "view -> table read edge missing: {:?}",
        m.deps
    );
}

#[test]
fn a_function_reading_a_table_is_a_dependency_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    assert!(
        m.deps.iter().any(|d| d.from.n == "total" && d.to.n == "orders"),
        "function -> table edge missing: {:?}",
        m.deps
    );
}

/// Dependency edges are not foreign keys and must not be mixed into `refs` —
/// a consumer drawing an ER diagram wants one, a call graph wants the other.
#[test]
fn dependency_edges_are_separate_from_foreign_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    assert!(!m.refs.is_empty(), "precondition: there is an FK");
    assert!(!m.deps.is_empty(), "precondition: there are deps");
    assert!(
        m.refs.iter().all(|r| r.from.c != *""),
        "an FK edge names columns; a dep edge does not"
    );
}

// ── Icons need these without re-deriving ────────────────────────────────────

/// `fk` and `uq` on the column, so a renderer does not have to scan `refs` and
/// the index list to decide which glyph to draw.
#[test]
fn a_column_says_whether_it_is_a_foreign_key_or_unique() {
    let tmp = tempfile::tempdir().unwrap();
    let m = model(tmp.path());
    let orders = m.tables.iter().find(|t| t.name == "orders").expect("orders");
    let col = |n: &str| orders.columns.iter().find(|c| c.name == n).expect(n);

    assert!(col("owner_id").fk, "owner_id references app.people");
    assert!(!col("id").fk, "id references nothing");
    assert!(col("code").uq, "code is declared unique");
    assert!(!col("owner_id").uq);
    assert!(col("id").pk, "and pk still works");
}
