//! Hand-rolled DBML *parser* — the inverse of the DBML exporter in [`crate::dbml`].
//!
//! [`parse_dbml`] reads a `.dbml` document and produces a `Vec<Entity>` that the
//! reverse engine can feed straight into its emit → write-plan → snapshot path.
//! The supported subset is exactly what `dbd dbml` emits plus standard
//! dbdiagram.io DBML for that subset:
//!
//! - `Project "name" { … }`           — parsed and skipped (metadata only).
//! - `Enum "schema"."name" { … }`      — → [`EntityType::Enum`] with `enum_values`.
//! - `Table "schema"."name" { … }`     — → [`EntityType::Table`] with a [`TableDef`].
//! - `Ref: <src> > <tgt> [settings]`   — → a [`ForeignKey`] appended to the source table.
//! - `TableGroup … { … }`, stray `Note`, `//` comments — skipped cleanly.
//!
//! A [`EntityType::Schema`] entity is synthesised for every distinct schema seen
//! (including `public`) so the generated project is applyable.
//!
//! The scanner is line-based: the document is split into logical lines, block
//! boundaries (`{` / `}`) are tracked by brace depth, and `[settings]` and
//! `'''`-quoted notes are handled with small character-level helpers. Malformed
//! input returns a [`DbdError::Parse`]; genuinely unknown blocks are skipped
//! leniently (dbdiagram.io grows constructs over time).

use crate::entity::{
    ColumnDef, Entity, EntityType, FkAction, ForeignKey, IndexColumn, IndexDef, TableConstraint, TableDef,
};
use crate::error::{DbdError, Result};

/// Parse a DBML document into entities (schemas, enums, tables, and the FK
/// constraints attached to their source tables).
pub fn parse_dbml(text: &str) -> Result<Vec<Entity>> {
    let mut p = Parser::new(text);
    p.parse()
}

fn parse_err(message: impl Into<String>) -> DbdError {
    DbdError::Parse {
        file: std::path::PathBuf::from("<dbml>"),
        message: message.into(),
    }
}

/// A pending standalone `Ref:` collected during the scan and resolved into a
/// [`ForeignKey`] on the source table after all tables are parsed.
struct PendingRef {
    src_schema: String,
    src_table: String,
    src_columns: Vec<String>,
    tgt_schema: String,
    tgt_table: String,
    tgt_columns: Vec<String>,
    on_delete: Option<FkAction>,
    on_update: Option<FkAction>,
}

