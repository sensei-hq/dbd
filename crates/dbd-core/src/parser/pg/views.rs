//! View DDL, parsed with libpg_query.

use crate::entity::{Entity, REF_TYPE_FUNCTION, Reference};
use crate::error::Result;

use super::common;

/// Parse a view DDL file.
///
/// A view entity carries only its references — nothing renders its body — so
/// unlike a materialized view it needs no verbatim SQL, which is what makes it
/// parity-clean against the incumbent.
pub(crate) fn parse_view(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Set the search path before any early return: references are qualified
    // against it, and an errored entity reporting `[]` instead of the
    // `["public"]` default is an invariant break the enum parser already hit.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    if !declares_a_view(&parsed) {
        entity
            .errors
            .push("this view file declares no `CREATE VIEW`".to_string());
        return Ok(entity);
    }

    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    // Relations the body reads — hard references, they drive apply order.
    let mut references = common::extract_view_refs_via_pg_query(sql, &default_schema);

    // Function calls — soft references. `resolve_references` keeps the ones
    // naming a known entity and drops the rest, because a body's built-in calls
    // are indistinguishable here from calls to a project-managed function.
    //
    // Sorted before appending: like `select_tables()`, `call_functions()` is
    // built from a `HashSet` internally, so its order is Rust's randomized
    // per-process hash order, not source order — confirmed empirically (same
    // SQL, same binary, different orderings across separate runs). Left
    // unsorted, `entity.refers` would vary from run to run for any view
    // calling more than one function.
    let mut function_names: Vec<String> = parsed
        .call_functions()
        .into_iter()
        .filter_map(|name| common::qualify_name_str(&name, &default_schema))
        .collect();
    function_names.sort();
    function_names.dedup();
    for qualified in function_names {
        if references.iter().any(|r| r.name == qualified) {
            continue;
        }
        references.push(Reference {
            name: qualified,
            ref_type: Some(REF_TYPE_FUNCTION.to_string()),
        });
    }

    entity.refers = references.iter().map(|r| r.name.clone()).collect();
    entity.references = references;
    Ok(entity)
}

/// Whether the file contains a `CREATE VIEW`.
fn declares_a_view(parsed: &pg_query::ParseResult) -> bool {
    parsed
        .protobuf
        .stmts
        .iter()
        .filter_map(|s| s.stmt.as_ref()?.node.as_ref())
        .any(|n| matches!(n, pg_query::NodeEnum::ViewStmt(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, REF_TYPE_FUNCTION};

    fn parse(sql: &str) -> Entity {
        parse_view(Entity::new(EntityType::View, "app.v"), sql).unwrap()
    }

    #[test]
    fn relation_references_are_captured_and_qualified() {
        let e = parse("set search_path to app;\ncreate view v as select a from t;");
        assert!(e.refers.contains(&"app.t".to_string()), "got {:?}", e.refers);
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn an_explicit_schema_is_not_overridden_by_the_search_path() {
        let e = parse("set search_path to app;\ncreate view v as select a from shop.orders;");
        assert!(e.refers.contains(&"shop.orders".to_string()), "got {:?}", e.refers);
    }

    /// Function calls are soft references: the resolver keeps the ones naming a
    /// known entity and drops the rest, so a body's built-ins are harmless.
    #[test]
    fn function_calls_are_captured_as_soft_references() {
        let e = parse("set search_path to app;\ncreate view v as select app.myfn(a) from t;");
        let myfn = e
            .references
            .iter()
            .find(|r| r.name == "app.myfn")
            .expect("function reference missing");
        assert_eq!(myfn.ref_type.as_deref(), Some(REF_TYPE_FUNCTION));
    }

    /// A CTE name is query-local, not a real relation.
    #[test]
    fn cte_names_are_not_references() {
        let e = parse("set search_path to app;\ncreate view v as with r as (select 1 n) select n from r;");
        assert!(!e.refers.contains(&"app.r".to_string()), "CTE leaked: {:?}", e.refers);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate view v as select a from t;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create view v as select a from t;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create view v as select * from ;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    /// Mirrors the enum parser: an error path still yields the `["public"]`
    /// default, because references are qualified against it.
    #[test]
    fn an_errored_view_still_has_a_search_path() {
        let e = parse("create view v as select * from ;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn a_file_declaring_no_view_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty(), "a view file with no CREATE VIEW must error");
    }
}
