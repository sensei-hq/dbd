use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::config::{FormatConfig, KeywordCase, QueryStyle};

mod create;
mod keyword_case;
mod river;
mod split;

use create::*;
use keyword_case::*;
use river::*;
use split::*;

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
        // Safety guard: never emit a reformatted statement that changed the SQL.
        // If the formatted output doesn't re-parse to the same AST, or drops a
        // comment or a `$$` body, keep the original statement untouched.
        if block_is_faithful(trimmed, &formatted) {
            output_parts.push(formatted);
        } else {
            output_parts.push(ensure_semicolon(trimmed));
        }
    }

    let mut result = output_parts.join("\n\n");
    // Ensure trailing newline
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Whether a formatted statement faithfully preserves the original: same parsed
/// AST (so semantics are unchanged), plus every comment and every `$$` body
/// retained. Statements sqlparser can't parse fall back to the
/// comment/body-preservation check (the formatter used a conservative
/// keyword-case/verbatim path for those).
fn block_is_faithful(original: &str, formatted: &str) -> bool {
    let dialect = PostgreSqlDialect {};
    let po = Parser::parse_sql(&dialect, &ensure_semicolon(original));
    let pf = Parser::parse_sql(&dialect, &ensure_semicolon(formatted));
    match (po, pf) {
        (Ok(a), Ok(b)) => {
            a == b && comments_preserved(original, formatted) && dollar_bodies_preserved(original, formatted)
        }
        // The formatter turned parseable SQL into something that no longer
        // parses — definitely not faithful.
        (Ok(_), Err(_)) => false,
        // Original wasn't sqlparser-parseable (SET, COMMENT ON, a $$ body, or
        // PG-specific DDL); trust the conservative path only if comments and
        // bodies are intact.
        (Err(_), _) => comments_preserved(original, formatted) && dollar_bodies_preserved(original, formatted),
    }
}

/// Every comment in `original` (— line and /* */ block) must appear in `formatted`.
fn comments_preserved(original: &str, formatted: &str) -> bool {
    extract_comments(original).iter().all(|c| formatted.contains(c.as_str()))
}

