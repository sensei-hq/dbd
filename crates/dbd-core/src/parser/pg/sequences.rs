//! Sequence DDL, parsed with libpg_query.
//!
//! A sequence carries no structure dbd models — no columns, no references — so
//! this is the smallest native parser: validate, record the search path and the
//! sequence's own comment, and confirm the file actually declares a sequence.
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
    entity.schema_path = common::resolve_schema_path(sql, &entity.schema_path);

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
    // The schema model lists sequences beside views and routines, and the
    // viewer describes each by its comment.
    entity.comment = common::entity_comment(&parsed);

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

/// What a `CREATE SEQUENCE` says, as written — `None` where it is silent.
///
/// The model keeps no sequence structure (see the module note), so `emit`,
/// which has to write a sequence for an engine whose defaults differ, reads it
/// here from libpg_query's tree. Silence is kept as silence because
/// PostgreSQL's defaults depend on direction: a descending sequence starts at
/// -1, not 1, and the caller resolves them.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct SequenceOptions {
    /// `AS <type>` as libpg_query names it: `int2`, `int4`, `int8`.
    pub data_type: Option<String>,
    pub increment: Option<i64>,
    /// `Some(None)` is an explicit `NO MINVALUE`.
    pub min: Option<Option<i64>>,
    /// `Some(None)` is an explicit `NO MAXVALUE`.
    pub max: Option<Option<i64>>,
    pub start: Option<i64>,
    pub cache: Option<i64>,
    pub cycle: Option<bool>,
    /// `OWNED BY table.column`, as written. `None` for no clause and for
    /// `OWNED BY NONE`, which says the same thing.
    pub owned_by: Option<String>,
}

/// The options of the first `CREATE SEQUENCE` in `sql`, or `None` when it has
/// none or does not parse.
pub(crate) fn sequence_options(sql: &str) -> Option<SequenceOptions> {
    use pg_query::NodeEnum;

    let parsed = pg_query::parse(sql).ok()?;
    let create = parsed
        .protobuf
        .stmts
        .iter()
        .find_map(|s| match s.stmt.as_ref()?.node.as_ref()? {
            NodeEnum::CreateSeqStmt(c) => Some(c.clone()),
            _ => None,
        })?;

    // Integers past i32 arrive as a `Float` holding the digits.
    let number = |node: Option<&pg_query::protobuf::Node>| -> Option<i64> {
        match node?.node.as_ref()? {
            NodeEnum::Integer(i) => Some(i64::from(i.ival)),
            NodeEnum::Float(f) => f.fval.parse().ok(),
            _ => None,
        }
    };
    let names = |items: &[pg_query::protobuf::Node]| -> Vec<String> {
        items
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(NodeEnum::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect()
    };

    let mut o = SequenceOptions::default();
    for opt in &create.options {
        let Some(NodeEnum::DefElem(d)) = opt.node.as_ref() else {
            continue;
        };
        let arg = d.arg.as_deref();
        match d.defname.as_str() {
            "as" => {
                if let Some(NodeEnum::TypeName(t)) = arg.and_then(|a| a.node.as_ref()) {
                    o.data_type = names(&t.names).pop();
                }
            }
            "increment" => o.increment = number(arg),
            // A bare `NO MINVALUE` / `NO MAXVALUE` carries no argument.
            "minvalue" => o.min = Some(number(arg)),
            "maxvalue" => o.max = Some(number(arg)),
            "start" => o.start = number(arg),
            "cache" => o.cache = number(arg),
            "cycle" => {
                o.cycle = match arg.and_then(|a| a.node.as_ref()) {
                    Some(NodeEnum::Boolean(b)) => Some(b.boolval),
                    _ => Some(true),
                };
            }
            "owned_by" => {
                if let Some(NodeEnum::List(l)) = arg.and_then(|a| a.node.as_ref()) {
                    let parts = names(&l.items);
                    if !(parts.len() == 1 && parts[0].eq_ignore_ascii_case("none")) {
                        o.owned_by = Some(parts.join("."));
                    }
                }
            }
            _ => {}
        }
    }
    Some(o)
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
        assert!(e.refers().next().is_none());
        assert!(e.table_def.is_none());
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate sequence s;");
        assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["app"]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create sequence s;");
        assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["public"]);
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
        assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["public"]);
    }

    #[test]
    fn a_file_declaring_no_sequence_records_an_error() {
        let e = parse("select 1;");
        assert!(
            !e.errors.is_empty(),
            "a sequence file with no CREATE SEQUENCE must error"
        );
    }
}
