//! Canonicalization of boolean SQL expressions — CHECK constraint bodies and
//! partial-index `WHERE` predicates.
//!
//! The same predicate reaches dbd in two spellings that must compare equal or
//! reconcile never converges:
//!
//! | source | spelling |
//! |---|---|
//! | authored DDL | `status in ('active', 'archived')` |
//! | `pg_get_constraintdef` / `pg_get_expr` | `(status = ANY (ARRAY['active'::text, 'archived'::text]))` |
//!
//! A libpg_query parse/deparse round-trip alone settles case and parentheses but
//! NOT these two, because Postgres stores the *analyzed* tree (where `IN`
//! becomes `= ANY (ARRAY[…])` and every literal carries its resolved type) while
//! the deparser only ever sees a *raw* parse tree. So the round-trip is bracketed
//! by two tree rewrites that erase exactly that analysis:
//!
//! 1. `= ANY (ARRAY[…])` → `IN (…)` (and `<> ALL (ARRAY[…])` → `NOT IN (…)`)
//! 2. a type cast on a *literal* is dropped: `'active'::text` → `'active'`
//!
//! Both normalize the introspected form toward the shorter authored one, and both
//! run on both sides, so an authored `= ANY (ARRAY[…])` converges too.
//!
//! **Deliberate lossiness.** Rewrite 2 makes `'5'::int` and `'5'::text` compare
//! equal. That is the same trade `normalize_fk` makes for `NO ACTION` and
//! `lift_pk_unique_keep_others` makes for constraint names: a spelling difference
//! Postgres itself does not preserve must not read as drift. Casts on a
//! *column* (`(col)::text`) are kept — those change comparison semantics.
//!
//! **Fails safe.** Only expression node kinds that appear in CHECK bodies and
//! index predicates are walked; anything else is left verbatim. An un-normalized
//! subtree therefore reads as *changed* (and callers surface an advisory), never
//! as falsely equal.

use pg_query::NodeEnum;
use pg_query::protobuf::{AExprKind, Node};

/// Canonicalize a boolean SQL expression, or `None` if it can't be parsed or
/// re-deparsed.
///
/// The result is a bare predicate — still valid SQL, so callers may store it
/// back and emit it in generated DDL. `None` means "leave the raw text alone and
/// tell the user", never "assume unchanged".
pub fn canonicalize_predicate(expr: &str) -> Option<String> {
    // `SELECT 1 WHERE (<expr>)` — the wrapper makes a bare predicate parseable
    // and the added parens keep a top-level `OR` from re-associating.
    canonicalize_wrapped(&format!("SELECT 1 WHERE ({expr})"), "SELECT 1 WHERE ")
}

/// Canonicalize a non-boolean SQL expression — an index key like
/// `(context ->> 'module')` or `lower(name)`.
///
/// Same normalization as [`canonicalize_predicate`]; the separate wrapper just
/// puts the expression in a select-list position, where a non-boolean type is
/// legal. Used so an authored expression index matches the
/// `(context ->> 'module'::text)` Postgres reports back.
pub fn canonicalize_expression(expr: &str) -> Option<String> {
    canonicalize_wrapped(&format!("SELECT ({expr})"), "SELECT ")
}

/// Parse `sql`, normalize its expression tree, re-deparse, and strip `prefix`.
fn canonicalize_wrapped(sql: &str, prefix: &str) -> Option<String> {
    let mut parsed = pg_query::parse(sql).ok()?;
    for stmt in &mut parsed.protobuf.stmts {
        if let Some(node) = stmt.stmt.as_mut() {
            normalize(node);
        }
    }
    let deparsed = pg_query::deparse(&parsed.protobuf).ok()?;
    deparsed.strip_prefix(prefix).map(str::to_string)
}

