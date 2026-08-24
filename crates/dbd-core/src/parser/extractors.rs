use sqlparser::ast::{Expr, Set, Statement, Value};

use crate::entity::{EnumValue, REF_TYPE_FUNCTION, Reference};

/// Extract search_path values from SET statements.
pub fn extract_search_paths(statements: &[Statement]) -> Vec<String> {
    for stmt in statements {
        if let Statement::Set(Set::SingleAssignment { variable, values, .. }) = stmt {
            let var_name = variable
                .0
                .iter()
                .filter_map(|part| part.as_ident())
                .map(|i| i.value.to_lowercase())
                .collect::<Vec<_>>()
                .join(".");
            if var_name == "search_path" {
                return values
                    .iter()
                    .filter_map(|v| match v {
                        Expr::Identifier(ident) => Some(ident.value.clone()),
                        Expr::Value(val) => match &val.value {
                            Value::SingleQuotedString(s) => Some(s.clone()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    vec!["public".to_string()]
}

/// Extract the objects a VIEW's SELECT query depends on, using the AST.
///
/// Two kinds of reference come out of a view body:
///
/// - **Relations** (tables, views, matviews, and FROM-clause table functions),
///   walked from the query's range tables. Using the AST rather than string
///   matching avoids the `alias.column` false positives a regex produces.
/// - **Function calls** anywhere in the body — target list, WHERE, GROUP BY,
///   a CASE arm. Postgres requires the function to exist before the view is
///   created, so these are genuine apply-order dependencies (issue #9).
///
/// Function references are tagged [`REF_TYPE_FUNCTION`] and resolved softly:
/// a body's built-in and aggregate calls are indistinguishable here from calls
/// to a project-managed function, so `references::resolve_references` keeps the
/// ones that name a known entity and drops the rest without warning.
pub fn extract_view_info(statements: &[Statement], search_paths: &[String]) -> Vec<Reference> {
    let mut references = Vec::new();
    let default_schema = search_paths.first().map(|s| s.as_str()).unwrap_or("public");

    for stmt in statements {
        if let Statement::CreateView(create_view) = stmt {
            extract_table_refs_from_query(&create_view.query, default_schema, &mut references);
            for reference in collect_function_refs(&create_view.query, default_schema) {
                if !references.iter().any(|r| r.name == reference.name) {
                    references.push(reference);
                }
            }
        }
    }

    references
}

/// Extract the SELECT body text of a `CREATE [MATERIALIZED] VIEW`.
///
/// Returns the query verbatim (as `Display`-rendered SQL), matching the
/// `writes[0]` contract the introspector uses for views (see `emit::emit_view`).
pub fn extract_view_body(statements: &[Statement]) -> Option<String> {
    for stmt in statements {
        if let Statement::CreateView(create_view) = stmt {
            return Some(create_view.query.to_string());
        }
    }
    None
}

/// Extract every table reference from a query, anywhere it appears.
///
/// Uses sqlparser's AST visitor to walk the *entire* query, so it sees table
/// references in any position: top-level FROM/JOIN, every branch of a compound
/// query (UNION / UNION ALL / INTERSECT / EXCEPT / MINUS), CTE bodies
/// (`WITH` / `WITH RECURSIVE`), derived tables (subqueries in FROM/JOIN),
/// scalar subqueries in the SELECT list, and IN / NOT IN / EXISTS predicates —
/// at arbitrary nesting depth.
///
/// CTE names are query-local, not real tables, so references that resolve to a
/// CTE (the recursive term, `SELECT ... FROM cte`, etc.) are dropped afterwards.
fn extract_table_refs_from_query(
    query: &sqlparser::ast::Query,
    default_schema: &str,
    refs: &mut Vec<Reference>,
) {
    for reference in collect_table_refs(query, default_schema) {
        if !refs.iter().any(|r| r.name == reference.name) {
            refs.push(reference);
        }
    }
}

/// Walk any AST node, collecting every table reference with query-local CTE
/// references filtered out. Works on a `Query`, a `Statement`, or any other
/// node that implements `Visit`.
///
/// A FROM-clause entry carrying arguments (`FROM app.srf_fn()`) is a call to a
/// set-returning function, not a table, and is tagged [`REF_TYPE_FUNCTION`] so
/// it resolves softly — otherwise every `FROM generate_series(1, 3)` in a view
/// body reports an unresolved reference to a built-in.
fn collect_table_refs<N: sqlparser::ast::Visit>(node: &N, default_schema: &str) -> Vec<Reference> {
    let mut visitor = TableRefVisitor {
        default_schema,
        cte_names: Vec::new(),
        table_functions: Vec::new(),
        refs: Vec::new(),
    };
    let _ = node.visit(&mut visitor);

    // CTE references are unqualified, so they resolve to `{default_schema}.{name}`.
    // Drop those — they're query-local, not real tables.
    visitor
        .refs
        .into_iter()
        .filter(|r| {
            !visitor
                .cte_names
                .iter()
                .any(|name| r.name == format!("{default_schema}.{name}"))
        })
        .map(|mut r| {
            if visitor.table_functions.contains(&r.name) {
                r.ref_type = Some(REF_TYPE_FUNCTION.to_string());
            }
            r
        })
        .collect()
}

/// Qualify a relation `ObjectName` to `schema.table`: apply the default schema
/// to unqualified names, and drop references into system schemas.
fn qualify_relation(name: &sqlparser::ast::ObjectName, default_schema: &str) -> Option<String> {
    let parts: Vec<&str> = name
        .0
        .iter()
        .filter_map(|p| p.as_ident())
        .map(|i| i.value.as_str())
        .collect();

    if parts.len() >= 2 {
        if SYSTEM_SCHEMAS.contains(&parts[0]) {
            return None;
        }
        Some(format!("{}.{}", parts[0], parts[1]))
    } else {
        parts.first().map(|table| format!("{default_schema}.{table}"))
    }
}

/// Collect every function/procedure call in an AST node as a soft reference.
///
/// Deliberately separate from [`collect_table_refs`]: that walker also feeds
/// `Entity::reads`/`writes` for functions and procedures, which downstream
/// consumers (import planning, scope analysis) read as *table* sets. Keeping
/// function calls out of it means a caller opts in.
///
/// Works on any `Visit` node, so callers can pass a whole query or a single
/// expression — a column `DEFAULT`, a `CHECK`, a generated-column expression or
/// an index key (see `tables::extract_table`).
pub(super) fn collect_function_refs<N: sqlparser::ast::Visit>(
    node: &N,
    default_schema: &str,
) -> Vec<Reference> {
    let mut visitor = FunctionRefVisitor {
        default_schema,
        refs: Vec::new(),
    };
    let _ = node.visit(&mut visitor);
    visitor.refs
}

/// AST visitor that collects called function names from every expression
/// position (target list, WHERE, GROUP BY, CASE arms, subqueries).
struct FunctionRefVisitor<'a> {
    default_schema: &'a str,
    refs: Vec<Reference>,
}

impl sqlparser::ast::Visitor for FunctionRefVisitor<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> core::ops::ControlFlow<Self::Break> {
        if let Expr::Function(func) = expr
            && let Some(qualified) = qualify_relation(&func.name, self.default_schema)
            && !self.refs.iter().any(|r| r.name == qualified)
        {
            self.refs.push(Reference {
                name: qualified,
                ref_type: Some(REF_TYPE_FUNCTION.to_string()),
            });
        }
        core::ops::ControlFlow::Continue(())
    }
}

/// AST visitor that collects table references, CTE names and FROM-clause
/// function calls in a single pass.
struct TableRefVisitor<'a> {
    default_schema: &'a str,
    cte_names: Vec<String>,
    table_functions: Vec<String>,
    refs: Vec<Reference>,
}

impl sqlparser::ast::Visitor for TableRefVisitor<'_> {
    type Break = ();

    /// Record CTE names so their (query-local) references can be filtered out.
    fn pre_visit_query(
        &mut self,
        query: &sqlparser::ast::Query,
    ) -> core::ops::ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.cte_names.push(cte.alias.name.value.clone());
            }
        }
        core::ops::ControlFlow::Continue(())
    }

    /// Note FROM-clause entries that carry arguments: those are set-returning
    /// function calls, which `pre_visit_relation` cannot tell apart from a plain
    /// table (it only receives the name). `args` is `None` for a real table.
    fn pre_visit_table_factor(
        &mut self,
        table_factor: &sqlparser::ast::TableFactor,
    ) -> core::ops::ControlFlow<Self::Break> {
        if let sqlparser::ast::TableFactor::Table {
            name,
            args: Some(_),
            ..
        } = table_factor
            && let Some(qualified) = qualify_relation(name, self.default_schema)
        {
            self.table_functions.push(qualified);
        }
        core::ops::ControlFlow::Continue(())
    }

    /// Collect each relation (table) referenced anywhere in the query.
    fn pre_visit_relation(
        &mut self,
        name: &sqlparser::ast::ObjectName,
    ) -> core::ops::ControlFlow<Self::Break> {
        if let Some(qualified) = qualify_relation(name, self.default_schema)
            && !self.refs.iter().any(|r| r.name == qualified)
        {
            self.refs.push(Reference {
                name: qualified,
                ref_type: Some("table".to_string()),
            });
        }
        core::ops::ControlFlow::Continue(())
    }
}

