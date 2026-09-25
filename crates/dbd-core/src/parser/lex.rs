//! The SQL tokeniser — batches, comments, quoting, and nothing else.
//!
//! # Why a lexer rather than a grammar
//!
//! Because dbd measured the alternatives on a real T-SQL corpus and none of
//! them can read the statements it wants. Over 2,154 files, `sqlparser`'s
//! `MsSqlDialect` — after splitting `GO` batches, which is the most generous
//! way to ask — fails:
//!
//! ```text
//! CREATE PROCEDURE    880 of 885   (99% lost)
//! ALTER PROCEDURE     196 of 196  (100% lost)
//! CREATE TRIGGER       88 of  89   (99% lost)
//! CREATE TABLE        331 of 350   (95% lost)
//! ```
//!
//! It passes `SET`, `IF EXISTS` and `INSERT` — the batches that declare
//! nothing. An 81.9% batch-level pass rate hides a near-total loss of exactly
//! the facts a reader is reading for. Microsoft's own ScriptDom is complete and
//! is .NET, a runtime dependency dbd does not have.
//!
//! So this is a lexer and a statement-head reader, sized to what the corpus is:
//! a change script declares one object, names it on one line, and refers to
//! tables by name. The nested scopes, overloads and generics that make a real
//! parser necessary for a programming language are not present in SQL DDL.
//!
//! # `GO` is not SQL
//!
//! It is a client directive — sqlcmd and SSMS split a file on it and send each
//! batch separately — so no grammar accepts it, and a parser handed a whole
//! file chokes on the first one. Batches are separated here, before anything
//! reads them.
//!
//! Ported from sensei's `indexer::lang::sql::lex`.
//!
//! # Known limit: bare non-ASCII identifiers
//!
//! A word starts at an ASCII letter or `_`. A bare identifier beginning with a
//! non-ASCII character is not read as a name (its bytes become punctuation the
//! reader ignores). Quoted ones are unaffected — `[café]` reads correctly,
//! because [`quoted`] slices from the source — and SSMS brackets identifiers
//! as a matter of course, so this is a narrow gap rather than a common one.

/// One token. Deliberately small: a reader needs to know WHICH WORD is here
/// and where a statement ends, and nothing about expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok<'a> {
    /// A bare identifier or a keyword. Comparison is case-insensitive; the
    /// text is as the source wrote it.
    Word(&'a str),
    /// An identifier the source QUOTED — `[Order Details]`, `"Order"`. Kept
    /// apart from [`Tok::Word`] because a quoted identifier is never a keyword,
    /// however it is spelled.
    Quoted(String),
    /// `@p`, `@@ROWCOUNT`, `#temp`. Never an object name a reader records, so
    /// they are consumed whole rather than mistaken for one.
    Var(&'a str),
    /// A string or numeric literal. Its content is not a name, so it is not
    /// kept.
    Literal,
    Punct(char),
}

impl Tok<'_> {
    /// The identifier text, for the two token kinds that carry one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Tok::Word(w) => Some(w),
            Tok::Quoted(q) => Some(q.as_str()),
            _ => None,
        }
    }

    /// Whether this token is the given keyword, ignoring case.
    ///
    /// A QUOTED identifier never matches: `[TABLE]` is a table called `TABLE`,
    /// which is exactly why the source bracketed it.
    pub fn is(&self, keyword: &str) -> bool {
        matches!(self, Tok::Word(w) if w.eq_ignore_ascii_case(keyword))
    }
}

/// Split a file into the batches a client would send separately.
///
/// Each batch carries the LINE it starts on, so a reader can point at the file
/// rather than at an offset into a fragment it was handed.
pub fn batches(text: &str) -> Vec<(u32, &str)> {
    let mut out = Vec::new();
    let mut start_line = 1u32;
    let mut start = 0usize;
    let mut at = 0usize;
    for (line, raw) in (1u32..).zip(text.split_inclusive('\n')) {
        let trimmed = raw.trim();
        // `GO` may carry a repeat count (`GO 5`). What it may never do is
        // appear inside a statement, so a line that BEGINS with it and holds
        // nothing else of substance is the separator.
        //
        // BY CHARS, not by byte slice: `trimmed[..2]` panics the moment a line
        // starts with a multi-byte character, and an SSMS export is full of
        // them.
        let mut chars = trimmed.chars();
        let is_go = matches!(chars.next(), Some('g' | 'G'))
            && matches!(chars.next(), Some('o' | 'O'))
            && chars
                .as_str()
                .trim_start()
                .chars()
                .next()
                .is_none_or(|c| c.is_ascii_digit());
        if is_go {
            if !text[start..at].trim().is_empty() {
                out.push((start_line, &text[start..at]));
            }
            start = at + raw.len();
            start_line = line + 1;
        }
        at += raw.len();
    }
    if !text[start..].trim().is_empty() {
        out.push((start_line, &text[start..]));
    }
    out
}

