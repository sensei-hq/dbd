use std::collections::HashSet;

use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::config::{CommaStyle, FormatConfig, KeywordCase};

/// Format a DDL file according to the given configuration.
///
/// Splits the input into statement blocks, formats each individually, and
/// reassembles. Unparseable statements are preserved with keyword-case
/// transformation only.
pub fn format_ddl(input: &str, config: &FormatConfig) -> String {
    let blocks = split_statements(input);
    let mut output_parts: Vec<String> = Vec::new();

    for block in &blocks {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }
        let formatted = format_statement_block(trimmed, config);
        output_parts.push(formatted);
    }

    let mut result = output_parts.join("\n\n");
    // Ensure trailing newline
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Split input SQL into statement blocks.
///
/// Respects `$$` delimited function bodies: semicolons inside `$$` blocks
/// do not split statements.
fn split_statements(input: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        current.push(c);

        if c == '$' && chars.peek() == Some(&'$') {
            current.push(chars.next().unwrap());
            in_dollar_quote = !in_dollar_quote;
            continue;
        }

        if c == ';' && !in_dollar_quote {
            blocks.push(current.clone());
            current.clear();
        }
    }

    // Remaining text (trailing content without semicolon)
    let remaining = current.trim();
    if !remaining.is_empty() {
        blocks.push(current);
    }

    blocks
}

/// Format a single statement block (including its trailing semicolon).
fn format_statement_block(block: &str, config: &FormatConfig) -> String {
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Check for $$ delimited function/procedure body
    if contains_dollar_quote(trimmed) {
        return format_dollar_quoted(trimmed, config);
    }

    // Try parsing with sqlparser
    let dialect = PostgreSqlDialect {};
    let sql_to_parse = ensure_semicolon(trimmed);
    match Parser::parse_sql(&dialect, &sql_to_parse) {
        Ok(statements) if !statements.is_empty() => {
            format_parsed_statements(&statements, trimmed, config)
        }
        _ => {
            // Unparseable: apply keyword case only
            let result = apply_keyword_case(trimmed, &config.keyword_case);
            ensure_semicolon(&result)
        }
    }
}

/// Format parsed statements, dispatching to specific formatters.
fn format_parsed_statements(
    statements: &[Statement],
    original: &str,
    config: &FormatConfig,
) -> String {
    // We typically get one statement per block after splitting
    if statements.len() == 1 {
        match &statements[0] {
            Statement::CreateTable(ct) => {
                return format_create_table(ct, config);
            }
            Statement::CreateIndex(ci) => {
                return format_create_index(ci, config);
            }
            _ => {}
        }
    }

    // For SET, COMMENT ON, and other statements: regex-based formatting
    let upper = original.trim().to_uppercase();
    if upper.starts_with("SET") {
        return format_set_statement(original, config);
    }
    if upper.starts_with("COMMENT") {
        return format_comment_on(original, config);
    }

    // Fallback: keyword case transformation
    let result = apply_keyword_case(original, &config.keyword_case);
    ensure_semicolon(&result)
}

fn contains_dollar_quote(s: &str) -> bool {
    s.contains("$$")
}

/// Format a $$ delimited statement (function/procedure).
/// Format the header, preserve the body verbatim.
fn format_dollar_quoted(block: &str, config: &FormatConfig) -> String {
    if let Some(first_dollar) = block.find("$$") {
        let header = &block[..first_dollar];
        let rest = &block[first_dollar..];
        let formatted_header = apply_keyword_case(header, &config.keyword_case);

        // Find the closing $$ after the opening $$
        let after_open = first_dollar + 2;
        if let Some(close_pos) = block[after_open..].find("$$") {
            let body = &block[after_open..after_open + close_pos];
            let trailer = &block[after_open + close_pos + 2..];
            let formatted_trailer = apply_keyword_case(trailer, &config.keyword_case);
            let result = format!("{}$${body}$${}", formatted_header, formatted_trailer.trim_end());
            ensure_semicolon(&result)
        } else {
            // Only opening $$, no closing — preserve as-is with keyword case on header
            format!("{formatted_header}{rest}")
        }
    } else {
        apply_keyword_case(block, &config.keyword_case)
    }
}

// ── CREATE TABLE formatter ──────────────────────────────

