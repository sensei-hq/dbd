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

/// Recursively extract table references from a query's FROM/JOIN clauses.
fn extract_table_refs_from_query(
    query: &sqlparser::ast::Query,
    default_schema: &str,
    refs: &mut Vec<Reference>,
) {
    if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
        for table_with_joins in &select.from {
            extract_table_ref_from_factor(&table_with_joins.relation, default_schema, refs);
            for join in &table_with_joins.joins {
                extract_table_ref_from_factor(&join.relation, default_schema, refs);
            }
        }
    }
}

/// Extract a table name from a TableFactor AST node.
fn extract_table_ref_from_factor(
    factor: &sqlparser::ast::TableFactor,
    default_schema: &str,
    refs: &mut Vec<Reference>,
) {
    if let sqlparser::ast::TableFactor::Table { name, .. } = factor {
        let parts: Vec<&str> = name.0.iter()
            .filter_map(|p| p.as_ident())
            .map(|i| i.value.as_str())
            .collect();

        let qualified = if parts.len() >= 2 {
            let schema = parts[0];
            let table = parts[1];
            if SYSTEM_SCHEMAS.contains(&schema) {
                return;
            }
            format!("{schema}.{table}")
        } else if let Some(table) = parts.first() {
            format!("{default_schema}.{table}")
        } else {
            return;
        };

        if !refs.iter().any(|r| r.name == qualified) {
            refs.push(Reference {
                name: qualified,
                ref_type: Some("table".to_string()),
            });
        }
    }
}

/// Extract enum values from CREATE TYPE ... AS ENUM statements.
pub fn extract_enum_values(statements: &[Statement]) -> Vec<EnumValue> {
    for stmt in statements {
        if let Statement::CreateType { representation, .. } = stmt {
            if let Some(sqlparser::ast::UserDefinedTypeRepresentation::Enum { labels }) = representation {
                return labels
                    .iter()
                    .map(|label| EnumValue {
                        name: label.value.clone(),
                        note: None,
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Extract reads and writes from a procedure/function body using pattern matching.
///
/// Why regex instead of sqlparser AST?
/// - sqlparser 0.61 does not support `CREATE [OR REPLACE] PROCEDURE` (parse fails)
/// - For functions, sqlparser captures the body as an opaque `DollarQuotedString` —
///   the PL/pgSQL inside is not parsed into AST nodes
/// - Both cases require scanning the body text for DML patterns
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

/// Extract table names from a SQL string by looking at structural positions only.
///
/// Only extracts names after FROM, JOIN, INTO, UPDATE — positions where table names appear.
/// Does NOT scan the entire SQL for schema.table patterns, which would match alias.column.
fn extract_tables_from_sql(sql: &str, search_paths: &[String]) -> Vec<String> {
    let lower = sql.to_lowercase();
    let mut tables = Vec::new();
    let default_schema = search_paths.first().map(|s| s.as_str()).unwrap_or("public");

    let sql_keywords = [
        "select", "from", "where", "and", "or", "not", "in", "on", "as", "is",
        "null", "true", "false", "inner", "outer", "left", "right", "cross",
        "join", "set", "into", "values", "update", "delete", "insert", "create",
        "table", "view", "index", "exists", "between", "like", "order", "by",
        "group", "having", "limit", "offset", "union", "all", "distinct",
        "case", "when", "then", "else", "end", "cast", "coalesce", "current_user",
        "now", "trim", "excluded", "conflict", "do", "begin", "replace", "function",
        "procedure", "lateral",
    ];

    // System schemas that should never be treated as project references
    let system_schemas = [
        "information_schema", "pg_catalog", "pg_toast",
    ];

    // Only match table names in structural positions: after FROM, JOIN, UPDATE, INTO
    // Handles both qualified (schema.table) and unqualified (table) names.
    let table_position_re = regex::Regex::new(
        r"(?i)\b(?:from|join|update|into)\s+([a-z_][a-z0-9_]*(?:\.[a-z_][a-z0-9_]*)?)"
    ).unwrap();

    for cap in table_position_re.captures_iter(&lower) {
        let name = cap.get(1).unwrap().as_str();

        // Skip SQL keywords
        if sql_keywords.contains(&name) {
            continue;
        }

        let qualified = if name.contains('.') {
            // Already qualified — check for system schemas
            let schema = name.split('.').next().unwrap_or("");
            if system_schemas.contains(&schema) {
                continue;
            }
            name.to_string()
        } else {
            // Unqualified — apply search_path
            format!("{default_schema}.{name}")
        };

        if !tables.contains(&qualified) {
            tables.push(qualified);
        }
    }

    tables
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