struct Parser<'a> {
    /// Logical lines (comments stripped, blanks kept for note bodies but skipped
    /// at the top level).
    lines: Vec<&'a str>,
    pos: usize,
    tables: Vec<Entity>,
    enums: Vec<Entity>,
    refs: Vec<PendingRef>,
    /// Distinct schemas seen, in first-seen order.
    schemas: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines().collect(),
            pos: 0,
            tables: Vec::new(),
            enums: Vec::new(),
            refs: Vec::new(),
            schemas: Vec::new(),
        }
    }

    fn note_schema(&mut self, schema: &str) {
        if !self.schemas.iter().any(|s| s == schema) {
            self.schemas.push(schema.to_string());
        }
    }

    fn parse(&mut self) -> Result<Vec<Entity>> {
        while self.pos < self.lines.len() {
            let raw = self.lines[self.pos];
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                self.pos += 1;
                continue;
            }

            // Keyword dispatch on the first token (case-insensitive for the
            // block keywords dbd/dbdiagram.io use). Strip a trailing colon so
            // `Ref:` / `Note:` match `ref` / `note`.
            let kw = keyword(line);
            match kw.as_str() {
                "project" => self.skip_block()?,
                "enum" => self.parse_enum()?,
                "table" => self.parse_table()?,
                "tablegroup" => self.skip_block()?,
                "ref" => self.parse_standalone_ref(line)?,
                "note" => {
                    // Stray top-level Note — may be a single line or a `'''`
                    // block. Consume its body and discard.
                    self.consume_note_value(line)?;
                }
                _ => {
                    // Unknown construct. If it opens a block, skip the whole
                    // block; otherwise skip the single line. Lenient by design.
                    if line.contains('{') {
                        self.skip_block()?;
                    } else {
                        self.pos += 1;
                    }
                }
            }
        }

        // Resolve standalone Refs onto their source tables.
        self.attach_refs()?;

        // Assemble: schemas (synthesised) → enums → tables.
        let mut out = Vec::new();
        for schema in &self.schemas {
            out.push(Entity::new(EntityType::Schema, schema));
        }
        out.append(&mut self.enums);
        out.append(&mut self.tables);
        Ok(out)
    }

    // ── block skipping ────────────────────────────────────────────────────

    /// Skip a `{ … }` block starting at the current line, balancing braces.
    /// Used for `Project`, `TableGroup`, and unknown blocks.
    fn skip_block(&mut self) -> Result<()> {
        let start = self.pos;
        // Advance until we find the opening brace (may be on a later line).
        let mut depth = 0usize;
        let mut saw_open = false;
        while self.pos < self.lines.len() {
            let line = strip_comment(self.lines[self.pos]);
            depth += line.matches('{').count();
            if line.contains('{') {
                saw_open = true;
            }
            depth = depth.saturating_sub(line.matches('}').count());
            self.pos += 1;
            if saw_open && depth == 0 {
                return Ok(());
            }
        }
        if saw_open {
            return Err(parse_err(format!("unterminated block starting at line {}", start + 1)));
        }
        // No brace at all — a single keyword line; we already advanced past it.
        Ok(())
    }

    // ── enum ───────────────────────────────────────────────────────────────

    fn parse_enum(&mut self) -> Result<()> {
        let header = strip_comment(self.lines[self.pos]).trim().to_string();
        let after_kw = header.get(first_word(&header).len()..).unwrap_or("").trim();
        // Everything up to the `{` is the name.
        let name_part = after_kw.split('{').next().unwrap_or("").trim();
        let (schema, base) = parse_qualified_name(name_part)?;
        self.note_schema(&schema);
        let qualified = format!("{schema}.{base}");
        let mut entity = Entity::new(EntityType::Enum, &qualified);
        entity.schema = Some(schema);

        // Consume value lines until the closing `}`.
        // The opening `{` may be on the header line.
        self.pos += 1;
        if !header.contains('{') {
            return Err(parse_err(format!("enum `{qualified}` is missing `{{`")));
        }
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err(format!("unterminated enum `{qualified}`")));
            }
            let line = strip_comment(self.lines[self.pos]).trim().to_string();
            self.pos += 1;
            if line.is_empty() {
                continue;
            }
            if line.starts_with('}') {
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
        let header = strip_comment(self.lines[self.pos]).trim().to_string();
        let (schema, base) = self.parse_table_header(&header)?;
        let qualified = format!("{schema}.{base}");

        if !header.contains('{') {
            return Err(parse_err(format!("table `{qualified}` is missing `{{`")));
        }

        let mut columns: Vec<ColumnDef> = Vec::new();
        let mut pk_cols: Vec<String> = Vec::new();
        let mut indexes: Vec<IndexDef> = Vec::new();
        let mut checks: Vec<TableConstraint> = Vec::new();
        let mut table_note: Option<String> = None;

        self.pos += 1;
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err(format!("unterminated table `{qualified}`")));
            }
            let raw = self.lines[self.pos];
            let line = strip_comment(raw).trim().to_string();
            if line.is_empty() {
                self.pos += 1;
                continue;
            }
            if line.starts_with('}') {
                self.pos += 1;
                break;
            }

            let kw = keyword(&line);
            match kw.as_str() {
                "indexes" => {
                    for entry in self.block_entries(&line, "indexes", &qualified)? {
                        indexes.push(parse_index_line(&entry, &qualified)?);
                    }
                }
                // Only as a block opener: an unquoted column may well be named
                // `checks`, and `checks int` is a column.
                "checks" if opens_block(&line) => {
                    for entry in self.block_entries(&line, "checks", &qualified)? {
                        checks.push(parse_check_line(&entry, &qualified)?);
                    }
                }
                "note" => {
                    table_note = Some(self.consume_note_value(&line)?);
                }
                _ => {
                    // A column definition, with any `check:` settings it carries.
                    let (col, col_checks) = parse_column(&line, &qualified)?;
                    if col.is_pk {
                        pk_cols.push(col.name.clone());
                    }
                    checks.extend(
                        col_checks
                            .into_iter()
                            .map(|expression| TableConstraint::Check { name: None, expression }),
                    );
                    columns.push(col);
                    self.pos += 1;
                }
            }
        }

        let mut constraints = Vec::new();
        if !pk_cols.is_empty() {
            constraints.push(TableConstraint::PrimaryKey {
                name: None,
                columns: pk_cols,
            });
        }
        constraints.append(&mut checks);

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

    /// Parse a table header line into `(schema, base)`, stripping the leading
    /// keyword, an inline `{`, and any trailing `[settings]`; notes the schema.
    fn parse_table_header(&mut self, header: &str) -> Result<(String, String)> {
        let after_kw = header.get(first_word(header).len()..).unwrap_or("").trim();
        let name_part = after_kw.split('{').next().unwrap_or("").trim();
        // Tables may carry their own `[settings]` (e.g. `[headercolor: …]`);
        // strip a trailing settings group from the name part.
        let name_only = name_part.split('[').next().unwrap_or(name_part).trim();
        let (schema, base) = parse_qualified_name(name_only)?;
        self.note_schema(&schema);
        Ok((schema, base))
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
    /// `checks`. `header` is the line beginning with `kind`; the `{` may be on
    /// that line or a following one.
    fn block_entries(&mut self, header: &str, kind: &str, table: &str) -> Result<Vec<String>> {
        self.advance_past_open_brace(header, kind, table)?;

        let mut entries = Vec::new();
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err(format!("unterminated {kind} block in `{table}`")));
            }
            let line = strip_comment(self.lines[self.pos]).trim().to_string();
            self.pos += 1;
            if line.is_empty() {
                continue;
            }
            if line.starts_with('}') {
                break;
            }
            entries.push(line);
        }
        Ok(entries)
    }

    /// Advance the cursor just past the `{` that opens a `kind` block —
    /// whether the brace is on the header line or a following line.
    fn advance_past_open_brace(&mut self, header: &str, kind: &str, table: &str) -> Result<()> {
        if header.contains('{') {
            self.pos += 1;
            return Ok(());
        }
        // Brace on a following line: advance to it.
        self.pos += 1;
        while self.pos < self.lines.len() {
            let l = strip_comment(self.lines[self.pos]).trim().to_string();
            self.pos += 1;
            if l.is_empty() {
                continue;
            }
            if l.contains('{') {
                return Ok(());
            }
            return Err(parse_err(format!("{kind} block in `{table}` is missing `{{`")));
        }
        Ok(())
    }

    // ── standalone Ref ───────────────────────────────────────────────────────

    fn parse_standalone_ref(&mut self, line: &str) -> Result<()> {
        // The line begins with `Ref` (possibly `Ref name:` or `Ref:`). dbd emits
        // `Ref: <src> <op> <tgt> [settings]`. A `Ref … { … }` group form opens a
        // block — handle that by skipping leniently (dbd never emits it).
        if line.contains('{') {
            self.skip_block()?;
            return Ok(());
        }
        // Strip everything up to and including the first ':'.
        let body = match line.split_once(':') {
            Some((_, rest)) => rest.trim(),
            None => {
                return Err(parse_err(format!("malformed Ref (no `:`): {line}")));
            }
        };
        let pending = parse_ref_body(body)?;
        if let Some(p) = pending {
            self.refs.push(PendingRef {
                src_schema: p.src_schema.clone(),
                src_table: p.src_table.clone(),
                src_columns: p.src_columns.clone(),
                tgt_schema: p.tgt_schema.clone(),
                tgt_table: p.tgt_table.clone(),
                tgt_columns: p.tgt_columns.clone(),
                on_delete: p.on_delete,
                on_update: p.on_update,
            });
            self.note_schema(&p.src_schema);
            self.note_schema(&p.tgt_schema);
        }
        self.pos += 1;
        Ok(())
    }

    /// Resolve each collected Ref onto its source table as a `ForeignKey`
    /// constraint. A Ref whose source table is absent is skipped (lenient).
    fn attach_refs(&mut self) -> Result<()> {
        let refs = std::mem::take(&mut self.refs);
        for r in refs {
            let src_qualified = format!("{}.{}", r.src_schema, r.src_table);
            let fk = ForeignKey {
                name: None,
                columns: r.src_columns,
                ref_schema: Some(r.tgt_schema),
                ref_table: r.tgt_table,
                ref_columns: r.tgt_columns,
                on_delete: r.on_delete,
                on_update: r.on_update,
                // A DBML `Ref` always names both sides' schemas, so nothing
                // here was inferred.
                ref_schema_source: crate::entity::SchemaSource::Stated,
            };
            if let Some(entity) = self.tables.iter_mut().find(|e| e.name == src_qualified)
                && let Some(td) = entity.table_def.as_mut()
            {
                td.constraints.push(TableConstraint::ForeignKey(fk));
            }
            // else: source table not found — skip leniently.
        }
        Ok(())
    }

    // ── note value ───────────────────────────────────────────────────────────

    /// Consume a `Note:` value starting at the current line and return its text.
    /// Handles single-line `'…'`, `"…"`, and triple-quoted `'''…'''` (possibly
    /// spanning multiple lines). Advances `self.pos` past the consumed lines.
    fn consume_note_value(&mut self, first_line: &str) -> Result<String> {
        // Take the part after the first ':'.
        let after = match first_line.split_once(':') {
            Some((_, rest)) => rest.trim_start(),
            None => "",
        };

        if let Some(rest) = after.strip_prefix("'''") {
            // Triple-quoted, possibly multi-line.
            return self.consume_triple_quoted(rest);
        }

        // Single-line quoted value on this line.
        self.pos += 1;
        let value = parse_single_line_string(after.trim())
            .ok_or_else(|| parse_err(format!("malformed Note value: {first_line}")))?;
        Ok(value)
    }

    /// `rest` is the text following the opening `'''` on the first line.
    fn consume_triple_quoted(&mut self, rest: &str) -> Result<String> {
        let mut body = String::new();
        // Closing `'''` may be on the same line.
        if let Some(end) = closing_triple_quote(rest) {
            self.pos += 1;
            return Ok(unescape_dbml(&rest[..end]));
        }
        if !rest.is_empty() {
            body.push_str(rest);
            body.push('\n');
        }
        self.pos += 1;
        loop {
            if self.pos >= self.lines.len() {
                return Err(parse_err("unterminated triple-quoted note"));
            }
            let raw = self.lines[self.pos];
            self.pos += 1;
            if let Some(end) = closing_triple_quote(raw) {
                body.push_str(&raw[..end]);
                break;
            }
            body.push_str(raw);
            body.push('\n');
        }
        // dbd emits `'''\n<text>\n'''`, so the first body line is empty and the
        // last has a trailing newline — trim one leading and one trailing
        // newline to recover the original text. Trimmed before unescaping, so
        // an escaped `\n` at either end is text, not layout.
        let trimmed = body.strip_prefix('\n').unwrap_or(&body);
        let trimmed = trimmed.strip_suffix('\n').unwrap_or(trimmed);
        Ok(unescape_dbml(trimmed))
    }
}

