//! Materialized view DDL, parsed with libpg_query.
//!
//! Unlike a plain view, a matview's body is re-executed only on `REFRESH`, and
//! `emit_matview` reconstructs the `CREATE` from `entity.writes[0]` — so what
//! that field holds is user-visible, not just an internal detail. The
//! sqlparser incumbent stored sqlparser's own re-rendering of the body; this
//! parser stores the author's SQL verbatim instead.
//!
//! Verbatim extraction can't use statement or node locations directly:
//! `RawStmt.stmt_location`/`stmt_len` bound the whole `CREATE MATERIALIZED
//! VIEW … AS … WITH DATA` statement, and inner node locations (e.g. a target
//! list entry) point at content, not at the `SELECT` keyword — neither
//! delimits the body. Instead this scopes `pg_query::scan()`'s token stream to
//! the statement's byte range and finds the body between the first `AS` and
//! the last `WITH` (if any) — tokenized, so an `as` inside a string, comment,
//! or dollar-quoted body is never mistaken for the boundary.

use crate::entity::{
    Entity, IndexColumn, IndexDef, IndexType, REF_TYPE_FUNCTION, Reference, SortOrder, TableComments,
    TableDef,
};
use crate::error::Result;

use super::common;

/// Parse a materialized view DDL file.
pub(crate) fn parse_matview(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Set before the parse-error early return, same as every other native
    // parser here: an errored entity must still carry the sqlparser path's
    // `["public"]` default, since references are qualified against it.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    let Some(raw_stmt) = matview_raw_stmt(&parsed) else {
        entity
            .errors
            .push("this materialized view file declares no `CREATE MATERIALIZED VIEW`".to_string());
        return Ok(entity);
    };

    if let Some(body) = extract_body(sql, raw_stmt) {
        entity.writes = vec![body];
    }

    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    // Relations + function calls, exactly like `pg::views::parse_view` — a
    // matview's body is read the same way a view's is, only the write side
    // (the body text kept in `writes[0]`) differs.
    let mut references = common::extract_view_refs_via_pg_query(sql, &default_schema);
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

    // Trailing CREATE INDEX statements land in table_def.indexes, exactly like
    // a table's indexes — there is no CREATE TABLE here, so only indexes are
    // populated (columns/constraints/comments stay empty).
    entity.table_def = Some(TableDef {
        columns: Vec::new(),
        constraints: Vec::new(),
        indexes: extract_indexes(&parsed, &mut entity.warnings),
        comments: TableComments::default(),
    });

    Ok(entity)
}

/// The file's `CREATE MATERIALIZED VIEW` statement, if it declares one.
///
/// `CreateTableAsStmt` also covers plain `CREATE TABLE AS` and `SELECT INTO`,
/// which share the node type in libpg_query's grammar — `objtype` is what
/// tells them apart.
fn matview_raw_stmt(parsed: &pg_query::ParseResult) -> Option<&pg_query::protobuf::RawStmt> {
    parsed.protobuf.stmts.iter().find(|s| {
        matches!(
            s.stmt.as_ref().and_then(|n| n.node.as_ref()),
            Some(pg_query::NodeEnum::CreateTableAsStmt(c))
                if c.objtype == pg_query::protobuf::ObjectType::ObjectMatview as i32
        )
    })
}

/// The verbatim body between `AS` and a trailing `WITH [NO] DATA`, per the
/// module doc comment's algorithm. `None` only if `pg_query::scan` itself
/// fails, which should not happen for SQL `pg_query::parse` just accepted.
fn extract_body(sql: &str, raw_stmt: &pg_query::protobuf::RawStmt) -> Option<String> {
    let start = raw_stmt.stmt_location as usize;
    // `stmt_len == 0` means "runs to the end of input" — true for a final
    // statement with no trailing `;` (confirmed empirically; see module docs).
    let end = if raw_stmt.stmt_len == 0 {
        sql.len()
    } else {
        start + raw_stmt.stmt_len as usize
    };

    let scan = pg_query::scan(sql).ok()?;
    let in_range = |t: &&pg_query::protobuf::ScanToken| {
        (t.start as usize) >= start && (t.end as usize) <= end
    };
    let text_of = |t: &pg_query::protobuf::ScanToken| sql[t.start as usize..t.end as usize].to_lowercase();

    let as_token = scan.tokens.iter().filter(in_range).find(|t| text_of(t) == "as")?;
    let body_start = as_token.end as usize;

    // The LAST `with` after the body start: `WITH DATA`/`WITH NO DATA` is the
    // final clause of the statement, so any earlier `with` belongs to the body
    // itself (a CTE's `WITH`, or one nested in a subquery).
    let body_end = scan
        .tokens
        .iter()
        .filter(in_range)
        .rfind(|t| (t.start as usize) >= body_start && text_of(t) == "with")
        .map(|t| t.start as usize)
        .unwrap_or(end);

    Some(sql[body_start..body_end].trim().trim_end_matches(';').trim().to_string())
}