fn format_create_table(
    ct: &sqlparser::ast::CreateTable,
    config: &FormatConfig,
) -> String {
    let mut out = String::new();

    // Header: create table [if not exists] <name>
    let kw = |s: &str| match config.keyword_case {
        KeywordCase::Lower => s.to_lowercase(),
        KeywordCase::Upper => s.to_uppercase(),
        KeywordCase::Preserve => s.to_string(),
    };

    out.push_str(&kw("CREATE TABLE"));
    if ct.if_not_exists {
        out.push_str(&format!(" {}", kw("IF NOT EXISTS")));
    }
    out.push_str(&format!(" {} (\n", ct.name.to_string().to_lowercase()));

    let indent = " ".repeat(config.indent);
    let type_col = config.type_alignment;

    // Format columns
    for (i, col) in ct.columns.iter().enumerate() {
        let col_name = col.name.value.to_lowercase();
        let type_str = format_column_type(&col.data_type, config);
        let constraints_str = format_column_constraints(&col.options, config);

        let line = if i == 0 {
            match config.comma_style {
                CommaStyle::Leading => {
                    format_column_line(&indent, &col_name, &type_str, &constraints_str, type_col)
                }
                CommaStyle::Trailing => {
                    let base = format_column_line(&indent, &col_name, &type_str, &constraints_str, type_col);
                    if i < ct.columns.len() - 1 || !ct.constraints.is_empty() {
                        format!("{base},")
                    } else {
                        base
                    }
                }
            }
        } else {
            match config.comma_style {
                CommaStyle::Leading => {
                    // Leading comma: ", name   type"
                    let prefix = format!("{}, ", &indent[..indent.len().saturating_sub(2)]);
                    // The column name starts after ", " which is at indent position
                    let name_with_pad = pad_to_width(&col_name, type_col - config.indent);
                    format!("{prefix}{name_with_pad}{type_str}{constraints_str}")
                }
                CommaStyle::Trailing => {
                    let base = format_column_line(&indent, &col_name, &type_str, &constraints_str, type_col);
                    if i < ct.columns.len() - 1 || !ct.constraints.is_empty() {
                        format!("{base},")
                    } else {
                        base
                    }
                }
            }
        };

        out.push_str(&line);
        out.push('\n');
    }

    // Format table-level constraints
    for (i, constraint) in ct.constraints.iter().enumerate() {
        let constraint_str = format_table_constraint(constraint, config);
        let line = match config.comma_style {
            CommaStyle::Leading => {
                if ct.columns.is_empty() && i == 0 {
                    format!("{indent}{constraint_str}")
                } else {
                    let prefix = format!("{}, ", &indent[..indent.len().saturating_sub(2)]);
                    format!("{prefix}{constraint_str}")
                }
            }
            CommaStyle::Trailing => {
                let base = format!("{indent}{constraint_str}");
                if i < ct.constraints.len() - 1 {
                    format!("{base},")
                } else {
                    base
                }
            }
        };
        out.push_str(&line);
        out.push('\n');
    }

    out.push_str(");");
    out
}

fn format_column_line(
    indent: &str,
    col_name: &str,
    type_str: &str,
    constraints_str: &str,
    type_col: usize,
) -> String {
    let name_with_pad = pad_to_width(col_name, type_col - indent.len());
    format!("{indent}{name_with_pad}{type_str}{constraints_str}")
}

fn pad_to_width(s: &str, width: usize) -> String {
    if s.len() >= width {
        format!("{s} ")
    } else {
        format!("{s}{}", " ".repeat(width - s.len()))
    }
}

fn format_column_type(
    data_type: &sqlparser::ast::DataType,
    config: &FormatConfig,
) -> String {
    let raw = data_type.to_string();
    apply_keyword_case(&raw, &config.keyword_case)
}

