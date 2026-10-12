// ── Reading SQL text with libpg_query ───────────────────
//
// Every rewrite the formatter makes — splitting a file into statements,
// recasing keywords, finding a table's inline comments, terminating a
// statement, checking that nothing changed — has to know which of the text is
// SQL and which is text: a string of any kind (`E''` with its backslash
// escapes, `U&''`, `B''`, `X''`, a `$$` or `$tag$` body), a quoted identifier,
// a comment. The hand-rolled scanners this replaced each knew part of that:
// `E'it\'s; tricky'` closed at `\'`, so its `;` ended the statement and the
// literal came out across three lines; a `$tag$` body was split at its own
// `;`s and its `END` recased. So all of them read the tokens PostgreSQL's own
// scanner produces, through libpg_query.

use pg_query::protobuf::KeywordKind;
use pg_query::protobuf::Token as Kind;

/// One token of a scanned text: its kind, its byte range, and the keyword
/// category the scanner gives its spelling.
pub(in crate::formatter) struct Lexeme {
    pub(in crate::formatter) kind: Kind,
    pub(in crate::formatter) start: usize,
    pub(in crate::formatter) end: usize,
    pub(in crate::formatter) keyword: KeywordKind,
}

impl Lexeme {
    pub(in crate::formatter) fn text<'a>(&self, sql: &'a str) -> &'a str {
        &sql[self.start..self.end]
    }

    pub(in crate::formatter) fn is_comment(&self) -> bool {
        matches!(self.kind, Kind::SqlComment | Kind::CComment)
    }

    /// A word of SQL — a keyword or an unquoted identifier spelled in ASCII:
    /// what a keyword case may recase. A string, a quoted identifier, a
    /// parameter, a number or an operator never is.
    pub(in crate::formatter) fn is_word(&self, sql: &str) -> bool {
        let text = self.text(sql);
        !matches!(
            self.kind,
            Kind::Sconst | Kind::Usconst | Kind::Bconst | Kind::Xconst | Kind::SqlComment | Kind::CComment
        ) && text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }

    /// Text rather than SQL: a string of any kind, a quoted identifier, a
    /// comment — what no rewrite may change.
    fn is_opaque(&self, sql: &str) -> bool {
        match self.kind {
            Kind::Sconst | Kind::Usconst | Kind::Bconst | Kind::Xconst | Kind::SqlComment | Kind::CComment => true,
            Kind::Ident | Kind::Uident => !self.is_word(sql),
            _ => false,
        }
    }
}

/// The tokens of `sql`, or `None` when PostgreSQL's scanner cannot read it —
/// an unterminated string or comment, which no rewrite can then tell apart
/// from SQL.
pub(in crate::formatter) fn lex(sql: &str) -> Option<Vec<Lexeme>> {
    let scanned = pg_query::scan(sql).ok()?;
    scanned
        .tokens
        .iter()
        .map(|t| {
            Some(Lexeme {
                kind: Kind::try_from(t.token).ok()?,
                start: usize::try_from(t.start).ok()?,
                end: usize::try_from(t.end).ok()?,
                keyword: KeywordKind::try_from(t.keyword_kind).ok()?,
            })
        })
        .collect()
}

/// Every opaque token of `sql` (see [`Lexeme::is_opaque`]) in order, or
/// `None` when it cannot be scanned. A line comment's trailing spaces are
/// layout, not text, so they are left off.
pub(in crate::formatter) fn opaque_text(sql: &str) -> Option<Vec<&str>> {
    Some(
        lex(sql)?
            .iter()
            .filter(|t| t.is_opaque(sql))
            .map(|t| match t.kind {
                Kind::SqlComment => t.text(sql).trim_end(),
                _ => t.text(sql),
            })
            .collect(),
    )
}

/// Whether `sql` holds a dollar-quoted body (`$$ … $$`, `$tag$ … $tag$`).
pub(in crate::formatter) fn has_dollar_body(sql: &str) -> bool {
    lex(sql).is_some_and(|tokens| {
        tokens
            .iter()
            .any(|t| t.kind == Kind::Sconst && t.text(sql).starts_with('$'))
    })
}

/// `s` ending its statement with a `;`: after its last token of SQL, so a
/// trailing comment keeps its text (`select 1 -- c` → `select 1; -- c`). Text
/// holding no SQL — a comment-only tail — has no statement to end, and text
/// that cannot be scanned is left as it is.
pub(in crate::formatter) fn ensure_semicolon(s: &str) -> String {
    let trimmed = s.trim_end();
    let Some(tokens) = lex(trimmed) else {
        return trimmed.to_string();
    };
    match tokens.iter().rev().find(|t| !t.is_comment()) {
        Some(last) if last.kind != Kind::Ascii59 => {
            format!("{};{}", &trimmed[..last.end], &trimmed[last.end..])
        }
        _ => trimmed.to_string(),
    }
}