/// Read a quoted identifier's body, doubling `closer` as the escape.
///
/// SLICED FROM THE SOURCE rather than pushed byte by byte: `b[i] as char` turns
/// every non-ASCII byte into a separate Latin-1 character, so a name with an
/// accent comes out mojibake — and an SSMS export is exactly where such names
/// live.
fn quoted(sql: &str, i: &mut usize, closer: u8) -> String {
    let b = sql.as_bytes();
    let mut name = String::new();
    let mut start = *i;
    while *i < b.len() {
        if b[*i] == closer {
            name.push_str(&sql[start..*i]);
            if b.get(*i + 1) == Some(&closer) {
                name.push(closer as char);
                *i += 2;
                start = *i;
                continue;
            }
            *i += 1;
            return name;
        }
        *i += 1;
    }
    // UNTERMINATED. The name is what was read, which is the honest answer for a
    // truncated file — and the loop has consumed to the end, so the caller
    // cannot spin.
    name.push_str(&sql[start..]);
    name
}

/// Tokenise one batch.
///
/// Comments and literals are CONSUMED rather than emitted: a table named in a
/// comment is not a reference, and a real corpus is full of commented-out SQL.
pub fn tokens(sql: &str) -> Vec<Tok<'_>> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        match c {
            _ if c.is_ascii_whitespace() => i += 1,
            // `-- line comment`
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            // `/* block */`, which T-SQL allows to NEST. A non-nesting reader
            // stops at the first `*/` and lexes the rest of the comment as code.
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1usize;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            // `'literal'`, with `''` as the escape. `N'…'` is the same thing
            // with a unicode prefix, and the prefix lexes as a word first.
            b'\'' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push(Tok::Literal);
            }
            // `[Order Details]`, with `]]` as the escape.
            b'[' => {
                i += 1;
                out.push(Tok::Quoted(quoted(sql, &mut i, b']')));
            }
            // `"Order"` under QUOTED_IDENTIFIER ON, which is the default.
            b'"' => {
                i += 1;
                out.push(Tok::Quoted(quoted(sql, &mut i, b'"')));
            }
            // `@p`, `@@ROWCOUNT`, `#temp`, `##global` — none of them a name
            // this reader records, but all of them consumed WHOLE so their tail
            // is not read as a bare identifier and minted as a phantom table.
            b'@' | b'#' => {
                let start = i;
                i += 1;
                while i < b.len() && (b[i] == b'@' || b[i] == b'#') {
                    i += 1;
                }
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Var(&sql[start..i]));
            }
            _ if c.is_ascii_digit() => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                out.push(Tok::Literal);
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                    i += 1;
                }
                out.push(Tok::Word(&sql[start..i]));
            }
            _ => {
                out.push(Tok::Punct(c as char));
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `GO` inside a string literal is still a separator, because batching
    /// happens BEFORE tokenising and cannot see literals. Pinned as a known
    /// limit rather than a bug: it is also how sqlcmd and SSMS behave, so a
    /// reader that disagreed would read the file differently from the tool that
    /// runs it.
    #[test]
    fn batching_happens_before_tokenising_and_that_is_deliberate() {
        let sql = "select 'first\nGO\nsecond'";
        assert_eq!(
            batches(sql).len(),
            2,
            "sqlcmd splits here too — matching it matters more than being clever"
        );
    }

    /// The `$` in an identifier is legal in T-SQL and common in generated
    /// names. It continues a word; it does not start one.
    #[test]
    fn a_dollar_continues_an_identifier() {
        assert_eq!(tokens("a$b")[0].name(), Some("a$b"));
    }

    /// `N'…'` is a unicode literal. The prefix lexes as a word, then the
    /// literal is consumed — so the `N` is the only thing a reader sees, and it
    /// is never a table.
    #[test]
    fn a_unicode_literal_prefix_does_not_leak_its_content() {
        let toks = tokens("where x = N'Users'");
        let names: Vec<&str> = toks.iter().filter_map(|t| t.name()).collect();
        assert_eq!(names, vec!["where", "x", "N"]);
    }
}
