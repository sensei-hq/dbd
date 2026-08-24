//! Enum DDL, parsed with libpg_query.

use crate::entity::{Entity, EnumValue};
use crate::error::Result;
use crate::parser::extractors;

/// Parse an enum DDL file.
///
/// Handles both spellings: a bare `CREATE TYPE … AS ENUM (…)`, and one wrapped
/// in the `DO $$ … EXCEPTION WHEN duplicate_object $$` guard that is Postgres's
/// only idiom for a conditional CREATE TYPE.
pub(crate) fn parse_enum(mut entity: Entity, sql: &str) -> Result<Entity> {
    // libpg_query is Postgres's own grammar, so its rejection is the definition
    // of invalid SQL. Recording an error only here keeps the invariant
    // `Design::ensure_fully_parsed` relies on: apply refuses only on real
    // breakage, never on a parser limitation.
    if let Err(e) = pg_query::parse(sql) {
        entity.errors.push(format!("Parse error: {e}"));
        return Ok(entity);
    }

    entity.search_paths = extractors::extract_search_paths_via_pg_query(sql);
    entity.enum_values = enum_values(sql);

    if entity.enum_values.is_empty() {
        entity
            .errors
            .push("no `CREATE TYPE … AS ENUM (…)` found in this enum file".to_string());
    }

    Ok(entity)
}

/// Enum labels, whichever spelling the file uses.
fn enum_values(sql: &str) -> Vec<EnumValue> {
    if let Ok(parsed) = pg_query::parse(sql) {
        let values = labels_from_parse_result(&parsed);
        if !values.is_empty() {
            return values;
        }
    }
    // Guarded form: the CREATE lives inside a PL/pgSQL block, which the
    // top-level statement walk above cannot see into.
    extractors::extract_enum_values_via_pg_query(sql)
}

/// Labels from the first `CreateEnumStmt` in a parsed statement list.
///
/// Shared with [`extractors::extract_enum_values_via_pg_query`], which runs this
/// over the statements it recovers from inside a `DO` block.
pub(crate) fn labels_from_parse_result(parsed: &pg_query::ParseResult) -> Vec<EnumValue> {
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::CreateEnumStmt(create)) =
            stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        let values: Vec<EnumValue> = create
            .vals
            .iter()
            .filter_map(|v| match v.node.as_ref() {
                Some(pg_query::NodeEnum::String(s)) => Some(EnumValue {
                    name: s.sval.clone(),
                    note: None,
                }),
                _ => None,
            })
            .collect();
        if !values.is_empty() {
            return values;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        let entity = Entity::new(EntityType::Enum, "app.status");
        parse_enum(entity, sql).unwrap()
    }

    #[test]
    fn plain_create_type_yields_its_labels() {
        let e = parse("set search_path to app;\ncreate type status as enum ('a', 'b');");
        let names: Vec<&str> = e.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(e.errors.is_empty(), "got: {:?}", e.errors);
    }

    /// Postgres has no `CREATE TYPE IF NOT EXISTS`, so the guarded DO block is
    /// the only idiom for a conditional enum.
    #[test]
    fn do_guarded_create_type_yields_its_labels() {
        let e = parse(
            "set search_path to app;\n\
             do $$ begin\n  create type status as enum ('a', 'b');\n\
             exception when duplicate_object then null;\nend $$;",
        );
        let names: Vec<&str> = e.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(e.errors.is_empty(), "got: {:?}", e.errors);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate type status as enum ('a');");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    /// Matches the sqlparser path's default so the two agree under parity.
    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create type status as enum ('a');");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    /// libpg_query names the offending token but reports no line/column — its
    /// Rust binding keeps only `error.message` and drops `cursorpos`. Assert the
    /// token, which is what we can actually guarantee.
    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create type status as enum (;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(
            e.errors[0].contains("syntax error at or near"),
            "got: {:?}",
            e.errors
        );
        assert!(e.enum_values.is_empty());
    }

    #[test]
    fn valid_sql_declaring_no_enum_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty(), "an enum file with no CREATE TYPE must error");
    }
}
