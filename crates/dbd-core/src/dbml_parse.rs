//! DBML *parser* — the inverse of the DBML exporter in [`crate::dbml`].
//!
//! [`parse_dbml`] reads a `.dbml` document and produces a `Vec<Entity>` that the
//! reverse engine can feed straight into its emit → write-plan → snapshot path.
//! It reads what `dbd dbml` writes and the standard DBML a person writes for
//! the same things, the way @dbml/core (DBML's reference implementation,
//! checked against 10.3.0) reads them:
//!
//! - `Project`, `TableGroup`, `TablePartial`, `Records` and a sticky `Note` —
//!   skipped: metadata, or data.
//! - `Enum schema.name { value [note: '…'] … }` → [`EntityType::Enum`].
//! - `Table schema.name [as alias] [settings] { … }` → [`EntityType::Table`]:
//!   columns and their settings — an inline `ref:` among them — an `indexes`
//!   block, whose `pk` entry is the primary key, a `checks` block, and a
//!   `Note: '…'` or `Note { '…' }`.
//! - `Ref [name]: a > b [settings]` and `Ref [name] { a > b … }` → a
//!   [`ForeignKey`] on the table holding the key; for a one-to-one `-`, the
//!   right-hand side, where @dbml/core's SQL puts it. A table's alias names it.
//!
//! What dbd cannot build as written is refused, not guessed: a many-to-many
//! `<>` Ref, which has no foreign key (@dbml/core builds a junction table for
//! it), and a table injecting a `TablePartial`'s columns, which are not read.
//!
//! A note reads as @dbml/core reads one — blank leading lines dropped, the
//! indentation its lines share removed — with one difference: the line a
//! closing `'''` sits on is layout, not a trailing line break. dbd's own
//! earlier documents wrote every multi-line note that way, and a comment
//! ending in a line break nobody typed is not what anyone documented.
//!
//! The document is lexed by @dbml/parse's token rules and laid out again one
//! entry per line ([`logical_lines`]); the reader then works an entry at a
//! time. A [`EntityType::Schema`] entity is synthesised for every distinct
//! schema seen (including `public`) so the generated project is applyable.

use crate::entity::{
    ColumnDef, Entity, EntityType, FkAction, ForeignKey, IndexColumn, IndexDef, IndexType, TableConstraint, TableDef,
};
use crate::error::{DbdError, Result};

/// Parse a DBML document into entities (schemas, enums, tables, and the FK
/// constraints attached to the tables that hold them).
pub fn parse_dbml(text: &str) -> Result<Vec<Entity>> {
    let mut p = Parser::new(logical_lines(text)?);
    p.parse()
}

fn parse_err(message: impl Into<String>) -> DbdError {
    DbdError::Parse {
        file: std::path::PathBuf::from("<dbml>"),
        message: message.into(),
    }
}

// ── Lexing, by @dbml/parse's rules ────────────────────────────────────────────

/// One token's place in the document, and what [`logical_lines`] needs of it.
struct Lexed {
    start: usize,
    end: usize,
    kind: LexKind,
    /// The token's text when it is not its own source: a `'''` string, written
    /// again as the one-line `'…'` string with the same text.
    rendered: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum LexKind {
    OpenBrace,
    CloseBrace,
    OpenGroup,
    CloseGroup,
    Other,
}

/// The tokens of `text`, read as @dbml/parse's lexer reads them: `'…'` and
/// `"…"` on one line, a backslash escaping the character after it; `'''…'''`
/// across lines, escapes read; `` `…` `` raw, across lines; `//` to the end of
/// the line and `/* … */` comments, dropped. Everything else is a character
/// or a word, which is all the layout needs.
fn lex(text: &str) -> Result<Vec<Lexed>> {
    let bytes = text.as_bytes();
    let line_at = |at: usize| text[..at].matches('\n').count() + 1;
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let token = |kind, end| Lexed {
            start: i,
            end,
            kind,
            rendered: None,
        };
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |p| i + p);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let close = text[i + 2..]
                    .find("*/")
                    .ok_or_else(|| parse_err(format!("unterminated /* comment at line {}", line_at(i))))?;
                i += 2 + close + 2;
            }
            b'\'' if text[i..].starts_with("'''") => {
                let body = i + 3;
                let close = closing_triple_quote(&text[body..])
                    .ok_or_else(|| parse_err(format!("unterminated ''' string at line {}", line_at(i))))?;
                let value = unescape_dbml(&text[body..body + close]);
                out.push(Lexed {
                    start: i,
                    end: body + close + 3,
                    kind: LexKind::Other,
                    rendered: Some(one_line_string(without_closing_layout(&value))),
                });
                i = body + close + 3;
            }
            quote @ (b'\'' | b'"') => {
                let mut j = i + 1;
                loop {
                    match bytes.get(j) {
                        Some(b'\\') => j += 2,
                        Some(b'\n') | None => {
                            return Err(parse_err(format!(
                                "unterminated {} at line {} — a quoted string ends on the line it starts \
                                 (a note across lines is written '''…''')",
                                if quote == b'"' { "quoted name" } else { "string" },
                                line_at(i)
                            )));
                        }
                        Some(&b) if b == quote => break,
                        Some(_) => j += 1,
                    }
                }
                out.push(token(LexKind::Other, j + 1));
                i = j + 1;
            }
            b'`' => {
                let close = text[i + 1..]
                    .find('`')
                    .ok_or_else(|| parse_err(format!("unterminated `expression` at line {}", line_at(i))))?;
                out.push(token(LexKind::Other, i + 1 + close + 1));
                i += close + 2;
            }
            b'{' => {
                out.push(token(LexKind::OpenBrace, i + 1));
                i += 1;
            }
            b'}' => {
                out.push(token(LexKind::CloseBrace, i + 1));
                i += 1;
            }
            b'[' | b'(' => {
                out.push(token(LexKind::OpenGroup, i + 1));
                i += 1;
            }
            b']' | b')' => {
                out.push(token(LexKind::CloseGroup, i + 1));
                i += 1;
            }
            b if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 => {
                // A word: ASCII letters, digits and `_`, and any non-ASCII
                // character whole (its bytes are all ≥ 0x80).
                let len = bytes[i..]
                    .iter()
                    .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80)
                    .count();
                out.push(token(LexKind::Other, i + len));
                i += len;
            }
            _ => {
                out.push(token(LexKind::Other, i + 1));
                i += 1;
            }
        }
    }
    Ok(out)
}

/// A `'''` string's text without the line its closing quotes sit on, when
/// that line holds nothing but indentation.
fn without_closing_layout(value: &str) -> &str {
    match value.rsplit_once('\n') {
        Some((text, last)) if last.trim().is_empty() => text,
        _ => value,
    }
}

/// `text` as a one-line DBML string: `'…'`, with every backslash, quote and
/// line break escaped as DBML's lexer reads them back.
fn one_line_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// The document as logical lines, each with the line number it starts on:
/// every element header, column, index, check, note and closing brace on a
/// line of its own, whatever the layout, and every comment gone.
///
/// The reader took each physical line for one entry: a note spanning lines
/// inside a column's `[ … ]` became several columns, a `Note { … }` block
/// failed, and a table written on one line lost its columns. So a line now
/// ends at a line break outside `[ ]` and `( )`, after a `{` — which joins the
/// line before it, however far down it was written — and on either side of a
/// `}`. Within a line, tokens keep the spacing between them unless a comment or
/// a line break stood there, which becomes one space.
fn logical_lines(text: &str) -> Result<Vec<(String, usize)>> {
    let line_at = |at: usize| text[..at].matches('\n').count() + 1;
    let mut lines: Vec<(String, usize)> = Vec::new();
    let mut current = String::new();
    let mut current_line = 0;
    let mut depth = 0usize;
    let mut previous_end: Option<usize> = None;
    let flush = |current: &mut String, current_line: usize, lines: &mut Vec<(String, usize)>| {
        if !current.trim().is_empty() {
            lines.push((std::mem::take(current), current_line));
        }
        current.clear();
    };
    for token in lex(text)? {
        if let Some(previous) = previous_end {
            let gap = &text[previous..token.start];
            let breaks = gap.contains('\n');
            // A `}` starts a line; so does a line break outside `[ ]` and
            // `( )`, unless what follows it is the `{` of the line before.
            let new_line =
                depth == 0 && (token.kind == LexKind::CloseBrace || (breaks && token.kind != LexKind::OpenBrace));
            if new_line {
                flush(&mut current, current_line, &mut lines);
            } else if !current.is_empty() {
                if !gap.is_empty() && gap.bytes().all(|b| b == b' ' || b == b'\t') {
                    current.push_str(gap);
                } else if !gap.is_empty() {
                    current.push(' ');
                }
            }
        }
        if current.is_empty() {
            current_line = line_at(token.start);
        }
        current.push_str(token.rendered.as_deref().unwrap_or(&text[token.start..token.end]));
        match token.kind {
            LexKind::OpenGroup => depth += 1,
            LexKind::CloseGroup => depth = depth.saturating_sub(1),
            LexKind::OpenBrace | LexKind::CloseBrace if depth == 0 => {
                flush(&mut current, current_line, &mut lines);
            }
            _ => {}
        }
        previous_end = Some(token.end);
    }
    flush(&mut current, current_line, &mut lines);
    Ok(lines)
}

