use std::collections::HashSet;

use crate::config::KeywordCase;

// ── Keyword case transformation ─────────────────────────

/// SQL keywords for case transformation.
pub(in crate::formatter) fn sql_keywords() -> HashSet<&'static str> {
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
/// Copy the rest of a single-quoted literal (opening quote already emitted)
/// into `result`, honoring `''` (SQL escaped-quote) the same way
/// `split::scan_single_quoted` does — a lone `'` closes the literal.
pub(in crate::formatter) fn copy_single_quoted(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, result: &mut String) {
    while let Some(ch) = chars.next() {
        result.push(ch);
        if ch == '\'' {
            // `''` escape stays inside the string; a lone `'` closes it.
            if chars.peek() == Some(&'\'') {
                result.push(chars.next().unwrap());
                continue;
            }
            break;
        }
    }
}

/// Copy the rest of a double-quoted identifier (opening quote already emitted).
pub(in crate::formatter) fn copy_quoted_ident(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, result: &mut String) {
    for ch in chars.by_ref() {
        result.push(ch);
        if ch == '"' {
            break;
        }
    }
}

/// Copy the rest of a `$$ … $$` body (opening `$$` already emitted).
pub(in crate::formatter) fn copy_dollar_body(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, result: &mut String) {
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
pub(in crate::formatter) fn read_word(first: char, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
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

/// Re-case `word` per `case` when it is a SQL keyword; otherwise return it as-is.
pub(in crate::formatter) fn recase_keyword(word: &str, keywords: &HashSet<&'static str>, case: &KeywordCase) -> String {
    if !keywords.contains(word.to_uppercase().as_str()) {
        return word.to_string();
    }
    match case {
        KeywordCase::Lower => word.to_lowercase(),
        KeywordCase::Upper => word.to_uppercase(),
        KeywordCase::Preserve => word.to_string(),
    }
}

pub(in crate::formatter) fn apply_keyword_case(sql: &str, case: &KeywordCase) -> String {
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
            result.push_str(&recase_keyword(&word, &keywords, case));
            continue;
        }

        result.push(c);
    }

    result
}
