//! A call reaches only something callable (#112).
//!
//! `default now()` in a table and a view named `now` in the same schema: the
//! call used to resolve to the view, so the table depended on a view that reads
//! it and the view on itself — a cycle that refused every apply.

use dbd_core::entity::{Entity, EntityType, Ref, RefKind};
use dbd_core::parser::parse_sql;
use dbd_core::references::resolve_references;

fn one(sql: &str) -> Entity {
    parse_sql(sql)
        .expect("reads")
        .entities
        .into_iter()
        .next()
        .expect("an entity")
}

fn refers(e: &Entity) -> Vec<&str> {
    e.refers().collect()
}

#[test]
fn a_builtin_call_does_not_resolve_to_a_view_of_the_same_name() {
    let mut entities = vec![
        one("set search_path to app;\n\
             create table if not exists events (\n\
               id int primary key,\n\
               created_at timestamptz not null default now()\n\
             );"),
        one("set search_path to app;\n\
             create or replace view now as\n\
               select id from events where created_at <= now();"),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert!(
        !refers(&entities[0]).contains(&"app.now"),
        "table: {:?}",
        refers(&entities[0])
    );
    assert_eq!(refers(&entities[1]), vec!["app.events"], "view");
}

#[test]
fn a_call_still_resolves_to_a_function_of_that_name() {
    let mut entities = vec![
        one("set search_path to app;\n\
             create function app.stamp() returns timestamptz language sql as $$ select now() $$;"),
        one("set search_path to app;\n\
             create table if not exists events (\n\
               id int primary key,\n\
               created_at timestamptz not null default stamp()\n\
             );"),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert!(
        refers(&entities[1]).contains(&"app.stamp"),
        "{:?}",
        refers(&entities[1])
    );
}

/// Every non-callable kind a call could collide with.
#[test]
fn a_call_never_resolves_to_a_relation_or_a_type() {
    for kind in [
        EntityType::Table,
        EntityType::View,
        EntityType::MaterializedView,
        EntityType::Sequence,
        EntityType::Enum,
    ] {
        let mut caller = Entity::new(EntityType::Table, "app.caller");
        caller.refs = vec![Ref::stated("app.thing", RefKind::Calls)];
        let mut entities = vec![Entity::new(kind, "app.thing"), caller];
        resolve_references(&mut entities, &[], &[]);

        assert!(refers(&entities[1]).is_empty(), "{kind:?}: {:?}", refers(&entities[1]));
    }
}

/// A non-call reference to the same view keeps resolving.
#[test]
fn a_read_of_a_view_still_resolves() {
    let mut reader = Entity::new(EntityType::View, "app.reader");
    reader.refs = vec![Ref::stated("app.now", RefKind::Reads)];
    let mut entities = vec![Entity::new(EntityType::View, "app.now"), reader];
    resolve_references(&mut entities, &[], &[]);

    assert_eq!(refers(&entities[1]), vec!["app.now"]);
}