/// A note's text as @dbml/core reads one: the blank lines before its first
/// line of text dropped, and the indentation its non-blank lines share
/// removed from every line.
fn note_text(value: &str) -> String {
    let lines: Vec<&str> = value.split('\n').collect();
    let Some(first) = lines.iter().position(|l| !l.trim_start().is_empty()) else {
        // All blank: @dbml/core keeps the last line as it is.
        return lines.last().map_or_else(String::new, |l| (*l).to_string());
    };
    let lines = &lines[first..];
    let indent = lines
        .iter()
        .filter(|l| !l.trim_start().is_empty())
        .map(|l| l.chars().count() - l.trim_start().chars().count())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| l.chars().skip(indent).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

// ── The reader ───────────────────────────────────────────────────────────────

/// One end of a Ref: the table as written — `schema` only when the document
/// gives one, so a bare name can still be an alias — and its columns.
#[derive(Clone)]
struct Endpoint {
    schema: Option<String>,
    table: String,
    columns: Vec<String>,
}

/// A relationship's operator.
#[derive(Clone, Copy, PartialEq)]
enum RefOp {
    /// `>` — many-to-one: the left side holds the key.
    ManyToOne,
    /// `<` — one-to-many: the right side holds it.
    OneToMany,
    /// `-` — one-to-one: the right side holds it, as @dbml/core's SQL puts it.
    OneToOne,
    /// `<>` — many-to-many: no foreign key at all.
    ManyToMany,
}

/// A foreign key collected during the scan and attached to the table that
/// holds it once every table — and alias — is known.
struct PendingRef {
    name: Option<String>,
    /// The side holding the key.
    holder: Endpoint,
    /// The side it references.
    target: Endpoint,
    on_delete: Option<FkAction>,
    on_update: Option<FkAction>,
}

impl PendingRef {
    /// The foreign key `left <op> right` describes, or why there is none.
    fn new(
        name: Option<String>,
        left: Endpoint,
        op: RefOp,
        right: Endpoint,
        on_delete: Option<FkAction>,
        on_update: Option<FkAction>,
    ) -> Result<Self> {
        let (holder, target) = match op {
            RefOp::ManyToOne => (left, right),
            RefOp::OneToMany | RefOp::OneToOne => (right, left),
            RefOp::ManyToMany => {
                return Err(parse_err(format!(
                    "a many-to-many Ref (`<>`) between `{}` and `{}` has no foreign key to build — \
                     @dbml/core builds a junction table for it; declare the junction table and its two Refs instead",
                    left.table, right.table
                )));
            }
        };
        Ok(Self {
            name,
            holder,
            target,
            on_delete,
            on_update,
        })
    }
}

struct Parser {
    /// Logical lines (see [`logical_lines`]) and the line each starts on.
    lines: Vec<(String, usize)>,
    pos: usize,
    tables: Vec<Entity>,
    enums: Vec<Entity>,
    /// The stubs `dbd dbml` wrote, read back as what they stand for.
    externals: Vec<Entity>,
    refs: Vec<PendingRef>,
    /// Distinct schemas seen, in first-seen order.
    schemas: Vec<String>,
    /// Each `Table … as alias`: the alias, and the `(schema, table)` it names.
    aliases: std::collections::HashMap<String, (String, String)>,
}

impl Parser {
    fn new(lines: Vec<(String, usize)>) -> Self {
        Self {
            lines,
            pos: 0,
            tables: Vec::new(),
            enums: Vec::new(),
            externals: Vec::new(),
            refs: Vec::new(),
            schemas: Vec::new(),
            aliases: std::collections::HashMap::new(),
        }
    }

    fn line(&self) -> &str {
        self.lines[self.pos].0.trim()
    }

    fn line_number(&self) -> usize {
        self.lines.get(self.pos).map_or(0, |(_, n)| *n)
    }

    fn note_schema(&mut self, schema: &str) {
        if !self.schemas.iter().any(|s| s == schema) {
            self.schemas.push(schema.to_string());
        }
    }

    fn parse(&mut self) -> Result<Vec<Entity>> {
        while self.pos < self.lines.len() {
            let line = self.line().to_string();
            // Keyword dispatch on the first token (case-insensitive for the
            // block keywords dbd/dbdiagram.io use). Strip a trailing colon so
            // `Ref:` matches `ref`.
            match keyword(&line).as_str() {
                "enum" => self.parse_enum()?,
                "table" => self.parse_table()?,
                "ref" => self.parse_ref(&line)?,
                // `Project`, `TableGroup`, `TablePartial`, `Records`, a sticky
                // or stray `Note`, and anything newer: documentation, data or
                // metadata, none of it built. Lenient by design.
                _ => self.skip_element()?,
            }
        }

        // Resolve every Ref onto the table that holds its key.
        self.attach_refs()?;

        // Assemble: schemas (synthesised) → enums → tables.
        let mut out = Vec::new();
        for schema in &self.schemas {
            out.push(Entity::new(EntityType::Schema, schema));
        }
        out.append(&mut self.enums);
        out.append(&mut self.tables);
        out.append(&mut self.externals);
        Ok(out)
    }

    // ── element skipping ──────────────────────────────────────────────────

    /// Skip the element starting at the current line: the line alone, or —
    /// when it opens a block — through the `}` that closes it. A `{` only ever
    /// ends a logical line and a `}` is one alone, so the braces are counted
    /// exactly; one inside a string is part of the string's token.
    fn skip_element(&mut self) -> Result<()> {
        let start = self.line_number();
        if !self.line().ends_with('{') {
            self.pos += 1;
            return Ok(());
        }
        let mut depth = 0usize;
        while self.pos < self.lines.len() {
            let line = self.line();
            if line.ends_with('{') {
                depth += 1;
            } else if line == "}" {
                depth -= 1;
            }
            self.pos += 1;
            if depth == 0 {
                return Ok(());
            }
        }
        Err(parse_err(format!("unterminated block starting at line {start}")))
    }

    // ── enum ───────────────────────────────────────────────────────────────

    fn parse_enum(&mut self) -> Result<()> {
        let header = self.line().to_string();
        let head = parse_element_header(&header)?;
        let (schema, base) = head.name;
        self.note_schema(&schema);
        let qualified = format!("{schema}.{base}");
        if !head.opens_block {
            return Err(parse_err(format!("enum `{qualified}` is missing `{{`")));
        }
        let mut entity = Entity::new(EntityType::Enum, &qualified);
        entity.schema = Some(schema);

        // Value lines until the closing `}`.
        self.pos += 1;
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err(format!("unterminated enum `{qualified}`")));
            }
            let line = self.line().to_string();
            self.pos += 1;
            if line == "}" {
                break;
            }
            // `"value" [note: '…']` or bare `value`.
            let (token, rest) = take_identifier(&line)?;
            let note = parse_value_note(rest.trim());
            entity.enum_values.push(crate::entity::EnumValue { name: token, note });
        }
        self.enums.push(entity);
        Ok(())
    }

    // ── table ────────────────────────────────────────────────────────────────

    fn parse_table(&mut self) -> Result<()> {
        let header = self.line().to_string();
        let head = parse_element_header(&header)?;
        let (schema, base) = head.name;
        let qualified = format!("{schema}.{base}");
        if !head.opens_block {
            return Err(parse_err(format!("table `{qualified}` is missing `{{`")));
        }
        if let Some(alias) = head.alias {
            self.aliases.insert(alias, (schema.clone(), base.clone()));
        }

        let mut columns: Vec<ColumnDef> = Vec::new();
        let mut pk_cols: Vec<String> = Vec::new();
        let mut pk_index: Option<(Option<String>, Vec<String>)> = None;
        let mut indexes: Vec<IndexDef> = Vec::new();
        let mut uniques: Vec<TableConstraint> = Vec::new();
        let mut checks: Vec<TableConstraint> = Vec::new();
        // A note in the header's settings, unless the body has its own.
        let mut table_note: Option<String> = head.settings.as_deref().and_then(note_setting);

        self.pos += 1;
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err(format!("unterminated table `{qualified}`")));
            }
            let line = self.line().to_string();
            if line == "}" {
                self.pos += 1;
                break;
            }

            match keyword(&line).as_str() {
                // Only as block openers: an unquoted column may well be named
                // `indexes` or `checks`, and `checks int` is a column.
                "indexes" if opens_block(&line) => {
                    for entry in self.block_entries("indexes", &qualified)? {
                        match parse_index_line(&entry, &qualified)? {
                            IndexEntry::Index(index) => indexes.push(index),
                            IndexEntry::Unique(unique) => uniques.push(unique),
                            IndexEntry::PrimaryKey(name, keys) => {
                                if pk_index.is_some() {
                                    return Err(parse_err(format!("table `{qualified}` has two `pk` indexes")));
                                }
                                pk_index = Some((name, keys));
                            }
                        }
                    }
                }
                "checks" if opens_block(&line) => {
                    for entry in self.block_entries("checks", &qualified)? {
                        checks.push(parse_check_line(&entry, &qualified)?);
                    }
                }
                // `Note: '…'` or `Note { '…' }` — not a column named `note`.
                "note" if is_note(&line) => {
                    table_note = Some(self.consume_note(&line, &qualified)?);
                }
                "records" if line.ends_with('{') => self.skip_element()?,
                _ if line.starts_with('~') => {
                    return Err(parse_err(format!(
                        "table `{qualified}` takes columns from a TablePartial (`{line}`), which dbd does not read — \
                         write the columns into the table"
                    )));
                }
                _ => {
                    // A column definition, with any `check:` and `ref:`
                    // settings it carries.
                    let column = parse_column(&line, &qualified)?;
                    let col = column.def;
                    if col.is_pk {
                        pk_cols.push(col.name.clone());
                    }
                    checks.extend(
                        column
                            .checks
                            .into_iter()
                            .map(|expression| TableConstraint::Check { name: None, expression }),
                    );
                    let here = Endpoint {
                        schema: Some(schema.clone()),
                        table: base.clone(),
                        columns: vec![col.name.clone()],
                    };
                    // An inline ref is written from the column's side: `[ref:
                    // > t.c]` is `col > t.c`, but `[ref: - t.c]` keys the
                    // column, as @dbml/core reads it (`t.c - col`).
                    for (op, other) in column.refs {
                        let pending = match op {
                            RefOp::OneToOne => PendingRef::new(None, other, op, here.clone(), None, None)?,
                            _ => PendingRef::new(None, here.clone(), op, other, None, None)?,
                        };
                        self.refs.push(pending);
                    }
                    columns.push(col);
                    self.pos += 1;
                }
            }
        }

        let mut constraints = Vec::new();
        // The primary key: the columns marked `pk` (two or more are a composite
        // key, as in @dbml/core), or the `pk` index — which is how DBML names
        // one. Both may say the same thing; two different keys are refused.
        match pk_index {
            Some((name, keys)) => {
                if !pk_cols.is_empty() && pk_cols != keys {
                    return Err(parse_err(format!(
                        "table `{qualified}` declares two primary keys: ({}) by its columns and ({}) by its indexes",
                        pk_cols.join(", "),
                        keys.join(", ")
                    )));
                }
                for col in columns.iter_mut().filter(|c| keys.contains(&c.name)) {
                    col.is_pk = true;
                    col.nullable = false;
                }
                constraints.push(TableConstraint::PrimaryKey { name, columns: keys });
            }
            None if !pk_cols.is_empty() => constraints.push(TableConstraint::PrimaryKey {
                name: None,
                columns: pk_cols,
            }),
            None => {}
        }
        constraints.append(&mut uniques);
        constraints.append(&mut checks);

        // A stub `dbd dbml` wrote stands in for a table this document does not
        // define, and is declared, not built — its schema with it.
        if table_note.as_deref().is_some_and(crate::dbml::is_stub_note) {
            let mut external = Entity::new(EntityType::External, &qualified);
            external.schema = Some(schema);
            self.externals.push(external);
            return Ok(());
        }
        self.note_schema(&schema);

        let comments = Self::table_comments_from(table_note, &columns);

        let mut entity = Entity::new(EntityType::Table, &qualified);
        entity.schema = Some(schema);
        entity.table_def = Some(TableDef {
            columns,
            constraints,
            indexes,
            comments,
        });
        self.tables.push(entity);
        Ok(())
    }

    /// Assemble a table's `TableComments` from its `note` plus per-column notes
    /// (mirrors what introspection surfaces).
    fn table_comments_from(table_note: Option<String>, columns: &[ColumnDef]) -> crate::entity::TableComments {
        let mut comments = crate::entity::TableComments {
            table: table_note,
            ..Default::default()
        };
        for col in columns {
            if let Some(ref c) = col.comment {
                comments.columns.insert(col.name.clone(), c.clone());
            }
        }
        comments
    }

    /// The entry lines of a `<kind> { … }` block inside a table — `indexes` or
    /// `checks` — whose header is the current line.
    fn block_entries(&mut self, kind: &str, table: &str) -> Result<Vec<String>> {
        self.pos += 1;
        let mut entries = Vec::new();
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err(format!("unterminated {kind} block in `{table}`")));
            }
            let line = self.line().to_string();
            self.pos += 1;
            if line == "}" {
                break;
            }
            entries.push(line);
        }
        Ok(entries)
    }

    /// A table's note, from the current line: `Note: '…'`, or a `Note {`
    /// whose string is the next line and whose `}` follows it.
    fn consume_note(&mut self, line: &str, table: &str) -> Result<String> {
        let malformed = || parse_err(format!("malformed Note in `{table}`: {line}"));
        if line.ends_with('{') {
            self.pos += 1;
            let value = self
                .lines
                .get(self.pos)
                .and_then(|(l, _)| parse_single_line_string(l))
                .ok_or_else(malformed)?;
            self.pos += 1;
            if self.lines.get(self.pos).map(|(l, _)| l.trim()) != Some("}") {
                return Err(malformed());
            }
            self.pos += 1;
            return Ok(note_text(&value));
        }
        self.pos += 1;
        let after = line.split_once(':').map(|(_, rest)| rest).ok_or_else(malformed)?;
        parse_single_line_string(after.trim())
            .map(|v| note_text(&v))
            .ok_or_else(malformed)
    }

    // ── Ref ──────────────────────────────────────────────────────────────────

    /// `Ref[ name]: <a> <op> <b> [settings]`, or the long form `Ref[ name] {`
    /// with one relationship per line until `}`. The name is the foreign
    /// key's.
    fn parse_ref(&mut self, line: &str) -> Result<()> {
        let rest = line.get(3..).unwrap_or("").trim_start();
        let (name, rest) = if rest.starts_with(':') || rest.starts_with('{') {
            (None, rest)
        } else {
            let (name, rest) = take_name_part(rest)?;
            (Some(name), rest.trim_start())
        };
        if rest.starts_with('{') {
            let start = self.line_number();
            self.pos += 1;
            loop {
                let Some((body, _)) = self.lines.get(self.pos) else {
                    return Err(parse_err(format!("unterminated Ref block starting at line {start}")));
                };
                let body = body.trim().to_string();
                self.pos += 1;
                if body == "}" {
                    return Ok(());
                }
                self.refs.push(parse_ref_body(&body, name.clone())?);
            }
        }
        let body = rest
            .strip_prefix(':')
            .ok_or_else(|| parse_err(format!("malformed Ref (no `:`): {line}")))?;
        self.refs.push(parse_ref_body(body.trim(), name)?);
        self.pos += 1;
        Ok(())
    }

    /// The `(schema, table)` an endpoint names: as written when it states a
    /// schema, the table an alias stands for, else `public`.
    fn resolve(&self, end: &Endpoint) -> (String, String) {
        match &end.schema {
            Some(schema) => (schema.clone(), end.table.clone()),
            None => self
                .aliases
                .get(&end.table)
                .cloned()
                .unwrap_or_else(|| ("public".to_string(), end.table.clone())),
        }
    }

    /// Attach each collected Ref to the table holding its key as a
    /// `ForeignKey` constraint. A Ref whose holder is absent is skipped
    /// (lenient).
    fn attach_refs(&mut self) -> Result<()> {
        let refs = std::mem::take(&mut self.refs);
        for r in refs {
            let (holder_schema, holder_table) = self.resolve(&r.holder);
            let (target_schema, target_table) = self.resolve(&r.target);
            let fk = ForeignKey {
                name: r.name,
                columns: r.holder.columns,
                ref_schema: Some(target_schema),
                ref_table: target_table,
                ref_columns: r.target.columns,
                on_delete: r.on_delete,
                on_update: r.on_update,
                // DBML has no words for these; a `Ref` is a plain key.
                match_full: false,
                deferrable: false,
                initially_deferred: false,
                // A DBML Ref's tables resolve to a schema, so nothing here
                // was inferred from a search path.
                ref_schema_source: crate::entity::SchemaSource::Stated,
            };
            let holder = format!("{holder_schema}.{holder_table}");
            if let Some(entity) = self.tables.iter_mut().find(|e| e.name == holder)
                && let Some(td) = entity.table_def.as_mut()
            {
                td.constraints.push(TableConstraint::ForeignKey(fk));
            }
            // else: the holder is not in the document — skip leniently.
        }
        Ok(())
    }
}

