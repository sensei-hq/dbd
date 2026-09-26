//! The libpg_query island: every helper that parses SQL through libpg_query
//! (Postgres's own grammar) lives here, plus `SYSTEM_SCHEMAS`, which the
//! per-type parsers share.
//!
//! This module was carved out to break a cycle: `pg::enums` imported the
//! sqlparser-side `extractors` while `extractors` imported `pg::enums`, and
//! every new native parser deepened it. Moving the libpg_query helpers here
//! made the dependency run one way. `extractors` is gone now, but the
//! arrangement is worth keeping — the per-type parsers share these helpers,
//! and a future non-Postgres grammar would sit beside `pg`, not inside it.

use crate::entity::{EnumValue, PathEntry, Ref, RefKind, SchemaPath, SchemaSource};

use super::enums;

/// Enum labels from a `DO $$ … $$` guarded `CREATE TYPE … AS ENUM`, read off
/// libpg_query's AST (Postgres's own parser).
///
/// Postgres has no `CREATE TYPE IF NOT EXISTS`, so wrapping the CREATE in a DO
/// block that swallows `duplicate_object` is the only idiom available for a
/// conditional enum. A DO block is opaque to a plain statement parse, so this
/// takes the same route the PL/pgSQL body walk does: parse the block, take the
/// SQL it embeds, and re-parse that into a real statement tree.
///
/// Returns an empty vec when the input is not a PL/pgSQL block libpg_query
/// accepts, or when it declares no enum — the caller keeps its parse error, so a
/// genuinely broken file is never quietly waved through.
pub(in crate::parser) fn extract_enum_values_via_pg_query(raw_sql: &str) -> Vec<EnumValue> {
    let Ok(tree) = pg_query::parse_plpgsql(raw_sql) else {
        return Vec::new();
    };

    let mut queries = Vec::new();
    collect_plpgsql_queries(&tree, &mut queries);

    for query in &queries {
        let Ok(parsed) = pg_query::parse(query) else {
            continue;
        };
        if let Some(values) = enums::labels_from_parse_result(&parsed)
            && !values.is_empty()
        {
            return values;
        }
    }
    Vec::new()
}

/// `search_path` schemas from libpg_query's AST.
///
/// Defaults to `["public"]` when the file sets no search path. That default is
/// not optional: reads and view references are qualified against it, so an
/// empty list silently re-qualifies `t` to `public.t` — a plausibly-wrong edge
/// pointing at a different table, which is worse than no edge at all.
///
/// `pub(crate)`, wider than this file's other libpg_query helpers, because
/// [`design::hooks`] — outside `crate::parser` entirely — resolves an
/// after-script's `search_path` the same way a view or routine body does, to
/// qualify the table names a hook script depends on.
///
/// [`design::hooks`]: crate::design::hooks
/// The path an entity should carry: the file's own if it states one, else the
/// fallback already seeded on the entity, else PostgreSQL's session default.
///
/// The seeded value is `source.search_path` — the project's answer for a file
/// that forgets its `SET search_path`. Threaded in on the entity rather than as
/// a parameter to every per-type parser, so the fallback reaches all of them
/// through the one line each already had.
pub(in crate::parser) fn resolve_schema_path(raw_sql: &str, seeded: &SchemaPath) -> SchemaPath {
    let from_file = extract_search_paths_via_pg_query(raw_sql);
    if from_file.stated() || seeded.source == crate::entity::PathSource::SessionDefault {
        return from_file;
    }
    seeded.clone()
}

pub(crate) fn extract_search_paths_via_pg_query(raw_sql: &str) -> SchemaPath {
    let Ok(parsed) = pg_query::parse(raw_sql) else {
        return SchemaPath::postgres_default();
    };
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::VariableSetStmt(set)) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
            continue;
        };
        if !set.name.eq_ignore_ascii_case("search_path") {
            continue;
        }
        let entries: Vec<PathEntry> = set.args.iter().filter_map(const_str).map(path_entry).collect();
        if !entries.is_empty() {
            return SchemaPath::from_file(entries);
        }
    }
    SchemaPath::postgres_default()
}

/// `"$user"` is a placeholder, not a schema. Reading it as one produced
/// references to a schema called `$user`, which cannot exist.
fn path_entry(name: String) -> PathEntry {
    if name == "$user" {
        PathEntry::CurrentUser
    } else {
        PathEntry::Schema(name)
    }
}

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

