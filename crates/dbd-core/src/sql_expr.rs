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
//! by the tree rewrites below, which erase exactly that analysis:
//!
//! 1. `= ANY (ARRAY[…])` → `IN (…)` (and `<> ALL (ARRAY[…])` → `NOT IN (…)`)
//! 2. a type cast on a *literal* is dropped: `'active'::text` → `'active'`
//! 3. `BETWEEN` is expanded to the comparison pair Postgres stores in its place:
//!    `n BETWEEN 1 AND 7` → `n >= 1 AND n <= 7`
//! 4. a uniform per-element cast is lifted onto its `ARRAY[…]` constructor,
//!    undoing the push-down Postgres performs:
//!    `ARRAY[1::smallint, 2::smallint]` → `ARRAY[1, 2]::smallint[]`
//! 5. a same-operator `AND`/`OR` chain is flattened in every argument position:
//!    `AND(a, AND(b, c))` → `AND(a, b, c)`
//!
//! All five normalize the introspected form toward the shorter authored one — or,
//! for 3 and 5, both sides toward the form Postgres itself stores — and all run
//! on both sides, so an authored `= ANY (ARRAY[…])` converges too.
//!
//! **The canonical form is also emitted.** It is not only a comparison key: it
//! becomes the `ADD CHECK (…)` that `dbd diff` builds from the normalized
//! snapshot. So a rewrite here may not erase anything that changes what the SQL
//! *means* — which is why rewrite 4 lifts rather than pushes down. Pushing the
//! cast onto the elements would put it exactly where rewrite 2 erases it, and
//! `days <@ ARRAY[1,2,3]::smallint[]` would be emitted as
//! `days <@ ARRAY[1,2,3]`, which Postgres rejects on a `smallint[]` column.
//! (Index predicates are held to the same rule from the other side: the parser
//! and the adapter store the text as written, and `schema_diff::normalize_index`
//! canonicalizes a copy for matching.)
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
use pg_query::protobuf::{AArrayExpr, AExpr, AExprKind, BoolExpr, BoolExprType, Node, TypeCast, TypeName};

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
    between_to_comparisons(node);
    lift_array_element_casts(node);

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

    // Last, because expanding a `BETWEEN` child above is what creates the
    // nesting this collapses.
    flatten_bool_chain(node);
}

