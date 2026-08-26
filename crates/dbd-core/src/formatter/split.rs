// ── Statement splitter / tokenizer ──────────────────────

/// Return the index just past a single-quoted string literal starting at
/// `start` (handling `''` escapes; a lone `'` closes it).
pub(in crate::formatter) fn scan_single_quoted(bytes: &[u8], start: usize) -> usize {
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
pub(in crate::formatter) fn scan_double_quoted(bytes: &[u8], start: usize) -> usize {
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
pub(in crate::formatter) fn scan_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Return the index just past a `/* … */` block comment starting at `start`.
pub(in crate::formatter) fn scan_block_comment(bytes: &[u8], start: usize) -> usize {
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
pub(in crate::formatter) fn scan_dollar_quote(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i + 1 < bytes.len() && !(bytes[i] == b'$' && bytes[i + 1] == b'$') {
        i += 1;
    }
    i + 2
}

/// Return the index just past an identifier-like token starting at `start`.
pub(in crate::formatter) fn scan_identifier(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    i
}

/// True when the accumulated block (still holding its trailing `;`) ends with a
/// standalone `END` token — i.e. the `;` closes a trigger body rather than an
/// inner statement or a CASE expression.
pub(in crate::formatter) fn trigger_body_ends(current: &str) -> bool {
    let head = current[..current.len() - 1].trim_end();
    if head.len() < 3 || !head.as_bytes()[head.len() - 3..].eq_ignore_ascii_case(b"END") {
        return false;
    }
    head.len() == 3 || {
        let b = head.as_bytes()[head.len() - 4];
        !(b.is_ascii_alphanumeric() || b == b'_')
    }
}

/// Lexical token used when splitting a DDL script into statements.
pub(in crate::formatter) enum Token<'a> {
    /// A `$$` delimiter — toggles the dollar-quoted region.
    DollarToggle,
    /// A run copied verbatim (string literal, line/block comment).
    Text(&'a str),
    /// An identifier word (checked for `TRIGGER`/`BEGIN`).
    Word(&'a str),
    /// A single non-special character (a `;` here may be a boundary).
    Char(char),
}

/// Consume the next token starting at byte offset `i`. `in_dollar_quote` makes
/// everything except a closing `$$` opaque (a char at a time), matching
/// PostgreSQL dollar-quoting. Slices preserve multi-byte UTF-8.
pub(in crate::formatter) fn next_token<'a>(
    input: &'a str,
    bytes: &[u8],
    i: usize,
    in_dollar_quote: bool,
) -> (Token<'a>, usize) {
    let c = bytes[i] as char;
    // `$$` always toggles the dollar-quoted region (even inside one).
    if c == '$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
        return (Token::DollarToggle, i + 2);
    }
    // Inside a dollar-quoted body, everything else is verbatim.
    if in_dollar_quote {
        return (Token::Char(c), i + 1);
    }
    // Single-quoted string literal — a `;` inside is not a boundary.
    if c == '\'' {
        let end = scan_single_quoted(bytes, i);
        return (Token::Text(&input[i..end]), end);
    }
    // Line comment `-- …` to end of line.
    if c == '-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
        let end = scan_line_comment(bytes, i);
        return (Token::Text(&input[i..end]), end);
    }
    // Block comment `/* … */`.
    if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
        let end = scan_block_comment(bytes, i);
        return (Token::Text(&input[i..end]), end);
    }
    // Identifier-like token, consumed atomically so keywords are spotted at word
    // boundaries without false positives mid-name.
    if c.is_ascii_alphabetic() || c == '_' {
        let end = scan_identifier(bytes, i);
        return (Token::Word(&input[i..end]), end);
    }
    (Token::Char(c), i + 1)
}

/// Split a DDL script into statement blocks (each including its trailing `;`),
/// respecting dollar-quoted bodies, string/comment literals, and
/// `CREATE TRIGGER … BEGIN … END;` blocks (whose inner `;`s are not boundaries).
pub(in crate::formatter) fn split_statements(input: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut in_dollar_quote = false;
    let mut in_trigger_body = false;
    let mut seen_trigger = false;
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let (token, next) = next_token(input, bytes, i, in_dollar_quote);
        i = next;
        match token {
            Token::DollarToggle => {
                current.push_str("$$");
                in_dollar_quote = !in_dollar_quote;
            }
            Token::Text(s) => current.push_str(s),
            Token::Word(word) => {
                current.push_str(word);
                match word.to_ascii_uppercase().as_str() {
                    "TRIGGER" if !in_trigger_body => seen_trigger = true,
                    "BEGIN" if seen_trigger && !in_trigger_body => in_trigger_body = true,
                    _ => {}
                }
            }
            Token::Char(c) => {
                current.push(c);
                // A `;` inside a dollar-quoted body is never a boundary.
                if c != ';' || in_dollar_quote {
                    continue;
                }
                if in_trigger_body {
                    // A `;` only closes the trigger body when it follows a
                    // standalone `END`; inner separators keep accumulating.
                    if trigger_body_ends(&current) {
                        in_trigger_body = false;
                        seen_trigger = false;
                        blocks.push(std::mem::take(&mut current));
                    }
                } else {
                    // Normal statement boundary.
                    blocks.push(std::mem::take(&mut current));
                    seen_trigger = false;
                }
            }
        }
    }

    let remaining = current.trim();
    if !remaining.is_empty() {
        blocks.push(current);
    }

    blocks
}
