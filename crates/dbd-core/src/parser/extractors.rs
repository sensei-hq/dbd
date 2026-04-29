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

/// Extract table references from a VIEW's SELECT query.
pub fn extract_view_info(
    statements: &[Statement],
    search_paths: &[String],
) -> (Vec<Reference>, Vec<String>) {
    let mut references = Vec::new();
    let columns = Vec::new();

    for stmt in statements {
        if let Statement::CreateView(create_view) = stmt {
            // Extract table references from the query body string
            let query_str = create_view.query.to_string();
            let tables = extract_tables_from_sql(&query_str, search_paths);
            for table in tables {
                references.push(Reference {
                    name: table,
                    ref_type: Some("table".to_string()),
                });
            }
        }
    }

    (references, columns)
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
/// Function bodies are opaque strings (dollar-quoted). We scan for DML patterns:
/// - SELECT ... FROM schema.table → read
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

/// Find qualified table names (schema.table) after a SQL keyword pattern.
fn regex_table_after(sql: &str, pattern: &str) -> Vec<String> {
    let re = regex::Regex::new(&format!(r"{pattern}([a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*)"))
        .unwrap();
    re.captures_iter(sql)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
        .collect()
}

/// Extract table names from a SQL string using simple pattern matching.
/// Qualifies unqualified names using search_paths.
fn extract_tables_from_sql(sql: &str, search_paths: &[String]) -> Vec<String> {
    let lower = sql.to_lowercase();
    let mut tables = Vec::new();
    let default_schema = search_paths.first().map(|s| s.as_str()).unwrap_or("public");

    // SQL keywords that should not be treated as table names
    let sql_keywords = [
        "select", "from", "where", "and", "or", "not", "in", "on", "as", "is",
        "null", "true", "false", "inner", "outer", "left", "right", "cross",
        "join", "set", "into", "values", "update", "delete", "insert", "create",
        "table", "view", "index", "exists", "between", "like", "order", "by",
        "group", "having", "limit", "offset", "union", "all", "distinct",
        "case", "when", "then", "else", "end", "cast", "coalesce", "current_user",
        "now", "trim", "excluded", "conflict", "do", "begin", "replace", "function",
        "procedure",
    ];

    // Match qualified names: schema.table
    let qualified_re =
        regex::Regex::new(r"\b([a-z_][a-z0-9_]*)\.([a-z_][a-z0-9_]*)\b").unwrap();
    for cap in qualified_re.captures_iter(&lower) {
        let schema = cap.get(1).unwrap().as_str();
        let table = cap.get(2).unwrap().as_str();
        // Skip common non-table patterns (aliases like lv.id, lkp.name)
        if ["lv", "lkp", "stg", "t", "s"].contains(&schema) {
            continue; // table aliases
        }
        let qualified = format!("{schema}.{table}");
        if !tables.contains(&qualified) {
            tables.push(qualified);
        }
    }

    // Match unqualified table names after FROM and JOIN keywords.
    // Qualify them with the default search_path schema.
    let unqualified_re = regex::Regex::new(
        r"(?i)\b(?:from|join)\s+([a-z_][a-z0-9_]*)\b"
    ).unwrap();
    for cap in unqualified_re.captures_iter(&lower) {
        let name = cap.get(1).unwrap().as_str();
        // Skip SQL keywords and common aliases
        if sql_keywords.contains(&name) {
            continue;
        }
        let qualified = format!("{default_schema}.{name}");
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