/// Extract trimmed comment text (line `-- …` and block `/* … */`) from SQL,
/// skipping comment markers inside string literals and `$$` bodies.
fn extract_comments(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // Skip $$-quoted bodies and string literals — `--`/`/*` inside them
            // are not comments.
            b'$' if i + 1 < bytes.len() && bytes[i + 1] == b'$' => {
                i = scan_dollar_quote(bytes, i);
            }
            b'\'' => {
                i = scan_single_quoted(bytes, i);
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                let end = scan_line_comment(bytes, i);
                let text = sql[i..end].trim_end().trim_start_matches('-').trim();
                if !text.is_empty() {
                    out.push(text.to_string());
                }
                i = end;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let start = i + 2;
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                let text = sql[start..i.min(sql.len())].trim();
                if !text.is_empty() {
                    out.push(text.to_string());
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

/// The `$$ … $$` body segments must be byte-identical between original and
/// formatted (the formatter preserves function/procedure bodies verbatim).
fn dollar_bodies_preserved(original: &str, formatted: &str) -> bool {
    fn bodies(s: &str) -> Vec<&str> {
        s.split("$$").skip(1).step_by(2).collect()
    }
    bodies(original) == bodies(formatted)
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
                return format_create_table(ct, original, config);
            }
            Statement::CreateIndex(ci) => {
                return format_create_index(ci, config);
            }
            Statement::CreateView(cv) => {
                return format_create_view(&cv.name, cv.or_replace, &cv.query, config);
            }
            Statement::CreateType { name, representation: Some(repr) } => {
                return format_create_type(name, repr, config);
            }
            Statement::Query(q) if config.query_style == QueryStyle::River => {
                return format_river_query(q, config);
            }
            _ => {}
        }
    }

    // Fallback: keyword case transformation (also covers SET and COMMENT ON,
    // which need no special-case handling beyond this).
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

// ── Helpers ─────────────────────────────────────────────

/// Apply keyword case to a single token (not a full SQL string).
fn kw_case(s: &str, case: &KeywordCase) -> String {
    match case {
        KeywordCase::Lower => s.to_lowercase(),
        KeywordCase::Upper => s.to_uppercase(),
        KeywordCase::Preserve => s.to_string(),
    }
}

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
    fn f10b_comment_on_escaped_quote_in_literal() {
        // The literal contains a SQL-escaped quote (`''`); the scanner must not
        // treat it as closing the string, or `AND` below would be seen as
        // outside the literal and the literal itself would be corrupted.
        let input = "COMMENT ON TABLE lookups IS\n'It''s a generic AND lookup table.';";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(
            result.contains("comment on table"),
            "COMMENT ON should be lowercased, got: {result}"
        );
        assert!(
            result.contains("It''s a generic AND lookup table."),
            "Literal content (including the escaped quote and the keyword-like \
             word `AND` inside it) should be preserved verbatim, got: {result}"
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

    // ── River formatter tests ─────────────────────────────

    fn river_config() -> FormatConfig {
        FormatConfig {
            query_style: crate::config::QueryStyle::River,
            ..Default::default()
        }
    }

    #[test]
    fn r1_river_select_keywords_right_aligned() {
        let input = "SELECT id, name FROM users WHERE id = 1;";
        let result = format_ddl(input, &river_config());
        // "select" right-aligned at gutter 10: 4 spaces + "select"
        assert!(result.contains("    select"), "select should be right-aligned: {result}");
        // "from" right-aligned: 6 spaces + "from"
        assert!(result.contains("      from"), "from should be right-aligned: {result}");
        // "where" right-aligned: 5 spaces + "where"
        assert!(result.contains("     where"), "where should be right-aligned: {result}");
    }

    #[test]
    fn r2_river_select_list_leading_commas() {
        let input = "SELECT id, name, email FROM users;";
        let result = format_ddl(input, &river_config());
        // Second item should be a river comma line
        assert!(result.contains(", name"), "comma before name: {result}");
        assert!(result.contains(", email"), "comma before email: {result}");
        // Commas at gutter position (9 spaces + comma)
        assert!(result.contains("         , "), "comma should be at gutter: {result}");
    }

    #[test]
    fn r3_river_alias_alignment() {
        let input = "SELECT id AS user_id, created_at AS ts FROM users;";
        let result = format_ddl(input, &river_config());
        // Both aliases should be present
        assert!(result.contains("as user_id"), "alias user_id: {result}");
        assert!(result.contains("as ts"), "alias ts: {result}");
        // Aliases should be padded to align: "id" padded to "created_at" length
        assert!(result.contains("id         as"), "id should be padded for alignment: {result}");
    }

    #[test]
    fn r4_river_inner_join() {
        let input = "SELECT u.id, o.amount FROM users u INNER JOIN orders o ON o.user_id = u.id;";
        let result = format_ddl(input, &river_config());
        // "inner join" is exactly 10 chars — no leading spaces
        assert!(result.contains("inner join"), "inner join present: {result}");
        // "on" right-aligned at gutter 10: 8 spaces + "on"
        assert!(result.contains("        on"), "on right-aligned: {result}");
    }

    #[test]
    fn r4b_river_plain_and_left_join_keep_qualifier_and_on() {
        // Plain `JOIN` (sqlparser JoinOperator::Join) and `LEFT JOIN`
        // (JoinOperator::Left) must keep their qualifier AND their ON clause —
        // dropping them silently turns the query into cross joins.
        let input = "SELECT n.id, f.name, p.name \
                     FROM nodes n \
                     JOIN folders f ON f.id = n.folder_id \
                     LEFT JOIN projects p ON p.id = f.project_id;";
        let result = format_ddl(input, &river_config());

        assert!(result.contains("left join"), "LEFT qualifier preserved: {result}");
        // Both ON conditions must survive.
        assert!(result.contains("on f.id = n.folder_id"), "first join's ON kept: {result}");
        assert!(result.contains("on p.id = f.project_id"), "left join's ON kept: {result}");
        // The plain join keyword is present (and the ON is not dropped).
        assert!(result.contains("join folders"), "plain join present: {result}");
    }

    #[test]
    fn river_cte_not_dropped() {
        // River doesn't render WITH/CTE — the guard must fall back so the CTE
        // (and its inner join) survive rather than being silently dropped.
        let input = "CREATE VIEW v AS WITH t AS (SELECT a, b FROM x JOIN y ON y.id = x.id) \
                     SELECT t.a FROM t;";
        let result = format_ddl(input, &river_config());
        assert!(result.to_lowercase().contains("with t as"), "CTE must survive: {result}");
        assert!(result.contains("y.id = x.id"), "CTE inner join must survive: {result}");
    }

    #[test]
    fn river_qualified_wildcard_not_doubled() {
        // `l.*` was being rendered as `l.*.*`. The guard must fall back so the
        // wildcard is emitted faithfully.
        let input = "CREATE VIEW v AS SELECT l.*, p.name FROM libs l JOIN projs p ON p.id = l.pid;";
        let result = format_ddl(input, &river_config());
        assert!(!result.contains(".*.*"), "qualified wildcard must not be doubled: {result}");
        assert!(result.contains("l.*"), "l.* preserved: {result}");
    }

    fn parses(sql: &str) -> bool {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).is_ok()
    }

    #[test]
    fn splitter_keeps_semicolon_inside_line_comment() {
        // A `;` inside a `-- comment` must NOT split the statement (was breaking
        // the CREATE TABLE into invalid fragments).
        let input = "create table t (\n  a int -- id; the value\n, b text\n);";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(parses(&result), "formatted output must be valid SQL: {result}");
        assert!(result.contains("id; the value"), "inline comment preserved: {result}");
    }

    #[test]
    fn index_using_and_partial_where_preserved() {
        // GIN method and the partial WHERE must survive (was dropped → btree,
        // all-rows). The guard keeps the original if the renderer can't.
        let gin = "create index t_tags_idx on t using gin (tags);";
        let part = "create index t_live_idx on t (x) where enabled = true;";
        for input in [gin, part] {
            let result = format_ddl(input, &FormatConfig::default());
            let a = Parser::parse_sql(&PostgreSqlDialect {}, input).unwrap();
            let b = Parser::parse_sql(&PostgreSqlDialect {}, &result).unwrap();
            assert_eq!(a, b, "index semantics must be preserved: {result}");
        }
    }

    #[test]
    fn table_inline_comments_preserved() {
        // CREATE TABLE rebuilt from the AST drops comments; they must survive —
        // now re-attached to the reformatted table rather than falling back.
        let input = "create table t (\n  a int  -- the primary key\n, b text -- a label\n);";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(result.contains("the primary key"), "comment 1 kept: {result}");
        assert!(result.contains("a label"), "comment 2 kept: {result}");
    }

    /// Trailing column comments survive a REFORMAT (not just the verbatim
    /// fallback): the columns are aligned and each comment re-attached.
    #[test]
    fn table_trailing_comments_are_reformatted() {
        let input = "create table t (a int -- c1\n, b text -- c2\n);";
        let result = format_ddl(input, &FormatConfig::default());
        // Reformatted: each column on its own indented line (verbatim fallback,
        // which keeps `(a int`, would not produce this).
        assert!(result.contains("\n  a"), "column a reformatted onto its own line: {result}");
        assert!(result.contains("\n, b"), "leading-comma style applied: {result}");
        // Comments preserved and re-attached.
        assert!(result.contains("-- c1"), "trailing comment 1 kept: {result}");
        assert!(result.contains("-- c2"), "trailing comment 2 kept: {result}");
        // Still faithful: re-parses to the same AST.
        let a = Parser::parse_sql(&PostgreSqlDialect {}, input).unwrap();
        let b = Parser::parse_sql(&PostgreSqlDialect {}, &result).unwrap();
        assert_eq!(a, b, "reformatted table must preserve semantics: {result}");
    }

    /// The issue's own example: a comment-bearing table reformats, preserves
    /// every comment (including one containing `|` and `;`), stays faithful, and
    /// is idempotent (formatting the result again is a no-op).
    #[test]
    fn table_realistic_comments_reformat_faithfully_and_idempotently() {
        let input = "create table if not exists knowledge_sources (\
             id uuid primary key default gen_random_uuid()\
             , kind text not null -- hive_mind | mcp | rest | webhook\n\
             , credential_ref text not null -- Keychain entry id; the key lives in the OS keychain\n\
             );";
        let cfg = FormatConfig::default();
        let out = format_ddl(input, &cfg);

        // Reformatted, not the verbatim fallback.
        assert!(out.contains("\n  id"), "reformatted onto aligned lines: {out}");
        // Every comment preserved verbatim.
        assert!(out.contains("-- hive_mind | mcp | rest | webhook"), "comment 1: {out}");
        assert!(
            out.contains("-- Keychain entry id; the key lives in the OS keychain"),
            "comment 2: {out}"
        );
        // Faithful (same AST) and idempotent.
        let a = Parser::parse_sql(&PostgreSqlDialect {}, input).unwrap();
        let b = Parser::parse_sql(&PostgreSqlDialect {}, &out).unwrap();
        assert_eq!(a, b, "semantics preserved: {out}");
        assert_eq!(out, format_ddl(&out, &cfg), "formatting must be idempotent");
    }

    /// A standalone comment line above a column is preserved on its own line —
    /// through a reformat (input is single-line so the verbatim fallback can't
    /// satisfy the layout asserts).
    #[test]
    fn table_standalone_comment_is_reformatted() {
        let input = "create table t (-- identity\n id uuid, name text);";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(result.contains("-- identity"), "standalone comment kept: {result}");
        assert!(result.contains("\n  id"), "id reformatted onto its own line: {result}");
        assert!(result.contains("\n, name"), "reformatted with leading comma: {result}");
        let a = Parser::parse_sql(&PostgreSqlDialect {}, input).unwrap();
        let b = Parser::parse_sql(&PostgreSqlDialect {}, &result).unwrap();
        assert_eq!(a, b, "must preserve semantics: {result}");
    }

    /// A `--` sequence inside a string default is NOT a comment and must not be
    /// mistaken for a trailing comment when re-attaching.
    #[test]
    fn table_comment_marker_inside_string_default_is_not_a_comment() {
        let input = "create table t (a text default 'x -- y' -- real\n, b int);";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(result.contains("\n  a"), "table reformatted: {result}");
        assert!(result.contains("'x -- y'"), "string default preserved intact: {result}");
        assert!(result.contains("-- real"), "the real trailing comment kept: {result}");
        // The string's inner `-- y` must not have leaked out as a second comment
        // onto its own line.
        assert!(!result.contains("\n  -- y"), "string content must not become a comment: {result}");
        let a = Parser::parse_sql(&PostgreSqlDialect {}, input).unwrap();
        let b = Parser::parse_sql(&PostgreSqlDialect {}, &result).unwrap();
        assert_eq!(a, b, "must preserve semantics: {result}");
    }

    #[test]
    fn river_is_the_default_query_style() {
        use crate::config::QueryStyle;
        assert_eq!(QueryStyle::default(), QueryStyle::River);
        assert_eq!(FormatConfig::default().query_style, QueryStyle::River);
        // A view formatted with the default config gets river styling.
        let result = format_ddl("CREATE VIEW v AS SELECT a, b FROM x;", &FormatConfig::default());
        assert!(result.contains("    select"), "default config should river-style: {result}");
    }

    #[test]
    fn river_faithful_view_still_uses_river() {
        // A query the river renderer DOES handle still gets multi-line river style.
        let input = "CREATE VIEW v AS SELECT a, b FROM x JOIN y ON y.id = x.id WHERE a > 1;";
        let result = format_ddl(input, &river_config());
        assert!(result.contains("    select"), "river applied (multi-line): {result}");
        assert!(result.contains("on y.id = x.id"), "join ON preserved: {result}");
    }

    #[test]
    fn r5_river_where_and_conditions() {
        let input = "SELECT id FROM users WHERE status = 'active' AND age > 18;";
        let result = format_ddl(input, &river_config());
        assert!(result.contains("     where"), "where right-aligned: {result}");
        // "and" right-aligned at gutter 10: 7 spaces + "and"
        assert!(result.contains("       and"), "and right-aligned: {result}");
    }

    #[test]
    fn r10_river_operator_alignment() {
        // Both conditions are simple comparisons — operators should align.
        let input = "SELECT id FROM users WHERE status = 'active' AND login_count > 5;";
        let result = format_ddl(input, &river_config());
        // "status" is 6 chars, "login_count" is 11 chars — "=" on status line
        // should be padded to align with ">" on login_count line.
        let lines: Vec<&str> = result.lines().collect();
        let where_line = lines.iter().find(|l| l.trim_start().starts_with("where")).unwrap();
        let and_line   = lines.iter().find(|l| l.trim_start().starts_with("and")).unwrap();
        // The operator column should be the same in both lines
        let op_col_where = where_line.find('=').expect("= in where line");
        let op_col_and   = and_line.find('>').expect("> in and line");
        assert_eq!(op_col_where, op_col_and,
            "operators must align:\n  where: {where_line}\n  and:   {and_line}");
    }

    #[test]
    fn r11_river_table_alias_alignment() {
        let input = "SELECT u.id, o.id FROM users u INNER JOIN orders o ON o.user_id = u.id;";
        let result = format_ddl(input, &river_config());
        // "users" is 5 chars, "orders" is 6 chars → "users" padded by 1
        // Both aliases should appear after padding
        assert!(result.contains("users  u") || result.contains("users u"),
            "users alias present: {result}");
        assert!(result.contains("orders o"), "orders alias present: {result}");
        // Check alias positions match
        let lines: Vec<&str> = result.lines().collect();
        let from_line = lines.iter().find(|l| l.trim_start().starts_with("from")).unwrap();
        let join_line = lines.iter().find(|l| l.trim_start().starts_with("inner")).unwrap();
        let alias_u = from_line.find(" u").expect("alias u");
        let alias_o = join_line.find(" o").expect("alias o");
        assert_eq!(alias_u, alias_o,
            "table aliases must align:\n  from: {from_line}\n  join: {join_line}");
    }

    #[test]
    fn r12_river_or_conditions() {
        let input = "SELECT id FROM users WHERE status = 'active' OR status = 'pending';";
        let result = format_ddl(input, &river_config());
        assert!(result.contains("     where"), "where right-aligned: {result}");
        // "or" right-aligned at gutter 10: 8 spaces + "or"
        assert!(result.contains("        or"), "or right-aligned: {result}");
        let lines: Vec<&str> = result.lines().collect();
        let or_count = lines.iter().filter(|l| l.trim_start().starts_with("or")).count();
        assert_eq!(or_count, 1, "exactly one or continuation line: {result}");
    }

    #[test]
    fn r14_river_or_within_and() {
        let input = "SELECT id FROM users WHERE active = true AND (status = 'approved' OR role = 'admin');";
        let result = format_ddl(input, &river_config());
        // OR group should be expanded with parens
        assert!(result.contains("       and ("), "and opens paren: {result}");
        assert!(result.contains("         or"), "or continuation indented: {result}");
        assert!(result.contains("'admin')"), "closing paren on last or: {result}");
        // Inner OR conditions should be on separate lines
        let lines: Vec<&str> = result.lines().collect();
        let and_paren = lines.iter().find(|l| l.trim_start().starts_with("and ("));
        let or_line = lines.iter().find(|l| l.trim_start().starts_with("or"));
        assert!(and_paren.is_some(), "and ( line present: {result}");
        assert!(or_line.is_some(), "or line present: {result}");
    }

    #[test]
    fn r15_river_or_within_and_alignment() {
        // Multiple AND conditions with an embedded OR group — operators align per group
        let input = "SELECT id FROM users WHERE active = true AND (status = 'approved' OR role = 'admin') AND count > 10;";
        let result = format_ddl(input, &river_config());
        // Non-OR conditions should be present
        assert!(result.contains("active"), "active condition: {result}");
        assert!(result.contains("count"), "count condition: {result}");
        // OR group should be expanded
        assert!(result.contains("(status"), "or group first: {result}");
        assert!(result.contains("'admin')"), "or group last with paren: {result}");
        // Inner OR operators should align (status=6 chars, role=4 → role padded)
        let lines: Vec<&str> = result.lines().collect();
        let or_line = lines.iter().find(|l| l.trim_start().starts_with("or")).unwrap();
        assert!(or_line.contains("role   =") || or_line.contains("role ="),
            "role should be padded for alignment: {result}");
    }

    #[test]
    fn r13_river_subquery_from() {
        let input = "SELECT id FROM (SELECT id, name FROM users WHERE active = true) sub WHERE sub.id > 1;";
        let result = format_ddl(input, &river_config());
        assert!(result.contains("      from ("), "from opens paren: {result}");
        assert!(result.contains(") sub"), "closes with alias: {result}");
        // Inner select must be indented beyond gutter
        let lines: Vec<&str> = result.lines().collect();
        let inner_select = lines.iter().find(|l| {
            let t = l.trim_start();
            t.starts_with("select") && l.len() > 10 && l.starts_with("            ")
        });
        assert!(inner_select.is_some(), "inner select should be indented: {result}");
    }

    #[test]
    fn r6_river_order_by() {
        let input = "SELECT id, name FROM users ORDER BY name DESC, id;";
        let result = format_ddl(input, &river_config());
        assert!(result.contains("  order by"), "order by right-aligned: {result}");
        assert!(result.contains("name desc"), "desc preserved: {result}");
    }

    #[test]
    fn r7_river_create_view() {
        let input = "CREATE VIEW active_users AS SELECT id, name FROM users WHERE active = true;";
        let result = format_ddl(input, &river_config());
        assert!(result.contains("create view active_users"), "view header: {result}");
        assert!(result.contains("    select"), "river select inside view: {result}");
        assert!(result.contains("      from"), "river from inside view: {result}");
    }

    #[test]
    fn r8_create_type_enum_multiline() {
        let input = "CREATE TYPE status AS ENUM ('active', 'inactive', 'pending');";
        let result = format_ddl(input, &FormatConfig::default());
        assert!(result.contains("create type status as enum"), "enum header: {result}");
        assert!(result.contains("'active'"), "first value: {result}");
        // Second and third values should have leading commas
        assert!(result.contains(", 'inactive'"), "leading comma inactive: {result}");
        assert!(result.contains(", 'pending'"), "leading comma pending: {result}");
    }

    #[test]
    fn r9_river_idempotent() {
        let input = "SELECT u.id AS uid, u.name AS full_name FROM users u INNER JOIN orders o ON o.user_id = u.id WHERE u.active = true AND o.amount > 100 ORDER BY u.name;";
        let config = river_config();
        let first = format_ddl(input, &config);
        let second = format_ddl(&first, &config);
        assert_eq!(first, second, "river formatting should be idempotent:\nfirst:\n{first}\nsecond:\n{second}");
    }

    // ── SQLite trigger splitter (R16–R19) ──────────────────────────────────

    #[test]
    fn r16_split_trigger_body_kept_as_single_block() {
        let input = "\
CREATE TABLE foo (id INTEGER);

CREATE TRIGGER foo_log AFTER INSERT ON foo
BEGIN
  INSERT INTO audit (event, row_id) VALUES ('insert', NEW.id);
  UPDATE counts SET n = n + 1 WHERE name = 'foo';
END;

CREATE INDEX foo_x ON foo(id);
";
        let blocks = super::split_statements(input);
        let nonempty: Vec<&String> = blocks.iter().filter(|b| !b.trim().is_empty()).collect();
        assert_eq!(
            nonempty.len(),
            3,
            "expected 3 statements (table, trigger, index); got {}: {:#?}",
            nonempty.len(),
            nonempty
        );
        assert!(nonempty[1].contains("CREATE TRIGGER"));
        assert!(nonempty[1].contains("INSERT INTO audit"));
        assert!(nonempty[1].contains("UPDATE counts"));
        assert!(nonempty[1].trim_end().ends_with("END;"));
    }

    #[test]
    fn r17_split_plain_begin_transaction_still_splits() {
        // A bare BEGIN/COMMIT pair (no TRIGGER keyword) must keep splitting
        // on every `;` — otherwise transaction wrappers in migration files
        // would silently collapse into one block.
        let input = "BEGIN; INSERT INTO t VALUES (1); INSERT INTO t VALUES (2); COMMIT;";
        let blocks = super::split_statements(input);
        let nonempty: Vec<&String> = blocks.iter().filter(|b| !b.trim().is_empty()).collect();
        assert_eq!(nonempty.len(), 4, "got: {nonempty:#?}");
    }

    #[test]
    fn r18_split_trigger_with_case_end_inside() {
        // CASE ... END inside the trigger body must NOT prematurely close
        // the trigger block — only `END;` at top level does.
        let input = "\
CREATE TRIGGER t1 AFTER UPDATE ON t
BEGIN
  UPDATE t2 SET v = CASE WHEN NEW.x > 0 THEN 'pos' ELSE 'neg' END WHERE id = NEW.id;
  INSERT INTO log VALUES (NEW.id);
END;
";
        let blocks = super::split_statements(input);
        let nonempty: Vec<&String> = blocks.iter().filter(|b| !b.trim().is_empty()).collect();
        assert_eq!(nonempty.len(), 1, "got: {nonempty:#?}");
        assert!(nonempty[0].contains("CASE WHEN"));
        assert!(nonempty[0].contains("INSERT INTO log"));
    }

    #[test]
    fn r19_split_dollar_quote_still_works() {
        // Regression: the rewritten splitter must still respect $$ bodies.
        let input = "\
CREATE FUNCTION f() RETURNS void AS $$
BEGIN
  RAISE NOTICE 'hi';
  PERFORM 1;
END;
$$ LANGUAGE plpgsql;
SELECT 1;
";
        let blocks = super::split_statements(input);
        let nonempty: Vec<&String> = blocks.iter().filter(|b| !b.trim().is_empty()).collect();
        assert_eq!(nonempty.len(), 2, "got: {nonempty:#?}");
        assert!(nonempty[0].contains("LANGUAGE plpgsql"));
    }
}