/// Extract enum values from CREATE TYPE ... AS ENUM statements.
pub fn extract_enum_values(statements: &[Statement]) -> Vec<EnumValue> {
    for stmt in statements {
        if let Statement::CreateType { representation, .. } = stmt
            && let Some(sqlparser::ast::UserDefinedTypeRepresentation::Enum { labels }) = representation {
                return labels
                    .iter()
                    .map(|label| EnumValue {
                        name: label.value.clone(),
                        note: None,
                    })
                    .collect();
            }
    }
    Vec::new()
}

/// Enum labels from a `DO $$ … $$` guarded `CREATE TYPE … AS ENUM`, read off
/// libpg_query's AST (Postgres's own parser).
///
/// Postgres has no `CREATE TYPE IF NOT EXISTS`, so wrapping the CREATE in a DO
/// block that swallows `duplicate_object` is the only idiom available for a
/// conditional enum — and sqlparser rejects `DO` outright. This is the same
/// second tier [`extract_proc_refs`] already uses for PL/pgSQL bodies: parse the
/// block, take the SQL it embeds, and re-parse that into a real statement tree.
///
/// Returns an empty vec when the input is not a PL/pgSQL block libpg_query
/// accepts, or when it declares no enum — the caller keeps its parse error, so a
/// genuinely broken file is never quietly waved through.
pub fn extract_enum_values_via_pg_query(raw_sql: &str) -> Vec<EnumValue> {
    let Ok(tree) = pg_query::parse_plpgsql(raw_sql) else {
        return Vec::new();
    };

    let mut queries = Vec::new();
    collect_plpgsql_queries(&tree, &mut queries);

    for query in &queries {
        let Ok(parsed) = pg_query::parse(query) else {
            continue;
        };
        let values = crate::parser::pg::enums::labels_from_parse_result(&parsed);
        if !values.is_empty() {
            return values;
        }
    }
    Vec::new()
}

