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
///
/// A file PostgreSQL's scanner cannot read — an unterminated string or
/// comment — is returned as it is: nothing in it can be told from text.
pub fn format_ddl(input: &str, config: &FormatConfig) -> String {
    if lex(input).is_none() {
        return input.to_string();
    }
    let blocks = split_statements(input);
    let mut output_parts: Vec<String> = Vec::new();

    for block in &blocks {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }
        output_parts.push(match around_statement(trimmed) {
            // The comments around a statement are text, kept as written and
            // where they were written — on its line, or on the lines above
            // it. Only the statement between them is formatted: a layout
            // carries no comment outside the statement, so one beside or
            // above it made the guard keep the whole statement as written.
            Some((leading, statement, trailing)) => {
                format!(
                    "{}{}{trailing}",
                    settle_leading(leading),
                    format_block(statement, config)
                )
            }
            None => trimmed.to_string(),
        });
    }

    let mut result = output_parts.join("\n\n");
    // Ensure trailing newline
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// The comments before a statement, with the space after them settled: a
/// space when they share the statement's line, else the line breaks alone.
/// A layout may start with indentation of its own (river's `    select`);
/// kept as written, the gap before it grew by that much on every pass.
fn settle_leading(leading: &str) -> String {
    let text = leading.trim_end();
    if text.is_empty() {
        return String::new();
    }
    match leading[text.len()..].matches('\n').count() {
        0 => format!("{text} "),
        breaks => format!("{text}{}", "\n".repeat(breaks)),
    }
}

/// One statement, formatted as far as stays faithful to it: laid out with its
/// keywords in the configured case; failing that, as written with its keywords
/// recased; failing that, as written.
///
/// Safety guard: never emit a reformatted statement that changed the SQL. A
/// layout that does not re-parse to the same tree, or changes a string, a
/// body, a quoted identifier or a comment, is not used (see
/// [`block_is_faithful`]). The layout covers some statements only partly (a table's
/// storage parameters), and falling back to the statement untouched left its
/// keywords in whatever case the file had — `upper` leaving a table lowercase —
/// so the fallback still recases them.
fn format_block(original: &str, config: &FormatConfig) -> String {
    let laid_out = match config.keyword_case {
        // The layout writes keywords of its own, so it is laid out in one case
        // and each keyword given back the spelling the statement used.
        KeywordCase::Preserve => {
            let lower = FormatConfig {
                keyword_case: KeywordCase::Lower,
                ..config.clone()
            };
            preserve_keyword_case(&format_statement_block(original, &lower), original)
        }
        // Recased again whole: the layout recases the pieces it writes one
        // at a time, and a piece alone — `COLLATE "C"`, `BIT(3)` — does not
        // parse, so the grammar cannot say which of its words are keywords.
        ref case => apply_keyword_case(&format_statement_block(original, config), case),
    };
    let laid_out = spelled_as_written(original, &laid_out);
    if block_is_faithful(original, &laid_out) {
        return laid_out;
    }
    let recased = ensure_semicolon(&apply_keyword_case(original, &config.keyword_case));
    if block_is_faithful(original, &recased) {
        recased
    } else {
        ensure_semicolon(original)
    }
}

