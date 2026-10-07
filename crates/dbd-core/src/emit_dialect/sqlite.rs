//! A view's query or a CHECK's expression, written again in SQLite's SQL from
//! libpg_query's tree — or refused, with the reason.
//!
//! # Why written again, and why refused rather than guessed
//!
//! Both used to go across verbatim, and on SQLite that broke the script. SQLite
//! has no schemas: `core.cities` names a database that is not attached, so the
//! CREATE VIEW failed, and a bare `regions` applied and then failed on the
//! first read, because the table is `core_regions` there. `POSITION('-' IN
//! code)` and `slug ~ '…'` are syntax errors, and a failed CREATE TABLE takes
//! the whole table with it.
//!
//! There were two ways out: rewrite the qualified names in the original text,
//! or leave out what SQLite cannot run. Rewriting names alone keeps the most
//! text and guarantees nothing — the same body can still hold a `::` cast or a
//! `DISTINCT ON`, and the script fails anyway. Leaving every view out
//! guarantees the script applies and throws away views SQLite runs perfectly
//! well.
//!
//! So both, decided per statement from the parse tree. The statement is
//! written node by node from an allowlist of what SQLite has with the same
//! meaning: table names become the names the script creates, `POSITION(a IN
//! b)` becomes `instr(b, a)`, PostgreSQL's NULL ordering and LIKE escape are
//! spelled out, and every result column is named the way PostgreSQL names it.
//! The first node outside the allowlist refuses the whole statement, and the
//! caller leaves it out and reports why. Everything emitted was written here,
//! so nothing PostgreSQL-only can slip through in the text — which is the
//! guarantee a copy could not give, and a refusal costs only what SQLite could
//! not have run.
//!
//! Written for SQLite 3.39 or later: RIGHT and FULL JOIN, NULLS FIRST/LAST,
//! FILTER and window functions all need it.

use pg_query::NodeEnum;
use pg_query::protobuf as pb;
use pg_query::protobuf::a_const::Val;

/// A table a query names, as the script has it.
pub(super) enum Relation {
    /// The script creates it, under this name, already quoted.
    Created(String),
    /// The script does not. The reason, as a clause.
    Missing(String),
}

/// Why a statement has no SQLite form, as a clause a report's reason opens
/// with.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct NoForm(pub(super) String);

type Written = Result<String, NoForm>;

/// How a query's table names are resolved: `(schema, name)`, the schema `None`
/// when the query left it to the search path.
pub(super) type Resolve<'a> = dyn Fn(Option<&str>, &str) -> Relation + 'a;

/// `SQLite has no …`.
fn none_for(what: impl std::fmt::Display) -> NoForm {
    NoForm(format!("SQLite has no {what}"))
}

/// A view's `SELECT`, written for SQLite.
pub(super) fn query(body: &str, resolve: &Resolve<'_>) -> Written {
    // Read as the CREATE VIEW it came from, so a trailing `WITH CHECK OPTION`
    // is a clause rather than a syntax error. SQLite's views are read-only,
    // so the option has nothing to check there.
    let parsed = pg_query::parse(&format!("CREATE VIEW v AS {body}"))
        .map_err(|e| NoForm(format!("dbd could not read its SELECT ({e})")))?;
    let view = match first_statement(&parsed) {
        Some(NodeEnum::ViewStmt(v)) => v,
        _ => return Err(NoForm("dbd could not read its SELECT".to_string())),
    };
    match view.query.as_deref().and_then(|n| n.node.as_ref()) {
        Some(NodeEnum::SelectStmt(s)) => Writer::new(Some(resolve), false).select(s),
        _ => Err(NoForm("dbd could not read its SELECT".to_string())),
    }
}

/// A CHECK expression, written for SQLite.
pub(super) fn check(expression: &str) -> Written {
    let parsed = pg_query::parse(&format!("SELECT {expression}"))
        .map_err(|e| NoForm(format!("dbd could not read the expression ({e})")))?;
    let target = match first_statement(&parsed) {
        Some(NodeEnum::SelectStmt(s)) => s.target_list.first().and_then(|n| n.node.as_ref()),
        _ => None,
    };
    match target {
        Some(NodeEnum::ResTarget(t)) if t.val.is_some() => {
            Writer::new(None, true).expr(t.val.as_deref().expect("checked"))
        }
        _ => Err(NoForm("dbd could not read the expression".to_string())),
    }
}

fn first_statement(parsed: &pg_query::ParseResult) -> Option<&NodeEnum> {
    parsed.protobuf.stmts.first()?.stmt.as_ref()?.node.as_ref()
}

/// An identifier, quoted the way SQLite reads one.
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A string literal. SQLite's has no backslash escapes, so only the quote
/// doubles.
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The names in a list of `String` nodes — a function's, a type's, a column
/// list.
fn strings(nodes: &[pb::Node]) -> Option<Vec<&str>> {
    nodes
        .iter()
        .map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect()
}