/// Whether libpg_query — Postgres's own grammar — accepts this file.
///
/// The authority on "is this valid SQL". sqlparser is a convenience parser that
/// reimplements the grammar and lags the server, so its rejection alone says
/// nothing about the file; this says whether Postgres itself would take it.
pub fn is_valid_postgres(raw_sql: &str) -> bool {
    pg_query::parse(raw_sql).is_ok()
}

/// `search_path` schemas from libpg_query's AST, for the fallback path where
/// sqlparser produced no statements for [`extract_search_paths`] to read.
///
/// Mirrors that function's contract, including its `["public"]` default when the
/// file sets no search path. Recovering this is not optional: reads and view
/// references are qualified against it, so an empty list silently re-qualifies
/// `t` to `public.t` — a plausibly-wrong edge pointing at a different table,
/// which is worse than no edge at all.
pub fn extract_search_paths_via_pg_query(raw_sql: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(raw_sql) else {
        return vec![DEFAULT_SEARCH_PATH.to_string()];
    };
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::VariableSetStmt(set)) =
            stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        if !set.name.eq_ignore_ascii_case("search_path") {
            continue;
        }
        let paths: Vec<String> = set.args.iter().filter_map(const_str).collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    vec![DEFAULT_SEARCH_PATH.to_string()]
}

/// The default schema when a file sets no `search_path` — matches
/// [`extract_search_paths`].
const DEFAULT_SEARCH_PATH: &str = "public";

/// The string behind a `SET` argument node: a bare identifier arrives as a
/// `ColumnRef` (`to app`), a quoted one as an `A_Const` string (`to 'app'`).
fn const_str(node: &pg_query::protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        pg_query::NodeEnum::String(s) => Some(s.sval.clone()),
        pg_query::NodeEnum::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Sval(s) => Some(s.sval.clone()),
            _ => None,
        },
        pg_query::NodeEnum::ColumnRef(r) => r.fields.first().and_then(const_str),
        _ => None,
    }
}

/// The tables a view's body reads, from libpg_query's AST — the fallback for a
/// view [`extract_view_info`] could not read because sqlparser rejected the file.
///
/// Without this a recovered view carries no dependency edge at all, so it could
/// be applied before the table it selects from: a loud skip traded for a silent
/// misordering.
pub fn extract_view_refs_via_pg_query(raw_sql: &str, default_schema: &str) -> Vec<Reference> {
    let Ok(parsed) = pg_query::parse(raw_sql) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for table in parsed.select_tables() {
        if let Some(name) = qualify_name_str(&table, default_schema) {
            push_unique(&mut names, name);
        }
    }
    names
        .into_iter()
        .map(|name| Reference {
            name,
            ref_type: Some("table".to_string()),
        })
        .collect()
}

/// What a function or procedure body depends on.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProcRefs {
    /// Tables and views the body reads.
    pub reads: Vec<String>,
    /// Tables and views the body writes.
    pub writes: Vec<String>,
    /// Functions the body calls. Soft references (see [`REF_TYPE_FUNCTION`]):
    /// a body's built-in calls are indistinguishable from calls to a
    /// project-managed function until the resolver runs.
    pub functions: Vec<String>,
}

/// Extract the dependencies of a function/procedure body.
///
/// Three tiers, tried in order:
/// 1. `LANGUAGE sql` bodies → re-parsed with sqlparser and walked by the view
///    visitor (sub-selects, set operations, CTEs, exact read/write split).
/// 2. PL/pgSQL bodies → parsed with libpg_query (Postgres's own parser), which
///    sees through `SELECT ... INTO`, `PERFORM`, `FOR ... IN ... LOOP`,
///    `RETURN QUERY`, and `IF` conditions.
/// 3. Anything neither parser accepts → regex scan of the raw text.
///
/// Called functions are collected from tier 1 only, and that is deliberate:
/// Postgres validates a `LANGUAGE sql` body when the routine is created (with
/// the default `check_function_bodies = on`), so a function it calls must exist
/// first. A PL/pgSQL body resolves names at run time instead, so its calls are
/// not creation-order dependencies.
///
/// Dynamic SQL (`EXECUTE 'text'`) is intentionally NOT tracked: it is a runtime
/// dependency, not a compile-time one, so it never affects whether the object
/// can be created. Tiers 1 and 2 ignore it for free (it is a string literal);
/// the tier-3 regex inherits the existing best-effort behavior.
pub fn extract_proc_refs(
    statements: &[Statement],
    raw_sql: &str,
    default_schema: &str,
) -> ProcRefs {
    // 1. `LANGUAGE sql` bodies: re-parse with sqlparser + the view visitor.
    if let Some(refs) = extract_proc_refs_via_ast(statements, default_schema) {
        return refs;
    }
    // 2. PL/pgSQL: parse with libpg_query (the real Postgres parser).
    if let Some((reads, writes)) = extract_proc_refs_via_pg_query(raw_sql, default_schema) {
        return ProcRefs { reads, writes, functions: Vec::new() };
    }
    // 3. Last resort: regex scan (bodies neither parser can handle).
    let (reads, writes) = extract_proc_reads_writes(raw_sql);
    ProcRefs { reads, writes, functions: Vec::new() }
}