/// Splice a same-operator `BoolExpr` into its parent's argument list, in any
/// position: `AND(a, AND(b, c))` → `AND(a, b, c)`.
///
/// `AND`/`OR` are associative, and neither of the two forms dbd compares
/// preserves the grouping. The design side loses it in the grammar, which
/// flattens a *leading* chain on sight (`makeAndExpr`); the live side loses it in
/// `pg_get_constraintdef(oid, TRUE)` — the pretty spelling the adapter reads,
/// which drops the parens around a nested same-operator child wherever it sits.
/// So `check (a > 0 and (b > 0 and c > 0))` is reported back as
/// `a > 0 AND b > 0 AND c > 0`, and only flattening every position lets the
/// authored form meet it.
///
/// This matters most to rewrite 3: it builds its expansion *after* parsing, so
/// the result never passes through the grammar's flattening at all. A CHECK whose
/// `BETWEEN` was not the first conjunct — `check (c > 0 and n between 1 and 7)` —
/// therefore stayed permanently drifted even though its `BETWEEN` expanded
/// correctly.
///
/// A *different* operator is a real grouping and is left alone: `(a AND b) OR c`
/// keeps its parens, and must never collapse into `a OR b OR c`.
fn flatten_bool_chain(node: &mut Node) {
    let Some(NodeEnum::BoolExpr(outer)) = node.node.as_mut() else {
        return;
    };
    // `NOT` takes a single argument — there is no chain to flatten.
    let boolop = outer.boolop;
    if !matches!(
        BoolExprType::try_from(boolop).unwrap_or(BoolExprType::Undefined),
        BoolExprType::AndExpr | BoolExprType::OrExpr
    ) {
        return;
    }
    let mut i = 0;
    while i < outer.args.len() {
        let same_op = matches!(
            outer.args[i].node.as_ref(),
            Some(NodeEnum::BoolExpr(inner)) if inner.boolop == boolop
        );
        if !same_op {
            i += 1;
            continue;
        }
        let Some(NodeEnum::BoolExpr(inner)) = outer.args[i].node.take() else {
            unreachable!("just matched a BoolExpr")
        };
        // Do not advance: a spliced-in child may itself be a same-operator chain.
        // Each splice removes one wrapper, so the loop strictly shrinks and ends.
        outer.args.splice(i..=i, inner.args);
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

/// Expand `BETWEEN` into the comparison pair Postgres stores in its place.
///
/// `pg_get_constraintdef` never reports a `BETWEEN`: the parse analysis that runs
/// before a constraint is stored rewrites it into `>=`/`<=` — duplicating the
/// tested expression, exactly as reproduced here — so an authored `BETWEEN` left
/// intact could never equal its own introspected form. All four spellings are
/// expanded the way Postgres expands them:
///
/// | authored | stored |
/// |---|---|
/// | `n BETWEEN a AND b` | `n >= a AND n <= b` |
/// | `n NOT BETWEEN a AND b` | `n < a OR n > b` |
/// | `n BETWEEN SYMMETRIC a AND b` | `(n >= a AND n <= b) OR (n >= b AND n <= a)` |
/// | `n NOT BETWEEN SYMMETRIC a AND b` | `(n < a OR n > b) AND (n < b OR n > a)` |
fn between_to_comparisons(node: &mut Node) {
    let Some(NodeEnum::AExpr(expr)) = node.node.as_ref() else {
        return;
    };
    let kind = AExprKind::try_from(expr.kind).unwrap_or(AExprKind::Undefined);
    if !is_between(kind) {
        return;
    }
    let Some(tested) = expr.lexpr.as_deref() else {
        return;
    };
    // The bounds always arrive as a two-item List. Any other shape is one dbd
    // has no expansion for, so leave it to read as changed rather than guess.
    let Some(NodeEnum::List(bounds)) = expr.rexpr.as_deref().and_then(|n| n.node.as_ref()) else {
        return;
    };
    let [lo, hi] = bounds.items.as_slice() else {
        return;
    };

    // This rewrite DUPLICATES its operands — the tested expression twice, and for
    // `SYMMETRIC` each bound twice as well. That is what Postgres does, and it is
    // harmless once. Nested, it squares: a `BETWEEN` inside another's duplicated
    // operand multiplies the tree by 2^k (4^k symmetric), and `normalize` then
    // walks every copy. Eight levels — a 300-byte `ddl/` file libpg_query parses
    // happily — already yields a megabyte; eleven never returns, hanging every
    // command that reads the design directory with no output naming the file.
    //
    // So refuse when an operand this rewrite would copy holds a `BETWEEN` of its
    // own. Expansion then stays linear, every shape `pg_get_constraintdef` can
    // emit is still expanded (Postgres itself stores nothing nested — it expanded
    // it at constraint creation), and the pathological input falls through to the
    // module's documented fail-safe: left verbatim, and read as changed.
    if contains_between(tested) || contains_between(lo) || contains_between(hi) {
        return;
    }

    let at = expr.location;
    let cmp = |op: &str, bound: &Node| compare(op, tested, bound, at);
    let within = |lo: &Node, hi: &Node| join(BoolExprType::AndExpr, vec![cmp(">=", lo), cmp("<=", hi)], at);
    let outside = |lo: &Node, hi: &Node| join(BoolExprType::OrExpr, vec![cmp("<", lo), cmp(">", hi)], at);

    let expanded = match kind {
        AExprKind::AexprBetween => within(lo, hi),
        AExprKind::AexprNotBetween => outside(lo, hi),
        AExprKind::AexprBetweenSym => join(BoolExprType::OrExpr, vec![within(lo, hi), within(hi, lo)], at),
        // AexprNotBetweenSym — the match above admits nothing else.
        _ => join(BoolExprType::AndExpr, vec![outside(lo, hi), outside(hi, lo)], at),
    };
    *node = expanded;
}

/// The four `BETWEEN` spellings, which all expand rather than deparse as written.
fn is_between(kind: AExprKind) -> bool {
    matches!(
        kind,
        AExprKind::AexprBetween
            | AExprKind::AexprNotBetween
            | AExprKind::AexprBetweenSym
            | AExprKind::AexprNotBetweenSym
    )
}

/// Whether this subtree holds a `BETWEEN` anywhere — the guard that keeps
/// [`between_to_comparisons`] from duplicating a subtree that will itself
/// duplicate. Walks the same node kinds [`normalize`] does; an unwalked kind
/// cannot contain a rewritable `BETWEEN` for the same reason it is not
/// normalized.
fn contains_between(node: &Node) -> bool {
    let Some(inner) = node.node.as_ref() else {
        return false;
    };
    match inner {
        NodeEnum::AExpr(e) => {
            is_between(AExprKind::try_from(e.kind).unwrap_or(AExprKind::Undefined))
                || [e.lexpr.as_deref(), e.rexpr.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(contains_between)
        }
        NodeEnum::BoolExpr(e) => e.args.iter().any(contains_between),
        NodeEnum::CoalesceExpr(e) => e.args.iter().any(contains_between),
        NodeEnum::FuncCall(f) => f.args.iter().any(contains_between),
        NodeEnum::AArrayExpr(a) => a.elements.iter().any(contains_between),
        NodeEnum::List(l) => l.items.iter().any(contains_between),
        NodeEnum::RowExpr(r) => r.args.iter().any(contains_between),
        NodeEnum::NullTest(t) => t.arg.as_deref().is_some_and(contains_between),
        NodeEnum::BooleanTest(t) => t.arg.as_deref().is_some_and(contains_between),
        NodeEnum::TypeCast(c) => c.arg.as_deref().is_some_and(contains_between),
        _ => false,
    }
}

/// A binary comparison `left <op> right` as a raw parse tree would carry it.
fn compare(op: &str, left: &Node, right: &Node, location: i32) -> Node {
    Node {
        node: Some(NodeEnum::AExpr(Box::new(AExpr {
            kind: AExprKind::AexprOp as i32,
            name: vec![Node {
                node: Some(NodeEnum::String(pg_query::protobuf::String { sval: op.to_string() })),
            }],
            lexpr: Some(Box::new(left.clone())),
            rexpr: Some(Box::new(right.clone())),
            location,
        }))),
    }
}

/// An `AND`/`OR` over `args`.
fn join(boolop: BoolExprType, args: Vec<Node>, location: i32) -> Node {
    Node {
        node: Some(NodeEnum::BoolExpr(Box::new(BoolExpr {
            xpr: None,
            boolop: boolop as i32,
            args,
            location,
        }))),
    }
}

/// Lift a uniform per-element cast up onto the `ARRAY[…]` constructor:
/// `ARRAY[1::smallint, 2::smallint]` → `ARRAY[1, 2]::smallint[]`.
///
/// Postgres pushes a constructor cast DOWN onto the elements when it stores a
/// constraint, so an authored `ARRAY[1,2]::smallint[]` is reported back as
/// `ARRAY[(1)::smallint, (2)::smallint]`. Either direction converges the pair;
/// dbd lifts because the canonical form is not only compared but **emitted** —
/// as an index `WHERE`, and as the `ADD CHECK (…)` `dbd diff` builds from the
/// normalized snapshot. Pushing down puts the casts where rewrite 2 erases them,
/// so `days <@ ARRAY[1,2,3]::smallint[]` canonicalizes to `days <@ ARRAY[1,2,3]`
/// and dbd emits DDL Postgres rejects with
/// `operator does not exist: smallint[] <@ integer[]`. Lifting keeps the type.
///
/// Only a *uniform* cast lifts. Elements cast to different types are not one
/// array cast; an empty constructor carries its type solely in its own cast; and
/// an element that is itself an array would need a second bound. Each of those
/// is left verbatim and so reads as changed, never as falsely equal.
fn lift_array_element_casts(node: &mut Node) {
    let Some(NodeEnum::AArrayExpr(array)) = node.node.as_ref() else {
        return;
    };
    let [first, rest @ ..] = array.elements.as_slice() else {
        return;
    };
    let Some(element_type) = cast_target(first) else {
        return;
    };
    // Compared without source positions: every element of one array carries the
    // same type at a different offset, so a raw `==` on `TypeName` — which
    // includes `location` — reports each as a different type and nothing lifts.
    let wanted = type_key(element_type);
    if rest
        .iter()
        .any(|e| cast_target(e).map(type_key) != Some(wanted.clone()))
    {
        return;
    }
    // An element that is itself an array would make this a second bound, not the
    // same one — and `format_type` has no spelling for that here.
    if array
        .elements
        .iter()
        .filter_map(|e| cast_arg(e))
        .any(|a| matches!(a.node, Some(NodeEnum::AArrayExpr(_))))
    {
        return;
    }

    let bare: Vec<Node> = array.elements.iter().filter_map(|e| cast_arg(e).cloned()).collect();
    if bare.len() != array.elements.len() {
        return;
    }
    let array_type = TypeName {
        // `-1` is the bound Postgres records for an unsized `[]`.
        array_bounds: vec![Node {
            node: Some(NodeEnum::Integer(pg_query::protobuf::Integer { ival: -1 })),
        }],
        ..element_type.clone()
    };
    *node = Node {
        node: Some(NodeEnum::TypeCast(Box::new(TypeCast {
            arg: Some(Box::new(Node {
                node: Some(NodeEnum::AArrayExpr(AArrayExpr {
                    elements: bare,
                    location: array.location,
                })),
            })),
            type_name: Some(array_type),
            location: array.location,
        }))),
    };
}

/// A `TypeName` with every source position zeroed, so two spellings of the same
/// type at different offsets compare equal. Modifiers are included — `varchar(30)`
/// and `varchar(60)` must still differ.
fn type_key(t: &TypeName) -> TypeName {
    let zero = |n: &mut Node| {
        if let Some(NodeEnum::AConst(c)) = n.node.as_mut() {
            c.location = 0;
        }
    };
    let mut t = t.clone();
    t.location = 0;
    t.typmods.iter_mut().for_each(zero);
    t.array_bounds.iter_mut().for_each(zero);
    t
}

/// The `TypeName` a node is cast to, when it is a cast at all.
fn cast_target(node: &Node) -> Option<&TypeName> {
    match node.node.as_ref() {
        Some(NodeEnum::TypeCast(c)) => c.type_name.as_ref(),
        _ => None,
    }
}

/// The value inside a cast, when the node is a cast.
fn cast_arg(node: &Node) -> Option<&Node> {
    match node.node.as_ref() {
        Some(NodeEnum::TypeCast(c)) => c.arg.as_deref(),
        _ => None,
    }
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

    /// Postgres expands `BETWEEN` into comparisons *before* it stores a
    /// constraint, so the authored spelling has to expand the same way or the
    /// CHECK reads as drift on every run. Each introspected form below is
    /// verbatim `pg_get_constraintdef` output for the authored form beside it.
    #[test]
    fn between_converges_with_the_expansion_postgres_stores() {
        assert_converges("n between 1 and 7", "((n >= 1) AND (n <= 7))");
        assert_converges("n not between 1 and 7", "((n < 1) OR (n > 7))");
        assert_converges(
            "n between symmetric 1 and 7",
            "(((n >= 1) AND (n <= 7)) OR ((n >= 7) AND (n <= 1)))",
        );
        assert_converges(
            "n not between symmetric 1 and 7",
            "(((n < 1) OR (n > 7)) AND ((n < 7) OR (n > 1)))",
        );
    }

    /// Expanding `BETWEEN` must not blur the bounds themselves, nor the negation.
    #[test]
    fn different_ranges_stay_different() {
        let seven = canonicalize_predicate("n between 1 and 7").unwrap();
        assert_ne!(seven, canonicalize_predicate("n between 1 and 8").unwrap());
        assert_ne!(seven, canonicalize_predicate("n not between 1 and 7").unwrap());
        assert_ne!(seven, canonicalize_predicate("n between symmetric 1 and 7").unwrap());
    }

    /// Postgres pushes a cast on an `ARRAY[…]` constructor down onto the elements;
    /// dbd converges the two spellings by lifting them back up.
    ///
    /// The direction matters. Pushing DOWN also converges, but rewrite 2 then
    /// erases the pushed casts and the canonical form loses the element type —
    /// and the canonical form is emitted, both as an index `WHERE` and as the
    /// `ADD CHECK (…)` `dbd diff` builds from the normalized snapshot. Lifting UP
    /// converges the same pairs while keeping the type.
    #[test]
    fn an_element_wise_array_cast_lifts_to_the_constructor() {
        assert_converges(
            "days <@ array[1,2,3]::smallint[]",
            "days <@ ARRAY[(1)::smallint, (2)::smallint, (3)::smallint]",
        );
        assert_converges(
            "a <@ array[n, n]::smallint[]",
            "a <@ ARRAY[(n)::smallint, (n)::smallint]",
        );
    }

    /// The element type must survive canonicalization, because the canonical text
    /// is emitted. Erasing `::smallint[]` yields `days <@ ARRAY[1, 2, 3]`, which
    /// Postgres rejects on a `smallint[]` column with
    /// `operator does not exist: smallint[] <@ integer[]`.
    #[test]
    fn an_array_constructors_element_type_survives() {
        assert_eq!(
            canonicalize_predicate("days <@ array[1,2,3]::smallint[]").unwrap(),
            "days <@ ARRAY[1, 2, 3]::smallint[]"
        );
        assert_eq!(
            canonicalize_predicate("days <@ ARRAY[1::smallint, 2::smallint]").unwrap(),
            "days <@ ARRAY[1, 2]::smallint[]"
        );
    }

    /// Only a uniformly-cast constructor lifts. Elements cast to *different*
    /// types are not one array cast, and an empty constructor carries its type
    /// solely in the cast — inventing or moving one there would be a guess.
    #[test]
    fn a_non_uniform_or_empty_array_does_not_lift() {
        let mixed = canonicalize_predicate("a <@ ARRAY[m::smallint, n::integer]").unwrap();
        assert_eq!(mixed, "a <@ ARRAY[m::smallint, n::int]", "mixed types must not lift");
        let empty = canonicalize_predicate("days <@ array[]::smallint[]").unwrap();
        assert_eq!(
            empty, "days <@ ARRAY[]::smallint[]",
            "an empty constructor keeps its cast"
        );
    }

    /// A same-operator chain flattens in EVERY position, because that is the form
    /// dbd actually reads: the adapter takes `pg_get_constraintdef(oid, true)`,
    /// whose pretty printer drops the parens around a nested same-operator child
    /// wherever it sits — `a > 0 AND (b > 0 AND c > 0)` comes back as
    /// `a > 0 AND b > 0 AND c > 0`.
    ///
    /// Pinned as ABSOLUTE output rather than as agreement between two spellings.
    /// The version of this test that shipped in review paired predicates that
    /// parse to structurally identical trees, so every rewrite — including a
    /// corrupt one that spliced an `AND` into an `OR` — was applied to both sides
    /// alike and the test could not fail.
    #[test]
    fn same_operator_chains_flatten_in_every_position() {
        let canon = |e: &str| canonicalize_predicate(e).expect("parses");
        assert_eq!(canon("(a > 0 and b > 0) and c > 0"), "a > 0 AND b > 0 AND c > 0");
        assert_eq!(canon("a > 0 and (b > 0 and c > 0)"), "a > 0 AND b > 0 AND c > 0");
        assert_eq!(canon("(a > 0 or b > 0) or c > 0"), "a > 0 OR b > 0 OR c > 0");
        assert_eq!(canon("a > 0 or (b > 0 or c > 0)"), "a > 0 OR b > 0 OR c > 0");
        // A different operator is a real grouping and must survive verbatim.
        assert_eq!(canon("(a > 0 and b > 0) or c > 0"), "(a > 0 AND b > 0) OR c > 0");
        assert_eq!(canon("a > 0 and (b > 0 or c > 0)"), "a > 0 AND (b > 0 OR c > 0)");
        // …and must never collapse into the flat form of the other operator.
        assert_ne!(canon("(a > 0 and b > 0) or c > 0"), canon("a > 0 or b > 0 or c > 0"));
    }

    /// The introspected side of each pair is verbatim `pg_get_constraintdef(oid,
    /// true)` output — the pretty spelling the adapter reads. A nested chain that
    /// is not the FIRST conjunct is the case a leading-only flatten missed, so
    /// any CHECK whose `BETWEEN` was not written first stayed permanently drifted.
    #[test]
    fn pretty_printed_checks_converge_with_their_authored_form() {
        assert_converges("c > 0 and n between 1 and 7", "c > 0 AND n >= 1 AND n <= 7");
        assert_converges("a > 0 and (b > 0 and c > 0)", "a > 0 AND b > 0 AND c > 0");
        assert_converges(
            "arr <@ array[1,2]::smallint[] and n between 1 and 7",
            "arr <@ ARRAY[1::smallint, 2::smallint] AND n >= 1 AND n <= 7",
        );
    }

    /// The real `sensei.schedules.days` CHECK: a `BETWEEN`, a cast array
    /// constructor and an `OR` in one predicate. First argument is the authored
    /// DDL; the other two are what `pg_get_constraintdef` reports for it, pretty-
    /// printed (which is how the adapter reads it) and raw. This is the pair that
    /// kept `dbd diff --scope default` reporting drift on an in-sync database.
    #[test]
    fn the_schedules_days_check_converges() {
        let authored = "days is null or (array_length(days, 1) between 1 and 7 \
             and days <@ array[1,2,3,4,5,6,7]::smallint[])";
        assert_converges(
            authored,
            "days IS NULL OR array_length(days, 1) >= 1 AND array_length(days, 1) <= 7 \
             AND days <@ ARRAY[1::smallint, 2::smallint, 3::smallint, 4::smallint, \
             5::smallint, 6::smallint, 7::smallint]",
        );
        assert_converges(
            authored,
            "((days IS NULL) OR (((array_length(days, 1) >= 1) AND (array_length(days, 1) <= 7)) \
             AND (days <@ ARRAY[(1)::smallint, (2)::smallint, (3)::smallint, (4)::smallint, \
             (5)::smallint, (6)::smallint, (7)::smallint])))",
        );
    }

    /// Expanding `BETWEEN` duplicates the tested expression (four times for
    /// `SYMMETRIC`), so a `BETWEEN` nested inside another one's tested position
    /// multiplies the tree by 2^k / 4^k. Left unguarded that is a denial of
    /// service reachable from a `ddl/` file: at depth 11 — 390 bytes of DDL that
    /// libpg_query parses happily — `dbd inspect` never returns.
    #[test]
    fn nested_between_does_not_expand_exponentially() {
        let nested = |k: usize| {
            let mut e = "n".to_string();
            for _ in 0..k {
                e = format!("({e} between symmetric 1 and 2)::int");
            }
            e
        };
        let out = canonicalize_predicate(&format!("{} > 0", nested(8)));
        assert!(
            out.as_ref().is_none_or(|s| s.len() < 10_000),
            "nested BETWEEN must not blow up; got {} bytes",
            out.map_or(0, |s| s.len())
        );
    }

    /// The guard must not cost the ordinary cases their expansion.
    #[test]
    fn sibling_betweens_still_expand() {
        assert_converges(
            "a between 1 and 7 and b between 2 and 8",
            "a >= 1 AND a <= 7 AND b >= 2 AND b <= 8",
        );
    }

    #[test]
    fn unparseable_expression_is_none() {
        assert_eq!(canonicalize_predicate("this is not )( sql"), None);
    }
}