/// Trailing `CREATE INDEX` statements' `IndexStmt` nodes, converted the way
/// `tables::extract_table` converts sqlparser's `CreateIndex` for a table.
///
/// Deliberately partial: an index whose key is an expression, or that carries
/// a `WHERE` predicate, an `INCLUDE` list, or storage parameters, cannot be
/// rendered back to SQL through this crate's `pg_query` bindings —
/// `Node::deparse` only reconstructs whole statements, not a bare expression
/// (confirmed empirically: it errors "unsupported top-level node type" on an
/// `IndexElem`'s expr or a `WHERE` clause node). Rather than silently emit a
/// wrong or truncated index, such an index is skipped with a warning. `Table`
/// going native will need the fuller version — this one only carries what
/// `emit_index_sql` needs for the common case.
fn extract_indexes(parsed: &pg_query::ParseResult, warnings: &mut Vec<String>) -> Vec<IndexDef> {
    let mut indexes = Vec::new();
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::IndexStmt(ix)) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        match extract_one_index(ix) {
            Some(def) => indexes.push(def),
            None => {
                let name = if ix.idxname.is_empty() { "<unnamed>" } else { &ix.idxname };
                warnings.push(format!(
                    "index {name:?} uses an expression key, predicate, INCLUDE list, or storage \
                     parameters, which the native matview parser does not yet extract; skipped"
                ));
            }
        }
    }
    indexes
}

/// One index, or `None` if it uses a feature this minimal extractor skips —
/// see [`extract_indexes`].
fn extract_one_index(ix: &pg_query::protobuf::IndexStmt) -> Option<IndexDef> {
    if ix.where_clause.is_some() || !ix.options.is_empty() {
        return None;
    }

    let mut columns = Vec::with_capacity(ix.index_params.len());
    for param in &ix.index_params {
        let Some(pg_query::NodeEnum::IndexElem(elem)) = param.node.as_ref() else {
            return None;
        };
        // An empty `name` means the key is an expression (`expr` is set
        // instead), and a non-empty `opclass` names an operator class — both
        // out of scope here (see the module doc comment). Bailing rather than
        // dropping either silently: an opclass changes which operators the
        // index can answer, so losing it would emit a wrong index, not just
        // an incomplete one.
        if elem.name.is_empty() || !elem.opclass.is_empty() {
            return None;
        }
        columns.push(IndexColumn {
            name: elem.name.clone(),
            is_expression: false,
            order: sort_order(elem.ordering),
            nulls_first: nulls_first(elem.nulls_ordering),
            opclass: None,
        });
    }

    let mut include = Vec::with_capacity(ix.index_including_params.len());
    for param in &ix.index_including_params {
        match param.node.as_ref() {
            Some(pg_query::NodeEnum::IndexElem(elem)) if !elem.name.is_empty() => {
                include.push(elem.name.clone());
            }
            // An INCLUDE column can only ever be a plain name in Postgres
            // grammar, so this arm is unreached in practice; kept as a safe
            // bailout rather than assumed away.
            _ => return None,
        }
    }

    let access_method = ix.access_method.to_lowercase();
    let index_type = (access_method != "btree" && !access_method.is_empty())
        .then(|| IndexType::from_amname(&ix.access_method));

    Some(IndexDef {
        name: (!ix.idxname.is_empty()).then(|| ix.idxname.clone()),
        columns,
        unique: ix.unique,
        index_type,
        predicate: None,
        include,
        nulls_not_distinct: ix.nulls_not_distinct,
        with_options: std::collections::BTreeMap::new(),
    })
}