/// Extract reads/writes from a PL/pgSQL body using libpg_query (Postgres's own
/// parser), which cleanly separates embedded SQL from PL/pgSQL control flow
/// (`SELECT ... INTO`, `PERFORM`, `FOR ... IN ... LOOP`, `RETURN QUERY`, `IF`).
///
/// Returns `None` when the input isn't a PL/pgSQL routine libpg_query can parse
/// (e.g. a `LANGUAGE sql` body, or invalid PL/pgSQL), so the caller falls back.
///
/// Dynamic SQL (`EXECUTE '...'`) is ignored: the embedded text is a string
/// literal, so re-parsing it yields a constant with no table references.
fn extract_proc_refs_via_pg_query(
    raw_sql: &str,
    default_schema: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let tree = pg_query::parse_plpgsql(raw_sql).ok()?;

    let mut queries = Vec::new();
    collect_plpgsql_queries(&tree, &mut queries);

    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for query in &queries {
        // A `query` may be a full statement, or a bare expression (e.g. an `IF`
        // condition). Parse it directly, else `SELECT`-wrap it so any subqueries
        // are still seen. A dynamic-SQL string literal yields no tables either way.
        let Ok(parsed) =
            pg_query::parse(query).or_else(|_| pg_query::parse(&format!("SELECT {query}")))
        else {
            continue;
        };
        for table in parsed.select_tables() {
            if let Some(name) = qualify_name_str(&table, default_schema) {
                push_unique(&mut reads, name);
            }
        }
        for table in parsed.dml_tables() {
            if let Some(name) = qualify_name_str(&table, default_schema) {
                push_unique(&mut writes, name);
            }
        }
    }
    Some((reads, writes))
}

/// Recursively collect every embedded SQL `query` string from a parsed PL/pgSQL
/// JSON tree (libpg_query stores each statement's SQL under a `"query"` key).
fn collect_plpgsql_queries(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                if key == "query"
                    && let Some(s) = val.as_str()
                    && !s.is_empty()
                {
                    out.push(s.to_string());
                }
                collect_plpgsql_queries(val, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_plpgsql_queries(item, out);
            }
        }
        _ => {}
    }
}

/// Qualify a `schema.table` / `table` string (as returned by libpg_query):
/// apply the default schema to unqualified names, drop system-schema refs.
fn qualify_name_str(name: &str, default_schema: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('.').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [.., schema, table] => {
            if SYSTEM_SCHEMAS.contains(schema) {
                return None;
            }
            Some(format!("{schema}.{table}"))
        }
        [table] => Some(format!("{default_schema}.{table}")),
        _ => None,
    }
}

/// Try to extract a `LANGUAGE sql` function body's dependencies via the AST.
///
/// Returns `Some` once at least one statically-parseable SQL body is found
/// (even if it references nothing), so the caller knows the AST path applied
/// and the regex fallback should be skipped. Returns `None` when no body could
/// be parsed as SQL (e.g. PL/pgSQL), leaving the caller to fall back.
fn extract_proc_refs_via_ast(
    statements: &[Statement],
    default_schema: &str,
) -> Option<ProcRefs> {
    let dialect = sqlparser::dialect::PostgreSqlDialect {};
    let mut refs = ProcRefs::default();
    let mut parsed_any = false;

    for stmt in statements {
        let Statement::CreateFunction(cf) = stmt else {
            continue;
        };
        // Only LANGUAGE sql bodies are statically-parseable SQL; PL/pgSQL is not.
        let is_sql = cf
            .language
            .as_ref()
            .is_some_and(|l| l.value.eq_ignore_ascii_case("sql"));
        if !is_sql {
            continue;
        }
        let Some(body_text) = function_body_text(cf) else {
            continue;
        };
        let Ok(body_stmts) = sqlparser::parser::Parser::parse_sql(&dialect, body_text) else {
            continue;
        };

        parsed_any = true;
        for body_stmt in &body_stmts {
            classify_body_refs(body_stmt, default_schema, &mut refs);
            // Calls in expression position (target list, WHERE, CASE arms).
            for reference in collect_function_refs(body_stmt, default_schema) {
                push_unique(&mut refs.functions, reference.name);
            }
        }
    }

    parsed_any.then_some(refs)
}

/// Sort one body statement's references into reads / writes / called functions
/// (a reference that is also a write target counts as a write), and record write
/// targets that never surface as a visited relation (e.g. an INSERT target).
///
/// A FROM-clause entry that is really a function call (`FROM generate_series(…)`)
/// arrives tagged [`REF_TYPE_FUNCTION`] and must not be filed as a table read —
/// otherwise it is reported as an unresolved reference to a missing table.
fn classify_body_refs(body_stmt: &Statement, default_schema: &str, refs: &mut ProcRefs) {
    let targets = proc_write_targets(body_stmt, default_schema);
    for reference in collect_table_refs(body_stmt, default_schema) {
        if reference.ref_type.as_deref() == Some(REF_TYPE_FUNCTION) {
            push_unique(&mut refs.functions, reference.name);
        } else if targets.contains(&reference.name) {
            push_unique(&mut refs.writes, reference.name);
        } else {
            push_unique(&mut refs.reads, reference.name);
        }
    }
    for target in targets {
        push_unique(&mut refs.writes, target);
    }
}

/// Extract the raw SQL text of a function body (dollar-quoted or string literal).
fn function_body_text(cf: &sqlparser::ast::CreateFunction) -> Option<&str> {
    use sqlparser::ast::{CreateFunctionBody, Expr, Value};

    let body_expr = match cf.function_body.as_ref()? {
        CreateFunctionBody::AsBeforeOptions { body, .. } => body,
        CreateFunctionBody::AsAfterOptions(body) => body,
        _ => return None,
    };
    if let Expr::Value(vws) = body_expr {
        match &vws.value {
            Value::DollarQuotedString(dq) => return Some(&dq.value),
            Value::SingleQuotedString(s) => return Some(s),
            _ => {}
        }
    }
    None
}

