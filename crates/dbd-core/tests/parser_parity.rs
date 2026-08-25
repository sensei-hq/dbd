//! Differential gate: the Postgres-native parser must agree with the incumbent
//! on every type it claims, and must do better on files the incumbent rejects.
//!
//! Restricting the sweep to `pg_native_types()` is load-bearing. A type that
//! still delegates would compare `SqlparserDdl` against itself and pass for
//! free — a green test proving nothing.

use dbd_core::{Entity, EntityType};
use dbd_core::parser::{ParserChoice, parse_entity_with, pg_native_types};
use std::path::{Path, PathBuf};

/// Entity types with no independent second implementation to compare against.
///
/// The gate's whole value is "the native parser agrees with the incumbent". For
/// Role there is no incumbent: sqlparser cannot parse `DO $$ … $$` at all, so
/// the historical implementation was a regex scanner, and that was deleted
/// rather than kept — a user falling back to it would have got the *worse*
/// implementation, whose object-grant exclusion was a text lookahead.
///
/// `SqlparserDdl` therefore delegates Role to the same `pg::roles::parse_role`
/// the native path uses, which means a Role file here would compare that
/// function against itself and pass for free. Skipping it is honest; counting
/// it would be a green check proving nothing.
///
/// Role's correctness rests on its unit tests in `parser::pg::roles` and on
/// live verification (apply ordering, `pg_auth_members`), not on this gate.
const NO_SECOND_IMPLEMENTATION: &[EntityType] = &[EntityType::Role];

/// Every `.ddl`/`.sql` file under `root`, recursively.
fn ddl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(ddl_files(&path));
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("ddl") | Some("sql")
        ) {
            out.push(path);
        }
    }
    out
}

fn corpus() -> Vec<PathBuf> {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut files = ddl_files(&fixtures);
    files.sort();
    files
}

/// Strip soft (function) references before comparing.
///
/// Postgres parses `COALESCE`, `NULLIF`, `GREATEST` and `LEAST` as dedicated
/// expression nodes rather than function calls, so libpg_query does not report
/// them; sqlparser does. The difference is inert — those names resolve to no
/// entity, so `resolve_references` drops them on both sides and the apply graph
/// is identical — but it would fail a whole-Entity comparison on any view using
/// `COALESCE`, which is very common SQL.
///
/// This is the only step here that narrows the gate — it DROPS data, so it
/// needs its own justification (above). [`canonicalize_reference_order`]
/// below is a different kind of step: it reorders, it does not drop, so a
/// genuine difference in which references a parser found still fails.
fn without_soft_refs(entity: &Entity) -> Entity {
    let mut e = entity.clone();
    e.references
        .retain(|r| r.ref_type.as_deref() != Some(dbd_core::entity::REF_TYPE_FUNCTION));
    e.refers = e.references.iter().map(|r| r.name.clone()).collect();
    e
}

/// Canonicalise reference ORDER before comparing.
///
/// Distinct from [`without_soft_refs`] above: that drops data, this does
/// not. Sorting preserves set equality, so a genuine difference in which
/// references a parser found still fails the gate. No consumer depends on
/// order — `dependency.rs:31` and `:198` collect `refers` into a `HashSet`
/// before the topological sort, so apply ordering cannot see it; the only
/// order-visible effect is the edge list in `dbd graph` output.
fn canonicalize_reference_order(mut entity: Entity) -> Entity {
    entity
        .references
        .sort_by(|a, b| (&a.name, &a.ref_type).cmp(&(&b.name, &b.ref_type)));
    entity.refers.sort();
    entity
}

/// The comparison view of an `Entity`: soft refs excluded, then reference
/// order canonicalised. See the two functions above for what each step does
/// and does not do to the underlying data.
fn comparable(entity: &Entity) -> serde_json::Value {
    let e = canonicalize_reference_order(without_soft_refs(entity));
    serde_json::to_value(&e).expect("Entity serializes")
}

#[test]
fn native_types_match_sqlparser_on_every_corpus_file() {
    let covered = pg_native_types();
    let mut checked = 0usize;

    for file in corpus() {
        let sql = std::fs::read_to_string(&file).expect("corpus file is readable");
        // The path decides the entity type, exactly as the scan loop does.
        let entity_type = Entity::from_file(&file).entity_type;
        if !covered.contains(&entity_type) {
            continue;
        }
        // See NO_SECOND_IMPLEMENTATION: this type would compare its own native
        // parser against itself, which can never disagree and proves nothing.
        if NO_SECOND_IMPLEMENTATION.contains(&entity_type) {
            continue;
        }
        checked += 1;

        let old = parse_entity_with(ParserChoice::Sqlparser, &file, &sql).unwrap();
        let new = parse_entity_with(ParserChoice::PgQuery, &file, &sql).unwrap();

        if old.errors.is_empty() {
            // No regression: identical Entity for anything the incumbent reads.
            assert_eq!(
                comparable(&old),
                comparable(&new),
                "parsers disagree on {}",
                file.display()
            );
        } else {
            // Improvement: the native parser must read what the incumbent could not.
            assert!(
                new.errors.is_empty(),
                "{} is valid Postgres the native parser should read, got: {:?}",
                file.display(),
                new.errors
            );
        }
    }

    if !covered.is_empty() {
        assert!(
            checked > 0,
            "no corpus file exercises the covered types {covered:?} — the gate is vacuous"
        );
    }
}