/// `SortByDir` → our `SortOrder`; `SortbyDefault` (no explicit `ASC`/`DESC`)
/// is `None`, matching `tables::extract_index`'s treatment of sqlparser's
/// `asc: None`.
fn sort_order(ordering: i32) -> Option<SortOrder> {
    use pg_query::protobuf::SortByDir;
    match ordering {
        x if x == SortByDir::SortbyAsc as i32 => Some(SortOrder::Asc),
        x if x == SortByDir::SortbyDesc as i32 => Some(SortOrder::Desc),
        _ => None,
    }
}

/// `SortByNulls` → the tri-state `nulls_first`; the access method's default
/// (no explicit `NULLS FIRST`/`NULLS LAST`) is `None`.
fn nulls_first(nulls_ordering: i32) -> Option<bool> {
    use pg_query::protobuf::SortByNulls;
    match nulls_ordering {
        x if x == SortByNulls::SortbyNullsFirst as i32 => Some(true),
        x if x == SortByNulls::SortbyNullsLast as i32 => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        parse_matview(Entity::new(EntityType::MaterializedView, "app.m"), sql).unwrap()
    }

    fn body(sql: &str) -> String {
        parse(sql).writes.first().cloned().unwrap_or_default()
    }

    /// The whole point of this change: the author's SQL survives, rather than a
    /// parser's re-rendering of it.
    #[test]
    fn the_body_is_verbatim_not_re_rendered() {
        let e = body("set search_path to app;\ncreate materialized view m as\n  select a,\n         b\n  from t\nwith data;");
        assert_eq!(e, "select a,\n         b\n  from t");
    }

    #[test]
    fn with_no_data_is_not_part_of_the_body() {
        assert_eq!(body("create materialized view m as select a from t with no data;"), "select a from t");
    }

    #[test]
    fn a_missing_with_clause_still_yields_the_body() {
        assert_eq!(body("create materialized view m as select a from t;"), "select a from t");
    }

    /// The tokenizer must not mistake an `as` inside a string for the keyword.
    #[test]
    fn an_as_inside_a_string_literal_is_not_the_boundary() {
        let b = body("create materialized view m as select 'x as y' as lbl from t with data;");
        assert_eq!(b, "select 'x as y' as lbl from t");
    }

    #[test]
    fn a_comment_before_the_body_does_not_break_extraction() {
        let b = body("create materialized view m as -- a note\n  select a from t\nwith data;");
        assert!(b.contains("select a from t"), "got {b:?}");
    }

    #[test]
    fn a_trailing_create_index_is_not_part_of_the_body() {
        let b = body("create materialized view m as select a from t with data;\ncreate index i on m(a);");
        assert_eq!(b, "select a from t");
    }

    #[test]
    fn a_trailing_index_lands_in_table_def() {
        let e = parse("create materialized view m as select a from t with data;\ncreate unique index i on m(a);");
        let ix = e.table_def.as_ref().expect("table_def").indexes.clone();
        assert_eq!(ix.len(), 1, "got {ix:?}");
        assert!(ix[0].unique);
    }

    /// An opclass changes which operators the index can answer, so losing it
    /// silently would emit a *wrong* index rather than an incomplete one —
    /// this extractor must skip it (with a warning) instead of dropping it.
    #[test]
    fn an_index_with_an_explicit_opclass_is_skipped_not_silently_mangled() {
        let e = parse(
            "create materialized view m as select a from t with data;\n\
             create index i on m (a text_pattern_ops);",
        );
        let ix = e.table_def.as_ref().expect("table_def").indexes.clone();
        assert!(ix.is_empty(), "got {ix:?}");
        assert!(
            e.warnings.iter().any(|w| w.contains("\"i\"")),
            "expected a warning naming the skipped index, got {:?}",
            e.warnings
        );
    }

    #[test]
    fn relations_and_function_calls_become_references() {
        let e = parse("set search_path to app;\ncreate materialized view m as select app.myfn(a) from t with data;");
        assert!(e.refers.contains(&"app.t".to_string()), "got {:?}", e.refers);
        assert!(e.refers.contains(&"app.myfn".to_string()), "got {:?}", e.refers);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate materialized view m as select a from t with data;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create materialized view m as select a from t with data;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create materialized view m as select * from ;");
        assert!(!e.errors.is_empty());
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    #[test]
    fn a_file_declaring_no_matview_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty());
    }
}
