use std::collections::HashSet;

use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::config::{CommaStyle, FormatConfig, KeywordCase, QueryStyle};

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

/// Return the index just past a single-quoted string literal starting at
/// `start` (handling `''` escapes; a lone `'` closes it).
fn scan_single_quoted(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            // `''` escape stays inside the string; a lone `'` closes it.
            if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                i += 2;
                continue;
            }
            i += 1;
            break;
        }
        i += 1;
    }
    i
}

/// Return the index just past a double-quoted identifier starting at `start`.
/// (Postgres has no `""` escape at the lexer level that we need to model here —
/// a lone `"` closes it.)
fn scan_double_quoted(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            break;
        }
        i += 1;
    }
    i
}

/// Return the index at the newline (or EOF) ending a `-- …` line comment.
fn scan_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Return the index just past a `/* … */` block comment starting at `start`.
fn scan_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i < bytes.len() {
        if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            break;
        }
        i += 1;
    }
    i
}

/// Return the index just past a `$$ … $$` body starting at `start`. If the
/// closing `$$` is missing, returns an index past the end (the caller's
/// `while i < len` loop then terminates).
fn scan_dollar_quote(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i + 1 < bytes.len() && !(bytes[i] == b'$' && bytes[i + 1] == b'$') {
        i += 1;
    }
    i + 2
}

/// Return the index just past an identifier-like token starting at `start`.
fn scan_identifier(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    i
}

/// True when the accumulated block (still holding its trailing `;`) ends with a
/// standalone `END` token — i.e. the `;` closes a trigger body rather than an
/// inner statement or a CASE expression.
fn trigger_body_ends(current: &str) -> bool {
    let head = current[..current.len() - 1].trim_end();
    if head.len() < 3 || !head.as_bytes()[head.len() - 3..].eq_ignore_ascii_case(b"END") {
        return false;
    }
    head.len() == 3 || {
        let b = head.as_bytes()[head.len() - 4];
        !(b.is_ascii_alphanumeric() || b == b'_')
    }
}