/// Identify the table(s) a statement writes to (INSERT/UPDATE/DELETE targets).
fn proc_write_targets(stmt: &Statement, default_schema: &str) -> Vec<String> {
    use sqlparser::ast::{FromTable, TableFactor, TableObject};

    let mut targets = Vec::new();
    match stmt {
        Statement::Insert(insert) => {
            if let TableObject::TableName(name) = &insert.table
                && let Some(q) = qualify_relation(name, default_schema)
            {
                push_unique(&mut targets, q);
            }
        }
        Statement::Update(update) => {
            if let TableFactor::Table { name, .. } = &update.table.relation
                && let Some(q) = qualify_relation(name, default_schema)
            {
                push_unique(&mut targets, q);
            }
        }
        Statement::Delete(delete) => {
            let twjs = match &delete.from {
                FromTable::WithFromKeyword(v) | FromTable::WithoutKeyword(v) => v,
            };
            for twj in twjs {
                if let TableFactor::Table { name, .. } = &twj.relation
                    && let Some(q) = qualify_relation(name, default_schema)
                {
                    push_unique(&mut targets, q);
                }
            }
            for name in &delete.tables {
                if let Some(q) = qualify_relation(name, default_schema) {
                    push_unique(&mut targets, q);
                }
            }
        }
        _ => {}
    }
    targets
}

/// Push a value only if not already present (preserves insertion order).
fn push_unique(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// Last-resort reads/writes scanner for bodies neither parser accepts.
///
/// `extract_proc_refs` handles `LANGUAGE sql` bodies via sqlparser and PL/pgSQL
/// via libpg_query; this regex scan runs only when both fail (e.g. a body that
/// is invalid PL/pgSQL, or an unsupported routine form).
///
/// Caveats: only matches qualified `schema.table` names (misses unqualified
/// ones), can over-match (e.g. `EXTRACT(... FROM x.y)` or tables inside an
/// `EXECUTE '...'` dynamic-SQL literal), and is blind to read/write
/// classification beyond the leading keyword. The libpg_query path has none of
/// these issues, so this is reached rarely.
///
/// Patterns matched:
/// - SELECT ... FROM schema.table → read
/// - JOIN schema.table → read
/// - INSERT INTO schema.table → write
/// - UPDATE schema.table → write
/// - DELETE FROM schema.table → write
pub fn extract_proc_reads_writes(sql: &str) -> (Vec<String>, Vec<String>) {
    let mut reads = Vec::new();
    let mut writes = Vec::new();

    let lower = sql.to_lowercase();

    // Extract table names after FROM (reads)
    for cap in regex_table_after(&lower, r"(?i)\bfrom\s+") {
        if !reads.contains(&cap) {
            reads.push(cap);
        }
    }

    // Extract table names after JOIN (reads)
    for cap in regex_table_after(&lower, r"(?i)\bjoin\s+") {
        if !reads.contains(&cap) {
            reads.push(cap);
        }
    }

    // Extract table names after INSERT INTO (writes)
    for cap in regex_table_after(&lower, r"(?i)\binsert\s+into\s+") {
        if !writes.contains(&cap) {
            writes.push(cap);
        }
    }

    // Extract table names after UPDATE (writes)
    for cap in regex_table_after(&lower, r"(?i)\bupdate\s+") {
        if !writes.contains(&cap) {
            writes.push(cap);
        }
    }

    // Extract table names after DELETE FROM (writes)
    for cap in regex_table_after(&lower, r"(?i)\bdelete\s+from\s+") {
        if !writes.contains(&cap) {
            writes.push(cap);
        }
    }

    (reads, writes)
}

/// System schemas to exclude from references.
const SYSTEM_SCHEMAS: &[&str] = &["information_schema", "pg_catalog", "pg_toast"];

/// Find qualified table names (schema.table) after a SQL keyword pattern.
/// Excludes system schema references.
fn regex_table_after(sql: &str, pattern: &str) -> Vec<String> {
    let re = regex::Regex::new(&format!(r"{pattern}([a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*)"))
        .unwrap();
    re.captures_iter(sql)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|name| {
            let schema = name.split('.').next().unwrap_or("");
            !SYSTEM_SCHEMAS.contains(&schema)
        })
        .collect()
}


#[cfg(test)]
mod tests {
    use super::*;


    /// Parse a CREATE FUNCTION/PROCEDURE and extract its read/write deps.
    ///
    /// Mirrors the real pipeline: when sqlparser can't parse the statement
    /// (e.g. `RETURNS SETOF`), `statements` is empty and extraction relies on
    /// the libpg_query / regex paths driven by the raw text.
    fn proc_refs(sql: &str) -> (Vec<String>, Vec<String>) {
        let refs = proc_all_refs(sql);
        (refs.reads, refs.writes)
    }

    /// Like `proc_refs`, but keeps the called-function set too.
    fn proc_all_refs(sql: &str) -> ProcRefs {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(&dialect, sql).unwrap_or_default();
        extract_proc_refs(&stmts, sql, "public")
    }

