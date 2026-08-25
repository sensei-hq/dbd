//! libpg_query helpers shared by the Postgres-native parsers.
//!
//! These live under `pg` rather than beside the sqlparser extractors so the
//! dependency runs one way: `extractors` (sqlparser) may call into `pg`, never
//! the reverse. The previous arrangement had `pg::enums` importing `extractors`
//! while `extractors` imported `pg::enums`, a cycle that each new native parser
//! would deepen.

use crate::entity::{EnumValue, Reference};

use super::enums;

/// Enum labels from a `DO $$ … $$` guarded `CREATE TYPE … AS ENUM`, read off
/// libpg_query's AST (Postgres's own parser).
///
/// Postgres has no `CREATE TYPE IF NOT EXISTS`, so wrapping the CREATE in a DO
/// block that swallows `duplicate_object` is the only idiom available for a
/// conditional enum — and sqlparser rejects `DO` outright. This is the same
/// second tier [`extractors::extract_proc_refs`] already uses for PL/pgSQL
/// bodies: parse the block, take the SQL it embeds, and re-parse that into a
/// real statement tree.
///
/// Returns an empty vec when the input is not a PL/pgSQL block libpg_query
/// accepts, or when it declares no enum — the caller keeps its parse error, so a
/// genuinely broken file is never quietly waved through.
///
/// [`extractors::extract_proc_refs`]: crate::parser::extractors::extract_proc_refs
pub(in crate::parser) fn extract_enum_values_via_pg_query(raw_sql: &str) -> Vec<EnumValue> {
    let Ok(tree) = pg_query::parse_plpgsql(raw_sql) else {
        return Vec::new();
    };

    let mut queries = Vec::new();
    // Shared with the sqlparser-side PL/pgSQL tier (`extract_proc_refs_via_pg_query`
    // in `extractors.rs`), so it stays defined there — referenced here by
    // fully-qualified path rather than `use` to keep this module's dependency
    // graph one-way (see the module doc comment).
    crate::parser::extractors::collect_plpgsql_queries(&tree, &mut queries);

    for query in &queries {
        let Ok(parsed) = pg_query::parse(query) else {
            continue;
        };
        if let Some(values) = enums::labels_from_parse_result(&parsed)
            && !values.is_empty()
        {
            return values;
        }
    }
    Vec::new()
}

/// Whether libpg_query — Postgres's own grammar — accepts this file.
///
/// The authority on "is this valid SQL". sqlparser is a convenience parser that
/// reimplements the grammar and lags the server, so its rejection alone says
/// nothing about the file; this says whether Postgres itself would take it.
pub(in crate::parser) fn is_valid_postgres(raw_sql: &str) -> bool {
    pg_query::parse(raw_sql).is_ok()
}

/// `search_path` schemas from libpg_query's AST, for the fallback path where
/// sqlparser produced no statements for [`extractors::extract_search_paths`] to
/// read.
///
/// Mirrors that function's contract, including its `["public"]` default when the
/// file sets no search path. Recovering this is not optional: reads and view
/// references are qualified against it, so an empty list silently re-qualifies
/// `t` to `public.t` — a plausibly-wrong edge pointing at a different table,
/// which is worse than no edge at all.
///
/// [`extractors::extract_search_paths`]: crate::parser::extractors::extract_search_paths
pub(in crate::parser) fn extract_search_paths_via_pg_query(raw_sql: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(raw_sql) else {
        return vec![DEFAULT_SEARCH_PATH.to_string()];
    };
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::VariableSetStmt(set)) =
            stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        if !set.name.eq_ignore_ascii_case("search_path") {
            continue;
        }
        let paths: Vec<String> = set.args.iter().filter_map(const_str).collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    vec![DEFAULT_SEARCH_PATH.to_string()]
}

/// The default schema when a file sets no `search_path` — matches
/// [`extractors::extract_search_paths`].
///
/// [`extractors::extract_search_paths`]: crate::parser::extractors::extract_search_paths
const DEFAULT_SEARCH_PATH: &str = "public";

/// The string behind a `SET` argument node: a bare identifier arrives as a
/// `ColumnRef` (`to app`), a quoted one as an `A_Const` string (`to 'app'`).
fn const_str(node: &pg_query::protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        pg_query::NodeEnum::String(s) => Some(s.sval.clone()),
        pg_query::NodeEnum::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Sval(s) => Some(s.sval.clone()),
            _ => None,
        },
        pg_query::NodeEnum::ColumnRef(r) => r.fields.first().and_then(const_str),
        _ => None,
    }
}

/// The tables a view's body reads, from libpg_query's AST — the fallback for a
/// view [`extractors::extract_view_info`] could not read because sqlparser
/// rejected the file.
///
/// Without this a recovered view carries no dependency edge at all, so it could
/// be applied before the table it selects from: a loud skip traded for a silent
/// misordering.
///
/// [`extractors::extract_view_info`]: crate::parser::extractors::extract_view_info
pub(in crate::parser) fn extract_view_refs_via_pg_query(
    raw_sql: &str,
    default_schema: &str,
) -> Vec<Reference> {
    let Ok(parsed) = pg_query::parse(raw_sql) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for table in parsed.select_tables() {
        // `qualify_name_str` and `push_unique` are shared with the sqlparser-side
        // PL/pgSQL tier in `extractors.rs`, so they stay defined there — referenced
        // here by fully-qualified path rather than `use` (see module doc comment).
        if let Some(name) = crate::parser::extractors::qualify_name_str(&table, default_schema) {
            crate::parser::extractors::push_unique(&mut names, name);
        }
    }
    names
        .into_iter()
        .map(|name| Reference {
            name,
            ref_type: Some("table".to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarded_enum_values_read_off_the_pg_query_ast() {
        let values = extract_enum_values_via_pg_query(
            "do $$ begin\n  create type status_t as enum ('active', 'archived');\n\
             exception when duplicate_object then null;\nend $$;",
        );
        let names: Vec<&str> = values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["active", "archived"]);
    }

    #[test]
    fn pg_query_enum_fallback_is_empty_when_no_enum_is_declared() {
        assert!(extract_enum_values_via_pg_query("do $$ begin perform 1; end $$;").is_empty());
        assert!(extract_enum_values_via_pg_query("NOT SQL AT ALL ;;;").is_empty());
    }
}