fn format_column_constraints(
    options: &[sqlparser::ast::ColumnOptionDef],
    config: &FormatConfig,
) -> String {
    let kw = |s: &str| match config.keyword_case {
        KeywordCase::Lower => s.to_lowercase(),
        KeywordCase::Upper => s.to_uppercase(),
        KeywordCase::Preserve => s.to_string(),
    };

    let mut parts = Vec::new();
    for opt in options {
        let s = match &opt.option {
            sqlparser::ast::ColumnOption::PrimaryKey(_) => {
                kw("PRIMARY KEY")
            }
            sqlparser::ast::ColumnOption::Unique(_) => {
                kw("UNIQUE")
            }
            sqlparser::ast::ColumnOption::NotNull => kw("NOT NULL"),
            sqlparser::ast::ColumnOption::Null => kw("NULL"),
            sqlparser::ast::ColumnOption::Default(expr) => {
                let expr_str = apply_keyword_case(&expr.to_string(), &config.keyword_case);
                format!("{} {expr_str}", kw("DEFAULT"))
            }
            sqlparser::ast::ColumnOption::ForeignKey(fk_constraint) => {
                let cols: Vec<String> = fk_constraint
                    .referred_columns
                    .iter()
                    .map(|c| c.value.to_lowercase())
                    .collect();
                let col_list = cols.join(", ");
                format!(
                    "{} {}({})",
                    kw("REFERENCES"),
                    fk_constraint.foreign_table.to_string().to_lowercase(),
                    col_list
                )
            }
            sqlparser::ast::ColumnOption::Check(expr) => {
                let expr_str = apply_keyword_case(&expr.to_string(), &config.keyword_case);
                format!("{} ({expr_str})", kw("CHECK"))
            }
            _ => {
                // Fallback: render via Display and apply keyword case
                let raw = format!("{}", opt.option);
                apply_keyword_case(&raw, &config.keyword_case)
            }
        };
        parts.push(s);
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(" "))
    }
}

fn format_table_constraint(
    constraint: &sqlparser::ast::TableConstraint,
    config: &FormatConfig,
) -> String {
    let raw = constraint.to_string();
    apply_keyword_case(&raw, &config.keyword_case)
}

// ── CREATE INDEX formatter ──────────────────────────────

fn format_create_index(
    ci: &sqlparser::ast::CreateIndex,
    config: &FormatConfig,
) -> String {
    let kw = |s: &str| match config.keyword_case {
        KeywordCase::Lower => s.to_lowercase(),
        KeywordCase::Upper => s.to_uppercase(),
        KeywordCase::Preserve => s.to_string(),
    };

    let mut out = String::new();
    out.push_str(&kw("CREATE"));

    if ci.unique {
        out.push_str(&format!(" {}", kw("UNIQUE")));
    }

    out.push_str(&format!(" {}", kw("INDEX")));

    if ci.if_not_exists {
        out.push_str(&format!(" {}", kw("IF NOT EXISTS")));
    }

    if let Some(ref name) = ci.name {
        let name_str = name.0.iter()
            .filter_map(|part| part.as_ident())
            .map(|i| i.value.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        out.push_str(&format!(" {name_str}"));
    }

    out.push_str(&format!(
        " {} {}",
        kw("ON"),
        ci.table_name.to_string().to_lowercase()
    ));

    // Format columns
    let col_strs: Vec<String> = ci
        .columns
        .iter()
        .map(|c| apply_keyword_case(&c.to_string(), &config.keyword_case))
        .collect();
    out.push_str(&format!("({})", col_strs.join(", ")));

    out.push(';');
    out
}

// ── SET statement formatter ─────────────────────────────

fn format_set_statement(original: &str, config: &FormatConfig) -> String {
    let result = apply_keyword_case(original, &config.keyword_case);
    ensure_semicolon(&result)
}

// ── COMMENT ON formatter ────────────────────────────────

fn format_comment_on(original: &str, config: &FormatConfig) -> String {
    let result = apply_keyword_case(original, &config.keyword_case);
    ensure_semicolon(&result)
}

// ── Keyword case transformation ─────────────────────────

/// SQL keywords for case transformation.
fn sql_keywords() -> HashSet<&'static str> {
    [
        "ADD", "ALL", "ALTER", "AND", "ANY", "AS", "ASC", "BETWEEN", "BIGINT", "BIGSERIAL",
        "BOOLEAN", "BY", "CASCADE", "CASE", "CHAR", "CHARACTER", "CHECK", "COLUMN",
        "COMMENT", "CONSTRAINT", "CREATE", "CROSS", "CURRENT", "DATE", "DECIMAL",
        "DEFAULT", "DELETE", "DESC", "DISTINCT", "DOUBLE", "DROP", "ELSE", "END",
        "ENUM", "EXCEPT", "EXECUTE", "EXISTS", "FALSE", "FETCH", "FLOAT", "FOR",
        "FOREIGN", "FROM", "FULL", "FUNCTION", "GRANT", "GROUP", "HAVING", "IF",
        "IN", "INDEX", "INNER", "INSERT", "INT", "INTEGER", "INTERSECT", "INTO",
        "IS", "JOIN", "KEY", "LANGUAGE", "LEFT", "LIKE", "LIMIT", "MATERIALIZED",
        "NOT", "NULL", "NUMERIC", "OF", "OFFSET", "ON", "OR", "ORDER",
        "OUTER", "PERFORM", "PRECISION", "PRIMARY", "PROCEDURE", "REAL", "REFERENCES",
        "REPLACE", "RETURNS", "REVOKE", "RIGHT", "ROLE", "ROLLBACK", "ROW", "SCHEMA",
        "SELECT", "SERIAL", "SET", "SMALLINT", "SMALLSERIAL", "TABLE", "TEXT", "THEN",
        "TIME", "TIMESTAMP", "TO", "TRIGGER", "TRUE", "TYPE", "UNION", "UNIQUE",
        "UPDATE", "USING", "UUID", "VALUES", "VARCHAR", "VARYING", "VIEW", "VOID",
        "WHEN", "WHERE", "WITH", "WITHOUT", "ZONE", "SEARCH_PATH",
        "BEGIN", "DECLARE", "RETURN", "RAISE", "NOTICE", "EXCEPTION", "FOUND",
        "LOOP", "WHILE", "EXIT", "CONTINUE",
        "PLPGSQL", "SQL",
        "SECURITY", "DEFINER", "INVOKER",
        "VOLATILE", "STABLE", "IMMUTABLE",
        "STRICT", "CALLED", "INPUT",
        "PARALLEL", "SAFE", "UNSAFE", "RESTRICTED",
        "OWNED", "NONE", "COST", "ROWS",
        "EACH", "BEFORE", "AFTER", "INSTEAD", "STATEMENT", "NEW", "OLD",
        "POLICY", "ENABLE", "FORCE",
        "RENAME", "OWNER", "TABLESPACE", "INHERITS", "PARTITION",
        "ONLY", "RECURSIVE", "TEMPORARY", "TEMP", "UNLOGGED",
        "DEFERRABLE", "INITIALLY", "DEFERRED", "IMMEDIATE",
        "EXCLUDE", "INCLUDE", "NULLS", "FIRST", "LAST",
        "CONCURRENTLY", "REINDEX", "VACUUM", "ANALYZE", "EXPLAIN",
        "NOTIFY", "LISTEN", "UNLISTEN", "COPY", "TRUNCATE",
        "EXTENSION", "SEQUENCE", "INCREMENT", "MINVALUE", "MAXVALUE",
        "START", "CACHE", "CYCLE", "NO",
        "ARRAY", "INTERVAL",
    ]
    .iter()
    .copied()
    .collect()
}

