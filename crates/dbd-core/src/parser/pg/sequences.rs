//! Sequence DDL, parsed with libpg_query.
//!
//! A sequence carries no structure dbd models — no columns, no references — so
//! this is the smallest native parser: validate, record the search path, and
//! confirm the file actually declares a sequence.
//!
//! It is also a bug fix. sqlparser cannot parse `INCREMENT BY`, so an ordinary
//! `create sequence … start with 1000 increment by 1;` produced a parse error,
//! and `Design::ensure_fully_parsed` then refused the entire project. `Sequence`
//! was never in the libpg_query recovery whitelist that spared Function,
//! Procedure and View, so nothing caught it.

use crate::entity::Entity;
use crate::error::Result;

use super::common;

/// Parse a sequence DDL file.
pub(in crate::parser) fn parse_sequence(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Before any early return, matching the other native parsers: an errored
    // entity reporting `[]` instead of the `["public"]` default is an invariant
    // break the enum parser already hit once.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    if !declares_a_sequence(&parsed) {
        entity
            .errors
            .push("this sequence file declares no `CREATE SEQUENCE`".to_string());
    }

    Ok(entity)
}

/// Whether the file contains a `CREATE SEQUENCE`.
fn declares_a_sequence(parsed: &pg_query::ParseResult) -> bool {
    parsed
        .protobuf
        .stmts
        .iter()
        .filter_map(|s| s.stmt.as_ref()?.node.as_ref())
        .any(|n| matches!(n, pg_query::NodeEnum::CreateSeqStmt(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        parse_sequence(Entity::new(EntityType::Sequence, "app.s"), sql).unwrap()
    }

    /// The case that was broken: sqlparser rejects `INCREMENT BY`, so this file
    /// errored and `ensure_fully_parsed` refused the whole project.
    #[test]
    fn an_increment_by_sequence_parses() {
        let e = parse(
            "set search_path to app;\n\
             create sequence if not exists s start with 1000 increment by 1;",
        );
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn a_plain_sequence_parses() {
        let e = parse("set search_path to app;\ncreate sequence if not exists s;");
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn the_full_option_set_parses() {
        let e = parse(
            "set search_path to app;\n\
             create sequence s as bigint increment by 2 minvalue 10 maxvalue 100 \
             start with 10 cache 5 cycle owned by app.t.id;",
        );
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    /// Matches the incumbent: a sequence carries no structure dbd models.
    #[test]
    fn a_sequence_has_no_references_or_table_def() {
        let e = parse("set search_path to app;\ncreate sequence s;");
        assert!(e.refers.is_empty());
        assert!(e.table_def.is_none());
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate sequence s;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create sequence s;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create sequence s start with ;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    #[test]
    fn an_errored_sequence_still_has_a_search_path() {
        let e = parse("create sequence s start with ;;;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn a_file_declaring_no_sequence_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty(), "a sequence file with no CREATE SEQUENCE must error");
    }
}