// ── free functions ──────────────────────────────────────────────────────────

/// The first whitespace-delimited word of a line.
fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// The lowercased leading keyword of a line, with a trailing `:` stripped so
/// `Ref:` → `ref` and `Note:` → `note`.
fn keyword(line: &str) -> String {
    first_word(line).trim_end_matches(':').to_ascii_lowercase()
}

/// Whether a line beginning with `note` is a note — `Note: '…'`, `Note {` —
/// rather than a column named `note`.
fn is_note(line: &str) -> bool {
    let rest = line.get(4..).unwrap_or("").trim_start();
    rest.starts_with(':') || rest.starts_with('{')
}

/// A `Table` or `Enum` header: `<keyword> <name> [as <alias>] [settings] {`.
struct ElementHeader {
    name: (String, String),
    alias: Option<String>,
    settings: Option<String>,
    opens_block: bool,
}

fn parse_element_header(header: &str) -> Result<ElementHeader> {
    let rest = header.get(first_word(header).len()..).unwrap_or("").trim();
    let (opens_block, rest) = match rest.strip_suffix('{') {
        Some(before) => (true, before.trim_end()),
        None => (false, rest),
    };
    let (parts, mut rest) = take_dotted_name(rest)?;
    let name = match parts.as_slice() {
        [base] => ("public".to_string(), base.clone()),
        [schema, base] => (schema.clone(), base.clone()),
        _ => return Err(parse_err(format!("unexpected qualified name in: {header}"))),
    };
    rest = rest.trim_start();
    let mut alias = None;
    if let Some(after) = rest.strip_prefix("as").or_else(|| rest.strip_prefix("AS"))
        && after.starts_with(char::is_whitespace)
    {
        let (a, after) = take_name_part(after.trim_start())?;
        alias = Some(a);
        rest = after.trim_start();
    }
    let settings = split_trailing_settings(rest).1.map(str::to_string);
    Ok(ElementHeader {
        name,
        alias,
        settings,
        opens_block,
    })
}

/// Take a dotted name — `a.b`, `"a"."b c"` — off the front of `s`.
fn take_dotted_name(s: &str) -> Result<(Vec<String>, &str)> {
    let mut parts = Vec::new();
    let mut rest = s;
    loop {
        let (part, after) = take_name_part(rest)?;
        parts.push(part);
        match after.strip_prefix('.') {
            Some(next) => rest = next,
            None => return Ok((parts, after)),
        }
    }
}

/// Take one name off the front of `s`: `"quoted"` (read with DBML's escapes),
/// or bare — up to whitespace or one of `.[{(:`.
fn take_name_part(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    if s.starts_with('"') {
        let bytes = s.as_bytes();
        let mut j = 1;
        while j < bytes.len() && bytes[j] != b'"' {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        if j >= bytes.len() {
            return Err(parse_err(format!("unterminated quoted name: {s}")));
        }
        return Ok((unescape_dbml(&s[1..j]), &s[j + 1..]));
    }
    let end = s
        .find(|c: char| c.is_whitespace() || ".[{(:".contains(c))
        .unwrap_or(s.len());
    if end == 0 {
        return Err(parse_err(format!("expected a name: {s}")));
    }
    Ok((s[..end].to_string(), &s[end..]))
}

/// Split on `.` separators that are outside of double quotes.
fn split_dotted(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut escaped = false;
    for c in input.chars() {
        match c {
            // An escaped character inside a quoted name is part of it — an
            // escaped quote does not end the name.
            _ if escaped => {
                escaped = false;
                cur.push(c);
            }
            '\\' if in_quote => {
                escaped = true;
                cur.push(c);
            }
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '.' if !in_quote => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() || !parts.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Strip a single pair of surrounding double quotes, if present.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        // A quoted name takes backslash escapes, as a string does in DBML:
        // `"we\"ird"` is `we"ird` to @dbml/core.
        unescape_dbml(&s[1..s.len() - 1])
    } else {
        s.to_string()
    }
}

/// Take a leading identifier (quoted `"x"` or bare) off the front of `s`,
/// returning `(identifier, remainder)`.
fn take_identifier(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        // The name ends at the first quote no backslash escapes.
        let mut escaped = false;
        for (at, c) in rest.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => return Ok((unescape_dbml(&rest[..at]), &rest[at + 1..])),
                _ => {}
            }
        }
        Err(parse_err(format!("unterminated quoted identifier: {s}")))
    } else {
        // Bare identifier: up to whitespace or `[`.
        let end = s.find(|c: char| c.is_whitespace() || c == '[').unwrap_or(s.len());
        if end == 0 {
            return Err(parse_err(format!("expected identifier: {s}")));
        }
        Ok((s[..end].to_string(), &s[end..]))
    }
}

/// The `note:` in a settings group's inner text, read as a note.
fn note_setting(settings: &str) -> Option<String> {
    extract_settings(settings)?
        .into_iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("note"))
        .and_then(|(_, value)| value.as_deref().and_then(parse_single_line_string))
        .map(|v| note_text(&v))
}

/// Parse an optional `[note: '…']` setting following an enum value, returning
/// the note text if present.
fn parse_value_note(rest: &str) -> Option<String> {
    let (_, inner) = split_trailing_settings(rest.trim());
    note_setting(inner?)
}

/// A column line read: the column, the expressions of its `check:` settings,
/// and its inline `ref:`s.
struct ParsedColumn {
    def: ColumnDef,
    checks: Vec<String>,
    refs: Vec<(RefOp, Endpoint)>,
}

/// Parse a column definition line: `"name" <type> [settings]`.
fn parse_column(line: &str, table: &str) -> Result<ParsedColumn> {
    let (name, rest) = take_identifier(line).map_err(|e| parse_err(format!("column in `{table}`: {e}")))?;
    let rest = rest.trim_start();

    // Split type from settings: the settings group is the trailing `[ … ]`.
    let (type_part, settings_part) = split_trailing_settings(rest);
    let data_type = parse_type(type_part.trim());

    let mut col = ColumnDef {
        name,
        data_type,
        nullable: true,
        default_value: None,
        is_pk: false,
        is_unique: false,
        identity: None,
        generated: None,
        comment: None,
        inline_fk: None,
        // DBML has no collation.
        collation: None,
    };
    let mut checks = Vec::new();
    let mut refs = Vec::new();

    if let Some(settings_str) = settings_part {
        let settings = extract_settings(settings_str)
            .ok_or_else(|| parse_err(format!("malformed column settings in `{table}`: {settings_str}")))?;
        for (key, value) in settings {
            let k = key.to_ascii_lowercase();
            match k.as_str() {
                "pk" | "primary key" => col.is_pk = true,
                "unique" => col.is_unique = true,
                "not null" => col.nullable = false,
                "null" => col.nullable = true,
                // DBML's auto-increment. A serial type already is one; any
                // other type is an identity column — which is what dbd's
                // exporter writes `increment` for. DBML cannot say ALWAYS or
                // BY DEFAULT, so it reads as BY DEFAULT: generated unless a
                // value is supplied, as serial behaves.
                // Settings apply in any order, so an explicit kind from the
                // `generated as identity` property is never overwritten here.
                "increment" if !crate::emit::is_serial_type(&col.data_type) => {
                    col.identity.get_or_insert(crate::entity::IdentityKind::ByDefault);
                    col.nullable = false;
                }
                "increment" => {}
                // dbd's custom property naming the identity kind `increment`
                // cannot. An unknown kind is refused: guessing would build a
                // column that accepts, or refuses, what the design did not say.
                key if key == crate::dbml::IDENTITY_PROPERTY && !crate::emit::is_serial_type(&col.data_type) => {
                    let kind = value.as_deref().and_then(parse_single_line_string).unwrap_or_default();
                    col.identity = Some(match kind.trim().to_ascii_lowercase().as_str() {
                        "always" => crate::entity::IdentityKind::Always,
                        "by default" => crate::entity::IdentityKind::ByDefault,
                        _ => {
                            return Err(parse_err(format!(
                                "column `{}` in `{table}`: unknown identity kind `{kind}` (expected 'always' or 'by default')",
                                col.name
                            )));
                        }
                    });
                    col.nullable = false;
                }
                "default" => {
                    if let Some(v) = value {
                        col.default_value = Some(parse_default_value(&v));
                    }
                }
                "note" => {
                    if let Some(v) = value
                        && let Some(s) = parse_single_line_string(&v)
                    {
                        col.comment = Some(note_text(&s));
                    }
                }
                // dbd's custom property for a generated column's expression.
                key if key == crate::dbml::GENERATED_PROPERTY => {
                    col.generated = value.as_deref().and_then(parse_single_line_string);
                }
                // DBML's column-level CHECK; a column may carry several.
                "check" => {
                    let expression = value.as_deref().and_then(backticked).ok_or_else(|| {
                        parse_err(format!(
                            "check on `{table}` is not a backticked expression: {settings_str}"
                        ))
                    })?;
                    checks.push(expression);
                }
                // An inline relationship: `ref: > table.column`.
                "ref" => {
                    let value = value.unwrap_or_default();
                    let (op, other) = parse_inline_ref(&value)
                        .ok_or_else(|| parse_err(format!("malformed ref on `{}` in `{table}`: {value}", col.name)))?;
                    refs.push((op, parse_ref_endpoint(other)?));
                }
                _ => { /* unknown setting — ignore (forward-compatible) */ }
            }
        }
    }

    Ok(ParsedColumn { def: col, checks, refs })
}

/// An inline `ref:` value — an operator, then the other end: `> table.column`.
fn parse_inline_ref(value: &str) -> Option<(RefOp, &str)> {
    let v = value.trim();
    let (op, rest) = if let Some(rest) = v.strip_prefix("<>") {
        (RefOp::ManyToMany, rest)
    } else if let Some(rest) = v.strip_prefix('<') {
        (RefOp::OneToMany, rest)
    } else if let Some(rest) = v.strip_prefix('>') {
        (RefOp::ManyToOne, rest)
    } else {
        (RefOp::OneToOne, v.strip_prefix('-')?)
    };
    Some((op, rest.trim()))
}

/// Whether a table-body line opens a block: the keyword alone, or followed by
/// its `{` — not a column that happens to share the keyword's name.
fn opens_block(line: &str) -> bool {
    let rest = line.get(first_word(line).len()..).unwrap_or("").trim();
    rest.is_empty() || rest.starts_with('{')
}

/// The expression inside a DBML backtick expression, or `None` if `s` is not
/// one.
fn backticked(s: &str) -> Option<String> {
    s.trim()
        .strip_prefix('`')?
        .strip_suffix('`')
        .map(|expression| expression.trim().to_string())
}

/// One entry of a `checks { … }` block: `` `<expression>` [name: '…'] ``.
fn parse_check_line(line: &str, table: &str) -> Result<TableConstraint> {
    let (expression_part, settings_part) = split_trailing_settings(line.trim());
    let expression = backticked(expression_part)
        .ok_or_else(|| parse_err(format!("check in `{table}` is not a backticked expression: {line}")))?;
    let mut name = None;
    if let Some(settings_str) = settings_part {
        let settings = extract_settings(settings_str)
            .ok_or_else(|| parse_err(format!("malformed check settings in `{table}`: {settings_str}")))?;
        for (key, value) in settings {
            if key.eq_ignore_ascii_case("name") {
                name = value.as_deref().and_then(parse_single_line_string);
            }
        }
    }
    Ok(TableConstraint::Check { name, expression })
}

/// Parse a column type token. Strips surrounding quotes (types with spaces are
/// quoted on export); preserves a trailing `[]` array suffix and parameters.
fn parse_type(input: &str) -> String {
    let input = input.trim();
    if let Some(after_quote) = input.strip_prefix('"') {
        // Quoted type: take to the closing quote, then re-append any suffix
        // (e.g. `"timestamp with time zone"[]`).
        if let Some(end_rel) = after_quote.find('"') {
            let inner = &after_quote[..end_rel];
            let suffix = &after_quote[end_rel + 1..];
            return format!("{inner}{}", suffix.trim());
        }
    }
    input.to_string()
}

