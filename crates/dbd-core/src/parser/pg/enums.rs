//! Enum DDL, parsed with libpg_query.

use crate::entity::{Entity, EnumValue};
use crate::error::Result;

use super::common;

/// Parse an enum DDL file.
///
/// Handles both spellings: a bare `CREATE TYPE … AS ENUM (…)`, and one wrapped
/// in the `DO $$ … EXCEPTION WHEN duplicate_object $$` guard that is Postgres's
/// only idiom for a conditional CREATE TYPE.
pub(crate) fn parse_enum(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Set before the parse-error early return: an errored entity must still
    // carry the sqlparser path's `["public"]` default, since View qualifies
    // its refs against `search_paths`.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    // libpg_query is Postgres's own grammar, so its rejection is the definition
    // of invalid SQL. Recording an error only here keeps the invariant
    // `Design::ensure_fully_parsed` relies on: apply refuses only on real
    // breakage, never on a parser limitation.
    if let Err(e) = pg_query::parse(sql) {
        entity.errors.push(format!("Parse error: {e}"));
        return Ok(entity);
    }

    match enum_values(sql) {
        // `CREATE TYPE e AS ENUM ()` is valid Postgres with zero labels — not
        // an absence of the statement, so it must not error.
        Some(values) => entity.enum_values = values,
        None => entity
            .errors
            .push("this enum file declares no `CREATE TYPE … AS ENUM`".to_string()),
    }

    Ok(entity)
}

/// Enum labels, whichever spelling the file uses.
///
/// `None` means no `CreateEnumStmt` was found anywhere (bare or DO-guarded);
/// `Some(vec![])` means one was found and it declares zero labels — those are
/// different outcomes and callers must not conflate them.
fn enum_values(sql: &str) -> Option<Vec<EnumValue>> {
    if let Ok(parsed) = pg_query::parse(sql)
        && let Some(values) = labels_from_parse_result(&parsed)
    {
        return Some(values);
    }
    // Guarded form: the CREATE lives inside a PL/pgSQL block, which the
    // top-level statement walk above cannot see into. This fallback returns a
    // plain `Vec`, so it can't distinguish "no CreateEnumStmt in the block"
    // from "found with zero labels" the way `labels_from_parse_result` does
    // above — an empty result here is treated as not found.
    let values = common::extract_enum_values_via_pg_query(sql);
    (!values.is_empty()).then_some(values)
}

/// Labels from the first `CreateEnumStmt` in a parsed statement list, or
/// `None` if the statement list contains no `CreateEnumStmt` at all.
///
/// Shared with [`common::extract_enum_values_via_pg_query`], which runs this
/// over the statements it recovers from inside a `DO` block.
pub(crate) fn labels_from_parse_result(parsed: &pg_query::ParseResult) -> Option<Vec<EnumValue>> {
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
        return Some(values);
    }
    None
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

    /// `CREATE TYPE e AS ENUM ()` is valid Postgres (confirmed against a live
    /// server) and is exactly what dbd's own emitter produces for a label-less
    /// enum — it must parse clean, not be mistaken for "no CREATE TYPE found".
    #[test]
    fn empty_enum_parses_with_zero_labels_and_no_error() {
        let e = parse("set search_path to app;\ncreate type status as enum ();");
        assert!(e.enum_values.is_empty(), "got: {:?}", e.enum_values);
        assert!(e.errors.is_empty(), "got: {:?}", e.errors);
    }

    /// The sqlparser path always defaults `search_paths` to `["public"]`, even
    /// on error; View qualifies its refs against this field, so the two parsers
    /// must not diverge on it just because one hit a parse error.
    #[test]
    fn errored_entity_still_defaults_search_paths_to_public() {
        let e = parse("create type status as enum (;;;");
        assert!(!e.errors.is_empty(), "sanity: this SQL must still error");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }
}