/// Split a DDL script into statement blocks (each including its trailing `;`
/// and the comments on the line it ends; what lies after that line and
/// before the next statement goes with the next one).
///
/// A final statement with no `;` ends at its last token of SQL, so what
/// follows it is cut exactly as it would be after a `;`: the comments on its
/// line stay with it, the lines after it are a block of their own. Cut at the
/// end of the file instead, a `-- c` on the next line belonged to the
/// statement until `format` added the `;`, and to nothing after it — so the
/// second pass laid the statement out again and moved the comment.
///
/// The boundaries are libpg_query's: when the whole script parses, a `;` ends
/// a statement unless it lies inside one — a `BEGIN ATOMIC … END` body. When
/// it does not (another engine's DDL, or syntax newer than the grammar), a
/// `;` in its scanner's tokens ends one unless it is inside parentheses or
/// inside the `BEGIN … END` body of a `CREATE TRIGGER`, `FUNCTION` or
/// `PROCEDURE` (SQLite writes trigger bodies that way). Strings, bodies and
/// comments are single tokens either way, so a `;` in one never splits.
///
/// Text the scanner cannot read is one block: no boundary in it can be told
/// from text.
pub(in crate::formatter) fn split_statements(input: &str) -> Vec<String> {
    let Some(tokens) = lex(input) else {
        return vec![input.to_string()];
    };
    let mut ends = match pg_query::parse(input) {
        Ok(parsed) => parsed_statement_ends(input, &parsed, &tokens),
        Err(_) => scanned_statement_ends(&tokens),
    };
    if let Some(last) = tokens.iter().rev().find(|t| !t.is_comment())
        && ends.last().is_none_or(|&end| last.end > end)
    {
        ends.push(last.end);
    }
    let ends: Vec<usize> = ends
        .into_iter()
        .map(|end| with_line_comments(input, &tokens, end))
        .collect();
    let mut blocks = Vec::with_capacity(ends.len() + 1);
    let mut from = 0;
    for end in ends {
        blocks.push(input[from..end].to_string());
        from = end;
    }
    if !input[from..].trim().is_empty() {
        blocks.push(input[from..].to_string());
    }
    blocks
}

/// `end` moved past the comments on the line it is on — when nothing but
/// comments follows it there. A comment written beside a statement's last
/// line is that statement's; with SQL after it on the line, it is the next's.
fn with_line_comments(input: &str, tokens: &[Lexeme], end: usize) -> usize {
    let mut at = end;
    for t in tokens.iter().skip_while(|t| t.start < end) {
        if input[at..t.start].contains('\n') {
            break;
        }
        if !t.is_comment() {
            return end;
        }
        at = t.end;
    }
    at
}

/// A block (see [`split_statements`]) cut around its statement: the comments
/// before it, the statement from its first token of SQL to its last, and the
/// comments after it on its line, each as written. `None` for a block that
/// holds only comments, or that cannot be scanned.
pub(in crate::formatter) fn around_statement(block: &str) -> Option<(&str, &str, &str)> {
    let tokens = lex(block)?;
    let first = tokens.iter().find(|t| !t.is_comment())?;
    let last = tokens.iter().rev().find(|t| !t.is_comment())?;
    Some((&block[..first.start], &block[first.start..last.end], &block[last.end..]))
}

/// The offset just past each `;` that ends a statement, by the parse: every
/// `;` token outside every statement's own range.
fn parsed_statement_ends(input: &str, parsed: &pg_query::ParseResult, tokens: &[Lexeme]) -> Vec<usize> {
    let ranges: Vec<(usize, usize)> = parsed
        .protobuf
        .stmts
        .iter()
        .map(|raw| {
            let start = usize::try_from(raw.stmt_location).unwrap_or(0);
            // `stmt_len` 0 means "to the end of the script".
            let end = match usize::try_from(raw.stmt_len).unwrap_or(0) {
                0 => input.len(),
                len => start + len,
            };
            (start, end)
        })
        .collect();
    tokens
        .iter()
        .filter(|t| t.kind == Kind::Ascii59)
        .filter(|t| !ranges.iter().any(|&(start, end)| t.start >= start && t.start < end))
        .map(|t| t.end)
        .collect()
}

/// The offset just past each `;` that ends a statement, by the tokens alone
/// (see [`split_statements`]).
fn scanned_statement_ends(tokens: &[Lexeme]) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut parens = 0usize;
    let mut creating = false;
    let mut has_body = false;
    let mut in_body = false;
    let mut cases = 0usize;
    for t in tokens {
        match t.kind {
            Kind::Ascii40 => parens += 1,
            Kind::Ascii41 => parens = parens.saturating_sub(1),
            Kind::Create => creating = true,
            Kind::Trigger | Kind::Function | Kind::Procedure if creating => has_body = true,
            Kind::BeginP if has_body && !in_body => in_body = true,
            Kind::Case if in_body => cases += 1,
            // An `END` closes a `CASE` first, and the body only when none is open.
            Kind::EndP if in_body => {
                if cases > 0 {
                    cases -= 1;
                } else {
                    in_body = false;
                    has_body = false;
                }
            }
            Kind::Ascii59 if parens == 0 && !in_body => {
                ends.push(t.end);
                creating = false;
                has_body = false;
            }
            _ => {}
        }
    }
    ends
}