/// The tables a view's body reads, from libpg_query's AST.
///
/// A view that carries no dependency edge could be applied before the table it
/// selects from — a silent misordering — so this is not optional detail.
///
/// Sorted before returning: `ParseResult::select_tables()` is built from a
/// `HashSet` internally, so its iteration order is not source order — it's
/// Rust's randomized per-process hash order, confirmed by parsing the same SQL
/// in the same binary across separate runs and observing different orderings.
/// Left unsorted, `entity.refers().collect::<Vec<_>>()`/`entity.refs` for any multi-relation
/// view would vary from run to run, which is a nondeterminism bug regardless of
/// how the result is later compared.
///
pub(in crate::parser) fn extract_view_refs_via_pg_query(raw_sql: &str, default_schema: &str) -> Vec<Ref> {
    let Ok(parsed) = pg_query::parse(raw_sql) else {
        return Vec::new();
    };
    qualify_all_sourced(parsed.select_tables(), default_schema)
        .into_iter()
        .map(|(name, schema_source)| Ref {
            name,
            kind: RefKind::Reads,
            schema_source,
            unresolved: false,
        })
        .collect()
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
///
/// Sorted before returning: `select_tables`/`dml_tables` are `HashSet`-derived
/// (see `extract_view_refs_via_pg_query`), and each embedded query here gets
/// its own `pg_query::parse` call — an independently-seeded `HashSet` — so two
/// calls on identical input can and do disagree on order within the same
/// process. Caught by the parser-parity gate once `Function`/`Procedure` were
/// covered: the sqlparser incumbent's PL/pgSQL tier calls this same function,
/// so an unsorted result compared two independent hash orderings of the same
/// set and failed nondeterministically.
#[allow(clippy::type_complexity)]
pub(in crate::parser) fn extract_proc_refs_via_pg_query(
    raw_sql: &str,
    default_schema: &str,
) -> Option<(Vec<(String, SchemaSource)>, Vec<(String, SchemaSource)>)> {
    let tree = pg_query::parse_plpgsql(raw_sql).ok()?;

    let mut queries = Vec::new();
    collect_plpgsql_queries(&tree, &mut queries);

    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for query in &queries {
        // A `query` may be a full statement, or a bare expression (e.g. an `IF`
        // condition). Parse it directly, else `SELECT`-wrap it so any subqueries
        // are still seen. A dynamic-SQL string literal yields no tables either way.
        let Ok(parsed) = pg_query::parse(query).or_else(|_| pg_query::parse(&format!("SELECT {query}"))) else {
            continue;
        };
        reads.extend(parsed.select_tables());
        writes.extend(parsed.dml_tables());
    }
    Some((
        qualify_all_sourced(reads, default_schema),
        qualify_all_sourced(writes, default_schema),
    ))
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
///
/// `pub(crate)`, wider than this file's other libpg_query helpers: `pg::views`
/// (within `crate::parser`) qualifies `call_functions()` names with it the same
/// way this module already qualifies `select_tables()` names, and
/// [`design::hooks`] (outside `crate::parser`) qualifies a hook script's
/// derived table references the same way.
///
/// [`design::hooks`]: crate::design::hooks
pub(crate) fn qualify_name_str(name: &str, default_schema: &str) -> Option<String> {
    qualify_name_source(name, default_schema).map(|(name, _)| name)
}

/// [`qualify_name_str`], also saying whether the schema was the source's or
/// dbd's.
///
/// The one place a bare name acquires a schema it never had, so the one place
/// that can tell a statement from a guess. See [`SchemaSource`].
pub(crate) fn qualify_name_source(name: &str, default_schema: &str) -> Option<(String, SchemaSource)> {
    let parts: Vec<&str> = name.split('.').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [.., schema, table] => {
            if SYSTEM_SCHEMAS.contains(schema) {
                return None;
            }
            Some((format!("{schema}.{table}"), SchemaSource::Stated))
        }
        [table] => Some((format!("{default_schema}.{table}"), SchemaSource::Inferred)),
        _ => None,
    }
}

/// Qualify a batch of names, sorted and deduplicated, keeping each one's
/// provenance.
///
/// Sorted for the reason [`extract_view_refs_via_pg_query`] gives: the
/// libpg_query accessors are `HashSet`-backed, so their order varies per
/// process.
///
/// Deduplicated **by name**, keeping the most trustworthy provenance. The same
/// table can be reached twice in one body — once written `app.t` and once bare
/// — and if dbd knows the schema from anywhere in the file it should not report
/// a guess.
pub(in crate::parser) fn qualify_all_sourced(names: Vec<String>, default_schema: &str) -> Vec<(String, SchemaSource)> {
    let mut out: Vec<(String, SchemaSource)> = names
        .iter()
        .filter_map(|n| qualify_name_source(n, default_schema))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.confidence_rank().cmp(&b.1.confidence_rank())));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// System schemas to exclude from references.
///
/// Generic — not libpg_query-specific — but lives here because the per-type
/// parsers share it. See the module doc comment.
pub(in crate::parser) const SYSTEM_SCHEMAS: &[&str] = &["information_schema", "pg_catalog", "pg_toast"];

#[cfg(test)]
mod tests {
    use super::*;

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
}