/// Parse a `default: <v>` DBML value into a SQL-ready `default_value` string.
///
/// The mapping exactly inverts [`crate::dbml::quote_default`] (the DBML exporter):
///
/// | DBML form        | SQL form stored            | Notes                                  |
/// |------------------|----------------------------|-----------------------------------------|
/// | `'str'`          | `'str'`                    | DBML `\'` → SQL `'` then SQL-doubled   |
/// | `` `expr` ``     | `expr`                     | raw/expression, no quoting             |
/// | bare token       | token as-is                | numbers, `true`, `false`, `null`       |
///
/// Concretely: `'claude'` → `'claude'`; `'{}'` → `'{}'`; `''` → `''`;
/// `'a\'b'` → `'a''b'`; `` `now()` `` → `now()`; `0` → `0`.
fn parse_default_value(v: &str) -> String {
    let v = v.trim();
    // Backtick-quoted expression (raw SQL expression in DBML) → strip backticks.
    if let Some(rest) = v.strip_prefix('`') {
        return rest.strip_suffix('`').unwrap_or(rest).to_string();
    }
    // Single-quoted DBML string literal → produce a SQL string literal.
    // 1. Use `parse_single_line_string` to strip the surrounding `'…'` and
    //    unescape DBML `\'` → `'`.
    // 2. Re-quote for SQL: wrap in single quotes and double any internal `'`
    //    (SQL standard escape), so `a'b` → `'a''b'`.
    if v.starts_with('\'')
        && let Some(inner) = parse_single_line_string(v)
    {
        // `inner` already has `\'` → `'` applied by parse_single_line_string.
        let sql_escaped = inner.replace('\'', "''");
        return format!("'{sql_escaped}'");
    }
    // Bare token (number, true, false, null) — store as-is.
    v.to_string()
}

/// An entry of an `indexes { … }` block: an index, the table-level UNIQUE
/// constraint dbd wrote as one (see `crate::dbml::index_note`), or the
/// primary key — a `pk` entry, named or not, on these columns.
enum IndexEntry {
    Index(IndexDef),
    Unique(TableConstraint),
    PrimaryKey(Option<String>, Vec<String>),
}

/// Parse an index line: `(<cols>) [settings]` or `<col> [settings]`.
fn parse_index_line(line: &str, table: &str) -> Result<IndexEntry> {
    let line = line.trim();
    let (cols_part, settings_part) = split_trailing_settings(line);
    let cols_part = cols_part.trim();

    // A tuple is split only at its top-level commas: a backticked expression
    // key such as `coalesce(b, 'x, y')` carries commas of its own.
    let raw_keys: Vec<String> = if let Some(inner) = cols_part.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        split_top_level_commas(inner)
            .ok_or_else(|| parse_err(format!("malformed index in `{table}`: {line}")))?
            .iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    } else {
        vec![cols_part.to_string()]
    };
    let keys: Vec<IndexColumn> = raw_keys.iter().map(|k| parse_index_key(k)).collect();

    if keys.is_empty() || keys.iter().any(|k| k.name.is_empty()) {
        return Err(parse_err(format!("malformed index in `{table}`: {line}")));
    }

    let mut unique = false;
    let mut pk = false;
    let mut name = None;
    let mut note = None;
    let mut index_type = None;
    if let Some(settings_str) = settings_part {
        let settings = extract_settings(settings_str)
            .ok_or_else(|| parse_err(format!("malformed index settings in `{table}`: {settings_str}")))?;
        for (key, value) in settings {
            let k = key.to_ascii_lowercase();
            match k.as_str() {
                "unique" => unique = true,
                "pk" => pk = true,
                "name" => {
                    if let Some(v) = value {
                        name = parse_single_line_string(&v);
                    }
                }
                "note" => {
                    note = value
                        .as_deref()
                        .and_then(parse_single_line_string)
                        .map(|n| note_text(&n))
                }
                // The access method, an identifier: `type: gin`.
                "type" => index_type = value.map(|v| IndexType::from_amname(v.trim())),
                _ => {}
            }
        }
    }

    // The primary key is a plain list of columns.
    if pk {
        if keys.iter().any(|k| k.is_expression) {
            return Err(parse_err(format!(
                "a `pk` index in `{table}` has an expression key, which a primary key cannot: {line}"
            )));
        }
        return Ok(IndexEntry::PrimaryKey(name, keys.into_iter().map(|k| k.name).collect()));
    }

    let mut index = IndexDef {
        name,
        columns: keys,
        unique,
        index_type,
        ..Default::default()
    };
    // What DBML's index settings cannot say — a partial `WHERE`, key order,
    // `NULLS NOT DISTINCT`, `INCLUDE`, operator classes, storage parameters —
    // dbd carries in the note (see `crate::dbml::index_note`).
    let constraint = note.as_deref().is_some_and(crate::dbml::index_note::marks_a_constraint);
    if let Some(note) = note {
        crate::dbml::index_note::read(&note, &mut index);
    }
    // A UNIQUE constraint is a plain list of columns; an entry marked as one
    // that holds anything a constraint cannot is the index it says it is.
    let plain = index.unique
        && index.predicate.is_none()
        && index.include.is_empty()
        && index.with_options.is_empty()
        && index.index_type.is_none()
        && index
            .columns
            .iter()
            .all(|k| !k.is_expression && k.order.is_none() && k.nulls_first.is_none() && k.opclass.is_none());
    if constraint && plain {
        return Ok(IndexEntry::Unique(TableConstraint::Unique {
            name: index.name,
            columns: index.columns.into_iter().map(|k| k.name).collect(),
            nulls_not_distinct: index.nulls_not_distinct,
        }));
    }
    Ok(IndexEntry::Index(index))
}

/// One index key: DBML writes an expression in backticks and a column by
/// (optionally quoted) name. Flagging the expression is what keeps the
/// emitter from quoting `lower(email)` as an identifier.
fn parse_index_key(key: &str) -> IndexColumn {
    let key = key.trim();
    match backticked(key) {
        Some(expression) => IndexColumn {
            name: expression,
            is_expression: true,
            ..Default::default()
        },
        None => IndexColumn {
            name: unquote(key),
            ..Default::default()
        },
    }
}

/// A Ref's body (everything after `Ref[ name]:`): `<a> <op> <b> [settings]`.
fn parse_ref_body(body: &str, name: Option<String>) -> Result<PendingRef> {
    let body = body.trim();
    let (refs_part, settings_part) = split_trailing_settings(body);
    let (left, op, right) = split_ref_operator(refs_part.trim())
        .ok_or_else(|| parse_err(format!("Ref missing relationship operator: {body}")))?;
    let left = parse_ref_endpoint(left)?;
    let right = parse_ref_endpoint(right)?;

    let mut on_delete = None;
    let mut on_update = None;
    if let Some(settings_str) = settings_part {
        let settings = extract_settings(settings_str)
            .ok_or_else(|| parse_err(format!("malformed Ref settings: {settings_str}")))?;
        for (key, value) in settings {
            match key.to_ascii_lowercase().as_str() {
                "delete" => on_delete = value.as_deref().and_then(FkAction::from_dbml),
                "update" => on_update = value.as_deref().and_then(FkAction::from_dbml),
                _ => {}
            }
        }
    }
    PendingRef::new(name, left, op, right, on_delete, on_update)
}

/// Locate the relationship operator (`<>`, `>`, `<`, `-`) outside of quotes and
/// parentheses, returning `(left, op, right)`.
fn split_ref_operator(s: &str) -> Option<(&str, RefOp, &str)> {
    let bytes = s.as_bytes();
    let mut in_quote = false;
    let mut paren = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '"' => in_quote = !in_quote,
            '(' if !in_quote => paren += 1,
            ')' if !in_quote => paren -= 1,
            '<' if !in_quote && paren == 0 => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    return Some((s[..i].trim(), RefOp::ManyToMany, s[i + 2..].trim()));
                }
                return Some((s[..i].trim(), RefOp::OneToMany, s[i + 1..].trim()));
            }
            '>' if !in_quote && paren == 0 => {
                return Some((s[..i].trim(), RefOp::ManyToOne, s[i + 1..].trim()));
            }
            '-' if !in_quote && paren == 0 => {
                return Some((s[..i].trim(), RefOp::OneToOne, s[i + 1..].trim()));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a Ref endpoint: `"s"."t"."c"` or `"s"."t".(c1, c2)` (or unquoted /
/// unqualified variants — an unqualified table may be an alias).
fn parse_ref_endpoint(s: &str) -> Result<Endpoint> {
    let s = s.trim();
    // Composite: a `.(...)` tuple at the end.
    if let Some(paren_at) = s.find(".(") {
        let prefix = &s[..paren_at];
        let tuple = &s[paren_at + 1..];
        let inner = tuple
            .strip_prefix('(')
            .and_then(|t| t.strip_suffix(')'))
            .ok_or_else(|| parse_err(format!("malformed composite ref endpoint: {s}")))?;
        let columns: Vec<String> = inner
            .split(',')
            .map(|c| unquote(c.trim()))
            .filter(|c| !c.is_empty())
            .collect();
        if columns.is_empty() {
            return Err(parse_err(format!("composite ref has no columns: {s}")));
        }
        let parts = split_dotted(prefix.trim());
        return match parts.as_slice() {
            [schema, table] => Ok(Endpoint {
                schema: Some(unquote(schema)),
                table: unquote(table),
                columns,
            }),
            [table] => Ok(Endpoint {
                schema: None,
                table: unquote(table),
                columns,
            }),
            _ => Err(parse_err(format!("malformed ref table prefix: {prefix}"))),
        };
    }

    // Simple: `schema.table.column` / `table.column`.
    let parts = split_dotted(s);
    match parts.as_slice() {
        [schema, table, col] => Ok(Endpoint {
            schema: Some(unquote(schema)),
            table: unquote(table),
            columns: vec![unquote(col)],
        }),
        [table, col] => Ok(Endpoint {
            schema: None,
            table: unquote(table),
            columns: vec![unquote(col)],
        }),
        _ => Err(parse_err(format!("malformed ref endpoint: {s}"))),
    }
}

/// Split a trailing `[ … ]` settings group off the end of `s`, returning
/// `(before, Some(inner))` or `(s, None)` when there is no settings group.
/// Only a settings group that is the literal tail of `s` is considered, so a
/// `[]` array-type suffix in the middle is left attached to the type.
fn split_trailing_settings(s: &str) -> (&str, Option<&str>) {
    let trimmed = s.trim_end();
    if !trimmed.ends_with(']') {
        return (s, None);
    }
    // Walk back to the matching `[`, balancing nested brackets and ignoring
    // brackets inside quotes.
    //
    // Escape-awareness (backward walk): a `'` preceded by an odd run of `\`
    // is an escaped apostrophe — it must NOT toggle `in_quote`. An even run is
    // escaped backslashes, so `'C:\\'` still ends at its last quote.
    let bytes = trimmed.as_bytes();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        let c = bytes[i] as char;
        match c {
            '\'' => {
                let backslashes = bytes[..i].iter().rev().take_while(|&&b| b == b'\\').count();
                if backslashes % 2 == 0 {
                    in_quote = !in_quote;
                }
            }
            '"' => in_quote = !in_quote,
            ']' if !in_quote => depth += 1,
            '[' if !in_quote => {
                depth -= 1;
                if depth == 0 {
                    // Guard against an empty `[]` array suffix being treated as
                    // settings — that belongs to the type.
                    let inner = &trimmed[i + 1..trimmed.len() - 1];
                    if inner.trim().is_empty() {
                        return (s, None);
                    }
                    return (&trimmed[..i], Some(inner));
                }
            }
            _ => {}
        }
    }
    (s, None)
}

/// Parse the inner text of a `[ … ]` settings group into `(key, value)` pairs.
/// `value` is `None` for flag settings (`pk`, `unique`, `not null`, …).
/// Splits on top-level commas (outside quotes/backticks/parens).
fn extract_settings(inner: &str) -> Option<Vec<(String, Option<String>)>> {
    let parts = split_top_level_commas(inner)?;
    let mut out = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // Split on the first top-level ':'.
        match split_first_colon(part) {
            Some((key, value)) => out.push((key.trim().to_string(), Some(value.trim().to_string()))),
            None => out.push((part.to_string(), None)),
        }
    }
    Some(out)
}

/// Split on commas that are outside quotes, backticks, and parentheses.
fn split_top_level_commas(s: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_back = false;
    let mut paren = 0i32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // While inside a single-quoted string a backslash escapes the next
            // character — consume both and push both without toggling quote state.
            '\\' if in_single => {
                cur.push(c);
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
                continue;
            }
            '\'' if !in_double && !in_back => in_single = !in_single,
            '"' if !in_single && !in_back => in_double = !in_double,
            '`' if !in_single && !in_double => in_back = !in_back,
            '(' if !in_single && !in_double && !in_back => paren += 1,
            ')' if !in_single && !in_double && !in_back => paren -= 1,
            ',' if !in_single && !in_double && !in_back && paren == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if in_single || in_double || in_back || paren != 0 {
        return None;
    }
    parts.push(cur);
    Some(parts)
}