/// Rewrite `node` in place, then recurse into the expression children of the
/// node kinds that can appear in a predicate. Node kinds that cannot are left
/// untouched — see the module note on failing safe.
fn normalize(node: &mut Node) {
    any_array_to_in(node);

    match node.node.as_mut() {
        Some(NodeEnum::SelectStmt(s)) => {
            if let Some(w) = s.where_clause.as_mut() {
                normalize(w);
            }
            normalize_each(&mut s.target_list);
        }
        Some(NodeEnum::ResTarget(t)) => {
            drop_literal_cast(&mut t.val);
            if let Some(val) = t.val.as_mut() {
                normalize(val);
            }
        }
        Some(NodeEnum::AExpr(e)) => {
            for slot in [&mut e.lexpr, &mut e.rexpr] {
                drop_literal_cast(slot);
                if let Some(child) = slot.as_mut() {
                    normalize(child);
                }
            }
        }
        Some(NodeEnum::BoolExpr(e)) => normalize_each(&mut e.args),
        Some(NodeEnum::CoalesceExpr(e)) => normalize_each(&mut e.args),
        Some(NodeEnum::FuncCall(f)) => normalize_each(&mut f.args),
        Some(NodeEnum::AArrayExpr(a)) => normalize_each(&mut a.elements),
        Some(NodeEnum::List(l)) => normalize_each(&mut l.items),
        Some(NodeEnum::RowExpr(r)) => normalize_each(&mut r.args),
        Some(NodeEnum::NullTest(t)) => {
            drop_literal_cast(&mut t.arg);
            if let Some(arg) = t.arg.as_mut() {
                normalize(arg);
            }
        }
        Some(NodeEnum::BooleanTest(t)) => {
            drop_literal_cast(&mut t.arg);
            if let Some(arg) = t.arg.as_mut() {
                normalize(arg);
            }
        }
        // A cast dbd keeps (its argument is not a bare literal) — normalize
        // beneath it so a nested `('x'::text)::varchar` still loses the inner one.
        Some(NodeEnum::TypeCast(c)) => {
            drop_literal_cast(&mut c.arg);
            if let Some(arg) = c.arg.as_mut() {
                normalize(arg);
            }
        }
        _ => {}
    }
}

/// Strip literal casts from, then recurse into, every element of a child list.
fn normalize_each(nodes: &mut [Node]) {
    for n in nodes.iter_mut() {
        if let Some(inner) = literal_cast_arg(n) {
            *n = inner;
        }
        normalize(n);
    }
}

/// Replace a `TypeCast` over a bare literal with the literal itself.
fn drop_literal_cast(slot: &mut Option<Box<Node>>) {
    let Some(node) = slot.as_deref() else { return };
    if let Some(inner) = literal_cast_arg(node) {
        *slot = Some(Box::new(inner));
    }
}

/// The literal inside a `TypeCast` over an `A_Const`, if that's what this is.
///
/// A cast over anything else (a column, a function call) is meaningful and is
/// reported as `None` so the caller keeps it.
fn literal_cast_arg(node: &Node) -> Option<Node> {
    let Some(NodeEnum::TypeCast(cast)) = &node.node else {
        return None;
    };
    let arg = cast.arg.as_deref()?;
    matches!(arg.node, Some(NodeEnum::AConst(_))).then(|| arg.clone())
}

/// Rewrite Postgres's analyzed array form back to the `IN` shorthand:
/// `x = ANY (ARRAY[…])` → `x IN (…)`, `x <> ALL (ARRAY[…])` → `x NOT IN (…)`.
///
/// Only the `=`/`<>` operators map onto `IN`/`NOT IN`; any other operator (e.g.
/// `> ANY (…)`) has no `IN` spelling and is left as is.
fn any_array_to_in(node: &mut Node) {
    let Some(NodeEnum::AExpr(expr)) = node.node.as_mut() else {
        return;
    };

    let kind = AExprKind::try_from(expr.kind).unwrap_or(AExprKind::Undefined);
    let op = match (kind, operator_name(expr).as_deref()) {
        (AExprKind::AexprOpAny, Some("=")) => "=",
        (AExprKind::AexprOpAll, Some("<>")) => "<>",
        _ => return,
    };

    // `ANY`/`ALL` also take a subquery or a plain array column, neither of which
    // is an `IN` list — only the literal `ARRAY[…]` constructor converts.
    let Some(NodeEnum::AArrayExpr(array)) = expr.rexpr.as_deref().and_then(|n| n.node.as_ref()) else {
        return;
    };

    expr.kind = AExprKind::AexprIn as i32;
    expr.rexpr = Some(Box::new(Node {
        node: Some(NodeEnum::List(pg_query::protobuf::List {
            items: array.elements.clone(),
        })),
    }));
    debug_assert_eq!(operator_name(expr).as_deref(), Some(op));
}