/// Split input SQL into statement blocks.
///
/// Respects two kinds of bodies that contain inner semicolons:
/// - `$$ ... $$` delimited (Postgres function bodies)
/// - SQLite `CREATE TRIGGER ... BEGIN <stmts;> END;` bodies — detected when
///   a `BEGIN` keyword appears after `TRIGGER` in the same in-flight block.
fn split_statements(input: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut in_trigger_body = false;
    let mut seen_trigger = false;
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i] as char;

        // $$ pair toggles a dollar-quoted region.
        if c == '$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            current.push_str("$$");
            in_dollar_quote = !in_dollar_quote;
            i += 2;
            continue;
        }
        if in_dollar_quote {
            current.push(c);
            i += 1;
            continue;
        }

        // Single-quoted string literal — copy verbatim; a `;` inside is not a
        // statement boundary. Sliced so multi-byte UTF-8 is preserved.
        if c == '\'' {
            let end = scan_single_quoted(bytes, i);
            current.push_str(&input[i..end]);
            i = end;
            continue;
        }

        // Line comment `-- …` to end of line — inner `;` is not a boundary.
        if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            let end = scan_line_comment(bytes, i);
            current.push_str(&input[i..end]);
            i = end;
            continue;
        }

        // Block comment `/* … */` — inner `;` is not a boundary.
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let end = scan_block_comment(bytes, i);
            current.push_str(&input[i..end]);
            i = end;
            continue;
        }

        // Identifier-like token: consume atomically so we can spot keywords
        // at word boundaries without false positives mid-name.
        if c.is_ascii_alphabetic() || c == '_' {
            let end = scan_identifier(bytes, i);
            let word = &input[i..end];
            current.push_str(word);
            i = end;
            match word.to_ascii_uppercase().as_str() {
                "TRIGGER" if !in_trigger_body => seen_trigger = true,
                "BEGIN" if seen_trigger && !in_trigger_body => in_trigger_body = true,
                _ => {}
            }
            continue;
        }

        current.push(c);
        i += 1;

        if c != ';' {
            continue;
        }

        if in_trigger_body {
            // A `;` only closes the trigger body when it follows a standalone
            // `END`; inner separators keep accumulating.
            if trigger_body_ends(&current) {
                in_trigger_body = false;
                seen_trigger = false;
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }

        // Normal statement boundary.
        blocks.push(std::mem::take(&mut current));
        seen_trigger = false;
    }

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
    original: &str,
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

    // Build each column/constraint line (in source-emit order: columns first),
    // then interleave the inline comments sqlparser discarded.
    let has_constraints = !ct.constraints.is_empty();
    let mut item_lines: Vec<String> = Vec::with_capacity(ct.columns.len() + ct.constraints.len());
    for (i, col) in ct.columns.iter().enumerate() {
        item_lines.push(format_table_column(
            col,
            i == 0,
            i == ct.columns.len() - 1,
            has_constraints,
            config,
        ));
    }
    for (i, constraint) in ct.constraints.iter().enumerate() {
        item_lines.push(format_table_constraint_line(
            constraint,
            i == 0,
            i == ct.constraints.len() - 1,
            ct.columns.is_empty(),
            config,
        ));
    }

    let indent = " ".repeat(config.indent);
    // Re-attach comments only when the parsed body's item count lines up 1:1 with
    // the AST's columns+constraints. On any mismatch (interleaved constraints,
    // exotic layout), emit lines bare — the round-trip guard then keeps the
    // original text, so a comment is never dropped.
    match extract_item_comments(original).filter(|c| c.len() == item_lines.len()) {
        Some(comments) => {
            for (line, ic) in item_lines.iter().zip(comments.iter()) {
                for lead in &ic.leading {
                    out.push_str(&indent);
                    out.push_str(lead);
                    out.push('\n');
                }
                out.push_str(line);
                for (k, trailing) in ic.trailing.iter().enumerate() {
                    if k == 0 {
                        out.push_str("  ");
                        out.push_str(trailing);
                    } else {
                        out.push('\n');
                        out.push_str(&indent);
                        out.push_str(trailing);
                    }
                }
                out.push('\n');
            }
        }
        None => {
            for line in &item_lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    out.push_str(");");
    out
}

// ── CREATE TABLE inline-comment preservation ────────────

/// Inline comments captured for one top-level column-list item (column or
/// table constraint), in source order.
#[derive(Default)]
struct ItemComments {
    /// Standalone comment lines that preceded the item's code (own lines above it).
    leading: Vec<String>,
    /// Comments after the item's code (trailing — usually end-of-line).
    trailing: Vec<String>,
}

/// Extract inline comments from a `CREATE TABLE` column list, one [`ItemComments`]
/// per top-level (comma-separated) item, in source order. Returns `None` if the
/// column-list parentheses can't be located. sqlparser discards comments, so this
/// recovers them from the raw text; the caller only uses the result when it lines
/// up 1:1 with the parsed items and otherwise keeps the original text verbatim.
fn extract_item_comments(original: &str) -> Option<Vec<ItemComments>> {
    Some(segment_item_comments(table_body(original)?))
}

/// The text inside a `CREATE TABLE`'s outermost `( … )` column list (exclusive of
/// the parentheses), or `None` if it can't be located. Skips string literals,
/// quoted identifiers, and comments while finding the opener and its match.
fn table_body(sql: &str) -> Option<&str> {
    let bytes = sql.as_bytes();
    let mut i = 0;

    // Find the column-list opener: the first top-level '(' that is not inside a
    // string/identifier/comment (a table name carries no parens).
    let open = loop {
        if i >= bytes.len() {
            return None;
        }
        match bytes[i] {
            b'\'' => i = scan_single_quoted(bytes, i),
            b'"' => i = scan_double_quoted(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = scan_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = scan_block_comment(bytes, i),
            b'(' => break i,
            _ => i += 1,
        }
    };

    // Scan to the matching close paren, tracking depth (and skipping the same
    // string/comment regions so a ')' inside them doesn't close early).
    let mut depth = 0i32;
    let mut j = open;
    while j < bytes.len() {
        match bytes[j] {
            b'\'' => {
                j = scan_single_quoted(bytes, j);
                continue;
            }
            b'"' => {
                j = scan_double_quoted(bytes, j);
                continue;
            }
            b'-' if bytes.get(j + 1) == Some(&b'-') => {
                j = scan_line_comment(bytes, j);
                continue;
            }
            b'/' if bytes.get(j + 1) == Some(&b'*') => {
                j = scan_block_comment(bytes, j);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&sql[open + 1..j]);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Split a column-list body into per-item comments at top-level commas. Within
/// each item, a comment before any code is `leading`; one after code is
/// `trailing`. Skips string literals and quoted identifiers so a `--`/`,` inside
/// them is not mistaken for a comment or an item boundary.
fn segment_item_comments(body: &str) -> Vec<ItemComments> {
    let bytes = body.as_bytes();
    let mut items: Vec<ItemComments> = Vec::new();
    let mut cur = ItemComments::default();
    let mut seen_code = false;
    let mut depth = 0i32;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                seen_code = true;
                i = scan_single_quoted(bytes, i);
            }
            b'"' => {
                seen_code = true;
                i = scan_double_quoted(bytes, i);
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                let end = scan_line_comment(bytes, i);
                let text = body[i..end].trim_end().to_string();
                if seen_code {
                    cur.trailing.push(text);
                } else {
                    cur.leading.push(text);
                }
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = scan_block_comment(bytes, i);
                let text = body[i..end].trim().to_string();
                if seen_code {
                    cur.trailing.push(text);
                } else {
                    cur.leading.push(text);
                }
                i = end;
            }
            b'(' => {
                depth += 1;
                seen_code = true;
                i += 1;
            }
            b')' => {
                depth -= 1;
                seen_code = true;
                i += 1;
            }
            b',' if depth == 0 => {
                items.push(std::mem::take(&mut cur));
                seen_code = false;
                i += 1;
            }
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            _ => {
                seen_code = true;
                i += 1;
            }
        }
    }
    items.push(cur);
    items
}

/// Render one column line for CREATE TABLE, applying the configured comma style.
/// A trailing comma is added unless this is the last column and there are no
/// following table-level constraints.
fn format_table_column(
    col: &sqlparser::ast::ColumnDef,
    is_first: bool,
    is_last: bool,
    has_trailing_constraints: bool,
    config: &FormatConfig,
) -> String {
    let indent = " ".repeat(config.indent);
    let type_col = config.type_alignment;
    let col_name = col.name.value.to_lowercase();
    let type_str = format_column_type(&col.data_type, config);
    let constraints_str = format_column_constraints(&col.options, config);

    match config.comma_style {
        CommaStyle::Trailing => {
            let base =
                format_column_line(&indent, &col_name, &type_str, &constraints_str, type_col);
            if !is_last || has_trailing_constraints {
                format!("{base},")
            } else {
                base
            }
        }
        CommaStyle::Leading if is_first => {
            format_column_line(&indent, &col_name, &type_str, &constraints_str, type_col)
        }
        CommaStyle::Leading => {
            // Leading comma: ", name   type" — the name starts after ", ".
            let prefix = format!("{}, ", &indent[..indent.len().saturating_sub(2)]);
            let name_with_pad = pad_to_width(&col_name, type_col - config.indent);
            format!("{prefix}{name_with_pad}{type_str}{constraints_str}")
        }
    }
}

/// Render one table-level constraint line for CREATE TABLE, applying comma style.
fn format_table_constraint_line(
    constraint: &sqlparser::ast::TableConstraint,
    is_first: bool,
    is_last: bool,
    no_columns: bool,
    config: &FormatConfig,
) -> String {
    let indent = " ".repeat(config.indent);
    let constraint_str = format_table_constraint(constraint, config);
    match config.comma_style {
        // First constraint with no preceding columns: no leading comma.
        CommaStyle::Leading if no_columns && is_first => format!("{indent}{constraint_str}"),
        CommaStyle::Leading => {
            let prefix = format!("{}, ", &indent[..indent.len().saturating_sub(2)]);
            format!("{prefix}{constraint_str}")
        }
        CommaStyle::Trailing => {
            let base = format!("{indent}{constraint_str}");
            if !is_last {
                format!("{base},")
            } else {
                base
            }
        }
    }
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

    // Index method, e.g. `USING gin` (was previously dropped → silent btree).
    if let Some(ref using) = ci.using {
        out.push_str(&format!(" {} {}", kw("USING"), kw(&using.to_string())));
    }

    // Indexed columns / expressions.
    let col_strs: Vec<String> = ci
        .columns
        .iter()
        .map(|c| apply_keyword_case(&c.to_string(), &config.keyword_case))
        .collect();
    out.push_str(&format!(" ({})", col_strs.join(", ")));

    // INCLUDE (covering) columns.
    if !ci.include.is_empty() {
        let inc: Vec<String> = ci.include.iter().map(|i| i.value.to_lowercase()).collect();
        out.push_str(&format!(" {} ({})", kw("INCLUDE"), inc.join(", ")));
    }

    match ci.nulls_distinct {
        Some(true) => out.push_str(&format!(" {}", kw("NULLS DISTINCT"))),
        Some(false) => out.push_str(&format!(" {}", kw("NULLS NOT DISTINCT"))),
        None => {}
    }

    if !ci.with.is_empty() {
        let w: Vec<String> = ci
            .with
            .iter()
            .map(|e| apply_keyword_case(&e.to_string(), &config.keyword_case))
            .collect();
        out.push_str(&format!(" {} ({})", kw("WITH"), w.join(", ")));
    }

    // Partial-index predicate (was previously dropped → indexed all rows).
    if let Some(ref pred) = ci.predicate {
        out.push_str(&format!(
            " {} {}",
            kw("WHERE"),
            apply_keyword_case(&pred.to_string(), &config.keyword_case)
        ));
    }

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
/// Copy the rest of a single-quoted literal (opening quote already emitted),
/// honoring `\`-escapes, into `result`.
fn copy_single_quoted(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, result: &mut String) {
    let mut escaped = false;
    for ch in chars.by_ref() {
        result.push(ch);
        if ch == '\'' && !escaped {
            break;
        }
        escaped = ch == '\\';
    }
}

/// Copy the rest of a double-quoted identifier (opening quote already emitted).
fn copy_quoted_ident(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, result: &mut String) {
    for ch in chars.by_ref() {
        result.push(ch);
        if ch == '"' {
            break;
        }
    }
}

/// Copy the rest of a `$$ … $$` body (opening `$$` already emitted).
fn copy_dollar_body(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, result: &mut String) {
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
}

/// Read an identifier-like word beginning with `first`, consuming trailing
/// alphanumeric/underscore characters from `chars`.
fn read_word(first: char, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut word = String::new();
    word.push(first);
    while let Some(&next) = chars.peek() {
        if next.is_ascii_alphanumeric() || next == '_' {
            word.push(chars.next().unwrap());
        } else {
            break;
        }
    }
    word
}

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
            copy_single_quoted(&mut chars, &mut result);
            continue;
        }

        // Skip quoted identifiers (double quotes)
        if c == '"' {
            result.push(c);
            copy_quoted_ident(&mut chars, &mut result);
            continue;
        }

        // Skip $$ quoted bodies (format_dollar_quoted normally handles these
        // for function bodies, so we shouldn't usually reach here).
        if c == '$' && chars.peek() == Some(&'$') {
            result.push(c);
            result.push(chars.next().unwrap());
            copy_dollar_body(&mut chars, &mut result);
            continue;
        }

        // Collect a word and re-case it when it is a keyword.
        if c.is_ascii_alphabetic() || c == '_' {
            let word = read_word(c, &mut chars);
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

// ── CREATE VIEW formatter ───────────────────────────────

fn format_create_view(
    name: &sqlparser::ast::ObjectName,
    or_replace: bool,
    query: &sqlparser::ast::Query,
    config: &FormatConfig,
) -> String {
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    let header = if or_replace {
        format!("{} {}", kw("create or replace view"), name.to_string().to_lowercase())
    } else {
        format!("{} {}", kw("create view"), name.to_string().to_lowercase())
    };

    let body = match (config.query_style == QueryStyle::River)
        .then(|| river_query_faithful(query, config))
        .flatten()
    {
        Some(river) => river,
        // Not river, or river couldn't faithfully render this query → keyword-case only.
        None => apply_keyword_case(&query.to_string(), &config.keyword_case),
    };

    ensure_semicolon(&format!("{} {}\n{}", header, kw("as"), body))
}

// ── CREATE TYPE formatter ───────────────────────────────

fn format_create_type(
    name: &sqlparser::ast::ObjectName,
    representation: &sqlparser::ast::UserDefinedTypeRepresentation,
    config: &FormatConfig,
) -> String {
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    match representation {
        sqlparser::ast::UserDefinedTypeRepresentation::Enum { labels } => {
            let mut out = format!(
                "{} {} {} (\n",
                kw("create type"),
                name.to_string().to_lowercase(),
                kw("as enum"),
            );
            let indent = " ".repeat(config.indent);
            for (i, label) in labels.iter().enumerate() {
                let label_str = label.value.as_str();
                if i == 0 {
                    out.push_str(&format!("{indent}'{label_str}'\n"));
                } else {
                    let prefix = format!("{}, ", &indent[..indent.len().saturating_sub(2)]);
                    out.push_str(&format!("{prefix}'{label_str}'\n"));
                }
            }
            out.push_str(");");
            out
        }
        other => {
            let raw = format!("create type {} {other}", name.to_string().to_lowercase());
            let result = apply_keyword_case(&raw, &config.keyword_case);
            ensure_semicolon(&result)
        }
    }
}

// ── River formatter ─────────────────────────────────────

/// Apply keyword case to a single token (not a full SQL string).
fn kw_case(s: &str, case: &KeywordCase) -> String {
    match case {
        KeywordCase::Lower => s.to_lowercase(),
        KeywordCase::Upper => s.to_uppercase(),
        KeywordCase::Preserve => s.to_string(),
    }
}

/// Emit one river-style line: keyword right-aligned within `gutter`, then
/// a space, then content.
fn river_line(gutter: usize, keyword: &str, content: &str) -> String {
    let pad = gutter.saturating_sub(keyword.chars().count());
    format!("{}{} {}", " ".repeat(pad), keyword, content)
}

/// Emit a continuation comma line — comma sits at position `gutter`,
/// aligning with the right edge of the keyword on the previous clause line.
fn river_comma_line(gutter: usize, content: &str) -> String {
    let pad = gutter.saturating_sub(1);
    format!("{}, {}", " ".repeat(pad), content)
}

/// Format a SELECT query (or bare SELECT) with river style.
/// Returns the formatted SQL with a trailing semicolon. Falls back to
/// keyword-case-only formatting if the river output wouldn't round-trip.
fn format_river_query(query: &sqlparser::ast::Query, config: &FormatConfig) -> String {
    match river_query_faithful(query, config) {
        Some(river) => ensure_semicolon(&river),
        None => ensure_semicolon(&apply_keyword_case(&query.to_string(), &config.keyword_case)),
    }
}

/// Build river-formatted lines for a Query. Used both for standalone SELECT
/// and for embedding inside CREATE VIEW.
fn river_select_lines(query: &sqlparser::ast::Query, config: &FormatConfig) -> Vec<String> {
    use sqlparser::ast::SetExpr;

    match &*query.body {
        SetExpr::Select(select) => {
            river_lines_from_select(select, query, config)
        }
        _ => {
            // UNION / INTERSECT / EXCEPT — fall back to keyword-case only
            vec![apply_keyword_case(&query.to_string(), &config.keyword_case)]
        }
    }
}

/// River-format a query, but ONLY if the output re-parses to the same AST.
///
/// The river renderer is an incomplete SQL pretty-printer — it does not cover
/// every construct (e.g. CTEs/`WITH`, qualified `t.*` wildcards, set
/// operations) and would otherwise silently emit different SQL. This guard
/// re-parses the river output and compares it to the source query; on any
/// mismatch — or a parse failure — it returns `None` so the caller falls back
/// to a faithful rendering. Correctness over style: an unsupported query keeps
/// its (non-river) formatting rather than being silently corrupted.
fn river_query_faithful(query: &sqlparser::ast::Query, config: &FormatConfig) -> Option<String> {
    let text = river_select_lines(query, config).join("\n");
    let dialect = PostgreSqlDialect {};
    match Parser::parse_sql(&dialect, &text) {
        Ok(stmts) if stmts.len() == 1 => match &stmts[0] {
            Statement::Query(reparsed) if **reparsed == *query => Some(text),
            _ => None,
        },
        _ => None,
    }
}

fn river_lines_from_select(
    select: &sqlparser::ast::Select,
    query: &sqlparser::ast::Query,
    config: &FormatConfig,
) -> Vec<String> {
    use sqlparser::ast::*;

    let g = config.gutter;
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    let mut lines: Vec<String> = Vec::new();

    river_emit_select_list(select, config, &mut lines);
    river_emit_from_joins(select, config, &mut lines);

    // ── WHERE ─────────────────────────────────────────────
    if let Some(selection) = &select.selection {
        let (conds, cont) = split_boolean_conditions(selection);
        emit_aligned_conditions(&conds, &kw("where"), &kw(cont), g, config, &mut lines);
    }

    // ── GROUP BY ──────────────────────────────────────────
    let group_exprs: Vec<String> = match &select.group_by {
        GroupByExpr::Expressions(exprs, _) => {
            exprs.iter().map(|e| apply_keyword_case(&e.to_string(), &config.keyword_case)).collect()
        }
        GroupByExpr::All(_) => vec![kw("all")],
    };
    if !group_exprs.is_empty() {
        lines.push(river_line(g, &kw("group by"), &group_exprs.join(", ")));
    }

    // ── HAVING ────────────────────────────────────────────
    if let Some(having) = &select.having {
        let (conds, cont) = split_boolean_conditions(having);
        emit_aligned_conditions(&conds, &kw("having"), &kw(cont), g, config, &mut lines);
    }

    river_emit_order_by(query, config, &mut lines);
    river_emit_limit(query, config, &mut lines);

    lines
}

/// Append the river-formatted SELECT projection list to `lines`.
fn river_emit_select_list(
    select: &sqlparser::ast::Select,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    use sqlparser::ast::*;

    let g = config.gutter;
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    let select_kw = if select.distinct.is_some() {
        kw("select distinct")
    } else {
        kw("select")
    };

    // Collect (rendered_expr, alias) pairs for alias-column alignment.
    let items: Vec<(String, Option<String>)> = select
        .projection
        .iter()
        .map(|item| match item {
            SelectItem::ExprWithAlias { expr, alias } => {
                let e = apply_keyword_case(&expr.to_string(), &config.keyword_case);
                (e, Some(alias.value.to_lowercase()))
            }
            SelectItem::UnnamedExpr(expr) => {
                (apply_keyword_case(&expr.to_string(), &config.keyword_case), None)
            }
            SelectItem::Wildcard(_) => ("*".to_string(), None),
            SelectItem::QualifiedWildcard(kind, _) => {
                // `kind` already renders the trailing `.*` (e.g. `l.*`); keyword-
                // case keeps identifier case like the expression arms above.
                (apply_keyword_case(&kind.to_string(), &config.keyword_case), None)
            }
        })
        .collect();

    // Max expression width across ALL items (not just aliased) so that
    // both aliased and non-aliased columns indent consistently.
    let any_aliased = items.iter().any(|(_, a)| a.is_some());
    let max_expr_len = if any_aliased {
        items.iter().map(|(e, _)| e.len()).max().unwrap_or(0)
    } else {
        0
    };

    for (i, (expr, alias)) in items.iter().enumerate() {
        let content = if any_aliased {
            let pad = max_expr_len.saturating_sub(expr.len());
            if let Some(al) = alias {
                format!("{}{} {} {}", expr, " ".repeat(pad), kw("as"), al)
            } else {
                // Non-aliased items still get padding so columns align
                format!("{}{}", expr, " ".repeat(pad))
            }
        } else {
            expr.clone()
        };
        let content = content.trim_end().to_string();

        if i == 0 {
            lines.push(river_line(g, &select_kw, &content));
        } else {
            lines.push(river_comma_line(g, &content));
        }
    }
}

/// Append the river-formatted FROM/JOIN clause to `lines`.
fn river_emit_from_joins(
    select: &sqlparser::ast::Select,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    use sqlparser::ast::*;

    let g = config.gutter;
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    // Compute max table-name length for alias alignment (only when at least one
    // table in the clause has an alias).
    let max_table_name_len: usize = {
        let mut max = 0usize;
        let any_alias = select.from.iter().any(|twj| {
            table_factor_has_alias(&twj.relation)
                || twj.joins.iter().any(|j| table_factor_has_alias(&j.relation))
        });
        if any_alias {
            for twj in &select.from {
                max = max.max(table_factor_name_len(&twj.relation));
                for join in &twj.joins {
                    max = max.max(table_factor_name_len(&join.relation));
                }
            }
        }
        max
    };

    for (j, twj) in select.from.iter().enumerate() {
        let from_kw = if j == 0 { kw("from") } else { kw(",") };

        match &twj.relation {
            TableFactor::Derived { subquery, alias, .. } => {
                lines.push(river_line(g, &from_kw, "("));
                let sub_lines = river_select_lines(subquery, config);
                let sub_indent = " ".repeat(g + 2);
                for sub_line in sub_lines {
                    lines.push(format!("{sub_indent}{sub_line}"));
                }
                let close = if let Some(a) = alias {
                    format!("{}) {}", " ".repeat(g + 1), a.name.value.to_lowercase())
                } else {
                    format!("{})", " ".repeat(g + 1))
                };
                lines.push(close);
            }
            _ => {
                let table_str = render_table_factor_aligned(&twj.relation, max_table_name_len);
                lines.push(river_line(g, &from_kw, &table_str));
            }
        }

        for join in &twj.joins {
            let (join_kw, on_expr) = extract_join_parts(join, &kw);
            let join_table = render_table_factor_aligned(&join.relation, max_table_name_len);
            lines.push(river_line(g, &join_kw, &join_table));
            if let Some(on) = on_expr {
                let (on_conds, cont) = split_boolean_conditions(on);
                emit_aligned_conditions(&on_conds, &kw("on"), &kw(cont), g, config, lines);
            }
        }
    }
}

/// Append the river-formatted ORDER BY clause to `lines`.
fn river_emit_order_by(
    query: &sqlparser::ast::Query,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    let g = config.gutter;
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    if let Some(order_by) = &query.order_by
        && let sqlparser::ast::OrderByKind::Expressions(exprs) = &order_by.kind
        && !exprs.is_empty()
    {
        let order_items: Vec<String> = exprs
            .iter()
            .map(|o| {
                let e = apply_keyword_case(&o.expr.to_string(), &config.keyword_case);
                match o.options.asc {
                    Some(false) => format!("{} {}", e, kw("desc")),
                    _ => e,
                }
            })
            .collect();
        lines.push(river_line(g, &kw("order by"), &order_items.join(", ")));
    }
}

/// Append the river-formatted LIMIT/OFFSET clause to `lines`.
fn river_emit_limit(query: &sqlparser::ast::Query, config: &FormatConfig, lines: &mut Vec<String>) {
    let g = config.gutter;
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    let Some(limit_clause) = &query.limit_clause else {
        return;
    };
    match limit_clause {
        sqlparser::ast::LimitClause::LimitOffset { limit, offset, .. } => {
            if let Some(lim) = limit {
                lines.push(river_line(g, &kw("limit"), &lim.to_string()));
            }
            if let Some(off) = offset {
                lines.push(river_line(g, &kw("offset"), &off.value.to_string()));
            }
        }
        sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
            lines.push(river_line(g, &kw("limit"), &limit.to_string()));
            lines.push(river_line(g, &kw("offset"), &offset.to_string()));
        }
    }
}

/// Emit a group of conditions (WHERE / HAVING / ON) with operator alignment.
///
/// When every condition in the group is a simple `lhs op rhs` comparison,
/// the LHS values are right-padded to the same width so operators align.
///
/// Nested OR groups `(a OR b)` within an AND chain are expanded inline:
/// ```text
///      where x     = 1
///        and (a    = 2
///          or b    = 3)
/// ```
fn emit_aligned_conditions(
    conditions: &[&sqlparser::ast::Expr],
    first_kw: &str,
    cont_kw: &str,
    gutter: usize,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    // Try to extract (lhs, op, rhs) from every condition.
    let parts: Vec<Option<(String, String, String)>> = conditions
        .iter()
        .map(|c| extract_comparison_parts(c, &config.keyword_case))
        .collect();

    let all_comparable = conditions.len() > 1 && parts.iter().all(|p| p.is_some());

    if all_comparable {
        let max_lhs = parts.iter()
            .filter_map(|p| p.as_ref().map(|(l, _, _)| l.len()))
            .max()
            .unwrap_or(0);

        for (i, part) in parts.iter().enumerate() {
            let (lhs, op, rhs) = part.as_ref().unwrap();
            let pad = max_lhs.saturating_sub(lhs.len());
            let content = format!("{}{} {} {}", lhs, " ".repeat(pad), op, rhs);
            let keyword = if i == 0 { first_kw } else { cont_kw };
            lines.push(river_line(gutter, keyword, &content));
        }
    } else {
        // Mixed: expand OR groups inline, align remaining comparisons
        let max_lhs = parts.iter()
            .filter_map(|p| p.as_ref().map(|(l, _, _)| l.len()))
            .max()
            .unwrap_or(0);
        let multi_cmp = parts.iter().filter(|p| p.is_some()).count() > 1;

        for (i, cond) in conditions.iter().enumerate() {
            let keyword = if i == 0 { first_kw } else { cont_kw };

            if let Some(or_parts) = extract_nested_or_parts(cond) {
                emit_or_group(&or_parts, keyword, gutter, config, lines);
            } else if multi_cmp && parts[i].is_some() {
                let (lhs, op, rhs) = parts[i].as_ref().unwrap();
                let pad = max_lhs.saturating_sub(lhs.len());
                let content = format!("{}{} {} {}", lhs, " ".repeat(pad), op, rhs);
                lines.push(river_line(gutter, keyword, &content));
            } else {
                let content = apply_keyword_case(&cond.to_string(), &config.keyword_case);
                lines.push(river_line(gutter, keyword, &content));
            }
        }
    }
}

/// If `expr` is `(a OR b OR ...)`, return the flattened OR parts.
fn extract_nested_or_parts(expr: &sqlparser::ast::Expr) -> Option<Vec<&sqlparser::ast::Expr>> {
    use sqlparser::ast::{BinaryOperator, Expr};
    match expr {
        Expr::Nested(inner) => match inner.as_ref() {
            Expr::BinaryOp { op: BinaryOperator::Or, .. } => Some(split_or_conditions(inner)),
            _ => None,
        },
        _ => None,
    }
}

/// Emit a parenthesized OR group with internal operator alignment.
///
/// ```text
///        and (status = 'approved'
///          or role   = 'admin')
/// ```
fn emit_or_group(
    or_conditions: &[&sqlparser::ast::Expr],
    keyword: &str,
    gutter: usize,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    let kw_or = kw_case("or", &config.keyword_case);
    let inner_gutter = gutter + 1;

    let parts: Vec<Option<(String, String, String)>> = or_conditions
        .iter()
        .map(|c| extract_comparison_parts(c, &config.keyword_case))
        .collect();
    let all_comparable = or_conditions.len() > 1 && parts.iter().all(|p| p.is_some());

    let render = |j: usize, content: &str, lines: &mut Vec<String>| {
        if j == 0 {
            lines.push(river_line(gutter, keyword, &format!("({content}")));
        } else if j == or_conditions.len() - 1 {
            lines.push(river_line(inner_gutter, &kw_or, &format!("{content})")));
        } else {
            lines.push(river_line(inner_gutter, &kw_or, content));
        }
    };

    if all_comparable {
        let max_lhs = parts.iter()
            .filter_map(|p| p.as_ref().map(|(l, _, _)| l.len()))
            .max()
            .unwrap_or(0);

        for (j, part) in parts.iter().enumerate() {
            let (lhs, op, rhs) = part.as_ref().unwrap();
            let pad = max_lhs.saturating_sub(lhs.len());
            let content = format!("{}{} {} {}", lhs, " ".repeat(pad), op, rhs);
            render(j, &content, lines);
        }
    } else {
        for (j, cond) in or_conditions.iter().enumerate() {
            let content = apply_keyword_case(&cond.to_string(), &config.keyword_case);
            render(j, &content, lines);
        }
    }
}

/// Try to decompose a condition expression into `(lhs_str, op_str, rhs_str)`
/// for a simple comparison.  Returns `None` for complex/compound expressions.
fn extract_comparison_parts(
    expr: &sqlparser::ast::Expr,
    case: &KeywordCase,
) -> Option<(String, String, String)> {
    use sqlparser::ast::Expr;
    match expr {
        Expr::BinaryOp { left, op, right } if is_comparison_op(op) => {
            let lhs = apply_keyword_case(&left.to_string(), case);
            let op_str = comparison_op_str(op, case);
            let rhs = apply_keyword_case(&right.to_string(), case);
            Some((lhs, op_str, rhs))
        }
        Expr::IsNull(e) => Some((
            apply_keyword_case(&e.to_string(), case),
            kw_case("is", case),
            kw_case("null", case),
        )),
        Expr::IsNotNull(e) => Some((
            apply_keyword_case(&e.to_string(), case),
            kw_case("is not", case),
            kw_case("null", case),
        )),
        _ => None,
    }
}

fn is_comparison_op(op: &sqlparser::ast::BinaryOperator) -> bool {
    use sqlparser::ast::BinaryOperator as B;
    matches!(
        op,
        B::Eq | B::NotEq | B::Gt | B::Lt | B::GtEq | B::LtEq
            | B::PGRegexMatch | B::PGRegexIMatch
            | B::PGRegexNotMatch | B::PGRegexNotIMatch
    )
}

fn comparison_op_str(op: &sqlparser::ast::BinaryOperator, case: &KeywordCase) -> String {
    use sqlparser::ast::BinaryOperator as B;
    match op {
        B::Eq => "=".to_string(),
        B::NotEq => "!=".to_string(),
        B::Gt => ">".to_string(),
        B::Lt => "<".to_string(),
        B::GtEq => ">=".to_string(),
        B::LtEq => "<=".to_string(),
        B::PGRegexMatch => "~".to_string(),
        B::PGRegexIMatch => "~*".to_string(),
        B::PGRegexNotMatch => "!~".to_string(),
        B::PGRegexNotIMatch => "!~*".to_string(),
        other => apply_keyword_case(&other.to_string(), case),
    }
}

fn table_factor_name_len(tf: &sqlparser::ast::TableFactor) -> usize {
    match tf {
        sqlparser::ast::TableFactor::Table { name, .. } => {
            name.to_string().to_lowercase().len()
        }
        _ => 0,
    }
}

fn table_factor_has_alias(tf: &sqlparser::ast::TableFactor) -> bool {
    match tf {
        sqlparser::ast::TableFactor::Table { alias, .. } => alias.is_some(),
        _ => false,
    }
}

fn render_table_factor_aligned(
    tf: &sqlparser::ast::TableFactor,
    max_name_len: usize,
) -> String {
    match tf {
        sqlparser::ast::TableFactor::Table { name, alias, .. } => {
            let n = name.to_string().to_lowercase();
            match alias {
                Some(a) => {
                    let pad = max_name_len.saturating_sub(n.len());
                    format!("{}{} {}", n, " ".repeat(pad), a.name.value.to_lowercase())
                }
                None => n,
            }
        }
        other => other.to_string().to_lowercase(),
    }
}

fn extract_join_parts<'a>(
    join: &'a sqlparser::ast::Join,
    kw: &impl Fn(&str) -> String,
) -> (String, Option<&'a sqlparser::ast::Expr>) {
    use sqlparser::ast::{JoinConstraint, JoinOperator};

    let (keyword, constraint) = match &join.join_operator {
        // Plain `JOIN` and `INNER JOIN` are distinct variants in sqlparser.
        JoinOperator::Join(c) => (kw("join"), Some(c)),
        JoinOperator::Inner(c) => (kw("inner join"), Some(c)),
        // `LEFT JOIN` (Left) and `LEFT OUTER JOIN` (LeftOuter) are distinct too —
        // both render as "left join". Same for right.
        JoinOperator::Left(c) | JoinOperator::LeftOuter(c) => (kw("left join"), Some(c)),
        JoinOperator::Right(c) | JoinOperator::RightOuter(c) => (kw("right join"), Some(c)),
        JoinOperator::FullOuter(c) => (kw("full join"), Some(c)),
        JoinOperator::CrossJoin(_) => return (kw("cross join"), None),
        // Exotic joins (semi/anti/apply/as-of/straight) don't appear in normal
        // DDL views; keep the bare keyword but still surface any ON below rather
        // than silently dropping it.
        JoinOperator::Semi(c) | JoinOperator::LeftSemi(c) | JoinOperator::RightSemi(c) => {
            (kw("join"), Some(c))
        }
        JoinOperator::Anti(c) | JoinOperator::LeftAnti(c) | JoinOperator::RightAnti(c) => {
            (kw("join"), Some(c))
        }
        JoinOperator::StraightJoin(c) => (kw("join"), Some(c)),
        _ => return (kw("join"), None),
    };

    let on_expr = constraint.and_then(|c| match c {
        JoinConstraint::On(expr) => Some(expr),
        _ => None,
    });

    (keyword, on_expr)
}

/// Flatten top-level AND conditions into a vec.
/// `a AND b AND c` → `[a, b, c]`
fn split_and_conditions(expr: &sqlparser::ast::Expr) -> Vec<&sqlparser::ast::Expr> {
    use sqlparser::ast::{BinaryOperator, Expr};
    match expr {
        Expr::BinaryOp { left, op: BinaryOperator::And, right } => {
            let mut v = split_and_conditions(left);
            v.extend(split_and_conditions(right));
            v
        }
        other => vec![other],
    }
}

/// Flatten top-level OR conditions into a vec.
/// `a OR b OR c` → `[a, b, c]`
fn split_or_conditions(expr: &sqlparser::ast::Expr) -> Vec<&sqlparser::ast::Expr> {
    use sqlparser::ast::{BinaryOperator, Expr};
    match expr {
        Expr::BinaryOp { left, op: BinaryOperator::Or, right } => {
            let mut v = split_or_conditions(left);
            v.extend(split_or_conditions(right));
            v
        }
        other => vec![other],
    }
}

/// Split a boolean expression into flat conditions, returning the continuation keyword.
/// Top-level AND → (parts, "and"); top-level OR → (parts, "or"); single → (vec![expr], "and").
fn split_boolean_conditions(expr: &sqlparser::ast::Expr) -> (Vec<&sqlparser::ast::Expr>, &'static str) {
    use sqlparser::ast::{BinaryOperator, Expr};
    match expr {
        Expr::BinaryOp { op: BinaryOperator::And, .. } => (split_and_conditions(expr), "and"),
        Expr::BinaryOp { op: BinaryOperator::Or, .. } => (split_or_conditions(expr), "or"),
        other => (vec![other], "and"),
    }
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
