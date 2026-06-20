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
    use sqlparser::ast::Visit;

    let mut visitor = TableRefVisitor {
        default_schema,
        cte_names: Vec::new(),
        refs: Vec::new(),
    };
    let _ = query.visit(&mut visitor);

    for reference in visitor.refs {
        // CTE references are unqualified, so they resolve to
        // `{default_schema}.{name}`. Drop those — they're query-local.
        let is_cte = visitor
            .cte_names
            .iter()
            .any(|name| reference.name == format!("{default_schema}.{name}"));
        if is_cte {
            continue;
        }
        if !refs.iter().any(|r| r.name == reference.name) {
            refs.push(reference);
        }
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
        let parts: Vec<&str> = name
            .0
            .iter()
            .filter_map(|p| p.as_ident())
            .map(|i| i.value.as_str())
            .collect();

        let qualified = if parts.len() >= 2 {
            let schema = parts[0];
            let table = parts[1];
            if SYSTEM_SCHEMAS.contains(&schema) {
                return core::ops::ControlFlow::Continue(());
            }
            format!("{schema}.{table}")
        } else if let Some(table) = parts.first() {
            format!("{}.{table}", self.default_schema)
        } else {
            return core::ops::ControlFlow::Continue(());
        };

        if !self.refs.iter().any(|r| r.name == qualified) {
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

/// Extract reads and writes from a procedure/function body using pattern matching.
///
/// WORKAROUND: sqlparser-plpgsql-body
/// Limitation: sqlparser captures function/procedure bodies as opaque
///             DollarQuotedString values. The PL/pgSQL inside is NOT parsed
///             into AST nodes — there are no Statement nodes for the body's
///             INSERT/SELECT/UPDATE/DELETE.
/// Impact:     Cannot use AST to extract table reads/writes from procedures.
/// Fix:        Scan the raw body text for DML patterns (FROM, JOIN, INSERT INTO, etc.)
/// Check:      If sqlparser adds PL/pgSQL body parsing, extract reads/writes from
///             the body's AST instead. Look for CreateFunction.function_body containing
///             parsed Statement nodes rather than a DollarQuotedString.
/// Alternative: pg_query crate has experimental parse_plpgsql() that returns JSON AST.
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
