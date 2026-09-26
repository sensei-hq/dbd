//! One list of references, and a body that is not one of them.
//!
//! # What this replaces
//!
//! `Entity` carried four parallel fields — `refers`, `references`, `reads`,
//! `writes` — holding overlapping views of the same facts:
//!
//! - `refers` was every name in `references`, rebuilt by hand at each
//!   producer (`references.iter().map(|r| r.name)`).
//! - `references` knew *provenance* (`schema_source`) but not *direction*:
//!   `ref_type` was `None` for reads and writes alike, `Some("function")` for
//!   calls, and `Some("table")` at two sites that nothing ever read.
//! - `reads`/`writes` knew direction but not provenance.
//!
//! So "which of my reads has a guessed schema?" meant joining two lists by
//! name — and the name is not a key: a routine that both reads and writes one
//! table produced `refers: ["app.audit", "app.audit"]` and two identical
//! `references` entries.
//!
//! `Ref { name, kind, schema_source }` carries all three facts on one row.
//!
//! # The overload this had to untangle first
//!
//! `writes` meant two unrelated things depending on who filled it. The
//! **parser** put table names there for a routine. The **introspector** put
//! the entity's own DDL body there — a view's `SELECT`, a sequence's `CREATE`,
//! one string per routine overload — and `emit_view`/`emit_sequence`/
//! `emit_routine` read `writes[0]` back out as that body.
//!
//! No live path mixed them: `emit_entity` is reached only from `reverse`
//! (introspected entities) and `matview_create_sql` (matviews, whose parser
//! deliberately followed the introspector's convention). It was a trap, not a
//! bug. `Entity::body` now holds the verbatim DDL and `refs` holds references,
//! so the two cannot be confused.

use dbd_core::entity::{Entity, EntityType, Ref, RefKind, SchemaSource};
use dbd_core::parser::{Dialect, parse_sql, parse_sql_as};

fn one(sql: &str) -> Entity {
    parse_sql(sql)
        .expect("reads")
        .entities
        .into_iter()
        .next()
        .expect("an entity")
}

// ── Direction and provenance on the same row ────────────────────────────────

#[test]
fn a_routine_reports_each_reference_with_its_direction_and_provenance() {
    let e = one("set search_path to app, shared;\n\
         create function app.sync() returns void language sql as $$\n\
           insert into audit select * from shared.events;\n\
           select app.normalise(x) from lookup;\n\
         $$;");

    let find = |name: &str| {
        e.refs
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", e.refs))
    };

    // `shared.events` — written by the source, read.
    let events = find("shared.events");
    assert_eq!(events.kind, RefKind::Reads);
    assert_eq!(events.schema_source, SchemaSource::Stated);

    // `lookup` — bare, so the schema is dbd's guess, and still a read.
    let lookup = find("app.lookup");
    assert_eq!(lookup.kind, RefKind::Reads);
    assert_eq!(lookup.schema_source, SchemaSource::Inferred);

    // `audit` — bare and written.
    let audit = find("app.audit");
    assert_eq!(audit.kind, RefKind::Writes);
    assert_eq!(audit.schema_source, SchemaSource::Inferred);

    // `app.normalise` — a call, which is a SOFT reference.
    assert_eq!(find("app.normalise").kind, RefKind::Calls);
}

/// The question the four-field shape could not answer without a join.
#[test]
fn the_guessed_reads_can_be_asked_for_directly() {
    let e = one("set search_path to app;\n\
         create function app.f() returns void language sql as $$\n\
           select * from other.stated join bare on true;\n\
         $$;");
    let guessed: Vec<&str> = e
        .reads()
        .filter(|r| r.schema_source.is_guess())
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(guessed, vec!["app.bare"]);
}

// ── A name is not a key ─────────────────────────────────────────────────────

/// Both directions of the same table are two rows, distinguishable by kind —
/// where before they were two identical `references` entries and a duplicated
/// `refers` string.
#[test]
fn a_table_both_read_and_written_is_two_rows_not_two_duplicates() {
    let e = one("set search_path to app;\n\
         create function app.roll() returns void language sql as $$\n\
           insert into audit select * from audit;\n\
         $$;");
    let mut kinds: Vec<RefKind> = e
        .refs
        .iter()
        .filter(|r| r.name == "app.audit")
        .map(|r| r.kind)
        .collect();
    kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(kinds, vec![RefKind::Reads, RefKind::Writes]);

    // And the name appears once in the deduplicated name view.
    assert_eq!(e.refers().filter(|n| *n == "app.audit").count(), 1);
}

#[test]
fn the_same_reference_twice_in_one_body_is_recorded_once() {
    let e = one("set search_path to app;\n\
         create view app.v as select * from t where id in (select id from t);");
    assert_eq!(e.refs.iter().filter(|r| r.name == "app.t").count(), 1);
}

// ── Accessors ───────────────────────────────────────────────────────────────

#[test]
fn the_accessors_partition_by_kind() {
    let e = one("set search_path to app;\n\
         create function app.f() returns void language sql as $$\n\
           insert into w select * from r;\n\
           select app.c(1);\n\
         $$;");
    assert_eq!(e.reads().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["app.r"]);
    assert_eq!(e.writes().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["app.w"]);
    assert_eq!(e.calls().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["app.c"]);
}

/// `refers()` is every name, deduplicated, in first-seen order — what a
/// dependency graph wants.
#[test]
fn refers_is_every_name_once() {
    let e = one("set search_path to app;\n\
         create function app.f() returns void language sql as $$\n\
           insert into w select * from r;\n\
         $$;");
    let names: Vec<&str> = e.refers().collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"app.r") && names.contains(&"app.w"));
}

// ── The body is not a reference ─────────────────────────────────────────────

