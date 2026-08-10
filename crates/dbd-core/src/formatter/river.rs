use crate::config::{FormatConfig, KeywordCase};

use super::*;

// ── River formatter ─────────────────────────────────────

/// Emit one river-style line: keyword right-aligned within `gutter`, then
/// a space, then content.
pub(in crate::formatter) fn river_line(gutter: usize, keyword: &str, content: &str) -> String {
    let pad = gutter.saturating_sub(keyword.chars().count());
    format!("{}{} {}", " ".repeat(pad), keyword, content)
}

/// Emit a continuation comma line — comma sits at position `gutter`,
/// aligning with the right edge of the keyword on the previous clause line.
pub(in crate::formatter) fn river_comma_line(gutter: usize, content: &str) -> String {
    let pad = gutter.saturating_sub(1);
    format!("{}, {}", " ".repeat(pad), content)
}

/// Format a SELECT query (or bare SELECT) with river style.
/// Returns the formatted SQL with a trailing semicolon. Falls back to
/// keyword-case-only formatting if the river output wouldn't round-trip.
pub(in crate::formatter) fn format_river_query(query: &sqlparser::ast::Query, config: &FormatConfig) -> String {
    match river_query_faithful(query, config) {
        Some(river) => ensure_semicolon(&river),
        None => ensure_semicolon(&apply_keyword_case(&query.to_string(), &config.keyword_case)),
    }
}

