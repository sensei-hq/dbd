//! The catalog level — telling two same-named tables in different databases
//! apart.
//!
//! dbd has always modelled an entity as `schema.name`, which is enough for
//! PostgreSQL: cross-database references are not possible on one connection, so
//! every entity a scan sees belongs to the same database and the database name
//! never has to be written down.
//!
//! T-SQL and MySQL break that. `OtherDb.dbo.Users` is an ordinary reference in
//! a SQL Server codebase, and MySQL's `db.users` puts the *database* where
//! dbd's model expects a schema. Without a third level, `dbo.Users` in two
//! databases is one entity, and a scan across a multi-database repository
//! silently merges them.
//!
//! The rule mirrors how a bare schema already resolves along `search_path`: a
//! reference that names no catalog resolves within the **referring entity's**
//! catalog first, then against a catalog-less entity. A reference that names
//! one is taken at its word.
//!
//! The load-bearing test here is the first one. Every existing project has no
//! catalog anywhere, and this must be invisible to all of them.

use dbd_core::entity::{Entity, EntityType};
use dbd_core::references::resolve_references;

fn table(catalog: Option<&str>, name: &str, refers: &[&str]) -> Entity {
    let mut e = Entity::new(EntityType::Table, name);
    e.catalog = catalog.map(str::to_string);
    e.refers = refers.iter().map(|s| s.to_string()).collect();
    e
}

// ── Nothing changes without a catalog ───────────────────────────────────────

/// The regression guard. No dbd project today has a catalog on anything, so
/// introducing the level must be invisible to every one of them.
#[test]
fn resolution_is_unchanged_when_nothing_has_a_catalog() {
    let mut entities = vec![
        table(None, "app.users", &[]),
        table(None, "app.orders", &["app.users"]),
        table(None, "app.broken", &["app.missing"]),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert_eq!(
        entities[1].refers,
        vec!["app.users"],
        "a resolvable edge still resolves"
    );
    assert!(entities[1].warnings.is_empty());

    assert!(entities[2].refers.is_empty(), "an unresolvable edge is still dropped");
    assert_eq!(entities[2].warnings.len(), 1, "and still warns");
    assert!(entities[2].warnings[0].contains("app.missing"));
}

// ── Two databases, one name ─────────────────────────────────────────────────

/// The case the level exists for: without it these are one entity.
#[test]
fn the_same_schema_and_name_in_two_catalogs_are_two_entities() {
    let main = table(Some("MainDb"), "dbo.Users", &[]);
    let other = table(Some("OtherDb"), "dbo.Users", &[]);

    assert_eq!(main.name, other.name, "the names really are identical");
    assert_ne!(
        main.qualified_key(),
        other.qualified_key(),
        "but the keys resolution uses must not be"
    );
    assert_eq!(main.qualified_key(), "MainDb.dbo.Users");
}

/// A catalog-less entity keys on `schema.name`, exactly as before — so a
/// PostgreSQL project's keys are byte-identical to what they were.
#[test]
fn a_catalog_less_entity_keys_on_schema_and_name() {
    assert_eq!(table(None, "app.users", &[]).qualified_key(), "app.users");
}

// ── Which one a bare reference means ────────────────────────────────────────

/// A reference naming no catalog means the one it was written in. Resolving it
/// to another database would be the silent merge this level exists to prevent.
#[test]
fn a_bare_reference_resolves_within_the_referring_entitys_catalog() {
    let mut entities = vec![
        table(Some("MainDb"), "dbo.Users", &[]),
        table(Some("OtherDb"), "dbo.Users", &[]),
        table(Some("MainDb"), "dbo.Orders", &["dbo.Users"]),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert_eq!(
        entities[2].refers,
        vec!["MainDb.dbo.Users"],
        "MainDb.dbo.Orders must reach MainDb's Users, not OtherDb's"
    );
    assert!(entities[2].warnings.is_empty());
}

/// A reference that names a catalog is taken at its word — that is the whole
/// point of writing one.
#[test]
fn a_reference_naming_a_catalog_crosses_to_it() {
    let mut entities = vec![
        table(Some("MainDb"), "dbo.Users", &[]),
        table(Some("OtherDb"), "dbo.Users", &[]),
        table(Some("MainDb"), "dbo.Audit", &["OtherDb.dbo.Users"]),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert_eq!(entities[2].refers, vec!["OtherDb.dbo.Users"]);
    assert!(entities[2].warnings.is_empty());
}

/// A scan can hold both kinds at once — a catalogued T-SQL tree beside a
/// catalog-less PostgreSQL project. A bare reference falls back to the
/// catalog-less entity rather than failing.
#[test]
fn a_bare_reference_falls_back_to_a_catalog_less_entity() {
    let mut entities = vec![
        table(None, "app.shared", &[]),
        table(Some("MainDb"), "app.report", &["app.shared"]),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert_eq!(entities[1].refers, vec!["app.shared"]);
    assert!(entities[1].warnings.is_empty());
}

/// Naming a catalog that is not in the scan is unresolved, not quietly matched
/// to a same-named table in a different database.
#[test]
fn a_reference_to_an_absent_catalog_stays_unresolved() {
    let mut entities = vec![
        table(Some("MainDb"), "dbo.Users", &[]),
        table(Some("MainDb"), "dbo.Audit", &["Nowhere.dbo.Users"]),
    ];
    resolve_references(&mut entities, &[], &[]);

    assert!(
        entities[1].refers.is_empty(),
        "got {:?} — a cross-database edge must not fall back to the local one",
        entities[1].refers
    );
    assert_eq!(entities[1].warnings.len(), 1);
    assert!(entities[1].warnings[0].contains("Nowhere.dbo.Users"));
}

// ── Snapshots written before the level existed ──────────────────────────────

/// Every snapshot on disk predates `catalog`. They must still load.
#[test]
fn an_entity_serialized_before_catalog_existed_still_loads() {
    let legacy = r#"{
        "entity_type": "table",
        "name": "app.users",
        "schema": "app",
        "file": null,
        "format": null,
        "refers": [],
        "references": [],
        "search_paths": [],
        "errors": [],
        "warnings": [],
        "reads": [],
        "writes": [],
        "table_def": null,
        "enum_values": []
    }"#;

    let e: Entity = serde_json::from_str(legacy).expect("a pre-catalog snapshot must still deserialize");
    assert_eq!(e.name, "app.users");
    assert_eq!(e.catalog, None, "absent means no catalog, not an error");
    assert_eq!(e.qualified_key(), "app.users");
}

/// And a catalog-less entity must not start writing the field, or every
/// snapshot in every project churns on the next write.
#[test]
fn a_catalog_less_entity_does_not_serialize_the_field() {
    let json = serde_json::to_string(&table(None, "app.users", &[])).unwrap();
    assert!(
        !json.contains("catalog"),
        "an absent catalog must stay absent on the wire: {json}"
    );

    let json = serde_json::to_string(&table(Some("MainDb"), "dbo.Users", &[])).unwrap();
    assert!(json.contains("MainDb"), "but a present one is recorded: {json}");
}
