//! Whether a reference's schema was written in the source or guessed by dbd.
//!
//! # Why this exists
//!
//! The PostgreSQL reader qualifies every bare name with the first entry on the
//! entity's `search_path` — `REFERENCES parent` under `search_path = app`
//! becomes `app.parent`. That guess then looks exactly like a source that
//! wrote `app.parent`, and nothing recorded which it was.
//!
//! Two consequences, and the second is a defect rather than an inconvenience:
//!
//! 1. A consumer parsing one file at a time cannot tell a confident edge from a
//!    guessed one. `resolve_references` corrects the guess, but it needs the
//!    whole entity set, so a per-file caller cannot run it (issue #22).
//!
//! 2. `recover_bare_target` used "the schema equals `default_schema`" as a
//!    *proxy* for "the parser guessed this" — its own comment called it "the
//!    parser's bare-qualification marker". A source that deliberately writes
//!    `app.parent` while its `search_path` is `app` matches that proxy, so an
//!    explicit qualification could be silently re-pointed at another schema.
//!
//! Recording the provenance answers both: the per-file caller gets the truth,
//! and the resolver tests the fact instead of a proxy for it.

use dbd_core::entity::{SchemaSource, TableConstraint};
use dbd_core::parser::parse_sql;

fn one(sql: &str) -> dbd_core::entity::Entity {
    let p = parse_sql(sql).expect("reads");
    assert_eq!(p.entities.len(), 1, "expected one entity: {:?}", p.errors);
    p.entities.into_iter().next().unwrap()
}

fn only_fk(e: &dbd_core::entity::Entity) -> dbd_core::entity::ForeignKey {
    let td = e.table_def.as_ref().expect("a table");
    if let Some(fk) = td.columns.iter().find_map(|c| c.inline_fk.clone()) {
        return fk;
    }
    td.constraints
        .iter()
        .find_map(|c| match c {
            TableConstraint::ForeignKey(fk) => Some(fk.clone()),
            _ => None,
        })
        .expect("a foreign key")
}

// ── A foreign key says where its schema came from ───────────────────────────

#[test]
fn a_written_target_schema_is_stated() {
    let e = one("set search_path to app;\ncreate table t (pid uuid references other.parent (id));");
    let fk = only_fk(&e);
    assert_eq!(fk.ref_schema.as_deref(), Some("other"));
    assert_eq!(fk.ref_schema_source, SchemaSource::Stated);
}

#[test]
fn a_bare_target_schema_is_inferred() {
    let e = one("set search_path to app;\ncreate table t (pid uuid references parent (id));");
    let fk = only_fk(&e);
    assert_eq!(fk.ref_schema.as_deref(), Some("app"), "the guess is still made");
    assert_eq!(
        fk.ref_schema_source,
        SchemaSource::Inferred,
        "but it is now marked as dbd's, not the source's"
    );
}

/// The case the proxy could not see: written, and identical to what a guess
/// would have produced.
#[test]
fn a_written_schema_that_matches_the_default_is_still_stated() {
    let e = one("set search_path to app;\ncreate table t (pid uuid references app.parent (id));");
    let fk = only_fk(&e);
    assert_eq!(fk.ref_schema.as_deref(), Some("app"));
    assert_eq!(
        fk.ref_schema_source,
        SchemaSource::Stated,
        "the source wrote `app.parent`; that it matches search_path[0] is a \
         coincidence, not a licence to re-point it"
    );
}

#[test]
fn a_table_constraint_foreign_key_is_marked_too() {
    let e = one("set search_path to app;\n\
         create table t (pid uuid, constraint fk foreign key (pid) references parent (id));");
    assert_eq!(only_fk(&e).ref_schema_source, SchemaSource::Inferred);
}

// ── So does a reference ─────────────────────────────────────────────────────

#[test]
fn a_bare_view_reference_is_inferred_and_a_written_one_is_not() {
    let bare = one("set search_path to app;\ncreate view v as select * from parent;");
    assert_eq!(
        bare.references
            .iter()
            .find(|r| r.name == "app.parent")
            .map(|r| r.schema_source),
        Some(SchemaSource::Inferred),
        "refs: {:?}",
        bare.references
    );

    let written = one("set search_path to app;\ncreate view v as select * from other.parent;");
    assert_eq!(
        written
            .references
            .iter()
            .find(|r| r.name == "other.parent")
            .map(|r| r.schema_source),
        Some(SchemaSource::Stated),
        "refs: {:?}",
        written.references
    );
}

/// T-SQL never invents a schema — an unqualified name stays unqualified — so
/// everything it reports is the source's own.
#[test]
fn the_tsql_reader_never_infers() {
    use dbd_core::parser::{Dialect, parse_sql_as};
    let p = parse_sql_as(Dialect::TSql, "CREATE VIEW dbo.v AS SELECT * FROM Issues;").expect("reads");
    for r in &p.entities[0].references {
        assert_eq!(r.schema_source, SchemaSource::Stated, "{r:?}");
    }
}

// ── The resolver upgrades a guess it can confirm ────────────────────────────