/// Whether a formatted statement faithfully preserves the original: it means
/// the same, and every string, body, quoted identifier and comment in it is
/// the same text, in the same order (see [`opaque_text`]).
///
/// What it means is libpg_query's tree, deparsed — keyword case, layout and
/// comments aside — whenever the original parses there. A statement libpg_query
/// cannot parse (another engine's DDL, a SQLite trigger) falls back to
/// sqlparser's tree, and one neither parses rests on its text alone.
///
/// sqlparser's trees are compared with both sides' keywords lowercased. A
/// keyword's case is not meaning — an unquoted word reads the same in any
/// case, Postgres folds it — but sqlparser keeps a word it reads as a name in
/// the case it was written: `serial`, `inet`, `search_path` and `sql` are
/// names to it, and `SERIAL` parsed to a different tree than `serial`.
fn block_is_faithful(original: &str, formatted: &str) -> bool {
    match (opaque_text(original), opaque_text(formatted)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return false,
    }
    if let Ok(before) = pg_query::parse(original) {
        // A layout libpg_query cannot read, or a tree it cannot deparse, is
        // not shown to mean the same — so it is not used.
        return match (
            before.deparse(),
            pg_query::parse(formatted).and_then(|after| after.deparse()),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
    }
    let dialect = PostgreSqlDialect {};
    let lowered = |sql: &str| ensure_semicolon(&apply_keyword_case(sql, &KeywordCase::Lower));
    match (
        Parser::parse_sql(&dialect, &lowered(original)),
        Parser::parse_sql(&dialect, &lowered(formatted)),
    ) {
        (Ok(a), Ok(b)) => a == b,
        // The formatter turned parseable SQL into something that no longer
        // parses — definitely not faithful.
        (Ok(_), Err(_)) => false,
        // Neither parser reads it; the text check above is what stands.
        (Err(_), _) => true,
    }
}

/// Format a single statement block (including its trailing semicolon).
fn format_statement_block(block: &str, config: &FormatConfig) -> String {
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Try parsing with sqlparser
    let dialect = PostgreSqlDialect {};
    let sql_to_parse = ensure_semicolon(trimmed);
    match Parser::parse_sql(&dialect, &sql_to_parse) {
        // A statement with a dollar-quoted body (a function's, a `DO`
        // block's) is only recased: the body is one token of text, left as
        // written. A table's `$$` string is a literal like any other — and
        // skipping it kept every table with a `$$` default as written.
        Ok(statements) if has_dollar_body(trimmed) && !matches!(statements.as_slice(), [Statement::CreateTable(_)]) => {
            ensure_semicolon(&apply_keyword_case(trimmed, &config.keyword_case))
        }
        Ok(statements) if !statements.is_empty() => format_parsed_statements(&statements, trimmed, config),
        _ => {
            // Unparseable: apply keyword case only
            let result = apply_keyword_case(trimmed, &config.keyword_case);
            ensure_semicolon(&result)
        }
    }
}

/// Format parsed statements, dispatching to specific formatters.
fn format_parsed_statements(statements: &[Statement], original: &str, config: &FormatConfig) -> String {
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
            Statement::CreateType {
                name,
                representation: Some(repr),
            } => {
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

// ── Helpers ─────────────────────────────────────────────

/// Apply keyword case to a single token (not a full SQL string).
fn kw_case(s: &str, case: &KeywordCase) -> String {
    match case {
        KeywordCase::Lower => s.to_lowercase(),
        KeywordCase::Upper => s.to_uppercase(),
        KeywordCase::Preserve => s.to_string(),
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
        assert!(
            !result.contains("CREATE"),
            "Should not contain uppercase CREATE, got: {result}"
        );
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
            result.contains("create or replace function") || result.contains("CREATE OR REPLACE FUNCTION"),
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

    // ── keyword_case: every mode, every keyword, every layout ──────────────

    /// One file holding every statement layout the formatter has: SET, a
    /// table (serial and inet columns, an inline foreign key with an action, a
    /// default, a comment, a named constraint), an index, a view (river), a
    /// COMMENT ON, an enum type, a function with a `$$` body, and a table the
    /// layout cannot render faithfully (its storage parameters), which keeps
    /// its own layout.
    const EVERY_LAYOUT: &str = "set search_path to app;\n\n\
        create table if not exists t (\n  \
          id serial primary key,\n  \
          addr inet not null,\n  \
          name varchar(20) default 'Select me', -- SELECT is a word here\n  \
          owner_id integer references owners (id) on delete cascade,\n  \
          created timestamptz not null default now(),\n  \
          constraint t_name_uq unique (name)\n\
        );\n\n\
        create index if not exists t_name_idx on t (name);\n\n\
        create or replace view v as select t.id, t.name from t inner join owners o on o.id = t.owner_id \
          where t.id > 1 order by t.id;\n\n\
        comment on table t is 'A Table';\n\n\
        create type mood as enum ('happy', 'sad');\n\n\
        create or replace function f() returns integer language sql as $$ select 1 $$;\n\n\
        create table if not exists w (id integer) with (fillfactor = 70);\n";

    /// Every keyword in `sql` as written, outside string literals, quoted
    /// identifiers, comments and `$$` bodies — where a word is text, not SQL.
    fn keywords_in(sql: &str) -> Vec<String> {
        let keywords = sql_keywords();
        let mut out = Vec::new();
        let chars: Vec<char> = sql.chars().collect();
        let mut i = 0;
        let skip_to = |i: usize, end: &str| -> usize {
            let rest: String = chars[i..].iter().collect();
            rest.find(end)
                .map_or(chars.len(), |p| i + rest[..p].chars().count() + end.chars().count())
        };
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if c == '\'' {
                i = skip_to(i + 1, "'");
            } else if c == '"' {
                i = skip_to(i + 1, "\"");
            } else if c == '$' && next == Some('$') {
                i = skip_to(i + 2, "$$");
            } else if c == '-' && next == Some('-') {
                i = skip_to(i, "\n");
            } else if c == '/' && next == Some('*') {
                i = skip_to(i + 2, "*/");
            } else if c.is_ascii_alphabetic() || c == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if keywords.contains(word.to_uppercase().as_str()) {
                    out.push(word);
                }
            } else {
                i += 1;
            }
        }
        out
    }

    fn in_case(case: KeywordCase) -> FormatConfig {
        FormatConfig {
            keyword_case: case,
            ..Default::default()
        }
    }

    /// `lower` and `upper` recase every keyword — the SET that `upper` left
    /// lowercase, and the tables with a serial or inet column, which `upper`
    /// left wholly lowercase: their uppercased type no longer matched
    /// sqlparser's case-sensitive type name, the faithfulness guard took that
    /// for a change of meaning, and the original was kept. A table whose
    /// layout cannot be rendered is still recased. Strings, comments and `$$`
    /// bodies are text and keep their case.
    #[test]
    fn lower_and_upper_recase_every_keyword_in_every_layout() {
        for (case, expected) in [(KeywordCase::Lower, false), (KeywordCase::Upper, true)] {
            let out = format_ddl(EVERY_LAYOUT, &in_case(case.clone()));
            let words = keywords_in(&out);
            assert!(words.len() > 60, "the keywords are all there: {words:?}\n{out}");
            let wrong: Vec<&String> = words.iter().filter(|w| (w.to_uppercase() == **w) != expected).collect();
            assert!(wrong.is_empty(), "{case:?} left {wrong:?}:\n{out}");
            for text in ["'Select me'", "-- SELECT is a word here", "$$ select 1 $$", "'A Table'"] {
                assert!(out.contains(text), "{case:?} keeps {text} as written:\n{out}");
            }
        }
    }

    /// Every word libpg_query's grammar reads as a keyword is recased, not
    /// just those on a hand-written list: `ATOMIC`, `COLLATE`, `BIT`,
    /// `PARTITION BY RANGE` kept whatever case the file had. And a word the
    /// grammar reads as a name keeps its case though it is a keyword
    /// elsewhere — the columns `name`, `value` and `level`.
    #[test]
    fn every_keyword_the_grammar_reads_is_recased_and_no_name_is() {
        let sql = "CREATE TABLE t (name text COLLATE \"C\", value BIT(3), level int) PARTITION BY RANGE (level);\n\
                   CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; END;\n\
                   CREATE TABLE u (id int GENERATED ALWAYS AS IDENTITY, v text) WITH (fillfactor = 70);\n\
                   COMMENT ON COLUMN t.name IS 'x';\n";
        let lower = format_ddl(sql, &in_case(KeywordCase::Lower));
        for spelled in ["collate \"C\"", "bit(3)", "partition by range", "begin atomic", "end;"] {
            assert!(lower.contains(spelled), "lower writes {spelled}:\n{lower}");
        }
        for unspelled in ["COLLATE", "BIT", "PARTITION", "ATOMIC", "END"] {
            assert!(!lower.contains(unspelled), "lower leaves no {unspelled}:\n{lower}");
        }
        let upper = format_ddl(
            &sql.to_lowercase().replace("\"c\"", "\"C\""),
            &in_case(KeywordCase::Upper),
        );
        for spelled in [
            "COLLATE \"C\"",
            "BIT(3)",
            "PARTITION BY RANGE",
            "BEGIN ATOMIC",
            "END;",
            "WITH (fillfactor",
        ] {
            assert!(upper.contains(spelled), "upper writes {spelled}:\n{upper}");
        }
        for name in ["NAME", "VALUE", "LEVEL", "FILLFACTOR"] {
            assert!(
                !upper.contains(name),
                "upper leaves the name {name} as written:\n{upper}"
            );
        }
    }

    /// `preserve` uppercased a lowercase file: the layout writes its keywords
    /// itself (`CREATE TABLE`, `PRIMARY KEY`, sqlparser's `VARCHAR`), and wrote
    /// them uppercase whatever the file said. Each keyword now keeps the
    /// spelling the statement gave it — over a lowercase file, an uppercase one
    /// and a mixed one alike.
    #[test]
    fn preserve_keeps_every_keyword_as_the_statement_spelled_it() {
        let upper: String = {
            let mut out = EVERY_LAYOUT.to_string();
            for word in keywords_in(EVERY_LAYOUT) {
                out = out.replace(&format!(" {word} "), &format!(" {} ", word.to_uppercase()));
            }
            format_ddl(&out, &in_case(KeywordCase::Upper))
        };
        // Every other keyword uppercased, consistently: one spelling a word.
        let mut distinct: Vec<String> = keywords_in(EVERY_LAYOUT).iter().map(|w| w.to_lowercase()).collect();
        distinct.sort();
        distinct.dedup();
        let shout: std::collections::HashSet<&str> = distinct.iter().step_by(2).map(String::as_str).collect();
        let mixed = format_ddl(&upper, &in_case(KeywordCase::Lower))
            .split_inclusive(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .map(|piece| {
                let word = piece.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                if shout.contains(word) {
                    piece.replacen(word, &word.to_uppercase(), 1)
                } else {
                    piece.to_string()
                }
            })
            .collect::<String>();

        for (input, label) in [
            (EVERY_LAYOUT.to_string(), "lowercase"),
            (upper.clone(), "uppercase"),
            (mixed.clone(), "mixed"),
        ] {
            let written: std::collections::HashMap<String, String> =
                keywords_in(&input).into_iter().map(|w| (w.to_uppercase(), w)).collect();
            let out = format_ddl(&input, &in_case(KeywordCase::Preserve));
            let wrong: Vec<String> = keywords_in(&out)
                .into_iter()
                .filter(|w| written.get(&w.to_uppercase()).is_some_and(|spelled| spelled != w))
                .collect();
            assert!(
                wrong.is_empty(),
                "{label}: {wrong:?} changed case:\n{input}\n--- became ---\n{out}"
            );
        }
        let lowered = format_ddl(EVERY_LAYOUT, &in_case(KeywordCase::Preserve));
        assert!(
            keywords_in(&lowered).iter().all(|w| w.to_lowercase() == *w),
            "a lowercase file stays lowercase, the keywords the layout adds included:\n{lowered}"
        );
        let shouted = format_ddl(&upper, &in_case(KeywordCase::Preserve));
        assert!(
            keywords_in(&shouted).iter().all(|w| w.to_uppercase() == *w),
            "an uppercase file stays uppercase:\n{shouted}"
        );
    }

    /// A column's `REFERENCES … ON DELETE CASCADE` lost its action in the
    /// layout, so the guard kept every such table as it was written: no
    /// layout, and no keyword case.
    #[test]
    fn a_column_foreign_key_keeps_its_action_through_the_layout() {
        let out = format_ddl(EVERY_LAYOUT, &in_case(KeywordCase::Lower));
        assert!(out.contains(", owner_id"), "the table is laid out:\n{out}");
        assert!(out.contains("on delete cascade"), "{out}");
    }

    // ── Literals, bodies and comments are text ──────────────

    /// Statements whose text holds what a hand-rolled scanner misreads: an
    /// `E''` string with a backslash-escaped quote, `U&''` with `UESCAPE`,
    /// bit and hex strings, a `$tag$` body (one nesting `$$`), a `BEGIN
    /// ATOMIC` body, nested block comments, quoted identifiers holding `"`,
    /// `;` and `--`, a comment-only tail and a last statement with no `;`.
    const TRICKY: &[&str] = &[
        "create table if not exists t (\n  id int primary key,\n  note text default E'it\\'s; tricky'\n);",
        "create table u (a text default U&'d\\0061t\\+000061 select', b text default U&'x!0041' uescape '!');",
        "create table b (b bit(4) default B'0101', x bytea default '\\xDEAD', y bytea default X'FF');",
        "create table q (a text default 'O''Brien; -- not a comment', b text default 'SELECT /* nor */ this');",
        "comment on table t is E'line one\\nline two; -- still text';",
        "create or replace function app.f() returns text language plpgsql as $body$\nBEGIN\n  -- SELECT stays; here\n  RETURN 'x';\nEND;\n$body$;",
        "create function g() returns text language sql as $fn$ select $$a;b$$ || 'c' $fn$;",
        "create view v as select 'a;b' as \"Semi;Colon\", \"we\"\"ird\" from t where note <> E'back\\\\slash';",
        "create table \"we\"\"ird;--\" (\"Col; x\" int, y text default '');",
        "/* outer /* nested; */ still; */\n-- it's; here\ncreate table c (id int);",
        "create function h() returns int language sql begin atomic select 1; select 2; end;",
        "create index t_live_idx on t (note) where note <> 'x;y' and note <> E'a\\'b';",
        "create type mood as enum ('happy;', 'sad''s', E'it\\'s');",
        "set search_path to app; -- path\ncreate table app.t2 (id int, s text default 'set search_path to x;');",
        "do $$ begin raise notice 'it''s; %', 1; end $$;",
        "create table k (a int -- first; one\n, b text default E'x\\'y' -- second\n);",
        "create table z (id int);\n-- the end",
        "select 1 -- trailing, no semicolon",
    ];

    /// Every token of `sql` that is text rather than SQL — strings of every
    /// kind (`$tag$` bodies included), quoted identifiers and comments — in
    /// order, as libpg_query's scanner reads them. A comment's trailing
    /// spaces are layout, not text.
    fn opaque_tokens(sql: &str) -> Vec<String> {
        use pg_query::protobuf::Token;
        let scanned = pg_query::scan(sql).unwrap_or_else(|e| panic!("{e}:\n{sql}"));
        scanned
            .tokens
            .iter()
            .filter_map(|t| {
                let text = &sql[t.start as usize..t.end as usize];
                match Token::try_from(t.token).ok()? {
                    Token::Sconst | Token::Usconst | Token::Bconst | Token::Xconst | Token::CComment => {
                        Some(text.to_string())
                    }
                    Token::SqlComment => Some(text.trim_end().to_string()),
                    Token::Ident | Token::Uident
                        if !text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') =>
                    {
                        Some(text.to_string())
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// What `sql` means: libpg_query's tree for it, deparsed — keyword case,
    /// layout and comments aside.
    fn meaning(sql: &str) -> String {
        pg_query::parse(sql)
            .and_then(|p| p.deparse())
            .unwrap_or_else(|e| panic!("{e}:\n{sql}"))
    }

    /// `E'it\'s; tricky'` became a three-line default: the splitter ended the
    /// statement at the `;` inside the literal, which it took to close at
    /// `\'`. A `$tag$` body was split at its own `;`s and its `END` recased.
    /// Splitting and every rewrite now read the text with libpg_query, so for
    /// every input, in every keyword case and query style, the output means
    /// what the input did and every literal, quoted name and comment in it is
    /// the same, byte for byte.
    #[test]
    fn formatting_changes_no_literal_and_no_meaning() {
        let configs = [KeywordCase::Lower, KeywordCase::Upper, KeywordCase::Preserve]
            .into_iter()
            .flat_map(|case| {
                [crate::config::QueryStyle::River, crate::config::QueryStyle::None].map(|style| FormatConfig {
                    keyword_case: case.clone(),
                    query_style: style,
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();
        let whole = TRICKY.join("\n\n");
        for input in TRICKY.iter().copied().chain([whole.as_str()]) {
            for config in &configs {
                let out = format_ddl(input, config);
                let label = format!("{:?}/{:?}", config.keyword_case, config.query_style);
                assert_eq!(
                    meaning(&out),
                    meaning(input),
                    "{label} changed the meaning of:\n{input}\n--- into ---\n{out}"
                );
                assert_eq!(
                    opaque_tokens(&out),
                    opaque_tokens(input),
                    "{label} changed a literal, name or comment in:\n{input}\n--- into ---\n{out}"
                );
            }
        }
    }

    /// Inputs a second pass treated differently from the first: a final
    /// statement with no `;` and a comment after it, on its line or the next;
    /// a comment on the line a statement ends; leading comments; bodies.
    const SECOND_PASS: &[&str] = &[
        "create table app.t (\n  id int primary key,\n  note text\n)\n-- c\n",
        "create table app.t (id int primary key, note text) -- c",
        "create table a (id int); -- the a table\ncreate table b (id int);",
        "-- header\ncreate table b (id int primary key, note text);",
        "/* lead */ create table c (id int);",
        "create function f() returns int language sql as $$ select 1; $$",
        "create function g() returns int language plpgsql as $tag$ begin return 1; end $tag$; -- done",
        "create function h() returns int language sql begin atomic select 1; select 2; end",
        "create function h2() returns int language sql begin atomic select 1; end -- atomic",
        "create table d (id int, note text default $$x$$); /* after */",
        "set search_path to app;\n\ncreate table e (\n  id int -- the id\n, note text\n);\n-- trailing\n",
        "-- only a comment\n",
        "select 1 /* a */ ; select 2 -- b",
        "create table f (id int)\n\n\n-- c1\n-- c2\n",
        "comment on table t is 'a; b'",
        "create table g (n text default E'it\\'s; tricky') -- x",
        "create view v as select 1 as a -- c",
        "create index i on t (a) -- c",
        "create type mood as enum ('a', 'b') -- c",
        "CREATE TABLE T (ID INT) -- C",
        "create table h (id int); -- one\n-- two\ncreate table i (id int);",
    ];

    /// `format` then `format` again changes nothing, and every comment comes
    /// out once, as written, in order. A final statement with no `;` and a
    /// `-- c` after it became `…; -- c`, and the next pass moved the comment
    /// to a line of its own — so `format --check` failed right after `format`.
    /// A comment on the line a statement ends stays on that line: it moved
    /// below it, onto the next statement.
    #[test]
    fn formatting_twice_changes_nothing_and_keeps_every_comment() {
        let configs = [KeywordCase::Lower, KeywordCase::Upper, KeywordCase::Preserve]
            .into_iter()
            .flat_map(|case| {
                [CommaStyle::Leading, CommaStyle::Trailing].map(|comma_style| FormatConfig {
                    keyword_case: case.clone(),
                    comma_style,
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();
        for input in SECOND_PASS.iter().chain(TRICKY) {
            for config in &configs {
                let label = format!("{:?}/{:?}", config.keyword_case, config.comma_style);
                let once = format_ddl(input, config);
                let twice = format_ddl(&once, config);
                assert_eq!(
                    twice, once,
                    "{label}: a second pass changed\n{input}\n--- once ---\n{once}"
                );
                assert_eq!(
                    opaque_tokens(&once),
                    opaque_tokens(input),
                    "{label} changed a literal, name or comment in:\n{input}\n--- into ---\n{once}"
                );
            }
        }
        let out = format_ddl(SECOND_PASS[2], &FormatConfig::default());
        let line = out.lines().find(|l| l.contains("-- the a table")).unwrap();
        assert!(
            line.ends_with("); -- the a table"),
            "stays where it was written:\n{out}"
        );
        let out = format_ddl(SECOND_PASS[1], &FormatConfig::default());
        assert!(out.trim_end().ends_with("); -- c"), "{out}");
    }

    /// A table is laid out whatever its literals, names and clauses — as
    /// long as the layout means the same. Each of these was recased and left
    /// as written: the layout re-spelled `U&'d\0061t'` as `U&'dat'`, skipped
    /// any statement with a `$$` string, lowercased a quoted name or dropped
    /// its quotes, wrote a column's CHECK as `check (check (…))`, dropped a
    /// column constraint's name, and wrote nothing after the column list.
    /// (The layout is sqlparser's tree, so a table sqlparser cannot read —
    /// `UNLOGGED`, `EXCLUDE`, `LIKE` — is still only recased.)
    #[test]
    fn a_table_is_laid_out_whatever_its_literals_names_and_clauses() {
        for input in [
            "create table t (id int, note text default U&'d\\0061t');",
            "create table t (id int, note text default $$x$$);",
            "create table t (id int, note text default $tag$x$tag$);",
            "create table t (id int, \"Note\" text, \"a b\" int, \"x\"\"y\" int, \"note\" text);",
            "create table t (id int, note text check (note <> 'x'), n int check (n > 0));",
            "create table \"T\" (id int);",
            "create table app.\"T\" (id int);",
            "create table t (id int constraint nn not null, n int constraint n_pk primary key);",
            "create table t (id int) with (fillfactor = 70);",
            "create table t (id int) partition by range (id);",
            "create table t (id int) inherits (u);",
            "create table t (id int) tablespace pg_default;",
            "create temp table if not exists t (id int);",
            "-- header\ncreate table t (id int);",
        ] {
            for config in [
                in_case(KeywordCase::Lower),
                in_case(KeywordCase::Upper),
                in_case(KeywordCase::Preserve),
            ] {
                let out = format_ddl(input, &config);
                let first = out
                    .lines()
                    .find(|l| l.trim_start().starts_with("id"))
                    .unwrap_or_default();
                assert!(
                    first.starts_with("  id                       "),
                    "{:?} lays out:\n{input}\n--- into ---\n{out}",
                    config.keyword_case
                );
                assert_eq!(meaning(&out), meaning(input), "{input}\n--- into ---\n{out}");
                assert_eq!(
                    opaque_tokens(&out),
                    opaque_tokens(input),
                    "{input}\n--- into ---\n{out}"
                );
            }
        }
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
        assert!(
            result.contains("    select"),
            "select should be right-aligned: {result}"
        );
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
        assert!(
            result.contains("id         as"),
            "id should be padded for alignment: {result}"
        );
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
        assert!(
            result.contains("on f.id = n.folder_id"),
            "first join's ON kept: {result}"
        );
        assert!(
            result.contains("on p.id = f.project_id"),
            "left join's ON kept: {result}"
        );
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
        assert!(
            result.to_lowercase().contains("with t as"),
            "CTE must survive: {result}"
        );
        assert!(result.contains("y.id = x.id"), "CTE inner join must survive: {result}");
    }

    #[test]
    fn river_qualified_wildcard_not_doubled() {
        // `l.*` was being rendered as `l.*.*`. The guard must fall back so the
        // wildcard is emitted faithfully.
        let input = "CREATE VIEW v AS SELECT l.*, p.name FROM libs l JOIN projs p ON p.id = l.pid;";
        let result = format_ddl(input, &river_config());
        assert!(
            !result.contains(".*.*"),
            "qualified wildcard must not be doubled: {result}"
        );
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
        assert!(
            result.contains("\n  a"),
            "column a reformatted onto its own line: {result}"
        );
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
        assert!(
            !result.contains("\n  -- y"),
            "string content must not become a comment: {result}"
        );
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
        assert!(
            result.contains("    select"),
            "default config should river-style: {result}"
        );
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
        let and_line = lines.iter().find(|l| l.trim_start().starts_with("and")).unwrap();
        // The operator column should be the same in both lines
        let op_col_where = where_line.find('=').expect("= in where line");
        let op_col_and = and_line.find('>').expect("> in and line");
        assert_eq!(
            op_col_where, op_col_and,
            "operators must align:\n  where: {where_line}\n  and:   {and_line}"
        );
    }

    #[test]
    fn r11_river_table_alias_alignment() {
        let input = "SELECT u.id, o.id FROM users u INNER JOIN orders o ON o.user_id = u.id;";
        let result = format_ddl(input, &river_config());
        // "users" is 5 chars, "orders" is 6 chars → "users" padded by 1
        // Both aliases should appear after padding
        assert!(
            result.contains("users  u") || result.contains("users u"),
            "users alias present: {result}"
        );
        assert!(result.contains("orders o"), "orders alias present: {result}");
        // Check alias positions match
        let lines: Vec<&str> = result.lines().collect();
        let from_line = lines.iter().find(|l| l.trim_start().starts_with("from")).unwrap();
        let join_line = lines.iter().find(|l| l.trim_start().starts_with("inner")).unwrap();
        let alias_u = from_line.find(" u").expect("alias u");
        let alias_o = join_line.find(" o").expect("alias o");
        assert_eq!(
            alias_u, alias_o,
            "table aliases must align:\n  from: {from_line}\n  join: {join_line}"
        );
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
        let input =
            "SELECT id FROM users WHERE active = true AND (status = 'approved' OR role = 'admin') AND count > 10;";
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
        assert!(
            or_line.contains("role   =") || or_line.contains("role ="),
            "role should be padded for alignment: {result}"
        );
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
        assert_eq!(
            first, second,
            "river formatting should be idempotent:\nfirst:\n{first}\nsecond:\n{second}"
        );
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

    /// The formatter knew only `$$`: a `$tag$` body was split at each `;` in
    /// it, a blank line went in at every split, and `keyword_case: upper`
    /// uppercased the body's `end`. In every keyword case the body is one
    /// statement and comes out byte for byte.
    #[test]
    fn a_tag_quoted_body_is_one_statement_and_kept_as_written() {
        let body = "$body$\nbegin\n  -- select stays; here\n  return 'x';\nend;\n$body$";
        let input = format!("create or replace function f() returns text language plpgsql as {body};\nselect 1;\n");
        let blocks = super::split_statements(&input);
        let nonempty: Vec<&String> = blocks.iter().filter(|b| !b.trim().is_empty()).collect();
        assert_eq!(nonempty.len(), 2, "the body ends no statement: {nonempty:#?}");
        for case in [KeywordCase::Lower, KeywordCase::Upper, KeywordCase::Preserve] {
            let out = format_ddl(&input, &in_case(case.clone()));
            assert!(out.contains(body), "{case:?} keeps the body as written:\n{out}");
        }
    }
}
