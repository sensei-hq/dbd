use crate::config::{CommaStyle, FormatConfig, KeywordCase, QueryStyle};

use super::*;

// ── CREATE TABLE formatter ──────────────────────────────

pub(in crate::formatter) fn format_create_table(
    ct: &sqlparser::ast::CreateTable,
    original: &str,
    config: &FormatConfig,
) -> String {
    // What lies outside the column list is written as the statement has it:
    // the layout wrote `create table [if not exists] <name>` and nothing
    // after `)`, so a quoted name lost its case, and `temp`, `unlogged`,
    // `with (…)`, `partition by …`, `inherits (…)` and `tablespace …` were
    // dropped — and every such table kept as written.
    // A table with no column list — `partition of u for values from (1)
    // to (10)`, `of mytype` — has nothing to lay out, and its first `(` is
    // not a column list.
    let around = (!ct.columns.is_empty() || !ct.constraints.is_empty())
        .then(|| around_column_list(original))
        .flatten();
    let Some((header, tail)) = around else {
        return ensure_semicolon(&apply_keyword_case(original, &config.keyword_case));
    };
    let mut out = format!("{header} (\n");

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
    emit_table_items(&mut out, &item_lines, original, &indent);

    out.push(')');
    if !tail.is_empty() {
        out.push(' ');
        out.push_str(&tail);
    }
    out.push(';');
    out
}

/// The text of a `CREATE TABLE` before its column list's `(` and after its
/// `)` — `create unlogged table if not exists app."T"`, `with (fillfactor =
/// 70)` — each as the statement writes it, with its unquoted names in lower
/// case as the layout writes the columns' and the space between its tokens
/// settled to one (a line break after a `--` comment). `None` when the
/// statement has no column list to find.
fn around_column_list(original: &str) -> Option<(String, String)> {
    use pg_query::protobuf::Token as Kind;
    let tokens = lex(original)?;
    // The column list opens at the first `(`: a table name carries none.
    let open = tokens.iter().position(|t| t.kind == Kind::Ascii40)?;
    let mut depth = 0usize;
    let close = open
        + tokens[open..].iter().position(|t| {
            match t.kind {
                Kind::Ascii40 => depth += 1,
                Kind::Ascii41 => depth -= 1,
                _ => {}
            }
            depth == 0
        })?;
    let end = tokens
        .iter()
        .rposition(|t| t.kind != Kind::Ascii59 && !t.is_comment())?;
    let keywords = keyword_flags(original, &tokens);
    let written = |range: std::ops::Range<usize>| {
        let mut out = String::new();
        for i in range.clone() {
            let t = &tokens[i];
            if i > range.start && t.start > tokens[i - 1].end {
                out.push(if tokens[i - 1].kind == Kind::SqlComment {
                    '\n'
                } else {
                    ' '
                });
            }
            let text = t.text(original);
            if t.is_word(original) && !keywords[i] {
                out.push_str(&text.to_lowercase());
            } else {
                out.push_str(text);
            }
        }
        out
    };
    Some((written(0..open), written(close + 1..(end + 1).max(close + 1))))
}