/// Apply keyword case transformation to a SQL string.
///
/// Preserves quoted identifiers and string literals.
fn apply_keyword_case(sql: &str, case: &KeywordCase) -> String {
    if matches!(case, KeywordCase::Preserve) {
        return sql.to_string();
    }

    let keywords = sql_keywords();
    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();

    while let Some(c) = chars.next() {
        // Skip string literals (single quotes)
        if c == '\'' {
            result.push(c);
            let mut escaped = false;
            for ch in chars.by_ref() {
                result.push(ch);
                if ch == '\'' && !escaped {
                    break;
                }
                escaped = ch == '\\';
            }
            continue;
        }

        // Skip quoted identifiers (double quotes)
        if c == '"' {
            result.push(c);
            for ch in chars.by_ref() {
                result.push(ch);
                if ch == '"' {
                    break;
                }
            }
            continue;
        }

        // Skip $$ quoted bodies (but we shouldn't reach here for function bodies
        // since format_dollar_quoted handles them separately)
        if c == '$' && chars.peek() == Some(&'$') {
            result.push(c);
            result.push(chars.next().unwrap());
            // Copy everything until next $$
            loop {
                match chars.next() {
                    Some('$') if chars.peek() == Some(&'$') => {
                        result.push('$');
                        result.push(chars.next().unwrap());
                        break;
                    }
                    Some(ch) => result.push(ch),
                    None => break,
                }
            }
            continue;
        }

        // Collect a word
        if c.is_ascii_alphabetic() || c == '_' {
            let mut word = String::new();
            word.push(c);
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    word.push(chars.next().unwrap());
                } else {
                    break;
                }
            }

            if keywords.contains(word.to_uppercase().as_str()) {
                match case {
                    KeywordCase::Lower => result.push_str(&word.to_lowercase()),
                    KeywordCase::Upper => result.push_str(&word.to_uppercase()),
                    KeywordCase::Preserve => result.push_str(&word),
                }
            } else {
                result.push_str(&word);
            }
            continue;
        }

        result.push(c);
    }

    result
}