fn names(nodes: &[pb::Node]) -> Written {
    let names = strings(nodes).ok_or_else(|| NoForm("dbd could not read a column list".to_string()))?;
    Ok(names.iter().map(|n| ident(n)).collect::<Vec<_>>().join(", "))
}

/// Something outside the allowlist, named for the reader.
fn unsupported(node: Option<&NodeEnum>) -> NoForm {
    let what = match node {
        Some(NodeEnum::AArrayExpr(_)) => "ARRAY[…] constructor".to_string(),
        Some(NodeEnum::AIndirection(_)) => "subscript or field selection".to_string(),
        Some(NodeEnum::RowExpr(_)) => "ROW(…) constructor".to_string(),
        Some(NodeEnum::CollateClause(_)) => "COLLATE clause that means what PostgreSQL's does".to_string(),
        Some(NodeEnum::ParamRef(_)) => "parameter here".to_string(),
        Some(NodeEnum::GroupingSet(_)) => "GROUPING SETS, ROLLUP or CUBE".to_string(),
        Some(NodeEnum::GroupingFunc(_)) => "GROUPING()".to_string(),
        Some(NodeEnum::MinMaxExpr(_)) => {
            "GREATEST or LEAST — its max() and min() return NULL when any argument is".to_string()
        }
        Some(NodeEnum::XmlExpr(_) | NodeEnum::XmlSerialize(_)) => "XML functions".to_string(),
        Some(NodeEnum::RangeFunction(_) | NodeEnum::RangeTableFunc(_)) => {
            "function in FROM (`generate_series`, `unnest` and the like)".to_string()
        }
        Some(other) => {
            // The variant's name — `JsonObjectConstructor` — is what a reader
            // can look up; the rest of the debug text is the whole subtree.
            let debug = format!("{other:?}");
            format!(
                "form dbd writes for PostgreSQL's `{}`",
                debug.split('(').next().unwrap_or("?")
            )
        }
        None => "form for an empty node".to_string(),
    };
    none_for(what)
}

/// What a result column is called in PostgreSQL when the query does not name
/// it — `FigureColname`, for the shapes this module writes. SQLite names such a
/// column after its text (`count(*)`), so a query reading the view by
/// PostgreSQL's name (`count`) found nothing.
fn column_name(node: &pb::Node) -> String {
    figure(node).unwrap_or_else(|| "?column?".to_string())
}