#[test]
fn resolving_against_the_whole_set_confirms_a_guess() {
    let mut entities = vec![
        one("set search_path to app;\ncreate table parent (id uuid primary key);"),
        one("set search_path to app;\ncreate table child (pid uuid references parent (id));"),
    ];
    dbd_core::references::resolve_references(&mut entities, &[], &[]);
    let fk = only_fk(&entities[1]);
    assert_eq!(fk.ref_schema.as_deref(), Some("app"));
    assert_eq!(
        fk.ref_schema_source,
        SchemaSource::Resolved,
        "the guess was checked against the real set and held"
    );
}

#[test]
fn resolving_repoints_a_guess_and_says_so() {
    let mut entities = vec![
        one("set search_path to other;\ncreate table parent (id uuid primary key);"),
        one("set search_path to app, other;\ncreate table child (pid uuid references parent (id));"),
    ];
    dbd_core::references::resolve_references(&mut entities, &[], &[]);
    let fk = only_fk(&entities[1]);
    assert_eq!(fk.ref_schema.as_deref(), Some("other"), "re-pointed along search_path");
    assert_eq!(fk.ref_schema_source, SchemaSource::Resolved);
}

/// A guess that nothing confirms stays a guess. This is the honest answer, and
/// the one a caller most needs.
#[test]
fn an_unconfirmable_guess_stays_inferred() {
    let mut entities = vec![one(
        "set search_path to app;\ncreate table child (pid uuid references nowhere (id));",
    )];
    dbd_core::references::resolve_references(&mut entities, &[], &[]);
    assert_eq!(only_fk(&entities[0]).ref_schema_source, SchemaSource::Inferred);
}

/// The defect the proxy allowed: an explicit qualification must survive the
/// resolver even when a same-named table exists elsewhere on the search_path.
#[test]
fn the_resolver_does_not_repoint_a_schema_the_source_wrote() {
    let mut entities = vec![
        one("set search_path to other;\ncreate table parent (id uuid primary key);"),
        one("set search_path to app, other;\ncreate table child (pid uuid references app.parent (id));"),
    ];
    dbd_core::references::resolve_references(&mut entities, &[], &[]);
    let fk = only_fk(&entities[1]);
    assert_eq!(
        fk.ref_schema.as_deref(),
        Some("app"),
        "the source said app.parent; other.parent existing does not change that"
    );
    assert_eq!(fk.ref_schema_source, SchemaSource::Stated);
}

// ── Provenance is metadata, and must not become part of the schema ──────────

/// Two foreign keys that differ only in where dbd learned the schema are the
/// same foreign key. If provenance entered `PartialEq`, every inferred FK would
/// read as drift against an introspected one forever — the #18 failure shape.
#[test]
fn provenance_does_not_affect_foreign_key_equality() {
    use dbd_core::entity::ForeignKey;
    let stated = ForeignKey {
        ref_schema: Some("app".into()),
        ref_table: "parent".into(),
        ref_schema_source: SchemaSource::Stated,
        ..Default::default()
    };
    let inferred = ForeignKey {
        ref_schema_source: SchemaSource::Inferred,
        ..stated.clone()
    };
    assert_eq!(stated, inferred);
}

/// And it must not enter a snapshot. A foreign key reaches one through
/// `inline_fk` and `TableConstraint::ForeignKey`; a snapshot records what the
/// schema IS, and emitting provenance would rewrite every existing snapshot the
/// next time one was generated.
#[test]
fn a_foreign_keys_provenance_is_never_serialized() {
    use dbd_core::entity::ForeignKey;
    for source in [SchemaSource::Inferred, SchemaSource::Resolved, SchemaSource::Stated] {
        let fk = ForeignKey {
            ref_schema: Some("app".into()),
            ref_table: "parent".into(),
            ref_schema_source: source,
            ..Default::default()
        };
        let json = serde_json::to_string(&fk).expect("serializes");
        assert!(
            !json.contains("schema_source"),
            "provenance leaked into the snapshot form for {source:?}: {json}"
        );
    }
}

/// A `Reference`, by contrast, exists to be handed to a consumer and never
/// lands in a snapshot — so it carries provenance across a JSON boundary too.
/// Omitted when `Stated`, which is the overwhelming majority and the default.
#[test]
fn a_references_provenance_survives_json() {
    use dbd_core::entity::Reference;
    let inferred = Reference {
        name: "app.parent".into(),
        ref_type: None,
        schema_source: SchemaSource::Inferred,
    };
    let json = serde_json::to_string(&inferred).expect("serializes");
    assert!(json.contains("\"schema_source\":\"inferred\""), "{json}");
    let back: Reference = serde_json::from_str(&json).expect("round-trips");
    assert_eq!(back.schema_source, SchemaSource::Inferred);

    let stated = Reference {
        schema_source: SchemaSource::Stated,
        ..inferred
    };
    let json = serde_json::to_string(&stated).expect("serializes");
    assert!(
        !json.contains("schema_source"),
        "the default is not worth emitting: {json}"
    );
}

/// And an older snapshot, written before this existed, still loads.
#[test]
fn a_snapshot_without_provenance_still_loads() {
    use dbd_core::entity::ForeignKey;
    let fk: ForeignKey =
        serde_json::from_str(r#"{"name":null,"columns":["pid"],"ref_schema":"app","ref_table":"parent","ref_columns":["id"],"on_delete":null,"on_update":null}"#)
            .expect("an older snapshot still deserializes");
    assert_eq!(fk.ref_table, "parent");
    assert_eq!(fk.ref_schema_source, SchemaSource::Stated);
}