    #[test]
    fn guarded_enum_values_read_off_the_pg_query_ast() {
        let values = extract_enum_values_via_pg_query(
            "do $$ begin\n  create type status_t as enum ('active', 'archived');\n\
             exception when duplicate_object then null;\nend $$;",
        );
        let names: Vec<&str> = values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["active", "archived"]);
    }

    #[test]
    fn pg_query_enum_fallback_is_empty_when_no_enum_is_declared() {
        assert!(extract_enum_values_via_pg_query("do $$ begin perform 1; end $$;").is_empty());
        assert!(extract_enum_values_via_pg_query("NOT SQL AT ALL ;;;").is_empty());
    }

    #[test]
    fn proc_sql_body_extracts_reads_via_ast() {
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ \
             SELECT id FROM real.orders WHERE id IN (SELECT id FROM real.allow) $$;",
        );
        assert!(reads.contains(&"real.orders".to_string()), "reads={reads:?}");
        assert!(reads.contains(&"real.allow".to_string()), "reads={reads:?}");
        assert!(writes.is_empty(), "writes={writes:?}");
    }

    #[test]
    fn proc_sql_body_resolves_unqualified_with_default_schema() {
        // Differentiator vs regex: unqualified table resolves to default schema.
        let (reads, _) = proc_refs(
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT id FROM orders $$;",
        );
        assert!(reads.contains(&"public.orders".to_string()), "reads={reads:?}");
    }

    #[test]
    fn proc_sql_body_classifies_insert_write_and_select_read() {
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$ \
             INSERT INTO sink.t SELECT id FROM (SELECT id FROM nested.src) s $$;",
        );
        assert!(writes.contains(&"sink.t".to_string()), "writes={writes:?}");
        assert!(reads.contains(&"nested.src".to_string()), "reads={reads:?}");
        assert!(!reads.contains(&"sink.t".to_string()), "write target leaked into reads: {reads:?}");
    }

    #[test]
    fn proc_sql_body_classifies_update_and_delete() {
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$ \
             UPDATE tgt.t SET x = 1 FROM ref.r WHERE t.id = r.id; \
             DELETE FROM del.t USING use.u WHERE t.id = u.id; $$;",
        );
        assert!(writes.contains(&"tgt.t".to_string()), "writes={writes:?}");
        assert!(writes.contains(&"del.t".to_string()), "writes={writes:?}");
        assert!(reads.contains(&"ref.r".to_string()), "reads={reads:?}");
        assert!(reads.contains(&"use.u".to_string()), "reads={reads:?}");
    }

    #[test]
    fn proc_sql_body_no_false_positive_from_extract() {
        // Differentiator vs regex: EXTRACT(... FROM col) must NOT yield a table.
        let (reads, _) = proc_refs(
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ \
             SELECT EXTRACT(epoch FROM ts.created) FROM real.events $$;",
        );
        assert!(reads.contains(&"real.events".to_string()), "reads={reads:?}");
        assert!(
            !reads.iter().any(|r| r.ends_with(".created")),
            "EXTRACT false positive: {reads:?}"
        );
    }

    #[test]
    fn proc_sql_body_collects_called_functions_not_reads() {
        // A `LANGUAGE sql` body is validated at CREATE time, so a function it
        // calls is a creation-order dependency.
        let refs = proc_all_refs(
            "CREATE FUNCTION f(n int) RETURNS int LANGUAGE sql AS $$ \
             SELECT app.helper(n) + 1 FROM real.t $$;",
        );
        assert!(refs.functions.contains(&"app.helper".to_string()), "{refs:?}");
        assert!(refs.reads.contains(&"real.t".to_string()), "{refs:?}");
        assert!(
            !refs.reads.contains(&"app.helper".to_string()),
            "a called function must not be filed as a table read: {refs:?}"
        );
    }

    #[test]
    fn proc_sql_body_from_clause_function_is_not_a_read() {
        // `FROM websearch_to_tsquery(...)` is a set-returning function call, not
        // a missing table — filing it under reads reports it as unresolved.
        let refs = proc_all_refs(
            "CREATE FUNCTION f(q text) RETURNS int LANGUAGE sql AS $$ \
             SELECT count(*) FROM real.docs d, websearch_to_tsquery('english', q) tsq \
              WHERE d.tsv @@ tsq $$;",
        );
        assert!(refs.reads.contains(&"real.docs".to_string()), "{refs:?}");
        assert!(
            !refs.reads.iter().any(|r| r.ends_with("websearch_to_tsquery")),
            "FROM-clause function leaked into reads: {refs:?}"
        );
        assert!(
            refs.functions.iter().any(|f| f.ends_with("websearch_to_tsquery")),
            "FROM-clause function should be a soft function ref: {refs:?}"
        );
    }

    #[test]
    fn proc_plpgsql_body_collects_no_called_functions() {
        // PL/pgSQL resolves names at run time, so its calls are not
        // creation-order dependencies and must not constrain the apply order.
        let refs = proc_all_refs(
            "CREATE FUNCTION g() RETURNS void LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM app.helper(1); END; $$;",
        );
        assert!(
            refs.functions.is_empty(),
            "plpgsql calls must not become dependencies: {refs:?}"
        );
    }

    #[test]
    fn proc_plpgsql_body_extracts_via_pg_query() {
        // PL/pgSQL is parsed by libpg_query (real Postgres parser).
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION g() RETURNS void LANGUAGE plpgsql AS $$ \
             BEGIN INSERT INTO sink.t SELECT id FROM real.src; END; $$;",
        );
        assert!(reads.contains(&"real.src".to_string()), "reads={reads:?}");
        assert!(writes.contains(&"sink.t".to_string()), "writes={writes:?}");
        assert!(!writes.contains(&"real.src".to_string()), "read leaked into writes: {writes:?}");
    }

    #[test]
    fn proc_plpgsql_unwraps_perform_for_loop_select_into_return_query() {
        // libpg_query strips PL/pgSQL wrappers, exposing the embedded SQL:
        // SELECT..INTO, PERFORM, FOR..IN..LOOP, IF EXISTS(..), RETURN QUERY.
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS SETOF int LANGUAGE plpgsql AS $$ \
             DECLARE v int; rec record; \
             BEGIN \
               SELECT count(*) INTO v FROM staging.lookups WHERE active; \
               PERFORM id FROM work.queue; \
               FOR rec IN SELECT id FROM work.items LOOP \
                 DELETE FROM work.items WHERE id = rec.id; \
               END LOOP; \
               IF EXISTS (SELECT 1 FROM audit.log) THEN RETURN; END IF; \
               RETURN QUERY SELECT id FROM report.summary; \
             END; $$;",
        );
        for t in ["staging.lookups", "work.queue", "work.items", "audit.log", "report.summary"] {
            assert!(reads.contains(&t.to_string()), "missing read {t}: {reads:?}");
        }
        assert!(writes.contains(&"work.items".to_string()), "writes={writes:?}");
    }

    #[test]
    fn proc_plpgsql_ignores_dynamic_sql() {
        // EXECUTE '...' is dynamic (runtime) SQL — its tables must NOT be tracked.
        let (reads, writes) = proc_refs(
            "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ \
             BEGIN \
               INSERT INTO real.audit(n) VALUES (1); \
               EXECUTE 'INSERT INTO dyn.sink SELECT * FROM dyn.src'; \
             END; $$;",
        );
        assert!(writes.contains(&"real.audit".to_string()), "writes={writes:?}");
        assert!(!reads.iter().any(|r| r.starts_with("dyn.")), "dynamic read leaked: {reads:?}");
        assert!(!writes.iter().any(|w| w.starts_with("dyn.")), "dynamic write leaked: {writes:?}");
    }

    #[test]
    fn proc_plpgsql_resolves_unqualified_with_default_schema() {
        let (reads, _) = proc_refs(
            "CREATE FUNCTION f() RETURNS SETOF int LANGUAGE plpgsql AS $$ \
             BEGIN RETURN QUERY SELECT id FROM orders; END; $$;",
        );
        assert!(reads.contains(&"public.orders".to_string()), "reads={reads:?}");
    }

    #[test]
    fn proc_plpgsql_excludes_cte_names() {
        // A CTE defined in the body must not be reported as a real table.
        let (reads, _) = proc_refs(
            "CREATE FUNCTION f() RETURNS SETOF int LANGUAGE plpgsql AS $$ \
             BEGIN RETURN QUERY WITH recent AS (SELECT id FROM sales.orders) \
               SELECT id FROM recent; END; $$;",
        );
        assert!(reads.contains(&"sales.orders".to_string()), "reads={reads:?}");
        assert!(!reads.contains(&"public.recent".to_string()), "CTE leaked: {reads:?}");
    }

    #[test]
    fn extract_proc_reads() {
        let sql = r#"
            CREATE PROCEDURE foo()
            AS $$ BEGIN
                SELECT * FROM staging.lookups;
                INSERT INTO config.lookups(name) SELECT name FROM staging.lookups;
            END; $$
        "#;
        let (reads, writes) = extract_proc_reads_writes(sql);
        assert!(reads.contains(&"staging.lookups".to_string()));
        assert!(writes.contains(&"config.lookups".to_string()));
    }

    #[test]
    fn extract_proc_deduplicates() {
        let sql = r#"
            $$ BEGIN
                SELECT * FROM staging.lookups;
                SELECT name FROM staging.lookups;
            END; $$
        "#;
        let (reads, _) = extract_proc_reads_writes(sql);
        assert_eq!(
            reads.iter().filter(|r| *r == "staging.lookups").count(),
            1
        );
    }

    #[test]
    fn extract_enum_from_statements() {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(
            &dialect,
            "CREATE TYPE status AS ENUM ('active', 'inactive');",
        )
        .unwrap();
        let values = extract_enum_values(&stmts);
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].name, "active");
        assert_eq!(values[1].name, "inactive");
    }

    /// Parse SQL and extract view references using the default `public` schema.
    fn view_refs(sql: &str) -> Vec<String> {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(&dialect, sql).unwrap();
        let refs = extract_view_info(&stmts, &["public".to_string()]);
        refs.into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn extract_view_collects_derived_table_in_from() {
        let names = view_refs("CREATE VIEW v AS SELECT * FROM (SELECT id FROM real.orders) sub;");
        assert!(names.contains(&"real.orders".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_subquery_in_join() {
        let names = view_refs(
            "CREATE VIEW v AS \
             SELECT * FROM main.a a JOIN (SELECT id FROM real.b) x ON a.id = x.id;",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.b".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_scalar_subquery_in_projection() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id, (SELECT max(amt) FROM real.payments) FROM main.a;",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.payments".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_where_in_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a WHERE id IN (SELECT id FROM real.allow);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.allow".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_where_not_in_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a WHERE id NOT IN (SELECT id FROM real.block);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.block".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_where_exists_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a \
             WHERE EXISTS (SELECT 1 FROM real.flags f WHERE f.id = a.id);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.flags".to_string()), "{names:?}");
        // Correlated table aliases (`a`, `f`) must not be collected as tables.
        assert!(!names.contains(&"public.a".to_string()), "alias leaked: {names:?}");
        assert!(!names.contains(&"public.f".to_string()), "alias leaked: {names:?}");
    }

    #[test]
    fn extract_view_collects_where_scalar_subquery() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a WHERE id = (SELECT max(id) FROM real.cap);",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"real.cap".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_deeply_nested_subqueries() {
        let names = view_refs(
            "CREATE VIEW v AS SELECT id FROM main.a \
             WHERE id IN (SELECT id FROM mid.b WHERE id IN (SELECT id FROM deep.c));",
        );
        assert!(names.contains(&"main.a".to_string()), "{names:?}");
        assert!(names.contains(&"mid.b".to_string()), "{names:?}");
        assert!(names.contains(&"deep.c".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_both_except_branches() {
        let names = view_refs("CREATE VIEW v AS SELECT id FROM a.t1 EXCEPT SELECT id FROM b.t2;");
        assert!(names.contains(&"a.t1".to_string()), "{names:?}");
        assert!(names.contains(&"b.t2".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_both_minus_branches() {
        // MINUS is Oracle's spelling of EXCEPT; sqlparser parses it as a set op,
        // so the SetOperation arm handles it like any other compound query.
        let names = view_refs("CREATE VIEW v AS SELECT id FROM a.t1 MINUS SELECT id FROM b.t2;");
        assert!(names.contains(&"a.t1".to_string()), "{names:?}");
        assert!(names.contains(&"b.t2".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_tables_inside_recursive_cte() {
        // Recursive CTEs UNION inside the CTE body, and the real table lives in
        // the WITH clause (a sibling of query.body). The recursive self-reference
        // (anc) must NOT be collected as a real table.
        let names = view_refs(
            "CREATE VIEW v AS \
             WITH RECURSIVE anc AS ( \
                SELECT id, parent_id FROM org.nodes WHERE parent_id IS NULL \
                UNION ALL \
                SELECT n.id, n.parent_id FROM org.nodes n JOIN anc a ON n.parent_id = a.id \
             ) SELECT * FROM anc;",
        );
        assert!(
            names.contains(&"org.nodes".to_string()),
            "real table inside recursive CTE missed: {names:?}"
        );
        assert!(
            !names.contains(&"public.anc".to_string()),
            "CTE name leaked as a table ref: {names:?}"
        );
    }

    #[test]
    fn extract_view_collects_tables_inside_plain_cte() {
        let names = view_refs(
            "CREATE VIEW v AS \
             WITH recent AS (SELECT id FROM sales.orders) \
             SELECT * FROM recent r JOIN cust.customers c ON r.id = c.id;",
        );
        assert!(
            names.contains(&"sales.orders".to_string()),
            "CTE-internal table missed: {names:?}"
        );
        assert!(names.contains(&"cust.customers".to_string()), "{names:?}");
        assert!(
            !names.contains(&"public.recent".to_string()),
            "CTE name leaked as a table ref: {names:?}"
        );
    }

    #[test]
    fn extract_view_handles_chained_ctes() {
        // CTE `b` references CTE `a`; neither CTE name should appear as a table,
        // and the real tables from both bodies must be collected.
        let names = view_refs(
            "CREATE VIEW v AS \
             WITH a AS (SELECT id FROM raw.events), \
                  b AS (SELECT id FROM a) \
             SELECT * FROM b JOIN dim.users u ON b.id = u.id;",
        );
        assert!(names.contains(&"raw.events".to_string()), "{names:?}");
        assert!(names.contains(&"dim.users".to_string()), "{names:?}");
        assert!(!names.contains(&"public.a".to_string()), "CTE name `a` leaked: {names:?}");
        assert!(!names.contains(&"public.b".to_string()), "CTE name `b` leaked: {names:?}");
    }

    #[test]
    fn extract_view_simple_select_uses_default_schema() {
        let names = view_refs("CREATE VIEW v AS SELECT id FROM orders;");
        assert_eq!(names, vec!["public.orders".to_string()]);
    }

    #[test]
    fn extract_view_collects_all_union_branches() {
        // Regression: a compound query (UNION) must collect table refs from
        // EVERY branch, not just the first. Before the SetOperation fix the
        // top-level body was a SetOperation (not a Select), so no refs at all
        // were collected from UNION views.
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT id, total FROM sales.orders \
             UNION ALL \
             SELECT id, total FROM archive.orders;",
        );
        assert!(
            names.contains(&"sales.orders".to_string()),
            "missing first branch: {names:?}"
        );
        assert!(
            names.contains(&"archive.orders".to_string()),
            "missing second UNION branch (the bug): {names:?}"
        );
    }

    #[test]
    fn extract_view_collects_nested_and_parenthesized_union_branches() {
        // Exercises both SetOperation recursion and the parenthesized
        // SetExpr::Query branch.
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT id FROM s1.t1 \
             UNION \
             (SELECT id FROM s2.t2 INTERSECT SELECT id FROM s3.t3);",
        );
        assert!(names.contains(&"s1.t1".to_string()), "{names:?}");
        assert!(names.contains(&"s2.t2".to_string()), "{names:?}");
        assert!(names.contains(&"s3.t3".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_collects_joins_within_union_branches() {
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT * FROM a.left_t l JOIN a.right_t r ON l.id = r.id \
             UNION ALL \
             SELECT * FROM b.other o;",
        );
        assert!(names.contains(&"a.left_t".to_string()), "{names:?}");
        assert!(names.contains(&"a.right_t".to_string()), "{names:?}");
        assert!(names.contains(&"b.other".to_string()), "{names:?}");
    }

    #[test]
    fn extract_view_excludes_system_schemas_in_union() {
        // System-schema tables stay excluded even when reached through a
        // UNION branch.
        let names = view_refs(
            "CREATE VIEW combined AS \
             SELECT id FROM app.widgets \
             UNION ALL \
             SELECT oid FROM pg_catalog.pg_class;",
        );
        assert!(names.contains(&"app.widgets".to_string()), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("pg_catalog.")),
            "system schema leaked: {names:?}"
        );
    }

    #[test]
    fn extract_search_paths_from_set() {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts = sqlparser::parser::Parser::parse_sql(
            &dialect,
            "SET search_path TO config, extensions;",
        )
        .unwrap();
        let paths = extract_search_paths(&stmts);
        assert_eq!(paths, vec!["config", "extensions"]);
    }

    #[test]
    fn search_paths_default_to_public() {
        let dialect = sqlparser::dialect::PostgreSqlDialect {};
        let stmts =
            sqlparser::parser::Parser::parse_sql(&dialect, "CREATE TABLE foo (id int);").unwrap();
        let paths = extract_search_paths(&stmts);
        assert_eq!(paths, vec!["public"]);
    }
}