/// An `A_Expr`'s operator, for the single-operator case (`=`, `<>`, …).
fn operator_name(expr: &pg_query::protobuf::AExpr) -> Option<String> {
    let [only] = expr.name.as_slice() else {
        return None;
    };
    match &only.node {
        Some(NodeEnum::String(s)) => Some(s.sval.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both spellings of the same predicate must canonicalize to one string —
    /// the property reconcile's convergence rests on.
    fn assert_converges(authored: &str, introspected: &str) {
        let a = canonicalize_predicate(authored)
            .unwrap_or_else(|| panic!("authored form did not canonicalize: {authored}"));
        let i = canonicalize_predicate(introspected)
            .unwrap_or_else(|| panic!("introspected form did not canonicalize: {introspected}"));
        assert_eq!(a, i, "\n  authored:     {authored}\n  introspected: {introspected}");
    }

    #[test]
    fn in_list_converges_with_analyzed_any_array() {
        assert_converges(
            "source in ('builtin', 'org', 'learned')",
            "source = ANY (ARRAY['builtin'::text, 'org'::text, 'learned'::text])",
        );
    }

    #[test]
    fn not_in_converges_with_analyzed_all_array() {
        assert_converges(
            "status not in ('draft', 'void')",
            "status <> ALL (ARRAY['draft'::text, 'void'::text])",
        );
    }

    #[test]
    fn enum_qualified_literal_casts_are_dropped() {
        assert_converges("status = 'active'", "status = 'active'::sensei.memory_status");
    }

    #[test]
    fn casts_inside_a_boolean_tree_are_dropped() {
        assert_converges(
            "(scope = 'user' and project_id is null) or (scope = 'project' and project_id is not null)",
            "((scope = 'user'::text) AND (project_id IS NULL)) OR ((scope = 'project'::text) AND (project_id IS NOT NULL))",
        );
    }

    #[test]
    fn enum_in_list_inside_a_conjunction_converges() {
        assert_converges(
            "status in ('proposed', 'withdrawn') and decided_at is null",
            "(status = ANY (ARRAY['proposed'::sensei.batch_status, 'withdrawn'::sensei.batch_status])) AND (decided_at IS NULL)",
        );
    }

    #[test]
    fn parens_and_keyword_case_are_settled() {
        assert_converges("file_path is not null", "(file_path IS NOT NULL)");
    }

    /// Postgres stores a boolean predicate in the spelling it was authored in
    /// (`where is_exported` stays bare, `where configured = true` keeps `= true`),
    /// so each form only ever has to converge with itself.
    #[test]
    fn boolean_predicates_converge_in_both_spellings() {
        assert_converges("is_exported", "is_exported");
        assert_converges("configured = true", "(configured = true)");
    }

    #[test]
    fn casts_in_function_arguments_are_dropped() {
        assert_converges("lower(name) <> ''", "lower(name) <> ''::text");
    }

    /// A cast on a column changes comparison semantics, so it is NOT drift-erased.
    #[test]
    fn column_casts_are_preserved() {
        let canon = canonicalize_predicate("(code)::text = 'x'").expect("parses");
        assert!(canon.contains("::text"), "column cast must survive; got {canon}");
    }

    /// Two genuinely different predicates must not collapse into one form.
    #[test]
    fn different_predicates_stay_different() {
        let a = canonicalize_predicate("status = 'active'").unwrap();
        let b = canonicalize_predicate("status = 'archived'").unwrap();
        assert_ne!(a, b);
        let c = canonicalize_predicate("a in ('x', 'y')").unwrap();
        let d = canonicalize_predicate("a in ('x', 'y', 'z')").unwrap();
        assert_ne!(c, d);
    }

    /// `> ANY (…)` has no `IN` spelling — it must survive the rewrite intact.
    #[test]
    fn non_equality_any_is_left_alone() {
        let canon = canonicalize_predicate("n > ANY (ARRAY[1, 2])").expect("parses");
        assert!(canon.contains("ANY"), "got {canon}");
    }

    /// The canonical form is still executable SQL — callers emit it in DDL.
    #[test]
    fn canonical_form_reparses() {
        for expr in [
            "source = ANY (ARRAY['a'::text, 'b'::text])",
            "status <> ALL (ARRAY['x'::text])",
            "(scope = 'user'::text) AND (project_id IS NULL)",
        ] {
            let canon = canonicalize_predicate(expr).expect("canonicalizes");
            assert!(
                canonicalize_predicate(&canon).as_deref() == Some(canon.as_str()),
                "not idempotent: {expr} -> {canon}"
            );
        }
    }

    #[test]
    fn unparseable_expression_is_none() {
        assert_eq!(canonicalize_predicate("this is not )( sql"), None);
    }
}