/// Split a setting on its first `:` that is outside quotes/backticks/parens.
fn split_first_colon(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_back = false;
    let mut paren = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            // While inside a single-quoted string a backslash escapes the next
            // character — skip both without toggling quote state.
            '\\' if in_single && i + 1 < bytes.len() => {
                i += 2;
                continue;
            }
            '\'' if !in_double && !in_back => in_single = !in_single,
            '"' if !in_single && !in_back => in_double = !in_double,
            '`' if !in_single && !in_double => in_back = !in_back,
            '(' if !in_single && !in_double && !in_back => paren += 1,
            ')' if !in_single && !in_double && !in_back => paren -= 1,
            ':' if !in_single && !in_double && !in_back && paren == 0 => {
                return Some((&s[..i], &s[i + 1..]));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a single-line quoted string value (`'…'` or `"…"`), reading its
/// escapes the way DBML does. Returns `None` if the input is not a quoted string.
fn parse_single_line_string(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = if let Some(rest) = s.strip_prefix('\'') {
        rest.strip_suffix('\'')?
    } else {
        let rest = s.strip_prefix('"')?;
        rest.strip_suffix('"')?
    };
    Some(unescape_dbml(inner))
}

/// Read the backslash escapes in a DBML quoted string, mirroring DBML's own
/// lexer: `\\`, `\'`, `\"`, `` \` ``, `\n`, `\t`, `\r`, `\0`, `\b`, `\v`, `\f`,
/// `\uHHHH`, an escaped line break (which joins the lines), and `\ ` (kept as
/// written). Any other `\x` reads as `x`. Reading only `\'` meant a `\\` that
/// dbdiagram.io shows as one backslash came back as two.
fn unescape_dbml(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('b') => out.push('\u{8}'),
            Some('v') => out.push('\u{b}'),
            Some('f') => out.push('\u{c}'),
            Some('\n') => {}
            Some('\r') => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            Some(' ') => out.push_str("\\ "),
            Some('u') => {
                let hex: String = chars.clone().take(4).collect();
                match u32::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 4)
                    .and_then(char::from_u32)
                {
                    Some(ch) => {
                        out.push(ch);
                        chars.nth(3);
                    }
                    None => out.push_str("\\u"),
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Byte offset of the `'''` that closes a DBML multi-line string in `s`,
/// skipping escaped characters — an escaped quote is part of the text, not
/// the start of the closing run.
fn closing_triple_quote(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\'' if s[i..].starts_with("'''") => return Some(i),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_table<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
        entities
            .iter()
            .find(|e| e.entity_type == EntityType::Table && e.name == name)
            .unwrap_or_else(|| panic!("table {name} not found"))
    }

    fn find_enum<'a>(entities: &'a [Entity], name: &str) -> &'a Entity {
        entities
            .iter()
            .find(|e| e.entity_type == EntityType::Enum && e.name == name)
            .unwrap_or_else(|| panic!("enum {name} not found"))
    }

    #[test]
    fn parse_project_block_is_skipped() {
        let dbml = "Project \"My App\" {\n  database_type: 'PostgreSQL'\n  Note: 'hi'\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        assert!(entities.is_empty(), "project block should produce no entities");
    }

    #[test]
    fn parse_enum_values_in_order() {
        let dbml =
            "Enum \"config\".\"status\" {\n  \"active\" [note: 'is active']\n  \"inactive\"\n  \"archived\"\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let e = find_enum(&entities, "config.status");
        assert_eq!(e.schema.as_deref(), Some("config"));
        let names: Vec<&str> = e.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["active", "inactive", "archived"]);
        assert_eq!(e.enum_values[0].note.as_deref(), Some("is active"));
        assert!(e.enum_values[1].note.is_none());
    }

    #[test]
    fn parse_table_with_every_column_setting() {
        let dbml = "Table \"app\".\"users\" {\n  \"id\" bigserial [pk, increment, not null]\n  \"email\" \"varchar(255)\" [not null, unique]\n  \"is_active\" boolean [default: true]\n  \"count\" int [default: 42]\n  \"created_at\" timestamptz [default: `now()`]\n  \"label\" text [default: 'hi', note: 'a label']\n  \"deleted\" boolean [default: null]\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let t = find_table(&entities, "app.users");
        let td = t.table_def.as_ref().unwrap();

        let id = &td.columns[0];
        assert_eq!(id.name, "id");
        assert_eq!(id.data_type, "bigserial");
        assert!(id.is_pk);
        assert!(!id.nullable);

        let email = &td.columns[1];
        assert_eq!(email.data_type, "varchar(255)");
        assert!(!email.nullable);
        assert!(email.is_unique);

        assert_eq!(td.columns[2].default_value.as_deref(), Some("true"));
        assert_eq!(td.columns[3].default_value.as_deref(), Some("42"));
        assert_eq!(td.columns[4].default_value.as_deref(), Some("now()"));
        // String default `'hi'` in DBML → SQL-ready `'hi'` (with quotes preserved).
        assert_eq!(td.columns[5].default_value.as_deref(), Some("'hi'"));
        assert_eq!(td.columns[5].comment.as_deref(), Some("a label"));
        assert_eq!(td.columns[6].default_value.as_deref(), Some("null"));

        // PK constraint synthesised from the [pk] column.
        assert!(td.constraints.iter().any(|c| matches!(
            c,
            TableConstraint::PrimaryKey { columns, .. } if columns == &vec!["id".to_string()]
        )));
    }

    /// `increment` is DBML's auto-increment. On a serial type the type already
    /// says so; on any other it is the identity dbd's exporter wrote it for,
    /// and DBML cannot say ALWAYS, so it reads as BY DEFAULT.
    #[test]
    fn increment_reads_as_identity_unless_the_type_is_serial() {
        let dbml = "Table \"app\".\"t\" {\n  \"id\" bigint [pk, increment]\n  \"seq\" bigserial [increment]\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert_eq!(td.columns[0].identity, Some(crate::entity::IdentityKind::ByDefault));
        assert!(!td.columns[0].nullable, "an identity column is NOT NULL");
        assert_eq!(td.columns[1].identity, None, "bigserial carries its own sequence");
    }

    /// The `generated as identity` custom property names the identity kind
    /// `increment` cannot; an unknown kind is refused rather than guessed.
    #[test]
    fn the_generated_as_identity_property_names_the_identity_kind() {
        use crate::entity::IdentityKind;
        let dbml = "Table \"app\".\"t\" {\n  \"a\" bigint [increment, generated as identity: 'always']\n  \"b\" bigint [increment, generated as identity: 'by default']\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert_eq!(td.columns[0].identity, Some(IdentityKind::Always));
        assert_eq!(td.columns[1].identity, Some(IdentityKind::ByDefault));

        let unknown = "Table \"app\".\"t\" {\n  \"a\" bigint [increment, generated as identity: 'sometimes']\n}\n";
        let err = parse_dbml(unknown).unwrap_err();
        assert!(err.to_string().contains("sometimes"), "got: {err}");
    }

    /// The `generated always as` custom property is a generated column's
    /// expression; a user's own custom property is metadata and changes nothing.
    #[test]
    fn the_generated_always_as_property_makes_a_generated_column() {
        let dbml = "Table \"app\".\"t\" {\n  \"a\" int\n  \"b\" numeric [generated always as: 'a / 100.0', not null]\n  \"c\" text [generated: \"by-etl\", pii: 'true']\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert_eq!(td.columns[1].generated.as_deref(), Some("a / 100.0"));
        assert!(!td.columns[1].nullable, "the other settings still apply");
        assert_eq!(td.columns[2].generated, None, "a user's tag is not dbd's convention");
    }

    #[test]
    fn parse_indexes_block_bare_and_parenthesized() {
        let dbml = "Table \"app\".\"t\" {\n  \"a\" int\n  \"b\" int\n\n  indexes {\n    a [unique, name: 'idx_a']\n    (a, b) [name: 'idx_ab']\n  }\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let t = find_table(&entities, "app.t");
        let td = t.table_def.as_ref().unwrap();
        assert_eq!(td.indexes.len(), 2);

        let idx_a = &td.indexes[0];
        assert_eq!(idx_a.name.as_deref(), Some("idx_a"));
        assert!(idx_a.unique);
        assert_eq!(idx_a.columns.len(), 1);
        assert_eq!(idx_a.columns[0].name, "a");

        let idx_ab = &td.indexes[1];
        assert_eq!(idx_ab.name.as_deref(), Some("idx_ab"));
        assert!(!idx_ab.unique);
        let cols: Vec<&str> = idx_ab.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, vec!["a", "b"]);
    }

    /// A backticked index key is an expression, and a comma inside one does
    /// not split the key list.
    #[test]
    fn a_backticked_index_key_is_an_expression() {
        let dbml = "Table \"app\".\"t\" {\n  \"a\" int\n  \"b\" text\n\n  indexes {\n    `lower(b)` [name: 'idx_lower']\n    (a, `coalesce(b, 'x, y')`) [name: 'idx_mixed']\n  }\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        let keys = |i: usize| -> Vec<(&str, bool)> {
            td.indexes[i]
                .columns
                .iter()
                .map(|c| (c.name.as_str(), c.is_expression))
                .collect()
        };
        assert_eq!(keys(0), vec![("lower(b)", true)]);
        assert_eq!(keys(1), vec![("a", false), ("coalesce(b, 'x, y')", true)]);
    }

    /// Only dbd's own note lines are read as index facts. A prose note from
    /// dbdiagram.io, or an `order:` line that does not cover every key, leaves
    /// the index as DBML's syntax describes it rather than half-applying.
    #[test]
    fn an_index_note_outside_the_convention_changes_nothing() {
        let dbml = "Table \"app\".\"t\" {\n  \"a\" int\n  \"b\" int\n\n  indexes {\n    (a, b) [name: 'idx_prose', note: 'Where the search page looks things up']\n    (a, b) [name: 'idx_short', note: 'order: desc']\n  }\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        for ix in &td.indexes {
            assert_eq!(ix.predicate, None, "{:?}", ix.name);
            assert!(!ix.nulls_not_distinct, "{:?}", ix.name);
            assert!(
                ix.columns.iter().all(|c| c.order.is_none() && c.nulls_first.is_none()),
                "{:?}",
                ix.name
            );
        }
    }

    /// Both of DBML's CHECK spellings read as table CHECK constraints: the
    /// `checks { … }` block (named or not) and a column's `check:` setting,
    /// which may repeat.
    #[test]
    fn checks_blocks_and_column_checks_read_as_check_constraints() {
        let dbml = "Table \"app\".\"t\" {\n  \"qty\" int [not null, check: `qty > 0`, check: `qty < 1000`]\n  \"total\" int\n\n  checks {\n    `total >= 0` [name: 't_total_positive']\n    `total <> 13`\n  }\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        let mut found: Vec<(Option<&str>, &str)> = td
            .constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::Check { name, expression } => Some((name.as_deref(), expression.as_str())),
                _ => None,
            })
            .collect();
        found.sort_unstable();
        assert_eq!(
            found,
            vec![
                (None, "qty < 1000"),
                (None, "qty > 0"),
                (None, "total <> 13"),
                (Some("t_total_positive"), "total >= 0"),
            ]
        );
        assert!(!td.columns[0].nullable, "the other settings still apply");
    }

    /// A multi-line note written flush against its quotes — the form
    /// @dbml/core reads back exactly — reads back exactly here too, a quote at
    /// its end included.
    #[test]
    fn a_multi_line_note_reads_back_exactly() {
        use crate::dbml::{DbmlParams, generate_dbml};
        use crate::entity::TableComments;

        for note in [
            "line one\nline two",
            "say\n'hi'",
            "two\nquotes ''",
            "a\n  indented\nnote",
        ] {
            let mut t = Entity::new(EntityType::Table, "app.t");
            t.table_def = Some(TableDef {
                columns: vec![],
                constraints: vec![],
                indexes: vec![],
                comments: TableComments {
                    table: Some(note.into()),
                    ..Default::default()
                },
            });
            let doc = generate_dbml(&DbmlParams {
                entities: &[t],
                project_name: "notes",
                database_type: "PostgreSQL",
                project_note: Some(note),
                include_schemas: vec![],
                exclude_schemas: vec![],
                include_tables: vec![],
                exclude_tables: vec![],
                groups: vec![],
                auto_group_by_schema: false,
            });
            assert!(
                !doc.content.contains("Note: '''\n") && !doc.content.lines().any(|l| l.trim() == "'''"),
                "flush against its quotes:\n{}",
                doc.content
            );
            let parsed = parse_dbml(&doc.content).expect("DBML must parse");
            let td = find_table(&parsed, "app.t").table_def.as_ref().unwrap();
            assert_eq!(td.comments.table.as_deref(), Some(note), "\n{}", doc.content);
        }
    }

    #[test]
    fn parse_multiline_triple_quoted_note() {
        let dbml = "Table \"app\".\"t\" {\n  \"id\" int\n\n  Note: '''\nLine one.\nLine two.\n'''\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let t = find_table(&entities, "app.t");
        let td = t.table_def.as_ref().unwrap();
        assert_eq!(td.comments.table.as_deref(), Some("Line one.\nLine two."));
    }

    #[test]
    fn parse_single_line_table_note() {
        let dbml = "Table \"app\".\"t\" {\n  \"id\" int\n\n  Note: 'A simple table'\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let t = find_table(&entities, "app.t");
        let td = t.table_def.as_ref().unwrap();
        assert_eq!(td.comments.table.as_deref(), Some("A simple table"));
    }

    #[test]
    fn parse_standalone_ref_simple() {
        let dbml = "Table \"app\".\"orders\" {\n  \"id\" int\n  \"user_id\" int\n}\nTable \"app\".\"users\" {\n  \"id\" int\n}\nRef: \"app\".\"orders\".\"user_id\" > \"app\".\"users\".\"id\" [delete: cascade, update: no action]\n";
        let entities = parse_dbml(dbml).unwrap();
        let orders = find_table(&entities, "app.orders");
        let td = orders.table_def.as_ref().unwrap();
        let fk = td
            .constraints
            .iter()
            .find_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some(fk),
                _ => None,
            })
            .expect("orders should have a FK");
        assert_eq!(fk.columns, vec!["user_id".to_string()]);
        assert_eq!(fk.ref_schema.as_deref(), Some("app"));
        assert_eq!(fk.ref_table, "users");
        assert_eq!(fk.ref_columns, vec!["id".to_string()]);
        assert_eq!(fk.on_delete, Some(FkAction::Cascade));
        assert_eq!(fk.on_update, Some(FkAction::NoAction));
    }

    #[test]
    fn parse_standalone_ref_composite() {
        let dbml = "Table \"shop\".\"orders\" {\n  \"user_id\" int\n  \"tenant_id\" int\n}\nRef: \"shop\".\"orders\".(\"user_id\", \"tenant_id\") > \"auth\".\"memberships\".(\"user_id\", \"tenant_id\") [delete: restrict]\n";
        let entities = parse_dbml(dbml).unwrap();
        let orders = find_table(&entities, "shop.orders");
        let td = orders.table_def.as_ref().unwrap();
        let fk = td
            .constraints
            .iter()
            .find_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some(fk),
                _ => None,
            })
            .expect("orders should have a composite FK");
        assert_eq!(fk.columns, vec!["user_id".to_string(), "tenant_id".to_string()]);
        assert_eq!(fk.ref_schema.as_deref(), Some("auth"));
        assert_eq!(fk.ref_table, "memberships");
        assert_eq!(fk.ref_columns, vec!["user_id".to_string(), "tenant_id".to_string()]);
        assert_eq!(fk.on_delete, Some(FkAction::Restrict));
    }

    #[test]
    fn parse_ref_each_action_maps_correctly() {
        for (kw, expected) in [
            ("cascade", FkAction::Cascade),
            ("restrict", FkAction::Restrict),
            ("set null", FkAction::SetNull),
            ("set default", FkAction::SetDefault),
            ("no action", FkAction::NoAction),
        ] {
            let dbml = format!(
                "Table \"s\".\"a\" {{\n  \"x\" int\n}}\nTable \"s\".\"b\" {{\n  \"id\" int\n}}\nRef: \"s\".\"a\".\"x\" > \"s\".\"b\".\"id\" [delete: {kw}]\n"
            );
            let entities = parse_dbml(&dbml).unwrap();
            let a = find_table(&entities, "s.a");
            let fk = a
                .table_def
                .as_ref()
                .unwrap()
                .constraints
                .iter()
                .find_map(|c| match c {
                    TableConstraint::ForeignKey(fk) => Some(fk),
                    _ => None,
                })
                .unwrap();
            assert_eq!(fk.on_delete, Some(expected), "action `{kw}`");
        }
    }

    #[test]
    fn parse_ref_less_than_swaps_sides() {
        // `parent < child` means the FK lives on the child (right side).
        let dbml = "Table \"s\".\"child\" {\n  \"pid\" int\n}\nTable \"s\".\"parent\" {\n  \"id\" int\n}\nRef: \"s\".\"parent\".\"id\" < \"s\".\"child\".\"pid\"\n";
        let entities = parse_dbml(dbml).unwrap();
        let child = find_table(&entities, "s.child");
        let fk = child
            .table_def
            .as_ref()
            .unwrap()
            .constraints
            .iter()
            .find_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some(fk),
                _ => None,
            })
            .expect("child should own the FK after swap");
        assert_eq!(fk.columns, vec!["pid".to_string()]);
        assert_eq!(fk.ref_table, "parent");
    }

    #[test]
    fn parse_unqualified_name_defaults_to_public() {
        let dbml = "Table users {\n  id int\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let t = find_table(&entities, "public.users");
        assert_eq!(t.schema.as_deref(), Some("public"));
        assert_eq!(t.table_def.as_ref().unwrap().columns[0].name, "id");
    }

    #[test]
    fn parse_schema_qualified_name() {
        let dbml = "Table \"reporting\".\"daily\" {\n  \"id\" int\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let t = find_table(&entities, "reporting.daily");
        assert_eq!(t.schema.as_deref(), Some("reporting"));
    }

    #[test]
    fn synthesizes_schema_entities() {
        let dbml = "Table \"app\".\"a\" {\n  \"id\" int\n}\nTable \"audit\".\"b\" {\n  \"id\" int\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let schemas: Vec<&str> = entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Schema)
            .map(|e| e.name.as_str())
            .collect();
        assert!(schemas.contains(&"app"));
        assert!(schemas.contains(&"audit"));
    }

    #[test]
    fn lenient_skip_of_unknown_block_and_table_group() {
        let dbml = "TableGroup \"core\" {\n  \"app\".\"users\"\n}\nSomeFutureBlock foo {\n  bar: baz\n}\n// a stray comment\nNote: 'top level note'\nTable \"app\".\"users\" {\n  \"id\" int\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        // The table is still parsed despite the surrounding unknown constructs.
        let t = find_table(&entities, "app.users");
        assert_eq!(t.table_def.as_ref().unwrap().columns.len(), 1);
    }

    #[test]
    fn line_comments_are_stripped() {
        let dbml = "Table \"app\".\"t\" {\n  \"id\" int // primary id\n  // a comment line\n  \"name\" text\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert_eq!(td.columns.len(), 2);
        assert_eq!(td.columns[0].name, "id");
        assert_eq!(td.columns[0].data_type, "int");
        assert_eq!(td.columns[1].name, "name");
    }

    #[test]
    fn array_type_suffix_is_preserved() {
        let dbml = "Table \"app\".\"t\" {\n  \"tags\" text[] [not null]\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert_eq!(td.columns[0].data_type, "text[]");
        assert!(!td.columns[0].nullable);
    }

    #[test]
    fn unterminated_table_errors() {
        let dbml = "Table \"app\".\"t\" {\n  \"id\" int\n";
        let err = parse_dbml(dbml).unwrap_err();
        assert!(matches!(err, DbdError::Parse { .. }));
    }

    // ── Escape-aware scanner tests ────────────────────────────────────────────

    /// A settings group whose note value contains an escaped apostrophe must
    /// parse both settings (note + nullable flag) without the apostrophe
    /// corrupting structural quote-tracking.
    #[test]
    fn settings_with_escaped_apostrophe_in_note() {
        // Hand-written: note value is `a'b` (escaped as `a\'b`).
        let inner = "note: 'a\\'b', not null";
        let settings = extract_settings(inner).expect("should parse");
        assert_eq!(settings.len(), 2, "both settings must be present");

        let note_setting = settings
            .iter()
            .find(|(k, _)| k == "note")
            .expect("note setting missing");
        // The raw value still contains the backslash escape; `parse_single_line_string`
        // will strip it when the caller interprets the value.
        assert_eq!(note_setting.1.as_deref(), Some("'a\\'b'"));

        let not_null = settings.iter().find(|(k, _)| k == "not null");
        assert!(not_null.is_some(), "not null flag must be present");
    }

    // ── Round-trip: exporter → parser → structural equality ──────────────────

    #[test]
    fn round_trips_with_apostrophes() {
        use crate::dbml::{DbmlParams, generate_dbml};
        use crate::entity::TableComments;

        // Table with a column whose comment (note) contains an apostrophe.
        // The exporter (emit_column) escapes it as `\'` in the DBML.
        let mut profiles = Entity::new(EntityType::Table, "public.profiles");
        profiles.table_def = Some(TableDef {
            columns: vec![
                ColumnDef {
                    name: "bio".into(),
                    data_type: "text".into(),
                    nullable: true,
                    default_value: None,
                    is_pk: false,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: Some("The user's primary biography".into()),
                    inline_fk: None,
                    collation: None,
                },
                ColumnDef {
                    name: "nickname".into(),
                    data_type: "text".into(),
                    nullable: true,
                    default_value: Some("guest".into()),
                    is_pk: false,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: Some("User's display name".into()),
                    inline_fk: None,
                    collation: None,
                },
            ],
            constraints: vec![],
            indexes: vec![],
            comments: TableComments::default(),
        });

        let entities = vec![profiles];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "ApostropheTest",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        let parsed = parse_dbml(&doc.content).unwrap();
        let t = find_table(&parsed, "public.profiles");
        let td = t.table_def.as_ref().unwrap();

        let bio = td.columns.iter().find(|c| c.name == "bio").unwrap();
        // data_type must be clean — no `[note: ...]` glued on.
        assert_eq!(
            bio.data_type, "text",
            "bio data_type must be clean, not glued with settings"
        );
        assert_eq!(
            bio.comment.as_deref(),
            Some("The user's primary biography"),
            "apostrophe in note must round-trip correctly"
        );

        let nickname = td.columns.iter().find(|c| c.name == "nickname").unwrap();
        assert_eq!(nickname.data_type, "text");
        assert_eq!(
            nickname.comment.as_deref(),
            Some("User's display name"),
            "apostrophe in second column note must round-trip"
        );
        // "guest" in SQL form is a bare string default; quote_default wraps it
        // as DBML `'guest'`; the fixed parser stores it back as SQL `'guest'`.
        assert_eq!(nickname.default_value.as_deref(), Some("'guest'"));
    }

    #[test]
    fn round_trips_through_generate_dbml() {
        use crate::dbml::{DbmlParams, generate_dbml};
        use crate::entity::{EnumValue, IndexColumn, TableComments};

        // Enum.
        let mut status = Entity::new(EntityType::Enum, "config.status");
        status.enum_values = vec![
            EnumValue {
                name: "active".into(),
                note: None,
            },
            EnumValue {
                name: "inactive".into(),
                note: Some("not active".into()),
            },
        ];

        // Parent table (auth.memberships) with composite PK.
        let mut memberships = Entity::new(EntityType::Table, "auth.memberships");
        memberships.table_def = Some(TableDef {
            columns: vec![
                ColumnDef {
                    name: "user_id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: true,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: None,
                    inline_fk: None,
                    collation: None,
                },
                ColumnDef {
                    name: "tenant_id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: true,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: None,
                    inline_fk: None,
                    collation: None,
                },
            ],
            constraints: vec![TableConstraint::PrimaryKey {
                name: None,
                columns: vec!["user_id".into(), "tenant_id".into()],
            }],
            indexes: vec![],
            comments: TableComments::default(),
        });

        // Child table (shop.orders) with serial PK, a unique col, an index,
        // a table note, a single FK and a composite FK.
        let mut orders = Entity::new(EntityType::Table, "shop.orders");
        let comments = TableComments {
            table: Some("Customer orders.\nOne row per order.".into()),
            ..Default::default()
        };
        orders.table_def = Some(TableDef {
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    data_type: "bigserial".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: true,
                    is_unique: false,
                    identity: Some(crate::entity::IdentityKind::ByDefault),
                    generated: None,
                    comment: None,
                    inline_fk: None,
                    collation: None,
                },
                ColumnDef {
                    name: "code".into(),
                    data_type: "varchar(20)".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: false,
                    is_unique: true,
                    identity: None,
                    generated: None,
                    comment: Some("Order code".into()),
                    inline_fk: None,
                    collation: None,
                },
                ColumnDef {
                    name: "user_id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: false,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: None,
                    inline_fk: None,
                    collation: None,
                },
                ColumnDef {
                    name: "tenant_id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: false,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: None,
                    inline_fk: None,
                    collation: None,
                },
                ColumnDef {
                    name: "qty".into(),
                    data_type: "int".into(),
                    nullable: true,
                    default_value: Some("1".into()),
                    is_pk: false,
                    is_unique: false,
                    identity: None,
                    generated: None,
                    comment: None,
                    inline_fk: None,
                    collation: None,
                },
            ],
            constraints: vec![
                TableConstraint::PrimaryKey {
                    name: None,
                    columns: vec!["id".into()],
                },
                TableConstraint::ForeignKey(ForeignKey {
                    name: None,
                    columns: vec!["user_id".into(), "tenant_id".into()],
                    ref_schema: Some("auth".into()),
                    ref_table: "memberships".into(),
                    ref_columns: vec!["user_id".into(), "tenant_id".into()],
                    on_delete: Some(FkAction::Cascade),
                    on_update: None,
                    ..Default::default()
                }),
            ],
            indexes: vec![IndexDef {
                name: Some("idx_orders_code".into()),
                columns: vec![IndexColumn {
                    name: "code".into(),
                    order: None,
                    ..Default::default()
                }],
                unique: true,
                index_type: None,
                ..Default::default()
            }],
            comments,
        });

        let entities = vec![status.clone(), memberships.clone(), orders.clone()];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "RoundTrip",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        let parsed = parse_dbml(&doc.content).unwrap();

        // ── Enum equality ──
        let p_status = find_enum(&parsed, "config.status");
        assert_eq!(p_status.schema.as_deref(), Some("config"));
        let pe: Vec<(&str, Option<&str>)> = p_status
            .enum_values
            .iter()
            .map(|v| (v.name.as_str(), v.note.as_deref()))
            .collect();
        assert_eq!(pe, vec![("active", None), ("inactive", Some("not active"))]);

        // ── memberships table ──
        let p_mem = find_table(&parsed, "auth.memberships");
        let mem_td = p_mem.table_def.as_ref().unwrap();
        let mem_cols: Vec<(&str, &str, bool)> = mem_td
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable))
            .collect();
        assert_eq!(mem_cols, vec![("user_id", "uuid", false), ("tenant_id", "uuid", false)]);
        // Composite PK survives.
        assert!(mem_td.constraints.iter().any(|c| matches!(
            c,
            TableConstraint::PrimaryKey { columns, .. }
                if columns == &vec!["user_id".to_string(), "tenant_id".to_string()]
        )));

        // ── orders table ──
        let p_orders = find_table(&parsed, "shop.orders");
        let o_td = p_orders.table_def.as_ref().unwrap();

        // Serial PK column survives as bigserial + pk + not null.
        let id = o_td.columns.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(id.data_type, "bigserial");
        assert!(id.is_pk);
        assert!(!id.nullable);

        // Unique column + comment.
        let code = o_td.columns.iter().find(|c| c.name == "code").unwrap();
        assert_eq!(code.data_type, "varchar(20)");
        assert!(code.is_unique);
        assert!(!code.nullable);
        assert_eq!(code.comment.as_deref(), Some("Order code"));

        // Numeric default.
        let qty = o_td.columns.iter().find(|c| c.name == "qty").unwrap();
        assert_eq!(qty.default_value.as_deref(), Some("1"));

        // Single PK constraint over id.
        assert!(o_td.constraints.iter().any(|c| matches!(
            c,
            TableConstraint::PrimaryKey { columns, .. } if columns == &vec!["id".to_string()]
        )));

        // Composite FK to auth.memberships with cascade-on-delete.
        let fk = o_td
            .constraints
            .iter()
            .find_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some(fk),
                _ => None,
            })
            .expect("orders should have a FK after round-trip");
        assert_eq!(fk.columns, vec!["user_id".to_string(), "tenant_id".to_string()]);
        assert_eq!(fk.ref_schema.as_deref(), Some("auth"));
        assert_eq!(fk.ref_table, "memberships");
        assert_eq!(fk.ref_columns, vec!["user_id".to_string(), "tenant_id".to_string()]);
        assert_eq!(fk.on_delete, Some(FkAction::Cascade));
        assert_eq!(fk.on_update, None);

        // Unique named index survives.
        let idx = &o_td.indexes[0];
        assert_eq!(idx.name.as_deref(), Some("idx_orders_code"));
        assert!(idx.unique);
        assert_eq!(idx.columns[0].name, "code");

        // Multi-line table note survives.
        assert_eq!(
            o_td.comments.table.as_deref(),
            Some("Customer orders.\nOne row per order.")
        );

        // Both schemas synthesised.
        let schemas: Vec<&str> = parsed
            .iter()
            .filter(|e| e.entity_type == EntityType::Schema)
            .map(|e| e.name.as_str())
            .collect();
        assert!(schemas.contains(&"auth"));
        assert!(schemas.contains(&"shop"));
        assert!(schemas.contains(&"config"));
    }

    // ── Default-value parsing unit tests ─────────────────────────────────────

    /// DBML string defaults must be stored as SQL string literals (with quotes).
    #[test]
    fn parse_default_value_string_literal_keeps_quotes() {
        // `'claude'` in DBML → SQL `'claude'`
        assert_eq!(parse_default_value("'claude'"), "'claude'");
        // Empty string: `''` → SQL `''`
        assert_eq!(parse_default_value("''"), "''");
        // JSON/struct value: `'{}'` → SQL `'{}'`
        assert_eq!(parse_default_value("'{}'"), "'{}'");
    }

    /// DBML backtick expressions must be stored as bare SQL expressions.
    #[test]
    fn parse_default_value_backtick_expr_strips_backticks() {
        assert_eq!(parse_default_value("`now()`"), "now()");
        assert_eq!(parse_default_value("`uuid_generate_v4()`"), "uuid_generate_v4()");
    }

    /// Bare tokens (numbers, booleans, null) must be stored as-is.
    #[test]
    fn parse_default_value_bare_tokens_stored_as_is() {
        assert_eq!(parse_default_value("0"), "0");
        assert_eq!(parse_default_value("true"), "true");
        assert_eq!(parse_default_value("false"), "false");
        assert_eq!(parse_default_value("null"), "null");
        assert_eq!(parse_default_value("42"), "42");
    }

    /// A DBML-escaped apostrophe `'a\'b'` must survive as SQL `'a''b'`.
    #[test]
    fn parse_default_value_dbml_escaped_apostrophe_becomes_sql_doubled() {
        // DBML: `'a\'b'` — the backslash-escaped apostrophe inside a DBML string.
        // parse_single_line_string strips outer quotes and converts `\'` → `'`,
        // giving inner `a'b`, which parse_default_value re-quotes as `'a''b'`.
        assert_eq!(parse_default_value("'a\\'b'"), "'a''b'");
    }

    // ── DBML → DDL validity round-trip ───────────────────────────────────────

    /// Parse a DBML table with various default kinds → emit DDL →
    /// assert the DDL contains valid SQL defaults → re-parse the DDL with
    /// the DDL parser (proving valid SQL).
    #[test]
    fn dbml_string_defaults_produce_valid_ddl() {
        use crate::emit::emit_table;

        let dbml = concat!(
            "Table \"public\".\"t\" {\n",
            "  \"a\" text [default: 'claude']\n",
            "  \"b\" text [default: '']\n",
            "  \"c\" timestamptz [default: `now()`]\n",
            "  \"d\" int [default: 0]\n",
            "}\n"
        );

        let entities = parse_dbml(dbml).expect("DBML must parse cleanly");
        let t = find_table(&entities, "public.t");

        let td = t.table_def.as_ref().unwrap();
        assert_eq!(td.columns[0].default_value.as_deref(), Some("'claude'"), "col a");
        assert_eq!(td.columns[1].default_value.as_deref(), Some("''"), "col b");
        assert_eq!(td.columns[2].default_value.as_deref(), Some("now()"), "col c");
        assert_eq!(td.columns[3].default_value.as_deref(), Some("0"), "col d");

        // Emit DDL and check the SQL text is correct.
        let sql = emit_table(t);
        assert!(sql.contains("DEFAULT 'claude'"), "expected DEFAULT 'claude' in:\n{sql}");
        assert!(sql.contains("DEFAULT ''"), "expected DEFAULT '' in:\n{sql}");
        assert!(sql.contains("DEFAULT now()"), "expected DEFAULT now() in:\n{sql}");
        assert!(sql.contains("DEFAULT 0"), "expected DEFAULT 0 in:\n{sql}");

        // Re-parse the emitted DDL to confirm it is valid SQL.
        let fake_path = std::path::Path::new("ddl/table/public/t.sql");
        crate::parser::parse_entity(fake_path, &sql)
            .unwrap_or_else(|e| panic!("emitted DDL failed to re-parse: {e}\nSQL:\n{sql}"));
    }

    /// Apostrophe-in-string-default survives DBML → entity → DDL as `'a''b'`.
    #[test]
    fn dbml_apostrophe_in_string_default_round_trips_to_valid_ddl() {
        use crate::emit::emit_table;

        // DBML `'a\'b'` is a string containing an apostrophe.
        let dbml = "Table \"public\".\"t\" {\n  \"x\" text [default: 'a\\'b']\n}\n";

        let entities = parse_dbml(dbml).expect("DBML must parse cleanly");
        let t = find_table(&entities, "public.t");
        let td = t.table_def.as_ref().unwrap();

        // Parser must produce the SQL-ready form with a doubled apostrophe.
        assert_eq!(
            td.columns[0].default_value.as_deref(),
            Some("'a''b'"),
            "apostrophe must be SQL-doubled in stored default"
        );

        // Emit and re-parse to confirm valid SQL.
        let sql = emit_table(t);
        assert!(sql.contains("DEFAULT 'a''b'"), "got:\n{sql}");
        let fake_path = std::path::Path::new("ddl/table/public/t.sql");
        crate::parser::parse_entity(fake_path, &sql)
            .unwrap_or_else(|e| panic!("emitted DDL failed to re-parse: {e}\nSQL:\n{sql}"));
    }

    // ── DBML string escapes ──────────────────────────────────────────────────

    /// DBML reads a backslash in a quoted string as an escape, exactly like
    /// its own lexer: `\\` is a backslash, `\n` a newline, `\'` a quote, and
    /// any other `\x` is `x`.
    #[test]
    fn quoted_strings_read_dbml_escapes() {
        assert_eq!(parse_single_line_string(r"'C:\\temp'").as_deref(), Some(r"C:\temp"));
        assert_eq!(parse_single_line_string(r"'one\ntwo'").as_deref(), Some("one\ntwo"));
        assert_eq!(parse_single_line_string(r"'it\'s'").as_deref(), Some("it's"));
        assert_eq!(parse_single_line_string(r"'\d+'").as_deref(), Some("d+"));
    }

    /// Every string dbd writes into DBML — an enum value's note, a column's
    /// note and string default, a table's single- and multi-line note — must
    /// come back with its quotes and backslashes intact. An unescaped `'`
    /// ended the string early (the enum note vanished, the `'it''s'` default
    /// came back as `'it''''s'`), and a `'''` inside a multi-line note ended
    /// the note.
    #[test]
    fn quotes_and_backslashes_survive_the_round_trip() {
        use crate::dbml::{DbmlParams, generate_dbml};
        use crate::entity::{EnumValue, TableComments};

        let mut kind = Entity::new(EntityType::Enum, "app.kind");
        kind.enum_values = vec![EnumValue {
            name: "plain".into(),
            note: Some(r"the user's C:\ drive".into()),
        }];

        let column = ColumnDef {
            name: "path".into(),
            data_type: "text".into(),
            nullable: true,
            default_value: Some("'it''s'".into()),
            is_pk: false,
            is_unique: false,
            identity: None,
            generated: None,
            comment: Some(r"Windows path, e.g. C:\temp — the user's".into()),
            inline_fk: None,
            collation: None,
        };
        let table_with_note = |name: &str, note: &str| {
            let mut t = Entity::new(EntityType::Table, name);
            t.table_def = Some(TableDef {
                columns: vec![column.clone()],
                constraints: vec![],
                indexes: vec![],
                comments: TableComments {
                    table: Some(note.into()),
                    ..Default::default()
                },
            });
            t
        };
        let single_note = r"Matches \d+ in the user's codes";
        let multi_note = "First line.\nA ''' run, a C:\\temp path, the user's note.";

        let doc = generate_dbml(&DbmlParams {
            entities: &[
                kind,
                table_with_note("app.single", single_note),
                table_with_note("app.multi", multi_note),
            ],
            project_name: "Escapes",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });
        let dbml = &doc.content;
        let parsed = parse_dbml(dbml).unwrap_or_else(|e| panic!("{e}\n{dbml}"));

        assert_eq!(
            find_enum(&parsed, "app.kind").enum_values[0].note.as_deref(),
            Some(r"the user's C:\ drive"),
            "\n{dbml}"
        );
        for (name, note) in [("app.single", single_note), ("app.multi", multi_note)] {
            let td = find_table(&parsed, name).table_def.as_ref().unwrap();
            assert_eq!(
                td.columns[0].comment.as_deref(),
                Some(r"Windows path, e.g. C:\temp — the user's"),
                "{name}:\n{dbml}"
            );
            assert_eq!(
                td.columns[0].default_value.as_deref(),
                Some("'it''s'"),
                "{name}:\n{dbml}"
            );
            assert_eq!(td.comments.table.as_deref(), Some(note), "{name}:\n{dbml}");
        }
    }

    // ── Hand-written DBML, read as @dbml/core reads it ───────────────────────
    //
    // Every document below is valid DBML: @dbml/core 10.3.0 parses each, and
    // the expected values are what it reads — but for the line a closing
    // `'''` sits on, which is layout here and a trailing line break there.

    /// The foreign keys of `table`, as `(columns, ref_schema.ref_table,
    /// ref_columns, name, on_delete)`.
    #[allow(clippy::type_complexity)]
    fn fks(
        entities: &[Entity],
        table: &str,
    ) -> Vec<(Vec<String>, String, Vec<String>, Option<String>, Option<FkAction>)> {
        let td = find_table(entities, table).table_def.as_ref().unwrap();
        td.constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some((
                    fk.columns.clone(),
                    format!("{}.{}", fk.ref_schema.as_deref().unwrap_or("public"), fk.ref_table),
                    fk.ref_columns.clone(),
                    fk.name.clone(),
                    fk.on_delete,
                )),
                _ => None,
            })
            .collect()
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `Table app.users as U` was read as a table named `users as U`, and a
    /// Ref to `U.id` as one to a table `public.U`.
    #[test]
    fn a_table_alias_names_the_table_it_stands_for() {
        let dbml = "Table app.users as U {\n  id int [pk]\n}\nTable app.posts {\n  id int [pk]\n  author_id int [ref: > U.id]\n  editor_id int\n}\nRef: app.posts.editor_id > U.id [delete: cascade]\n";
        let entities = parse_dbml(dbml).unwrap();
        assert!(
            entities
                .iter()
                .any(|e| e.entity_type == EntityType::Table && e.name == "app.users")
        );
        assert!(!entities.iter().any(|e| e.name.contains(" as ")), "{entities:#?}");
        let mut found = fks(&entities, "app.posts");
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            found,
            vec![
                (
                    strings(&["author_id"]),
                    "app.users".into(),
                    strings(&["id"]),
                    None,
                    None
                ),
                (
                    strings(&["editor_id"]),
                    "app.users".into(),
                    strings(&["id"]),
                    None,
                    Some(FkAction::Cascade)
                ),
            ]
        );
    }

    /// An inline `[ref: …]` was ignored as an unknown setting, so the key was
    /// silently missing. `>` and `-` both put the key on the column; a
    /// one-to-one `Ref: a - b` puts it on `b`, as @dbml/core's SQL does.
    #[test]
    fn an_inline_ref_is_a_foreign_key_and_one_to_one_keys_the_right_side() {
        let dbml = "Table app.users {\n  id int [pk]\n}\nTable app.posts {\n  id int [pk]\n  user_id int [ref: > app.users.id, not null]\n  buddy int [ref: - app.users.id]\n  solo int\n}\nRef: app.users.id - app.posts.solo\n";
        let entities = parse_dbml(dbml).unwrap();
        let mut found = fks(&entities, "app.posts");
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let key = |col: &str| (strings(&[col]), "app.users".to_string(), strings(&["id"]), None, None);
        assert_eq!(found, vec![key("buddy"), key("solo"), key("user_id")]);
        assert!(fks(&entities, "app.users").is_empty(), "nothing keys the users table");
        let td = find_table(&entities, "app.posts").table_def.as_ref().unwrap();
        assert!(!td.columns[1].nullable, "the other settings still apply");
    }

    /// A many-to-many Ref has no foreign key to build — @dbml/core makes a
    /// junction table of it. Reading it as a key from one side to the other
    /// built a constraint the document never described.
    #[test]
    fn a_many_to_many_ref_is_refused_with_why() {
        let dbml = "Table app.a {\n  id int [pk]\n}\nTable app.b {\n  id int [pk]\n}\nRef: app.a.id <> app.b.id\n";
        let err = parse_dbml(dbml).unwrap_err().to_string();
        assert!(err.contains("<>") && err.contains("junction"), "{err}");
    }

    /// A multi-line note in a column's settings broke the column into
    /// several, a table's `Note { … }` block failed to parse, and an indented
    /// note kept its indentation. Each reads as @dbml/core reads it: its
    /// common indentation removed, settings may span lines.
    #[test]
    fn multi_line_notes_and_settings_read_as_dbml_reads_them() {
        let dbml = "Table app.t {\n  id int [pk]\n  email text [note: '''\n    The address we write to.\n    Never shared.\n  ''']\n  code text [\n    not null,\n    note: 'x'\n  ]\n  Note {\n    '''\n    Blog posts.\n      Indented line.\n    '''\n  }\n}\nEnum app.mood {\n  happy [note: '''\n    very\n    happy\n  ''']\n  sad\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        let names: Vec<&str> = td.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "email", "code"]);
        assert_eq!(
            td.columns[1].comment.as_deref(),
            Some("The address we write to.\nNever shared.")
        );
        assert_eq!(td.columns[2].comment.as_deref(), Some("x"));
        assert!(!td.columns[2].nullable);
        assert_eq!(td.comments.table.as_deref(), Some("Blog posts.\n  Indented line."));
        let mood = find_enum(&entities, "app.mood");
        assert_eq!(mood.enum_values[0].note.as_deref(), Some("very\nhappy"));
        assert_eq!(mood.enum_values.len(), 2);
    }

    /// An index entry marked `pk` is the table's primary key — DBML's way of
    /// writing a composite one, and the only way to name one. It was read as
    /// a plain index, and the table had no key.
    #[test]
    fn a_pk_index_is_the_primary_key() {
        let dbml = "Table app.t {\n  a int\n  b int\n  indexes {\n    (a, b) [pk, name: 't_pk']\n  }\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert!(td.indexes.is_empty(), "{:?}", td.indexes);
        assert_eq!(
            td.constraints,
            vec![TableConstraint::PrimaryKey {
                name: Some("t_pk".into()),
                columns: strings(&["a", "b"]),
            }]
        );
        assert!(td.columns.iter().all(|c| c.is_pk && !c.nullable));
    }

    /// A Ref's name — `Ref name:` or the long form `Ref name { … }` — is the
    /// foreign key's name, and an index's `type:` its access method. A table
    /// may sit on one line.
    #[test]
    fn ref_names_index_types_and_one_line_tables_are_read() {
        let dbml = "Table app.a { id int [pk] }\nTable app.b {\n  id int [pk]\n  a_id int\n  c_id int\n  data jsonb\n  indexes {\n    data [type: gin, name: 'b_data_idx']\n  }\n}\nRef b_a_fk: app.b.a_id > app.a.id [delete: cascade]\nRef \"b c fk\" {\n  app.b.c_id > app.a.id\n}\n";
        let entities = parse_dbml(dbml).unwrap();
        let a = find_table(&entities, "app.a").table_def.as_ref().unwrap();
        assert_eq!(a.columns.len(), 1);
        let mut found = fks(&entities, "app.b");
        found.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(
            found,
            vec![
                (
                    strings(&["a_id"]),
                    "app.a".into(),
                    strings(&["id"]),
                    Some("b_a_fk".into()),
                    Some(FkAction::Cascade)
                ),
                (
                    strings(&["c_id"]),
                    "app.a".into(),
                    strings(&["id"]),
                    Some("b c fk".into()),
                    None
                ),
            ]
        );
        let b = find_table(&entities, "app.b").table_def.as_ref().unwrap();
        assert_eq!(b.indexes[0].index_type, Some(crate::entity::IndexType::Gin));
    }

    /// A quoted name reads its backslash escapes as @dbml/core reads them:
    /// `"we\"ird"` is the column `we"ird`, and `"it\'s"` the value `it's`.
    #[test]
    fn a_quoted_name_reads_its_escapes() {
        let entities = parse_dbml(
            "Enum \"app\".\"mood\" {\n  \"it\\'s\"\n  \"say \\\"hi\\\"\"\n}\n\
             Table \"app\".\"t\" {\n  \"we\\\"ird\" int\n  \"back\\\\slash\" int\n}\n",
        )
        .unwrap();
        let mood = find_enum(&entities, "app.mood");
        let values: Vec<&str> = mood.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(values, ["it's", "say \"hi\""]);
        let t = find_table(&entities, "app.t");
        let columns: Vec<&str> = t
            .table_def
            .as_ref()
            .unwrap()
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(columns, ["we\"ird", "back\\slash"]);
    }

    /// A `"` inside a `'…'` setting is text, as a `'` inside a `"…"` one is.
    /// The settings splitter kept one flag for both, so an odd number of `"`
    /// in a single-quoted value ended its settings: the `pk` entry
    /// `id [pk, name: 'we"ird_pk']` became an index on a column named
    /// `id [pk, name: 'we"ird_pk']`, and the table lost its primary key.
    #[test]
    fn a_quote_inside_a_setting_string_is_text_not_a_delimiter() {
        let dbml = "Enum app.mood {\n  happy [note: 'say \"hi']\n}\n\
                    Table app.t [note: 'a \"table'] {\n  \
                      id int [not null]\n  \
                      label text [note: 'odd \" quote', default: 'x\"y']\n  \
                      other text [note: \"it's\"]\n  \
                      checks {\n    `label <> ''` [name: 'c\"k']\n  }\n  \
                      indexes {\n    id [pk, name: 'we\"ird_pk']\n    label [name: 'l\"x']\n  }\n\
                    }\n";
        let entities = parse_dbml(dbml).unwrap();
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        assert_eq!(
            td.constraints,
            vec![
                TableConstraint::PrimaryKey {
                    name: Some("we\"ird_pk".into()),
                    columns: strings(&["id"]),
                },
                TableConstraint::Check {
                    name: Some("c\"k".into()),
                    expression: "label <> ''".into(),
                },
            ]
        );
        let indexes: Vec<(Option<&str>, Vec<&str>)> = td
            .indexes
            .iter()
            .map(|ix| (ix.name.as_deref(), ix.columns.iter().map(|k| k.name.as_str()).collect()))
            .collect();
        assert_eq!(indexes, vec![(Some("l\"x"), vec!["label"])]);
        assert_eq!(td.columns[1].comment.as_deref(), Some("odd \" quote"));
        assert_eq!(td.columns[1].default_value.as_deref(), Some("'x\"y'"));
        assert_eq!(td.columns[2].comment.as_deref(), Some("it's"));
        assert_eq!(td.comments.table.as_deref(), Some("a \"table"));
        let mood = find_enum(&entities, "app.mood");
        assert_eq!(mood.enum_values[0].note.as_deref(), Some("say \"hi"));
    }

    /// A Ref endpoint, an index key and a Ref's name read their escapes as
    /// a quoted name anywhere else does. The relationship operator was looked
    /// for with every `"` toggling a quote, escaped or not, so
    /// `"app"."c\"y"."w" > …` had no operator outside quotes and failed; an
    /// index tuple with an escaped quote was refused as malformed.
    #[test]
    fn ref_endpoints_and_index_keys_read_escaped_quotes() {
        let dbml = "Table \"app\".\"we\\\"ird\" {\n  \"id\" int [pk]\n  \"a,b\" int\n  \"c\\\"d\" int\n  \
                      indexes {\n    (\"a,b\", \"c\\\"d\") [unique, name: 'u']\n  }\n}\n\
                    Table \"app\".\"c\\\"y\" {\n  \"id\" int [pk]\n  \"w\" int\n  \"x,y\" int\n  \"z\\\"q\" int\n}\n\
                    Ref \"f\\\"k\": \"app\".\"c\\\"y\".\"w\" > \"app\".\"we\\\"ird\".\"id\"\n\
                    Ref: \"app\".\"c\\\"y\".(\"x,y\", \"z\\\"q\") > \"app\".\"we\\\"ird\".(\"a,b\", \"c\\\"d\")\n";
        let entities = parse_dbml(dbml).unwrap_or_else(|e| panic!("{e}"));
        let mut found = fks(&entities, "app.c\"y");
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            found,
            vec![
                (
                    strings(&["w"]),
                    "app.we\"ird".into(),
                    strings(&["id"]),
                    Some("f\"k".into()),
                    None
                ),
                (
                    strings(&["x,y", "z\"q"]),
                    "app.we\"ird".into(),
                    strings(&["a,b", "c\"d"]),
                    None,
                    None
                ),
            ]
        );
        let td = find_table(&entities, "app.we\"ird").table_def.as_ref().unwrap();
        let keys: Vec<&str> = td.indexes[0].columns.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(keys, ["a,b", "c\"d"]);
    }

    /// A column typed by an enum the document declares is written in SQL,
    /// whatever the enum's name holds. `"app"."mo\"od"` — DBML's spelling of
    /// a schema-qualified enum, which @dbml/core reads as the enum `mo"od` in
    /// `app` — came back as `app."mo\"od"`, which is not SQL, and the table
    /// file `init --from-dbml` wrote did not parse.
    #[test]
    fn a_column_typed_by_a_declared_enum_is_written_in_sql() {
        let dbml = "Enum \"app\".\"mo\\\"od\" {\n  calm\n}\nEnum \"Mood\" {\n  calm\n}\n\
                    Table app.t {\n  \
                      id int [pk]\n  \
                      a \"app\".\"mo\\\"od\"\n  \
                      b \"app\".\"mo\\\"od\"[]\n  \
                      c \"Mood\"\n  \
                      d \"timestamp with time zone\"\n  \
                      e app.thing\n  \
                      f \"app.\\\"my dom\\\"\"\n}\n";
        let entities = parse_dbml(dbml).unwrap_or_else(|e| panic!("{e}"));
        let td = find_table(&entities, "app.t").table_def.as_ref().unwrap();
        let types: Vec<&str> = td.columns.iter().map(|c| c.data_type.as_str()).collect();
        assert_eq!(
            types,
            [
                "int",
                "app.\"mo\"\"od\"",
                "app.\"mo\"\"od\"[]",
                "\"Mood\"",
                "timestamp with time zone",
                "app.thing",
                "app.\"my dom\"",
            ]
        );
    }
}