/// Emit each column/constraint line into `out`, re-attaching captured inline
/// comments only when they line up 1:1 with the items. On any mismatch
/// (interleaved constraints, exotic layout), lines are emitted bare — the
/// round-trip guard then keeps the original text, so a comment is never dropped.
pub(in crate::formatter) fn emit_table_items(out: &mut String, item_lines: &[String], original: &str, indent: &str) {
    match extract_item_comments(original).filter(|c| c.len() == item_lines.len()) {
        Some(comments) => {
            for (line, ic) in item_lines.iter().zip(comments.iter()) {
                for lead in &ic.leading {
                    out.push_str(indent);
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
                        out.push_str(indent);
                        out.push_str(trailing);
                    }
                }
                out.push('\n');
            }
        }
        None => {
            for line in item_lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
}

// ── CREATE TABLE inline-comment preservation ────────────

/// Inline comments captured for one top-level column-list item (column or
/// table constraint), in source order.
#[derive(Default)]
pub(in crate::formatter) struct ItemComments {
    /// Standalone comment lines that preceded the item's code (own lines above it).
    leading: Vec<String>,
    /// Comments after the item's code (trailing — usually end-of-line).
    trailing: Vec<String>,
}

/// Extract inline comments from a `CREATE TABLE` column list, one [`ItemComments`]
/// per top-level (comma-separated) item, in source order. Returns `None` if the
/// column list cannot be located. sqlparser discards comments, so this recovers
/// them from the text — read as libpg_query's scanner reads it, so a `--`, a
/// `,` or a `)` inside a string of any kind or a quoted identifier is neither a
/// comment nor a boundary. The caller only uses the result when it lines up 1:1
/// with the parsed items and otherwise keeps the original text verbatim.
///
/// Within each item, a comment before any code is `leading`; one after code is
/// `trailing`. A comment after an item's comma on the comma's own line is
/// that item's trailing comment too — `id int,  -- the id` documents `id`,
/// and is how the trailing-comma layout writes one. Read as the next item's,
/// it moved to a line of its own on every second pass.
pub(in crate::formatter) fn extract_item_comments(original: &str) -> Option<Vec<ItemComments>> {
    use pg_query::protobuf::Token as Kind;
    let tokens = lex(original)?;
    // The column list opens at the first `(`: a table name carries none.
    let open = tokens.iter().position(|t| t.kind == Kind::Ascii40)?;
    let mut items: Vec<ItemComments> = Vec::new();
    let mut cur = ItemComments::default();
    let mut seen_code = false;
    let mut depth = 1usize;
    // Where the last item-ending comma (or a comment after it on its line)
    // ended, while nothing but those has followed it on that line.
    let mut after_comma: Option<usize> = None;
    for token in &tokens[open + 1..] {
        let on_comma_line = after_comma.is_some_and(|at| !original[at..token.start].contains('\n'));
        after_comma = None;
        match token.kind {
            Kind::Ascii40 => {
                depth += 1;
                seen_code = true;
            }
            Kind::Ascii41 => {
                depth -= 1;
                if depth == 0 {
                    items.push(cur);
                    return Some(items);
                }
                seen_code = true;
            }
            Kind::Ascii44 if depth == 1 => {
                items.push(std::mem::take(&mut cur));
                seen_code = false;
                after_comma = Some(token.end);
            }
            Kind::SqlComment | Kind::CComment => {
                let text = token.text(original);
                let text = if token.kind == Kind::SqlComment {
                    text.trim_end()
                } else {
                    text.trim()
                };
                if seen_code {
                    cur.trailing.push(text.to_string());
                } else if on_comma_line && let Some(previous) = items.last_mut() {
                    previous.trailing.push(text.to_string());
                    after_comma = Some(token.end);
                } else {
                    cur.leading.push(text.to_string());
                }
            }
            _ => seen_code = true,
        }
    }
    None
}

/// Render one column line for CREATE TABLE, applying the configured comma style.
/// A trailing comma is added unless this is the last column and there are no
/// following table-level constraints.
pub(in crate::formatter) fn format_table_column(
    col: &sqlparser::ast::ColumnDef,
    is_first: bool,
    is_last: bool,
    has_trailing_constraints: bool,
    config: &FormatConfig,
) -> String {
    let indent = " ".repeat(config.indent);
    let type_col = config.type_alignment;
    let col_name = layout_ident(&col.name);
    let type_str = format_column_type(&col.data_type, config);
    let constraints_str = format_column_constraints(&col.options, config);

    match config.comma_style {
        CommaStyle::Trailing => {
            let base = format_column_line(&indent, &col_name, &type_str, &constraints_str, type_col);
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
pub(in crate::formatter) fn format_table_constraint_line(
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
            if !is_last { format!("{base},") } else { base }
        }
    }
}

pub(in crate::formatter) fn format_column_line(
    indent: &str,
    col_name: &str,
    type_str: &str,
    constraints_str: &str,
    type_col: usize,
) -> String {
    let name_with_pad = pad_to_width(col_name, type_col - indent.len());
    format!("{indent}{name_with_pad}{type_str}{constraints_str}")
}

/// A name as the layout writes it: as written, quotes and all, when it is
/// quoted — its case and its characters are the name — else in lower case,
/// the name Postgres folds it to. Lowercased whole, `"Note"` became `note`, a
/// different column.
pub(in crate::formatter) fn layout_ident(ident: &sqlparser::ast::Ident) -> String {
    if ident.quote_style.is_some() {
        ident.to_string()
    } else {
        ident.value.to_lowercase()
    }
}

pub(in crate::formatter) fn pad_to_width(s: &str, width: usize) -> String {
    if s.len() >= width {
        format!("{s} ")
    } else {
        format!("{s}{}", " ".repeat(width - s.len()))
    }
}

pub(in crate::formatter) fn format_column_type(data_type: &sqlparser::ast::DataType, config: &FormatConfig) -> String {
    let raw = data_type.to_string();
    apply_keyword_case(&raw, &config.keyword_case)
}

pub(in crate::formatter) fn format_column_constraints(
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
        // A named constraint — `constraint nn not null` — is written whole,
        // its name with it; written by kind, the name was dropped.
        if opt.name.is_some() {
            parts.push(apply_keyword_case(&opt.to_string(), &config.keyword_case));
            continue;
        }
        let s = match &opt.option {
            sqlparser::ast::ColumnOption::PrimaryKey(_) => kw("PRIMARY KEY"),
            sqlparser::ast::ColumnOption::Unique(_) => kw("UNIQUE"),
            sqlparser::ast::ColumnOption::NotNull => kw("NOT NULL"),
            sqlparser::ast::ColumnOption::Null => kw("NULL"),
            sqlparser::ast::ColumnOption::Default(expr) => {
                let expr_str = apply_keyword_case(&expr.to_string(), &config.keyword_case);
                format!("{} {expr_str}", kw("DEFAULT"))
            }
            // sqlparser's own rendering carries the referential actions, MATCH
            // and DEFERRABLE. Writing `REFERENCES t(id)` alone dropped them, so
            // the guard kept every table with `on delete cascade` as written —
            // no layout, and no keyword case.
            sqlparser::ast::ColumnOption::ForeignKey(_) => {
                apply_keyword_case(&opt.option.to_string(), &config.keyword_case)
            }
            // sqlparser's CHECK renders as `CHECK (…)` itself, its name with
            // it; wrapped again it was `check (check (…))`, so every column
            // with a CHECK of its own kept the table as written.
            sqlparser::ast::ColumnOption::Check(_) => apply_keyword_case(&opt.option.to_string(), &config.keyword_case),
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

pub(in crate::formatter) fn format_table_constraint(
    constraint: &sqlparser::ast::TableConstraint,
    config: &FormatConfig,
) -> String {
    let raw = constraint.to_string();
    apply_keyword_case(&raw, &config.keyword_case)
}

// ── CREATE INDEX formatter ──────────────────────────────

pub(in crate::formatter) fn format_create_index(ci: &sqlparser::ast::CreateIndex, config: &FormatConfig) -> String {
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
        let name_str = name
            .0
            .iter()
            .filter_map(|part| part.as_ident())
            .map(|i| i.value.to_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        out.push_str(&format!(" {name_str}"));
    }

    out.push_str(&format!(" {} {}", kw("ON"), ci.table_name.to_string().to_lowercase()));

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

// ── CREATE VIEW formatter ───────────────────────────────

pub(in crate::formatter) fn format_create_view(
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

pub(in crate::formatter) fn format_create_type(
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
