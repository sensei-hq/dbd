//! Differential gate: the Postgres-native parser must agree with the incumbent
//! on every type it claims, and must do better on files the incumbent rejects.
//!
//! Restricting the sweep to `pg_native_types()` is load-bearing. A type that
//! still delegates would compare `SqlparserDdl` against itself and pass for
//! free — a green test proving nothing.

use dbd_core::Entity;
use dbd_core::parser::{ParserChoice, parse_entity_with, pg_native_types};
use std::path::{Path, PathBuf};

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

fn json(entity: &Entity) -> serde_json::Value {
    serde_json::to_value(entity).expect("Entity serializes")
}

#[test]
fn native_types_match_sqlparser_on_every_corpus_file() {
    let covered = pg_native_types();
    let mut checked = 0usize;

    for file in corpus() {
        let sql = std::fs::read_to_string(&file).expect("corpus file is readable");
        // The path decides the entity type, exactly as the scan loop does.
        if !covered.contains(&Entity::from_file(&file).entity_type) {
            continue;
        }
        checked += 1;

        let old = parse_entity_with(ParserChoice::Sqlparser, &file, &sql).unwrap();
        let new = parse_entity_with(ParserChoice::PgQuery, &file, &sql).unwrap();

        if old.errors.is_empty() {
            // No regression: identical Entity for anything the incumbent reads.
            assert_eq!(json(&old), json(&new), "parsers disagree on {}", file.display());
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