/// Build river-formatted lines for a Query. Used both for standalone SELECT
/// and for embedding inside CREATE VIEW.
pub(in crate::formatter) fn river_select_lines(query: &sqlparser::ast::Query, config: &FormatConfig) -> Vec<String> {
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
pub(in crate::formatter) fn river_query_faithful(query: &sqlparser::ast::Query, config: &FormatConfig) -> Option<String> {
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

pub(in crate::formatter) fn river_lines_from_select(
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
pub(in crate::formatter) fn river_emit_select_list(
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
/// Max table-name length across a SELECT's FROM/JOIN relations, for alias
/// alignment. Returns `0` unless at least one relation has an alias.
pub(in crate::formatter) fn from_clause_alias_width(select: &sqlparser::ast::Select) -> usize {
    let any_alias = select.from.iter().any(|twj| {
        table_factor_has_alias(&twj.relation)
            || twj.joins.iter().any(|j| table_factor_has_alias(&j.relation))
    });
    if !any_alias {
        return 0;
    }
    let mut max = 0usize;
    for twj in &select.from {
        max = max.max(table_factor_name_len(&twj.relation));
        for join in &twj.joins {
            max = max.max(table_factor_name_len(&join.relation));
        }
    }
    max
}

/// Emit a derived-table (subquery) relation in river style: the opening `(`, the
/// indented sub-SELECT, then the closing `)` with an optional alias.
pub(in crate::formatter) fn emit_derived_relation(
    subquery: &sqlparser::ast::Query,
    alias: Option<&sqlparser::ast::TableAlias>,
    from_kw: &str,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    let g = config.gutter;
    lines.push(river_line(g, from_kw, "("));
    let sub_indent = " ".repeat(g + 2);
    for sub_line in river_select_lines(subquery, config) {
        lines.push(format!("{sub_indent}{sub_line}"));
    }
    let close = if let Some(a) = alias {
        format!("{}) {}", " ".repeat(g + 1), a.name.value.to_lowercase())
    } else {
        format!("{})", " ".repeat(g + 1))
    };
    lines.push(close);
}

pub(in crate::formatter) fn river_emit_from_joins(
    select: &sqlparser::ast::Select,
    config: &FormatConfig,
    lines: &mut Vec<String>,
) {
    use sqlparser::ast::*;

    let g = config.gutter;
    let kw = |s: &str| kw_case(s, &config.keyword_case);

    let max_table_name_len = from_clause_alias_width(select);

    for (j, twj) in select.from.iter().enumerate() {
        let from_kw = if j == 0 { kw("from") } else { kw(",") };

        match &twj.relation {
            TableFactor::Derived { subquery, alias, .. } => {
                emit_derived_relation(subquery, alias.as_ref(), &from_kw, config, lines);
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
pub(in crate::formatter) fn river_emit_order_by(
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
pub(in crate::formatter) fn river_emit_limit(query: &sqlparser::ast::Query, config: &FormatConfig, lines: &mut Vec<String>) {
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
pub(in crate::formatter) fn emit_aligned_conditions(
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
        let max_lhs = max_lhs_width(&parts);
        for (i, part) in parts.iter().enumerate() {
            let (lhs, op, rhs) = part.as_ref().unwrap();
            let content = aligned_comparison(lhs, op, rhs, max_lhs);
            let keyword = if i == 0 { first_kw } else { cont_kw };
            lines.push(river_line(gutter, keyword, &content));
        }
    } else {
        // Mixed: expand OR groups inline, align the remaining comparisons.
        let max_lhs = max_lhs_width(&parts);
        let multi_cmp = parts.iter().filter(|p| p.is_some()).count() > 1;

        for (i, cond) in conditions.iter().enumerate() {
            let keyword = if i == 0 { first_kw } else { cont_kw };

            if let Some(or_parts) = extract_nested_or_parts(cond) {
                emit_or_group(&or_parts, keyword, gutter, config, lines);
            } else if multi_cmp && parts[i].is_some() {
                let (lhs, op, rhs) = parts[i].as_ref().unwrap();
                let content = aligned_comparison(lhs, op, rhs, max_lhs);
                lines.push(river_line(gutter, keyword, &content));
            } else {
                let content = apply_keyword_case(&cond.to_string(), &config.keyword_case);
                lines.push(river_line(gutter, keyword, &content));
            }
        }
    }
}

/// Width of the widest LHS among the parsed comparison parts, for alignment.
pub(in crate::formatter) fn max_lhs_width(parts: &[Option<(String, String, String)>]) -> usize {
    parts
        .iter()
        .filter_map(|p| p.as_ref().map(|(l, _, _)| l.len()))
        .max()
        .unwrap_or(0)
}

/// Format one aligned `lhs<pad> op rhs` comparison, padding the LHS to `max_lhs`.
pub(in crate::formatter) fn aligned_comparison(lhs: &str, op: &str, rhs: &str, max_lhs: usize) -> String {
    let pad = max_lhs.saturating_sub(lhs.len());
    format!("{}{} {} {}", lhs, " ".repeat(pad), op, rhs)
}

/// If `expr` is `(a OR b OR ...)`, return the flattened OR parts.
pub(in crate::formatter) fn extract_nested_or_parts(expr: &sqlparser::ast::Expr) -> Option<Vec<&sqlparser::ast::Expr>> {
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
pub(in crate::formatter) fn emit_or_group(
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
        let max_lhs = max_lhs_width(&parts);

        for (j, part) in parts.iter().enumerate() {
            let (lhs, op, rhs) = part.as_ref().unwrap();
            let content = aligned_comparison(lhs, op, rhs, max_lhs);
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
pub(in crate::formatter) fn extract_comparison_parts(
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

pub(in crate::formatter) fn is_comparison_op(op: &sqlparser::ast::BinaryOperator) -> bool {
    use sqlparser::ast::BinaryOperator as B;
    matches!(
        op,
        B::Eq | B::NotEq | B::Gt | B::Lt | B::GtEq | B::LtEq
            | B::PGRegexMatch | B::PGRegexIMatch
            | B::PGRegexNotMatch | B::PGRegexNotIMatch
    )
}

pub(in crate::formatter) fn comparison_op_str(op: &sqlparser::ast::BinaryOperator, case: &KeywordCase) -> String {
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

pub(in crate::formatter) fn table_factor_name_len(tf: &sqlparser::ast::TableFactor) -> usize {
    match tf {
        sqlparser::ast::TableFactor::Table { name, .. } => {
            name.to_string().to_lowercase().len()
        }
        _ => 0,
    }
}

pub(in crate::formatter) fn table_factor_has_alias(tf: &sqlparser::ast::TableFactor) -> bool {
    match tf {
        sqlparser::ast::TableFactor::Table { alias, .. } => alias.is_some(),
        _ => false,
    }
}

pub(in crate::formatter) fn render_table_factor_aligned(
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

pub(in crate::formatter) fn extract_join_parts<'a>(
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
pub(in crate::formatter) fn split_and_conditions(expr: &sqlparser::ast::Expr) -> Vec<&sqlparser::ast::Expr> {
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
pub(in crate::formatter) fn split_or_conditions(expr: &sqlparser::ast::Expr) -> Vec<&sqlparser::ast::Expr> {
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
pub(in crate::formatter) fn split_boolean_conditions(expr: &sqlparser::ast::Expr) -> (Vec<&sqlparser::ast::Expr>, &'static str) {
    use sqlparser::ast::{BinaryOperator, Expr};
    match expr {
        Expr::BinaryOp { op: BinaryOperator::And, .. } => (split_and_conditions(expr), "and"),
        Expr::BinaryOp { op: BinaryOperator::Or, .. } => (split_or_conditions(expr), "or"),
        other => (vec![other], "and"),
    }
}
