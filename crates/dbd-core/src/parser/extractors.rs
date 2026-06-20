use sqlparser::ast::{Expr, Set, Statement, Value};

use crate::entity::{EnumValue, Reference};

/// Extract search_path values from SET statements.
pub fn extract_search_paths(statements: &[Statement]) -> Vec<String> {
    for stmt in statements {
        if let Statement::Set(Set::SingleAssignment { variable, values, .. }) = stmt {
            let var_name = variable
                .0
                .iter()
                .filter_map(|part| part.as_ident())
                .map(|i| i.value.to_lowercase())
                .collect::<Vec<_>>()
                .join(".");
            if var_name == "search_path" {
                return values
                    .iter()
                    .filter_map(|v| match v {
                        Expr::Identifier(ident) => Some(ident.value.clone()),
                        Expr::Value(val) => match &val.value {
                            Value::SingleQuotedString(s) => Some(s.clone()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    vec!["public".to_string()]
}

/// Extract table references from a VIEW's SELECT query using the AST.
///
/// Walks the parsed query's FROM clauses to find table references directly,
/// avoiding the alias.column false positives from string-based regex matching.
pub fn extract_view_info(
    statements: &[Statement],
    search_paths: &[String],
) -> (Vec<Reference>, Vec<String>) {
    let mut references = Vec::new();
    let columns = Vec::new();
    let default_schema = search_paths.first().map(|s| s.as_str()).unwrap_or("public");

    for stmt in statements {
        if let Statement::CreateView(create_view) = stmt {
            extract_table_refs_from_query(&create_view.query, default_schema, &mut references);
        }
    }

    (references, columns)
}

/// Extract every table reference from a query, anywhere it appears.
///
/// Uses sqlparser's AST visitor to walk the *entire* query, so it sees table
/// references in any position: top-level FROM/JOIN, every branch of a compound
/// query (UNION / UNION ALL / INTERSECT / EXCEPT / MINUS), CTE bodies
/// (`WITH` / `WITH RECURSIVE`), derived tables (subqueries in FROM/JOIN),
/// scalar subqueries in the SELECT list, and IN / NOT IN / EXISTS predicates —
/// at arbitrary nesting depth.
///
/// CTE names are query-local, not real tables, so references that resolve to a
/// CTE (the recursive term, `SELECT ... FROM cte`, etc.) are dropped afterwards.
fn extract_table_refs_from_query(
    query: &sqlparser::ast::Query,
    default_schema: &str,
    refs: &mut Vec<Reference>,
) {
    for reference in collect_table_refs(query, default_schema) {
        if !refs.iter().any(|r| r.name == reference.name) {
            refs.push(reference);
        }
    }
}

/// Walk any AST node, collecting every table reference with query-local CTE
/// references filtered out. Works on a `Query`, a `Statement`, or any other
/// node that implements `Visit`.
fn collect_table_refs<N: sqlparser::ast::Visit>(node: &N, default_schema: &str) -> Vec<Reference> {
    let mut visitor = TableRefVisitor {
        default_schema,
        cte_names: Vec::new(),
        refs: Vec::new(),
    };
    let _ = node.visit(&mut visitor);

    // CTE references are unqualified, so they resolve to `{default_schema}.{name}`.
    // Drop those — they're query-local, not real tables.
    visitor
        .refs
        .into_iter()
        .filter(|r| {
            !visitor
                .cte_names
                .iter()
                .any(|name| r.name == format!("{default_schema}.{name}"))
        })
        .collect()
}

/// Qualify a relation `ObjectName` to `schema.table`: apply the default schema
/// to unqualified names, and drop references into system schemas.
fn qualify_relation(name: &sqlparser::ast::ObjectName, default_schema: &str) -> Option<String> {
    let parts: Vec<&str> = name
        .0
        .iter()
        .filter_map(|p| p.as_ident())
        .map(|i| i.value.as_str())
        .collect();

    if parts.len() >= 2 {
        if SYSTEM_SCHEMAS.contains(&parts[0]) {
            return None;
        }
        Some(format!("{}.{}", parts[0], parts[1]))
    } else {
        parts.first().map(|table| format!("{default_schema}.{table}"))
    }
}

/// AST visitor that collects table references and CTE names in a single pass.
struct TableRefVisitor<'a> {
    default_schema: &'a str,
    cte_names: Vec<String>,
    refs: Vec<Reference>,
}

impl sqlparser::ast::Visitor for TableRefVisitor<'_> {
    type Break = ();

    /// Record CTE names so their (query-local) references can be filtered out.
    fn pre_visit_query(
        &mut self,
        query: &sqlparser::ast::Query,
    ) -> core::ops::ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.cte_names.push(cte.alias.name.value.clone());
            }
        }
        core::ops::ControlFlow::Continue(())
    }

    /// Collect each relation (table) referenced anywhere in the query.
    fn pre_visit_relation(
        &mut self,
        name: &sqlparser::ast::ObjectName,
    ) -> core::ops::ControlFlow<Self::Break> {
        if let Some(qualified) = qualify_relation(name, self.default_schema)
            && !self.refs.iter().any(|r| r.name == qualified)
        {
            self.refs.push(Reference {
                name: qualified,
                ref_type: Some("table".to_string()),
            });
        }
        core::ops::ControlFlow::Continue(())
    }
}

/// Extract enum values from CREATE TYPE ... AS ENUM statements.
pub fn extract_enum_values(statements: &[Statement]) -> Vec<EnumValue> {
    for stmt in statements {
        if let Statement::CreateType { representation, .. } = stmt
            && let Some(sqlparser::ast::UserDefinedTypeRepresentation::Enum { labels }) = representation {
                return labels
                    .iter()
                    .map(|label| EnumValue {
                        name: label.value.clone(),
                        note: None,
                    })
                    .collect();
            }
    }
    Vec::new()
}

/// Extract read/write table dependencies from a function/procedure.
///
/// When the body is statically-parseable SQL (a `LANGUAGE sql` function), the
/// body is re-parsed and walked with the AST visitor — giving the same
/// precision as view extraction (sub-selects, set operations, CTEs, correct
/// read/write classification, and no false positives). Bodies sqlparser cannot
/// parse (PL/pgSQL, `CREATE PROCEDURE`) fall back to the regex scanner.
///
/// Dynamic SQL (`EXECUTE 'text'`) is intentionally NOT tracked: it is a runtime
/// dependency, not a compile-time one, so it never affects whether the object
/// can be created. The AST path ignores it for free (it is a string literal);
/// the regex fallback inherits the existing best-effort behavior.
pub fn extract_proc_refs(
    statements: &[Statement],
    raw_sql: &str,
    default_schema: &str,
) -> (Vec<String>, Vec<String>) {
    if let Some((reads, writes)) = extract_proc_refs_via_ast(statements, default_schema) {
        return (reads, writes);
    }
    // PL/pgSQL and other bodies sqlparser can't statically parse.
    extract_proc_reads_writes(raw_sql)
}

/// Try to extract reads/writes from a `LANGUAGE sql` function body via the AST.
///
/// Returns `Some` once at least one statically-parseable SQL body is found
/// (even if it references no tables), so the caller knows the AST path applied
/// and the regex fallback should be skipped. Returns `None` when no body could
/// be parsed as SQL (e.g. PL/pgSQL), leaving the caller to fall back.
fn extract_proc_refs_via_ast(
    statements: &[Statement],
    default_schema: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let dialect = sqlparser::dialect::PostgreSqlDialect {};
    let mut reads: Vec<String> = Vec::new();
    let mut writes: Vec<String> = Vec::new();
    let mut parsed_any = false;

    for stmt in statements {
        let Statement::CreateFunction(cf) = stmt else {
            continue;
        };
        // Only LANGUAGE sql bodies are statically-parseable SQL; PL/pgSQL is not.
        let is_sql = cf
            .language
            .as_ref()
            .is_some_and(|l| l.value.eq_ignore_ascii_case("sql"));
        if !is_sql {
            continue;
        }
        let Some(body_text) = function_body_text(cf) else {
            continue;
        };
        let Ok(body_stmts) = sqlparser::parser::Parser::parse_sql(&dialect, body_text) else {
            continue;
        };

        parsed_any = true;
        for body_stmt in &body_stmts {
            let targets = proc_write_targets(body_stmt, default_schema);
            for reference in collect_table_refs(body_stmt, default_schema) {
                if targets.contains(&reference.name) {
                    push_unique(&mut writes, reference.name);
                } else {
                    push_unique(&mut reads, reference.name);
                }
            }
            // A write target may not surface as a visited relation (e.g. an
            // INSERT target); make sure it's recorded.
            for target in targets {
                push_unique(&mut writes, target);
            }
        }
    }

    parsed_any.then_some((reads, writes))
}

/// Extract the raw SQL text of a function body (dollar-quoted or string literal).
fn function_body_text(cf: &sqlparser::ast::CreateFunction) -> Option<&str> {
    use sqlparser::ast::{CreateFunctionBody, Expr, Value};

    let body_expr = match cf.function_body.as_ref()? {
        CreateFunctionBody::AsBeforeOptions { body, .. } => body,
        CreateFunctionBody::AsAfterOptions(body) => body,
        _ => return None,
    };
    if let Expr::Value(vws) = body_expr {
        match &vws.value {
            Value::DollarQuotedString(dq) => return Some(&dq.value),
            Value::SingleQuotedString(s) => return Some(s),
            _ => {}
        }
    }
    None
}

/// Identify the table(s) a statement writes to (INSERT/UPDATE/DELETE targets).
fn proc_write_targets(stmt: &Statement, default_schema: &str) -> Vec<String> {
    use sqlparser::ast::{FromTable, TableFactor, TableObject};

    let mut targets = Vec::new();
    match stmt {
        Statement::Insert(insert) => {
            if let TableObject::TableName(name) = &insert.table
                && let Some(q) = qualify_relation(name, default_schema)
            {
                push_unique(&mut targets, q);
            }
        }
        Statement::Update(update) => {
            if let TableFactor::Table { name, .. } = &update.table.relation
                && let Some(q) = qualify_relation(name, default_schema)
            {
                push_unique(&mut targets, q);
            }
        }
        Statement::Delete(delete) => {
            let twjs = match &delete.from {
                FromTable::WithFromKeyword(v) | FromTable::WithoutKeyword(v) => v,
            };
            for twj in twjs {
                if let TableFactor::Table { name, .. } = &twj.relation
                    && let Some(q) = qualify_relation(name, default_schema)
                {
                    push_unique(&mut targets, q);
                }
            }
            for name in &delete.tables {
                if let Some(q) = qualify_relation(name, default_schema) {
                    push_unique(&mut targets, q);
                }
            }
        }
        _ => {}
    }
    targets
}

/// Push a value only if not already present (preserves insertion order).
fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// Fallback reads/writes scanner for bodies that aren't statically-parseable SQL.
///
/// `extract_proc_refs` handles `LANGUAGE sql` bodies via the AST; this regex
/// scan is the fallback for the rest.
///
/// WORKAROUND: sqlparser-plpgsql-body
/// Limitation: sqlparser captures PL/pgSQL function/procedure bodies as opaque
///             DollarQuotedString values. The PL/pgSQL inside is NOT parsed
///             into AST nodes — there are no Statement nodes for the body's
///             INSERT/SELECT/UPDATE/DELETE. (`CREATE PROCEDURE` also fails to
///             parse entirely in sqlparser 0.61.)
/// Impact:     Cannot use the AST for PL/pgSQL bodies; scan the text instead.
/// Caveats:    Text scanning only matches qualified `schema.table` names (misses
///             unqualified ones) and can over-match (e.g. `EXTRACT(... FROM x.y)`).
///             It is also blind to read/write classification beyond the keyword.
/// Check:      If sqlparser adds PL/pgSQL body parsing, walk the body's AST with
///             `collect_table_refs` instead (as `extract_proc_refs` already does
///             for `LANGUAGE sql`).
/// Alternative: pg_query crate has experimental parse_plpgsql() that returns JSON AST.
///
/// Note: dynamic SQL (`EXECUTE 'text'`) is a runtime dependency, not compile-time,
/// and is out of scope for dependency tracking. The text scan may still match
/// qualified names inside such literals — a known limitation of the fallback.
///
/// Patterns matched:
/// - SELECT ... FROM schema.table → read
/// - JOIN schema.table → read
/// - INSERT INTO schema.table → write
/// - UPDATE schema.table → write
/// - DELETE FROM schema.table → write
pub fn extract_proc_reads_writes(sql: &str) -> (Vec<String>, Vec<String>) {
    let mut reads = Vec::new();
    let mut writes = Vec::new();

    let lower = sql.to_lowercase();

    // Extract table names after FROM (reads)
    for cap in regex_table_after(&lower, r"(?i)\bfrom\s+") {
        if !reads.contains(&cap) {
            reads.push(cap);
        }
    }

    // Extract table names after JOIN (reads)
    for cap in regex_table_after(&lower, r"(?i)\bjoin\s+") {
        if !reads.contains(&cap) {
            reads.push(cap);
        }
    }

    // Extract table names after INSERT INTO (writes)
    for cap in regex_table_after(&lower, r"(?i)\binsert\s+into\s+") {
        if !writes.contains(&cap) {
            writes.push(cap);
        }
    }

    // Extract table names after UPDATE (writes)
    for cap in regex_table_after(&lower, r"(?i)\bupdate\s+") {
        if !writes.contains(&cap) {
            writes.push(cap);
        }
    }

    // Extract table names after DELETE FROM (writes)
    for cap in regex_table_after(&lower, r"(?i)\bdelete\s+from\s+") {
        if !writes.contains(&cap) {
            writes.push(cap);
        }
    }

    (reads, writes)
}

/// System schemas to exclude from references.
const SYSTEM_SCHEMAS: &[&str] = &["information_schema", "pg_catalog", "pg_toast"];

/// Find qualified table names (schema.table) after a SQL keyword pattern.
/// Excludes system schema references.
fn regex_table_after(sql: &str, pattern: &str) -> Vec<String> {
    let re = regex::Regex::new(&format!(r"{pattern}([a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*)"))
        .unwrap();
    re.captures_iter(sql)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|name| {
            let schema = name.split('.').next().unwrap_or("");
            !SYSTEM_SCHEMAS.contains(&schema)
        })
        .collect()
}


#[cfg(test)]
mod tests {
    use super::*;


    /// Parse a CREATE FUNCTION/PROCEDURE and extract its read/write deps.
    fn proc_refs(sql: &str) -> (Vec<String>, Vec<String>) {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(&dialect, sql).unwrap();
        extract_proc_refs(&stmts, sql, "public")
    }

    #[test]
    fn proc_sql_body_extracts_reads_via_ast() {
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ \
             SELECT id FROM real.orders WHERE id IN (SELECT id FROM real.allow) $$;",
        );
        assert!(reads.contains(&"real.orders".to_string()), "reads={reads:?}");
        assert!(reads.contains(&"real.allow".to_string()), "reads={reads:?}");
        assert!(writes.is_empty(), "writes={writes:?}");
    }

    #[test]
    fn proc_sql_body_resolves_unqualified_with_default_schema() {
        // Differentiator vs regex: unqualified table resolves to default schema.
        let (reads, _) = proc_refs(
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT id FROM orders $$;",
        );
        assert!(reads.contains(&"public.orders".to_string()), "reads={reads:?}");
    }

    #[test]
    fn proc_sql_body_classifies_insert_write_and_select_read() {
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$ \
             INSERT INTO sink.t SELECT id FROM (SELECT id FROM nested.src) s $$;",
        );
        assert!(writes.contains(&"sink.t".to_string()), "writes={writes:?}");
        assert!(reads.contains(&"nested.src".to_string()), "reads={reads:?}");
        assert!(!reads.contains(&"sink.t".to_string()), "write target leaked into reads: {reads:?}");
    }

    #[test]
    fn proc_sql_body_classifies_update_and_delete() {
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$ \
             UPDATE tgt.t SET x = 1 FROM ref.r WHERE t.id = r.id; \
             DELETE FROM del.t USING use.u WHERE t.id = u.id; $$;",
        );
        assert!(writes.contains(&"tgt.t".to_string()), "writes={writes:?}");
        assert!(writes.contains(&"del.t".to_string()), "writes={writes:?}");
        assert!(reads.contains(&"ref.r".to_string()), "reads={reads:?}");
        assert!(reads.contains(&"use.u".to_string()), "reads={reads:?}");
    }

    #[test]
    fn proc_sql_body_no_false_positive_from_extract() {
        // Differentiator vs regex: EXTRACT(... FROM col) must NOT yield a table.
        let (reads, _) = proc_refs(
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ \
             SELECT EXTRACT(epoch FROM ts.created) FROM real.events $$;",
        );
        assert!(reads.contains(&"real.events".to_string()), "reads={reads:?}");
        assert!(
            !reads.iter().any(|r| r.ends_with(".created")),
            "EXTRACT false positive: {reads:?}"
        );
    }

    #[test]
    fn proc_plpgsql_body_falls_back_to_regex() {
        // PL/pgSQL isn't statically parseable SQL → regex fallback still works.
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION g() RETURNS void LANGUAGE plpgsql AS $$ \
             BEGIN INSERT INTO sink.t SELECT id FROM real.src; END; $$;",
        );
        assert!(reads.contains(&"real.src".to_string()), "reads={reads:?}");
        assert!(writes.contains(&"sink.t".to_string()), "writes={writes:?}");
    }

    #[test]
    fn extract_proc_reads() {
        let sql = r#"
            CREATE PROCEDURE foo()
            AS $$ BEGIN
                SELECT * FROM staging.lookups;
                INSERT INTO config.lookups(name) SELECT name FROM staging.lookups;
            END; $$
        "#;
        let (reads, writes) = extract_proc_reads_writes(sql);
        assert!(reads.contains(&"staging.lookups".to_string()));
        assert!(writes.contains(&"config.lookups".to_string()));
    }

    #[test]
    fn extract_proc_deduplicates() {
        let sql = r#"
            $$ BEGIN
                SELECT * FROM staging.lookups;
                SELECT name FROM staging.lookups;
            END; $$
        "#;
        let (reads, _) = extract_proc_reads_writes(sql);
        assert_eq!(
            reads.iter().filter(|r| *r == "staging.lookups").count(),
            1
        );
    }

    #[test]
    fn extract_enum_from_statements() {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(
            &dialect,
            "CREATE TYPE status AS ENUM ('active', 'inactive');",
        )
        .unwrap();
        let values = extract_enum_values(&stmts);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].name, "active");
        assert_eq!(values[1].name, "inactive");
    }

    /// Parse SQL and extract view references using the default `public` schema.
    fn view_refs(sql: &str) -> Vec<String> {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(&dialect, sql).unwrap();
        let (refs, _) = extract_view_info(&stmts, &["public".to_string()]);
        refs.into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn extract_view_collects_derived_table_in_from() {
        let names = view_refs("CREATE VIEW v AS SELECT * FROM (SELECT id FROM real.orders) sub;");
        assert!(names.contains(&"real.orders".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_subquery_in_join() {
        let names = view_refs(
            "CREATE VIEW v AS \
             SELECT * FROM main.a a JOIN (SELECT id FROM real.b) x ON a.id = x.id;",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.b".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_scalar_subquery_in_projection() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id, (SELECT max(amt) FROM real.payments) FROM main.a;",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.payments".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_where_in_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a WHERE id IN (SELECT id FROM real.allow);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.allow".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_where_not_in_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a WHERE id NOT IN (SELECT id FROM real.block);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.block".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_where_exists_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a \
             WHERE EXISTS (SELECT 1 FROM real.flags f WHERE f.id = a.id);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.flags".to_string()), "{names:?}");
        // Correlated table aliases (`a`, `f`) must not be collected as tables.
        assert!(!names.contains(&"public.a".to_string()), "alias leaked: {names:?}");
        assert!(!names.contains(&"public.f".to_string()), "alias leaked: {names:?}");
    }

    #[test]
    fn extract_view_collects_where_scalar_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a WHERE id = (SELECT max(id) FROM real.cap);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.cap".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_deeply_nested_subqueries() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a \
             WHERE id IN (SELECT id FROM mid.b WHERE id IN (SELECT id FROM deep.c));",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"mid.b".to_string()), "{names:?}");
        assert!(names.contains(&"deep.c".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_both_except_branches() {
        let names = view_refs("CREATE VIEW v AS SELECT id FROM a.t1 EXCEPT SELECT id FROM b.t2;");
        assert!(names.contains(&"a.t1".to_string()), "{names:?}");
        assert!(names.contains(&"b.t2".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_both_minus_branches() {
        // MINUS is Oracle's spelling of EXCEPT; sqlparser parses it as a set op,
        // so the SetOperation arm handles it like any other compound query.
        let names = view_refs("CREATE VIEW v AS SELECT id FROM a.t1 MINUS SELECT id FROM b.t2;");
        assert!(names.contains(&"a.t1".to_string()), "{names:?}");
        assert!(names.contains(&"b.t2".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_tables_inside_recursive_cte() {
        // Recursive CTEs UNION inside the CTE body, and the real table lives in
        // the WITH clause (a sibling of query.body). The recursive self-reference
        // (anc) must NOT be collected as a real table.
        let names = view_refs(
            "CREATE VIEW v AS \
             WITH RECURSIVE anc AS ( \
                SELECT id, parent_id FROM org.nodes WHERE parent_id IS NULL \
                UNION ALL \
                SELECT n.id, n.parent_id FROM org.nodes n JOIN anc a ON n.parent_id = a.id \
             ) SELECT * FROM anc;",
        );
        assert!(
            names.contains(&"org.nodes".to_string()),
            "real table inside recursive CTE missed: {names:?}"
        );
        assert!(
            !names.contains(&"public.anc".to_string()),
            "CTE name leaked as a table ref: {names:?}"
        );
    }

    #[test]
    fn extract_view_collects_tables_inside_plain_cte() {
        let names = view_refs(
            "CREATE VIEW v AS \
             WITH recent AS (SELECT id FROM sales.orders) \
             SELECT * FROM recent r JOIN cust.customers c ON r.id = c.id;",
        );
        assert!(
            names.contains(&"sales.orders".to_string()),
            "CTE-internal table missed: {names:?}"
        );
        assert!(names.contains(&"cust.customers".to_string()), "{names:?}");
        assert!(
            !names.contains(&"public.recent".to_string()),
            "CTE name leaked as a table ref: {names:?}"
        );
    }

    #[test]
    fn extract_view_handles_chained_ctes() {
        // CTE `b` references CTE `a`; neither CTE name should appear as a table,
        // and the real tables from both bodies must be collected.
        let names = view_refs(
            "CREATE VIEW v AS \
             WITH a AS (SELECT id FROM raw.events), \
                  b AS (SELECT id FROM a) \
             SELECT * FROM b JOIN dim.users u ON b.id = u.id;",
        );
        assert!(names.contains(&"raw.events".to_string()), "{names:?}");
        assert!(names.contains(&"dim.users".to_string()), "{names:?}");
        assert!(!names.contains(&"public.a".to_string()), "CTE name `a` leaked: {names:?}");
        assert!(!names.contains(&"public.b".to_string()), "CTE name `b` leaked: {names:?}");
    }

    #[test]
    fn extract_view_simple_select_uses_default_schema() {
        let names = view_refs("CREATE VIEW v AS SELECT id FROM orders;");
        assert_eq!(names, vec!["public.orders".to_string()]);
    }

    #[test]
    fn extract_view_collects_all_union_branches() {
        // Regression: a compound query (UNION) must collect table refs from
        // EVERY branch, not just the first. Before the SetOperation fix the
        // top-level body was a SetOperation (not a Select), so no refs at all
        // were collected from UNION views.
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT id, total FROM sales.orders \
             UNION ALL \
             SELECT id, total FROM archive.orders;",
        );
        assert!(
            names.contains(&"sales.orders".to_string()),
            "missing first branch: {names:?}"
        );
        assert!(
            names.contains(&"archive.orders".to_string()),
            "missing second UNION branch (the bug): {names:?}"
        );
    }

    #[test]
    fn extract_view_collects_nested_and_parenthesized_union_branches() {
        // Exercises both SetOperation recursion and the parenthesized
        // SetExpr::Query branch.
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT id FROM s1.t1 \
             UNION \
             (SELECT id FROM s2.t2 INTERSECT SELECT id FROM s3.t3);",
        );
        assert!(names.contains(&"s1.t1".to_string()), "{names:?}");
        assert!(names.contains(&"s2.t2".to_string()), "{names:?}");
        assert!(names.contains(&"s3.t3".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_joins_within_union_branches() {
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT * FROM a.left_t l JOIN a.right_t r ON l.id = r.id \
             UNION ALL \
             SELECT * FROM b.other o;",
        );
        assert!(names.contains(&"a.left_t".to_string()), "{names:?}");
        assert!(names.contains(&"a.right_t".to_string()), "{names:?}");
        assert!(names.contains(&"b.other".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_excludes_system_schemas_in_union() {
        // System-schema tables stay excluded even when reached through a
        // UNION branch.
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT id FROM app.widgets \
             UNION ALL \
             SELECT oid FROM pg_catalog.pg_class;",
        );
        assert!(names.contains(&"app.widgets".to_string()), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("pg_catalog.")),
            "system schema leaked: {names:?}"
        );
    }

    #[test]
    fn extract_search_paths_from_set() {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(
            &dialect,
            "SET search_path TO config, extensions;",
        )
        .unwrap();
        let paths = extract_search_paths(&stmts);
        assert_eq!(paths, vec!["config", "extensions"]);
    }

    #[test]
    fn search_paths_default_to_public() {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts =
            sqlparser::parser::Parser::parse_sql(&dialect, "CREATE TABLE foo (id int);").unwrap();
        let paths = extract_search_paths(&stmts);
        assert_eq!(paths, vec!["public"]);
    }
}