// ── Helpers ─────────────────────────────────────────────

fn ensure_semicolon(s: &str) -> String {
    let trimmed = s.trim_end();
    if trimmed.ends_with(';') {
        trimmed.to_string()
    } else {
        format!("{trimmed};")
    }
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CommaStyle, FormatConfig, KeywordCase};

    #[test]
    fn f1_uppercase_to_lowercase() {
        let input = "CREATE TABLE IF NOT EXISTS users (\n  id BIGINT PRIMARY KEY\n);";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(
            result.contains("create table if not exists"),
            "Expected lowercase keywords, got: {result}"
        );
        assert!(!result.contains("CREATE"), "Should not contain uppercase CREATE, got: {result}");
    }

    #[test]
    fn f2_trailing_to_leading_commas() {
        let input = "create table users (\n  id bigint,\n  name text,\n  email text\n);";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(
            result.contains(", name"),
            "Should have leading comma before name, got: {result}"
        );
        assert!(
            result.contains(", email"),
            "Should have leading comma before email, got: {result}"
        );
    }

    #[test]
    fn f3_type_alignment() {
        let result = format_ddl(
            "create table users (\n  id bigint,\n  name text\n);",
            &FormatConfig::default(),
        );
        // Types should start at column 27 (0-indexed)
        // First line: "  id                       bigint"
        //              ^-- col 0                  ^-- col 27
        for line in result.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("id") {
                // "  id" + spaces + "bigint" — "bigint" should start at column 27
                let type_pos = line.find("bigint");
                assert_eq!(
                    type_pos,
                    Some(27),
                    "Type 'bigint' should start at column 27 in line: '{line}'"
                );
            } else if trimmed.starts_with(", name") {
                let type_pos = line.find("text");
                assert_eq!(
                    type_pos,
                    Some(27),
                    "Type 'text' should start at column 27 in line: '{line}'"
                );
            }
        }
    }

    #[test]
    fn f4_unparseable_preserved() {
        let input = "SOME WEIRD STUFF THAT DOESN'T PARSE;";
        let result = format_ddl(input, &FormatConfig::default());
        // Content should be preserved (though keywords lowercased)
        assert!(
            result.contains("DOESN'T PARSE") || result.contains("doesn't"),
            "Content should be preserved, got: {result}"
        );
    }

    #[test]
    fn f5_idempotent() {
        let input = "CREATE TABLE users (\n  id BIGINT PRIMARY KEY,\n  name TEXT NOT NULL\n);";
        let config = FormatConfig::default();
        let first = format_ddl(input, &config);
        let second = format_ddl(&first, &config);
        assert_eq!(first, second, "formatting should be idempotent");
    }

    #[test]
    fn f8_function_body_preserved() {
        let input = "CREATE OR REPLACE FUNCTION my_func() RETURNS void AS $$\nBEGIN\n  RAISE NOTICE 'hello';\nEND;\n$$ LANGUAGE plpgsql;";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(
            result.contains("create or replace function")
                || result.contains("CREATE OR REPLACE FUNCTION"),
            "Header should be formatted, got: {result}"
        );
        assert!(
            result.contains("RAISE NOTICE 'hello'"),
            "function body should be preserved, got: {result}"
        );
    }

    #[test]
    fn f9_set_search_path() {
        let input = "SET search_path TO config, extensions;";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(
            result.contains("set search_path to"),
            "SET should be lowercased, got: {result}"
        );
    }

    #[test]
    fn f10_comment_on() {
        let input = "COMMENT ON TABLE lookups IS\n'Generic lookup table.';";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(
            result.contains("comment on table"),
            "COMMENT ON should be lowercased, got: {result}"
        );
        assert!(
            result.contains("Generic lookup table."),
            "Comment text should be preserved, got: {result}"
        );
    }

    #[test]
    fn f11_default_config() {
        let config = FormatConfig::default();
        assert!(matches!(config.keyword_case, KeywordCase::Lower));
        assert!(matches!(config.comma_style, CommaStyle::Leading));
        assert_eq!(config.type_alignment, 27);
        assert_eq!(config.indent, 2);
    }

    #[test]
    fn f12_upper_mode() {
        let input = "create table users (\n  id bigint\n);";
        let config = FormatConfig {
            keyword_case: KeywordCase::Upper,
            ..Default::default()
        };
        let result = format_ddl(input, &config);
        assert!(
            result.contains("CREATE TABLE"),
            "Should have uppercase keywords, got: {result}"
        );
    }
}