// ── free functions ──────────────────────────────────────────────────────────

/// Strip a `//` line comment (outside of quotes) from a raw line.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
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
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '/' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// The first whitespace-delimited word of a line.
fn first_word(line: &str) -> &str {
    line.split_whitespace().next().unwrap_or("")
}

/// The lowercased leading keyword of a line, with a trailing `:` stripped so
/// `Ref:` → `ref` and `Note:` → `note`.
fn keyword(line: &str) -> String {
    first_word(line).trim_end_matches(':').to_ascii_lowercase()
}

/// Parse a (possibly schema-qualified) name into `(schema, base)`.
/// Accepts `"s"."n"`, `s.n`, `"n"`, `n`. Unqualified names default to `public`.
fn parse_qualified_name(input: &str) -> Result<(String, String)> {
    let input = input.trim();
    if input.is_empty() {
        return Err(parse_err("empty name"));
    }
    // Split on the top-level '.' that is not inside quotes.
    let parts = split_dotted(input);
    match parts.as_slice() {
        [single] => Ok(("public".to_string(), unquote(single))),
        [schema, base] => Ok((unquote(schema), unquote(base))),
        _ => Err(parse_err(format!("unexpected qualified name: {input}"))),
    }
}

/// Split on `.` separators that are outside of double quotes.
fn split_dotted(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in input.chars() {
        match c {
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
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Take a leading identifier (quoted `"x"` or bare) off the front of `s`,
/// returning `(identifier, remainder)`.
fn take_identifier(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    if let Some(rest) = s.strip_prefix('"') {
        match rest.find('"') {
            Some(end) => Ok((rest[..end].to_string(), &rest[end + 1..])),
            None => Err(parse_err(format!("unterminated quoted identifier: {s}"))),
        }
    } else {
        // Bare identifier: up to whitespace or `[`.
        let end = s.find(|c: char| c.is_whitespace() || c == '[').unwrap_or(s.len());
        if end == 0 {
            return Err(parse_err(format!("expected identifier: {s}")));
        }
        Ok((s[..end].to_string(), &s[end..]))
    }
}

/// Parse an optional `[note: '…']` setting following an enum value, returning
/// the note text if present.
fn parse_value_note(rest: &str) -> Option<String> {
    let (_, inner) = split_trailing_settings(rest.trim());
    let settings = extract_settings(inner?)?;
    for (key, value) in settings {
        if key.eq_ignore_ascii_case("note")
            && let Some(v) = value
            && let Some(s) = parse_single_line_string(&v)
        {
            return Some(s);
        }
    }
    None
}

/// Parse a column definition line: `"name" <type> [settings]`, returning the
/// column and the expressions of any `check:` settings on it.
fn parse_column(line: &str, table: &str) -> Result<(ColumnDef, Vec<String>)> {
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
    };
    let mut checks = Vec::new();

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
                "increment" if !crate::emit::is_serial_type(&col.data_type) => {
                    col.identity = Some(crate::entity::IdentityKind::ByDefault);
                    col.nullable = false;
                }
                "increment" => {}
                "default" => {
                    if let Some(v) = value {
                        col.default_value = Some(parse_default_value(&v));
                    }
                }
                "note" => {
                    if let Some(v) = value
                        && let Some(s) = parse_single_line_string(&v)
                    {
                        col.comment = Some(s);
                    }
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
                _ => { /* unknown setting — ignore (forward-compatible) */ }
            }
        }
    }

    Ok((col, checks))
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

/// Parse an index line: `(<cols>) [settings]` or `<col> [settings]`.
fn parse_index_line(line: &str, table: &str) -> Result<IndexDef> {
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
    let mut name = None;
    let mut note = None;
    if let Some(settings_str) = settings_part {
        let settings = extract_settings(settings_str)
            .ok_or_else(|| parse_err(format!("malformed index settings in `{table}`: {settings_str}")))?;
        for (key, value) in settings {
            let k = key.to_ascii_lowercase();
            match k.as_str() {
                "unique" => unique = true,
                "name" => {
                    if let Some(v) = value {
                        name = parse_single_line_string(&v);
                    }
                }
                "note" => note = value.as_deref().and_then(parse_single_line_string),
                _ => { /* pk / type on indexes — ignore */ }
            }
        }
    }

    let mut index = IndexDef {
        name,
        columns: keys,
        unique,
        ..Default::default()
    };
    // The partial `WHERE`, key order and `NULLS NOT DISTINCT` have no DBML
    // syntax; dbd carries them in the note (see `crate::dbml::index_note`).
    // An operator class, `INCLUDE` and storage parameters are not carried and
    // stay at their defaults.
    if let Some(note) = note {
        crate::dbml::index_note::read(&note, &mut index);
    }
    Ok(index)
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

/// Parse the body of a standalone `Ref:` (everything after the `:`).
/// `<src> <op> <tgt> [settings]`. Returns `None` only if the body is empty.
fn parse_ref_body(body: &str) -> Result<Option<ParsedRef>> {
    let body = body.trim();
    if body.is_empty() {
        return Ok(None);
    }
    // Strip trailing `[settings]`.
    let (refs_part, settings_part) = split_trailing_settings(body);
    let refs_part = refs_part.trim();

    // Find the relationship operator. dbd emits `>`; accept `<`, `-`, `<>`.
    let (left, op, right) =
        split_ref_operator(refs_part).ok_or_else(|| parse_err(format!("Ref missing relationship operator: {body}")))?;

    let (mut l_schema, mut l_table, mut l_cols) = parse_ref_endpoint(left)?;
    let (mut r_schema, mut r_table, mut r_cols) = parse_ref_endpoint(right)?;

    // dbd emits many-to-one `child > parent` (source > target). For `<` the
    // source is on the right, so swap. `-`/`<>` are non-directional: treat the
    // left as the source (matches dbd's child-side convention) with no actions.
    if op == "<" {
        std::mem::swap(&mut l_schema, &mut r_schema);
        std::mem::swap(&mut l_table, &mut r_table);
        std::mem::swap(&mut l_cols, &mut r_cols);
    }

    let mut on_delete = None;
    let mut on_update = None;
    // Only `>`/`<` carry FK semantics; `-`/`<>` have no action.
    if (op == ">" || op == "<")
        && let Some(settings_str) = settings_part
    {
        let settings = extract_settings(settings_str)
            .ok_or_else(|| parse_err(format!("malformed Ref settings: {settings_str}")))?;
        for (key, value) in settings {
            let k = key.to_ascii_lowercase();
            match k.as_str() {
                "delete" => on_delete = value.as_deref().and_then(FkAction::from_dbml),
                "update" => on_update = value.as_deref().and_then(FkAction::from_dbml),
                _ => {}
            }
        }
    }

    Ok(Some(ParsedRef {
        src_schema: l_schema,
        src_table: l_table,
        src_columns: l_cols,
        tgt_schema: r_schema,
        tgt_table: r_table,
        tgt_columns: r_cols,
        on_delete,
        on_update,
    }))
}

struct ParsedRef {
    src_schema: String,
    src_table: String,
    src_columns: Vec<String>,
    tgt_schema: String,
    tgt_table: String,
    tgt_columns: Vec<String>,
    on_delete: Option<FkAction>,
    on_update: Option<FkAction>,
}

/// Locate the relationship operator (`<>`, `>`, `<`, `-`) outside of quotes and
/// parentheses, returning `(left, op, right)`.
fn split_ref_operator(s: &str) -> Option<(&str, &'static str, &str)> {
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
                // `<>` or `<`.
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    return Some((s[..i].trim(), "<>", s[i + 2..].trim()));
                }
                return Some((s[..i].trim(), "<", s[i + 1..].trim()));
            }
            '>' if !in_quote && paren == 0 => {
                return Some((s[..i].trim(), ">", s[i + 1..].trim()));
            }
            '-' if !in_quote && paren == 0 => {
                return Some((s[..i].trim(), "-", s[i + 1..].trim()));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parse a Ref endpoint: `"s"."t"."c"` or `"s"."t".(c1, c2)` (or unquoted /
/// unqualified variants). Returns `(schema, table, columns)`.
fn parse_ref_endpoint(s: &str) -> Result<(String, String, Vec<String>)> {
    let s = s.trim();
    // Composite: a `.(...)` tuple at the end.
    if let Some(paren_at) = s.find(".(") {
        let prefix = &s[..paren_at];
        let tuple = &s[paren_at + 1..];
        let inner = tuple
            .strip_prefix('(')
            .and_then(|t| t.strip_suffix(')'))
            .ok_or_else(|| parse_err(format!("malformed composite ref endpoint: {s}")))?;
        let cols: Vec<String> = inner
            .split(',')
            .map(|c| unquote(c.trim()))
            .filter(|c| !c.is_empty())
            .collect();
        let (schema, table) = parse_schema_table(prefix)?;
        if cols.is_empty() {
            return Err(parse_err(format!("composite ref has no columns: {s}")));
        }
        return Ok((schema, table, cols));
    }

    // Simple: `schema.table.column` / `table.column` / `column`.
    let parts = split_dotted(s);
    match parts.as_slice() {
        [schema, table, col] => Ok((unquote(schema), unquote(table), vec![unquote(col)])),
        [table, col] => Ok(("public".to_string(), unquote(table), vec![unquote(col)])),
        _ => Err(parse_err(format!("malformed ref endpoint: {s}"))),
    }
}

/// Parse a `schema.table` (or `table`) prefix used before a composite tuple.
fn parse_schema_table(s: &str) -> Result<(String, String)> {
    let parts = split_dotted(s.trim());
    match parts.as_slice() {
        [schema, table] => Ok((unquote(schema), unquote(table))),
        [table] => Ok(("public".to_string(), unquote(table))),
        _ => Err(parse_err(format!("malformed ref table prefix: {s}"))),
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
}