/// The overload, untangled. A matview's own `SELECT` is its body, and must not
/// appear among the tables it refers to.
#[test]
fn a_matview_body_is_body_not_a_reference() {
    let e = one("set search_path to app;\ncreate materialized view app.m as select * from app.src;");
    assert!(
        e.body.first().is_some_and(|b| b.contains("src")),
        "the body text is kept: {:?}",
        e.body
    );
    assert!(
        e.refs.iter().all(|r| !r.name.contains("select")),
        "no reference is a SQL fragment: {:?}",
        e.refs
    );
    assert!(
        e.refs.iter().any(|r| r.name == "app.src" && r.kind == RefKind::Reads),
        "and the table it reads is still a reference: {:?}",
        e.refs
    );
}

/// An entity built by hand with a body emits that body, and one built with
/// references does not confuse them.
#[test]
fn emitting_uses_the_body_and_never_a_reference() {
    let mut e = Entity::new(EntityType::View, "app.v");
    e.schema = Some("app".into());
    e.body = vec!["SELECT 1 AS x".into()];
    e.refs.push(Ref::stated("app.other", RefKind::Reads));
    let sql = dbd_core::emit::emit_entity(&e).expect("emits");
    assert!(sql.contains("SELECT 1 AS x"), "{sql}");
    assert!(!sql.contains("app.other"), "a reference leaked into the DDL: {sql}");
}

// ── The statement-head dialects use the same shape ──────────────────────────

#[test]
fn tsql_reports_the_same_three_facts() {
    let p = parse_sql_as(
        Dialect::TSql,
        "CREATE PROCEDURE dbo.sync AS\n\
         BEGIN\n\
           INSERT INTO dbo.Target SELECT * FROM dbo.Source;\n\
           EXEC dbo.Helper;\n\
         END",
    )
    .unwrap();
    let e = &p.entities[0];
    let kind = |n: &str| e.refs.iter().find(|r| r.name == n).map(|r| r.kind);
    assert_eq!(kind("dbo.Source"), Some(RefKind::Reads));
    assert_eq!(kind("dbo.Target"), Some(RefKind::Writes));
    assert_eq!(kind("dbo.Helper"), Some(RefKind::Calls));
    // T-SQL never invents a schema, so nothing here is a guess.
    assert!(e.refs.iter().all(|r| r.schema_source == SchemaSource::Stated));
}

// ── A role membership is its own kind ───────────────────────────────────────

/// A granted role is a hard dependency, but it is not a read — calling it one
/// would put it in `reads()` for every caller walking data flow.
#[test]
fn a_role_membership_is_not_a_read() {
    // Roles are read by the path-based entry point — `ddl/role/<name>.ddl` —
    // because a role has no schema to take identity from.
    //
    // `GRANT … TO …` is the form the reader knows. `CREATE ROLE … IN ROLE …`
    // expresses the same membership and is NOT read: a pre-existing gap, noted
    // here because this test is where someone would look for it.
    let e = dbd_core::parser::parse_entity(
        std::path::Path::new("ddl/role/advanced.ddl"),
        "create role advanced;\ngrant basic to advanced;",
    )
    .expect("reads");
    assert_eq!(e.entity_type, EntityType::Role);
    assert!(
        e.refs.iter().any(|r| r.name == "basic" && r.kind == RefKind::Member),
        "{:?}",
        e.refs
    );
    assert_eq!(e.reads().count(), 0);
}

// ── The building blocks hold their own contracts ────────────────────────────

/// `push_ref` deduplicates on **(name, kind)** — not on name alone, or a table
/// both read and written would collapse to one row and lose a direction.
///
/// Tested directly because every producer today deduplicates upstream, so a
/// regression here would be invisible through the parsers. It is public API.
#[test]
fn push_ref_deduplicates_on_name_and_kind_together() {
    let mut e = Entity::new(EntityType::Procedure, "app.p");

    e.push_ref(Ref::stated("app.t", RefKind::Reads));
    e.push_ref(Ref::stated("app.t", RefKind::Reads));
    assert_eq!(e.refs.len(), 1, "the same name and kind twice is one row");

    e.push_ref(Ref::stated("app.t", RefKind::Writes));
    assert_eq!(e.refs.len(), 2, "but the other direction is its own row");
    assert_eq!(e.refers().count(), 1, "and still one edge");
}

/// `set_refs_of` replaces only the kinds it names.
#[test]
fn set_refs_of_leaves_the_kinds_it_does_not_name() {
    let mut e = Entity::new(EntityType::Procedure, "app.p");
    e.push_ref(Ref::stated("app.r", RefKind::Reads));
    e.push_ref(Ref::stated("app.c", RefKind::Calls));

    e.set_refs_of(&[RefKind::Reads], vec![Ref::stated("app.r2", RefKind::Reads)]);

    assert_eq!(e.reads().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["app.r2"]);
    assert_eq!(
        e.calls().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["app.c"],
        "calls were not named, so they stand"
    );
}

/// An unresolved reference stays in `reads()` — it is the file's own account,
/// and the import plan matches a staging table against it — while `refers()`
/// omits it so the topological sort never waits on something absent.
#[test]
fn an_unresolved_reference_is_marked_not_deleted() {
    let mut entities = vec![one(
        "set search_path to app;\ncreate view app.v as select * from nowhere;",
    )];
    dbd_core::references::resolve_references(&mut entities, &[], &[]);
    let e = &entities[0];

    assert_eq!(
        e.reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["app.nowhere"],
        "the file said it reads this, and it still says so"
    );
    assert!(e.reads().all(|r| r.unresolved), "but marked: {:?}", e.refs);
    assert_eq!(e.refers().count(), 0, "so no edge is offered to the graph");
    assert!(e.warnings.iter().any(|w| w.contains("nowhere")), "{:?}", e.warnings);
}