fn figure(node: &pb::Node) -> Option<String> {
    use pb::SqlValueFunctionOp as Svf;
    match node.node.as_ref()? {
        NodeEnum::ColumnRef(c) => match c.fields.last()?.node.as_ref()? {
            NodeEnum::String(s) => Some(s.sval.clone()),
            _ => None,
        },
        NodeEnum::FuncCall(f) => strings(&f.funcname)?.last().map(|s| s.to_string()),
        // The argument's name if it has one, else the type's: `x::text` is
        // `x`, `'a'::text` is `text`.
        NodeEnum::TypeCast(c) => c
            .arg
            .as_deref()
            .and_then(figure)
            .or_else(|| strings(&c.type_name.as_ref()?.names)?.last().map(|s| s.to_string())),
        NodeEnum::CaseExpr(_) => Some("case".to_string()),
        NodeEnum::CoalesceExpr(_) => Some("coalesce".to_string()),
        NodeEnum::AExpr(a) if a.kind() == pb::AExprKind::AexprNullif => Some("nullif".to_string()),
        NodeEnum::SqlvalueFunction(f) => match f.op() {
            Svf::SvfopCurrentDate => Some("current_date".to_string()),
            Svf::SvfopCurrentTimestamp => Some("current_timestamp".to_string()),
            _ => None,
        },
        NodeEnum::SubLink(s) => match s.sub_link_type() {
            pb::SubLinkType::ExistsSublink => Some("exists".to_string()),
            pb::SubLinkType::ExprSublink => match s.subselect.as_deref()?.node.as_ref()? {
                NodeEnum::SelectStmt(q) => match q.target_list.first()?.node.as_ref()? {
                    NodeEnum::ResTarget(t) if !t.name.is_empty() => Some(t.name.clone()),
                    NodeEnum::ResTarget(t) => figure(t.val.as_deref()?),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

struct Writer<'a> {
    resolve: Option<&'a Resolve<'a>>,
    /// The CTE names in scope, innermost last. Query-local, never a table.
    ctes: Vec<String>,
    /// Writing a CHECK, where SQLite refuses anything that is not
    /// deterministic — the clock included.
    in_check: bool,
}

impl<'a> Writer<'a> {
    fn new(resolve: Option<&'a Resolve<'a>>, in_check: bool) -> Self {
        Self {
            resolve,
            ctes: Vec::new(),
            in_check,
        }
    }

    fn select(&mut self, s: &pb::SelectStmt) -> Written {
        if s.into_clause.is_some() {
            return Err(none_for("SELECT INTO here"));
        }
        if !s.locking_clause.is_empty() {
            return Err(none_for("row locking (FOR UPDATE, FOR SHARE)"));
        }
        let scope = self.ctes.len();
        let mut out = String::new();
        if let Some(with) = &s.with_clause {
            out.push_str(&self.with(with)?);
        }
        out.push_str(&self.compound(s)?);
        if !s.sort_clause.is_empty() {
            out.push_str(&format!(" ORDER BY {}", self.sort(&s.sort_clause)?));
        }
        if s.limit_option() == pb::LimitOption::WithTies {
            return Err(none_for("FETCH … WITH TIES"));
        }
        // `LIMIT ALL` is a NULL count: no limit at all.
        let count = s
            .limit_count
            .as_deref()
            .filter(|n| !matches!(n.node.as_ref(), Some(NodeEnum::AConst(c)) if c.isnull));
        match (count, s.limit_offset.as_deref()) {
            (Some(n), offset) => {
                out.push_str(&format!(" LIMIT {}", self.expr(n)?));
                if let Some(o) = offset {
                    out.push_str(&format!(" OFFSET {}", self.expr(o)?));
                }
            }
            // SQLite takes OFFSET only after a LIMIT; -1 is none.
            (None, Some(o)) => out.push_str(&format!(" LIMIT -1 OFFSET {}", self.expr(o)?)),
            (None, None) => {}
        }
        self.ctes.truncate(scope);
        Ok(out)
    }

    fn with(&mut self, w: &pb::WithClause) -> Written {
        let mut ctes = Vec::new();
        for n in &w.ctes {
            let Some(NodeEnum::CommonTableExpr(cte)) = n.node.as_ref() else {
                return Err(unsupported(n.node.as_ref()));
            };
            if cte.search_clause.is_some() || cte.cycle_clause.is_some() {
                return Err(none_for("SEARCH or CYCLE clause"));
            }
            let Some(NodeEnum::SelectStmt(q)) = cte.ctequery.as_deref().and_then(|n| n.node.as_ref()) else {
                return Err(none_for("data-modifying statement in WITH"));
            };
            // A recursive CTE reads itself; a plain one sees only those before it.
            if w.recursive {
                self.ctes.push(cte.ctename.clone());
            }
            let columns = if cte.aliascolnames.is_empty() {
                String::new()
            } else {
                format!(" ({})", names(&cte.aliascolnames)?)
            };
            ctes.push(format!("{}{columns} AS ({})", ident(&cte.ctename), self.select(q)?));
            if !w.recursive {
                self.ctes.push(cte.ctename.clone());
            }
        }
        Ok(format!(
            "WITH {}{} ",
            if w.recursive { "RECURSIVE " } else { "" },
            ctes.join(", ")
        ))
    }

    /// A plain SELECT, or a chain of UNION / INTERSECT / EXCEPT.
    fn compound(&mut self, s: &pb::SelectStmt) -> Written {
        use pb::SetOperation as Op;
        let op = s.op();
        if matches!(op, Op::SetopNone | Op::Undefined) {
            return self.simple(s);
        }
        let (Some(left), Some(right)) = (s.larg.as_deref(), s.rarg.as_deref()) else {
            return Err(none_for("form for a set operation missing an arm"));
        };
        // PostgreSQL's INTERSECT binds tighter than UNION and SQLite reads a
        // chain left to right, so only a left-leaning chain means the same.
        if !matches!(right.op(), Op::SetopNone | Op::Undefined) {
            return Err(none_for("way to nest one set operation inside another"));
        }
        for arm in [left, right] {
            if !arm.sort_clause.is_empty()
                || arm.limit_count.is_some()
                || arm.limit_offset.is_some()
                || arm.with_clause.is_some()
            {
                return Err(none_for("ORDER BY, LIMIT or WITH on one arm of a set operation"));
            }
        }
        let word = match (op, s.all) {
            (Op::SetopUnion, false) => "UNION",
            (Op::SetopUnion, true) => "UNION ALL",
            (Op::SetopIntersect, false) => "INTERSECT",
            (Op::SetopExcept, false) => "EXCEPT",
            (Op::SetopIntersect, true) => return Err(none_for("INTERSECT ALL")),
            (_, _) => return Err(none_for("EXCEPT ALL")),
        };
        Ok(format!("{} {word} {}", self.compound(left)?, self.compound(right)?))
    }

    fn simple(&mut self, s: &pb::SelectStmt) -> Written {
        if !s.values_lists.is_empty() {
            let rows = s
                .values_lists
                .iter()
                .map(|row| match row.node.as_ref() {
                    Some(NodeEnum::List(l)) => Ok(format!("({})", self.list(&l.items)?)),
                    other => Err(unsupported(other)),
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(format!("VALUES {}", rows.join(", ")));
        }
        let mut out = String::from("SELECT ");
        match s.distinct_clause.as_slice() {
            [] => {}
            // Plain DISTINCT is one empty node; DISTINCT ON lists its keys.
            [only] if only.node.is_none() => out.push_str("DISTINCT "),
            _ => return Err(none_for("DISTINCT ON")),
        }
        let targets = s
            .target_list
            .iter()
            .map(|n| match n.node.as_ref() {
                Some(NodeEnum::ResTarget(t)) => self.target(t),
                other => Err(unsupported(other)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        out.push_str(&targets.join(", "));
        if !s.from_clause.is_empty() {
            let from = s
                .from_clause
                .iter()
                .map(|n| self.source(n))
                .collect::<Result<Vec<_>, _>>()?;
            out.push_str(&format!(" FROM {}", from.join(", ")));
        }
        if let Some(w) = s.where_clause.as_deref() {
            out.push_str(&format!(" WHERE {}", self.expr(w)?));
        }
        if s.group_distinct {
            return Err(none_for("GROUP BY DISTINCT"));
        }
        if !s.group_clause.is_empty() {
            out.push_str(&format!(" GROUP BY {}", self.list(&s.group_clause)?));
        }
        if let Some(h) = s.having_clause.as_deref() {
            out.push_str(&format!(" HAVING {}", self.expr(h)?));
        }
        if !s.window_clause.is_empty() {
            return Err(none_for("named WINDOW dbd writes"));
        }
        Ok(out)
    }

    /// A result column, named the way PostgreSQL names it — see
    /// [`column_name`].
    fn target(&mut self, t: &pb::ResTarget) -> Written {
        let Some(val) = t.val.as_deref() else {
            return Err(unsupported(None));
        };
        if !t.indirection.is_empty() {
            return Err(unsupported(Some(&NodeEnum::AIndirection(Box::default()))));
        }
        let written = self.expr(val)?;
        let star = matches!(val.node.as_ref(), Some(NodeEnum::ColumnRef(c))
            if matches!(c.fields.last().and_then(|f| f.node.as_ref()), Some(NodeEnum::AStar(_))));
        if star {
            return Ok(written);
        }
        let name = if t.name.is_empty() {
            column_name(val)
        } else {
            t.name.clone()
        };
        Ok(format!("{written} AS {}", ident(&name)))
    }

    fn source(&mut self, n: &pb::Node) -> Written {
        match n.node.as_ref() {
            Some(NodeEnum::RangeVar(rv)) => self.table(rv),
            Some(NodeEnum::JoinExpr(j)) => self.join(j),
            Some(NodeEnum::RangeSubselect(sub)) => {
                if sub.lateral {
                    return Err(none_for("LATERAL"));
                }
                let Some(NodeEnum::SelectStmt(q)) = sub.subquery.as_deref().and_then(|n| n.node.as_ref()) else {
                    return Err(unsupported(None));
                };
                let alias = match &sub.alias {
                    Some(a) if !a.colnames.is_empty() => return Err(none_for("column list on a subquery's alias")),
                    Some(a) => format!(" AS {}", ident(&a.aliasname)),
                    None => String::new(),
                };
                Ok(format!("({}){alias}", self.select(q)?))
            }
            other => Err(unsupported(other)),
        }
    }

    /// A table, by the name the script creates it under — aliased by the
    /// name the query used, so `cities.name` still finds `core_cities`.
    fn table(&mut self, rv: &pb::RangeVar) -> Written {
        if !rv.catalogname.is_empty() {
            return Err(none_for("cross-database reference"));
        }
        let alias = match &rv.alias {
            Some(a) if !a.colnames.is_empty() => return Err(none_for("column list on a table's alias")),
            Some(a) => a.aliasname.clone(),
            None => rv.relname.clone(),
        };
        if rv.schemaname.is_empty() && self.ctes.contains(&rv.relname) {
            return Ok(match &rv.alias {
                Some(_) => format!("{} AS {}", ident(&rv.relname), ident(&alias)),
                None => ident(&rv.relname),
            });
        }
        let Some(resolve) = self.resolve else {
            return Err(none_for("table reference here"));
        };
        let schema = (!rv.schemaname.is_empty()).then_some(rv.schemaname.as_str());
        match resolve(schema, &rv.relname) {
            Relation::Created(name) => Ok(format!("{name} AS {}", ident(&alias))),
            Relation::Missing(why) => Err(NoForm(why)),
        }
    }

    fn join(&mut self, j: &pb::JoinExpr) -> Written {
        use pb::JoinType as J;
        if j.alias.is_some() || j.join_using_alias.is_some() {
            return Err(none_for("alias on a join"));
        }
        let (Some(left), Some(right)) = (j.larg.as_deref(), j.rarg.as_deref()) else {
            return Err(unsupported(None));
        };
        let bare = j.quals.is_none() && j.using_clause.is_empty() && !j.is_natural;
        let kind = match j.jointype() {
            J::JoinInner if bare => "CROSS JOIN",
            J::JoinInner => "JOIN",
            J::JoinLeft => "LEFT JOIN",
            J::JoinRight => "RIGHT JOIN",
            J::JoinFull => "FULL JOIN",
            _ => return Err(none_for("such join")),
        };
        let l = self.source(left)?;
        let r = match right.node.as_ref() {
            // A join on the right is grouped in PostgreSQL's tree; SQLite reads
            // a chain left to right, so the grouping is written out.
            Some(NodeEnum::JoinExpr(_)) => format!("({})", self.source(right)?),
            _ => self.source(right)?,
        };
        let natural = if j.is_natural { "NATURAL " } else { "" };
        let mut out = format!("{l} {natural}{kind} {r}");
        if !j.using_clause.is_empty() {
            out.push_str(&format!(" USING ({})", names(&j.using_clause)?));
        }
        if let Some(q) = j.quals.as_deref() {
            out.push_str(&format!(" ON {}", self.expr(q)?));
        }
        Ok(out)
    }

    /// ORDER BY keys. PostgreSQL sorts NULL above every value and SQLite below
    /// it, so the order PostgreSQL gives is spelled out on every key.
    fn sort(&mut self, keys: &[pb::Node]) -> Written {
        use pb::{SortByDir as Dir, SortByNulls as Nulls};
        let keys = keys
            .iter()
            .map(|n| {
                let Some(NodeEnum::SortBy(sb)) = n.node.as_ref() else {
                    return Err(unsupported(n.node.as_ref()));
                };
                let Some(key) = sb.node.as_deref() else {
                    return Err(unsupported(None));
                };
                let desc = match sb.sortby_dir() {
                    Dir::SortbyDesc => true,
                    Dir::SortbyUsing => return Err(none_for("ORDER BY … USING")),
                    _ => false,
                };
                let nulls = match sb.sortby_nulls() {
                    Nulls::SortbyNullsFirst => " NULLS FIRST",
                    Nulls::SortbyNullsLast => " NULLS LAST",
                    _ if desc => " NULLS FIRST",
                    _ => " NULLS LAST",
                };
                Ok(format!(
                    "{}{}{nulls}",
                    self.operand(key)?,
                    if desc { " DESC" } else { "" }
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys.join(", "))
    }

    fn list(&mut self, items: &[pb::Node]) -> Written {
        Ok(items
            .iter()
            .map(|n| self.expr(n))
            .collect::<Result<Vec<_>, _>>()?
            .join(", "))
    }

    /// An expression inside another, parenthesized when it is itself an
    /// operation. The tree already holds PostgreSQL's grouping, and the two
    /// engines rank operators differently (`a + b || c` groups one way in
    /// each), so the grouping is written out rather than left to precedence.
    fn operand(&mut self, n: &pb::Node) -> Written {
        let written = self.expr(n)?;
        let grouped = match n.node.as_ref() {
            Some(NodeEnum::AExpr(a)) => a.kind() != pb::AExprKind::AexprNullif,
            Some(NodeEnum::BoolExpr(_) | NodeEnum::NullTest(_) | NodeEnum::BooleanTest(_)) => true,
            Some(NodeEnum::SubLink(s)) => s.sub_link_type() == pb::SubLinkType::AnySublink,
            _ => false,
        };
        Ok(if grouped { format!("({written})") } else { written })
    }

    fn clock(&self, sql: &str) -> Written {
        if self.in_check {
            return Err(NoForm(
                "SQLite refuses the clock in a CHECK, which it requires to be deterministic".to_string(),
            ));
        }
        Ok(sql.to_string())
    }

    fn expr(&mut self, n: &pb::Node) -> Written {
        use pb::SqlValueFunctionOp as Svf;
        match n.node.as_ref() {
            Some(NodeEnum::ColumnRef(c)) => column_ref(c),
            Some(NodeEnum::AConst(c)) => constant(c),
            Some(NodeEnum::TypeCast(c)) => self.cast(c),
            Some(NodeEnum::AExpr(a)) => self.operator(a),
            Some(NodeEnum::BoolExpr(b)) => {
                let args = b.args.iter().map(|a| self.operand(a)).collect::<Result<Vec<_>, _>>()?;
                match b.boolop() {
                    pb::BoolExprType::AndExpr => Ok(args.join(" AND ")),
                    pb::BoolExprType::OrExpr => Ok(args.join(" OR ")),
                    pb::BoolExprType::NotExpr if args.len() == 1 => Ok(format!("NOT {}", args[0])),
                    _ => Err(unsupported(n.node.as_ref())),
                }
            }
            Some(NodeEnum::NullTest(t)) => {
                let arg = self.operand(t.arg.as_deref().ok_or_else(|| unsupported(None))?)?;
                match t.nulltesttype() {
                    pb::NullTestType::IsNull => Ok(format!("{arg} IS NULL")),
                    pb::NullTestType::IsNotNull => Ok(format!("{arg} IS NOT NULL")),
                    pb::NullTestType::Undefined => Err(unsupported(n.node.as_ref())),
                }
            }
            Some(NodeEnum::BooleanTest(t)) => {
                use pb::BoolTestType as B;
                let arg = self.operand(t.arg.as_deref().ok_or_else(|| unsupported(None))?)?;
                let test = match t.booltesttype() {
                    B::IsTrue => "IS TRUE",
                    B::IsNotTrue => "IS NOT TRUE",
                    B::IsFalse => "IS FALSE",
                    B::IsNotFalse => "IS NOT FALSE",
                    B::IsUnknown => "IS NULL",
                    B::IsNotUnknown => "IS NOT NULL",
                    B::Undefined => return Err(unsupported(n.node.as_ref())),
                };
                Ok(format!("{arg} {test}"))
            }
            Some(NodeEnum::CaseExpr(c)) => {
                let mut out = String::from("CASE");
                if let Some(arg) = c.arg.as_deref() {
                    out.push_str(&format!(" {}", self.operand(arg)?));
                }
                for w in &c.args {
                    let Some(NodeEnum::CaseWhen(w)) = w.node.as_ref() else {
                        return Err(unsupported(w.node.as_ref()));
                    };
                    let (Some(when), Some(then)) = (w.expr.as_deref(), w.result.as_deref()) else {
                        return Err(unsupported(None));
                    };
                    out.push_str(&format!(" WHEN {} THEN {}", self.expr(when)?, self.expr(then)?));
                }
                if let Some(d) = c.defresult.as_deref() {
                    out.push_str(&format!(" ELSE {}", self.expr(d)?));
                }
                Ok(out + " END")
            }
            // SQLite's coalesce() wants two arguments; PostgreSQL's takes one.
            Some(NodeEnum::CoalesceExpr(c)) => match c.args.as_slice() {
                [only] => self.expr(only),
                args => Ok(format!("coalesce({})", self.list(args)?)),
            },
            Some(NodeEnum::SqlvalueFunction(f)) => match f.op() {
                Svf::SvfopCurrentDate => self.clock("CURRENT_DATE"),
                Svf::SvfopCurrentTimestamp => self.clock("CURRENT_TIMESTAMP"),
                _ => Err(none_for(
                    "such SQL value function (current_user, localtime and the like)",
                )),
            },
            Some(NodeEnum::SubLink(s)) => self.sublink(s),
            Some(NodeEnum::FuncCall(f)) => self.call(f),
            other => Err(unsupported(other)),
        }
    }

    fn sublink(&mut self, s: &pb::SubLink) -> Written {
        use pb::SubLinkType as L;
        let Some(NodeEnum::SelectStmt(q)) = s.subselect.as_deref().and_then(|n| n.node.as_ref()) else {
            return Err(unsupported(None));
        };
        match s.sub_link_type() {
            L::ExistsSublink => Ok(format!("EXISTS ({})", self.select(q)?)),
            L::ExprSublink => Ok(format!("({})", self.select(q)?)),
            // `x IN (SELECT …)`, which the tree holds as `= ANY`.
            L::AnySublink if strings(&s.oper_name).is_some_and(|o| o.is_empty() || o == ["="]) => {
                let test = self.operand(s.testexpr.as_deref().ok_or_else(|| unsupported(None))?)?;
                Ok(format!("{test} IN ({})", self.select(q)?))
            }
            _ => Err(none_for("ANY or ALL comparison with a subquery")),
        }
    }

    fn cast(&mut self, c: &pb::TypeCast) -> Written {
        let ty = c.type_name.as_ref().ok_or_else(|| unsupported(None))?;
        let name = strings(&ty.names)
            .and_then(|n| n.last().map(|s| s.to_string()))
            .unwrap_or_default();
        if !ty.array_bounds.is_empty() {
            return Err(none_for(format!("arrays, so no cast to `{name}[]`")));
        }
        // A length or precision would bound or round the value, and SQLite's
        // CAST does neither.
        let affinity = match name.as_str() {
            _ if !ty.typmods.is_empty() => None,
            "text" | "varchar" | "citext" => Some("TEXT"),
            "int2" | "int4" | "int8" | "smallint" | "integer" | "int" | "bigint" => Some("INTEGER"),
            "float4" | "float8" | "real" => Some("REAL"),
            "numeric" | "decimal" => Some("NUMERIC"),
            _ => None,
        };
        let Some(affinity) = affinity else {
            return Err(NoForm(format!(
                "SQLite has no cast to `{name}` that means what PostgreSQL's does"
            )));
        };
        let arg = self.expr(c.arg.as_deref().ok_or_else(|| unsupported(None))?)?;
        Ok(format!("CAST({arg} AS {affinity})"))
    }

    fn operator(&mut self, a: &pb::AExpr) -> Written {
        use pb::AExprKind as K;
        let name = strings(&a.name).unwrap_or_default();
        let op = match name.as_slice() {
            [op] => *op,
            _ => return Err(none_for("schema-qualified OPERATOR(…)")),
        };
        let left = a.lexpr.as_deref();
        let right = a.rexpr.as_deref().ok_or_else(|| unsupported(None))?;
        match a.kind() {
            K::AexprOp => {
                if matches!(op, "~" | "~*" | "!~" | "!~*") {
                    return Err(none_for(format!("regular-expression match (`{op}`)")));
                }
                if !matches!(
                    op,
                    "=" | "<>" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/" | "%" | "||"
                ) {
                    return Err(none_for(format!("operator `{op}` that means what PostgreSQL's does")));
                }
                let r = self.operand(right)?;
                match left {
                    // A space keeps `- -1` from reading as a comment.
                    None if matches!(op, "-" | "+") => Ok(format!("{op} {r}")),
                    None => Err(none_for(format!("prefix operator `{op}`"))),
                    Some(l) => Ok(format!("{} {op} {r}", self.operand(l)?)),
                }
            }
            K::AexprDistinct | K::AexprNotDistinct => {
                let l = self.operand(left.ok_or_else(|| unsupported(None))?)?;
                let r = self.operand(right)?;
                let is = if a.kind() == K::AexprDistinct { "IS NOT" } else { "IS" };
                Ok(format!("{l} {is} {r}"))
            }
            K::AexprNullif => {
                let l = self.expr(left.ok_or_else(|| unsupported(None))?)?;
                Ok(format!("nullif({l}, {})", self.expr(right)?))
            }
            K::AexprIn => {
                let Some(NodeEnum::List(items)) = right.node.as_ref() else {
                    return Err(unsupported(right.node.as_ref()));
                };
                let l = self.operand(left.ok_or_else(|| unsupported(None))?)?;
                let not = if op == "<>" { "NOT " } else { "" };
                Ok(format!("{l} {not}IN ({})", self.list(&items.items)?))
            }
            // SQLite's LIKE ignores ASCII case, which makes it PostgreSQL's
            // ILIKE — and LIKE a looser match than it was, which the report's
            // reason names.
            K::AexprLike | K::AexprIlike => {
                let not = if op.starts_with('!') { "NOT " } else { "" };
                let l = self.operand(left.ok_or_else(|| unsupported(None))?)?;
                match right.node.as_ref() {
                    Some(NodeEnum::FuncCall(f))
                        if strings(&f.funcname).is_some_and(|n| n.last() == Some(&"like_escape"))
                            && f.args.len() == 2 =>
                    {
                        let (pattern, escape) = (self.operand(&f.args[0])?, self.operand(&f.args[1])?);
                        Ok(format!("{l} {not}LIKE {pattern} ESCAPE {escape}"))
                    }
                    // PostgreSQL's LIKE escapes with a backslash unless told
                    // otherwise; SQLite's escapes with nothing unless told.
                    _ => Ok(format!(
                        "{l} {not}LIKE {} ESCAPE {}",
                        self.operand(right)?,
                        literal("\\")
                    )),
                }
            }
            K::AexprBetween | K::AexprNotBetween => {
                let Some(NodeEnum::List(bounds)) = right.node.as_ref() else {
                    return Err(unsupported(right.node.as_ref()));
                };
                let [low, high] = bounds.items.as_slice() else {
                    return Err(unsupported(right.node.as_ref()));
                };
                let l = self.operand(left.ok_or_else(|| unsupported(None))?)?;
                let not = if a.kind() == K::AexprNotBetween { "NOT " } else { "" };
                Ok(format!(
                    "{l} {not}BETWEEN {} AND {}",
                    self.operand(low)?,
                    self.operand(high)?
                ))
            }
            K::AexprSimilar => Err(none_for("SIMILAR TO")),
            K::AexprOpAny | K::AexprOpAll => Err(none_for(format!("arrays, so no `{op} ANY/ALL (…)`"))),
            K::AexprBetweenSym | K::AexprNotBetweenSym => Err(none_for("BETWEEN SYMMETRIC")),
            K::Undefined => Err(unsupported(None)),
        }
    }

    /// A function call, from an allowlist of SQLite functions that do what
    /// PostgreSQL's of the same purpose do. Anything else — a built-in SQLite
    /// lacks, or one of the project's own functions, which the script never
    /// creates — refuses the statement.
    fn call(&mut self, f: &pb::FuncCall) -> Written {
        let funcname = strings(&f.funcname).unwrap_or_default();
        let name = match funcname.as_slice() {
            [name] | ["pg_catalog", name] => *name,
            [schema, name] => {
                return Err(NoForm(format!(
                    "it calls `{schema}.{name}()`, and the script creates no functions"
                )));
            }
            _ => return Err(unsupported(None)),
        };
        if !f.agg_order.is_empty() || f.agg_within_group {
            return Err(none_for(format!("ordered aggregate, so no `{name}(… ORDER BY …)`")));
        }
        if f.func_variadic {
            return Err(none_for("VARIADIC call"));
        }
        let args = f.args.iter().map(|a| self.expr(a)).collect::<Result<Vec<_>, _>>()?;
        let aggregate = matches!(name, "count" | "sum" | "avg" | "min" | "max");
        let window = matches!(
            name,
            "row_number"
                | "rank"
                | "dense_rank"
                | "percent_rank"
                | "cume_dist"
                | "ntile"
                | "lag"
                | "lead"
                | "first_value"
                | "last_value"
                | "nth_value"
        );
        if (f.agg_distinct || f.agg_filter.is_some()) && !aggregate {
            return Err(unsupported(None));
        }
        // `substring(s FROM 'pattern')` is a regular expression; a position
        // is a number.
        let regex_substring = name == "substring"
            && f.args
                .iter()
                .skip(1)
                .any(|a| matches!(a.node.as_ref(), Some(NodeEnum::AConst(c)) if matches!(c.val, Some(Val::Sval(_)))));
        let call = match (name, args.as_slice()) {
            ("count", []) if f.agg_star => "count(*)".to_string(),
            (_, [arg]) if aggregate => {
                format!("{name}({}{arg})", if f.agg_distinct { "DISTINCT " } else { "" })
            }
            (_, _) if window && f.over.is_some() => format!("{name}({})", args.join(", ")),
            ("lower" | "upper" | "length" | "abs", [arg]) => format!("{name}({arg})"),
            ("char_length" | "character_length", [arg]) => format!("length({arg})"),
            ("round", [_] | [_, _]) | ("ltrim" | "rtrim", [_] | [_, _]) | ("replace", [_, _, _]) => {
                format!("{name}({})", args.join(", "))
            }
            // `trim(x)` arrives as `btrim(x)`, the function it calls.
            ("btrim", [_] | [_, _]) => format!("trim({})", args.join(", ")),
            ("substr", [_, _] | [_, _, _]) => format!("substr({})", args.join(", ")),
            ("substring", [_, _] | [_, _, _]) if !regex_substring => format!("substr({})", args.join(", ")),
            // `POSITION(sub IN s)` arrives as `position(s, sub)`; `strpos` is
            // the same function. SQLite's is `instr(s, sub)`.
            ("position" | "strpos", [s, sub]) => format!("instr({s}, {sub})"),
            ("now", []) => self.clock("CURRENT_TIMESTAMP")?,
            _ => {
                return Err(NoForm(format!("SQLite has no `{name}()` dbd can map PostgreSQL's to")));
            }
        };
        let mut out = call;
        if let Some(filter) = f.agg_filter.as_deref() {
            out.push_str(&format!(" FILTER (WHERE {})", self.expr(filter)?));
        }
        if let Some(over) = f.over.as_deref() {
            out.push_str(&format!(" {}", self.over(over)?));
        }
        Ok(out)
    }

    fn over(&mut self, w: &pb::WindowDef) -> Written {
        // FRAMEOPTION_NONDEFAULT: the query wrote a frame of its own.
        const NONDEFAULT_FRAME: i32 = 0x1;
        if !w.name.is_empty() || !w.refname.is_empty() {
            return Err(none_for("named WINDOW dbd writes"));
        }
        if w.frame_options & NONDEFAULT_FRAME != 0 {
            return Err(none_for("window frame (ROWS, RANGE or GROUPS …) dbd writes"));
        }
        let mut parts = Vec::new();
        if !w.partition_clause.is_empty() {
            parts.push(format!("PARTITION BY {}", self.list(&w.partition_clause)?));
        }
        if !w.order_clause.is_empty() {
            parts.push(format!("ORDER BY {}", self.sort(&w.order_clause)?));
        }
        Ok(format!("OVER ({})", parts.join(" ")))
    }
}

fn column_ref(c: &pb::ColumnRef) -> Written {
    let parts = c
        .fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(NodeEnum::String(s)) => Ok(ident(&s.sval)),
            Some(NodeEnum::AStar(_)) => Ok("*".to_string()),
            other => Err(unsupported(other)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    match parts.as_slice() {
        [_] | [_, _] => Ok(parts.join(".")),
        // `core.cities.name`: every table is aliased by its own name, so the
        // schema has nothing left to say.
        [_, table, column] => Ok(format!("{table}.{column}")),
        _ => Err(none_for("cross-database reference")),
    }
}

fn constant(c: &pb::AConst) -> Written {
    if c.isnull {
        return Ok("NULL".to_string());
    }
    match &c.val {
        Some(Val::Ival(i)) => Ok(i.ival.to_string()),
        Some(Val::Fval(f)) => Ok(f.fval.clone()),
        // SQLite's TRUE is 1, and 1 is what every SQLite reads.
        Some(Val::Boolval(b)) => Ok(if b.boolval { "1" } else { "0" }.to_string()),
        Some(Val::Sval(s)) => Ok(literal(&s.sval)),
        Some(Val::Bsval(_)) => Err(none_for("bit-string literal")),
        None => Err(unsupported(None)),
    }
}
