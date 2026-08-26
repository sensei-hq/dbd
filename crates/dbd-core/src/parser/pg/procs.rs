//! Function and procedure DDL, parsed with libpg_query.
//!
//! The routine's `LANGUAGE` decides how its body is read, and that split is
//! load-bearing rather than an optimisation. Postgres validates a `LANGUAGE sql`
//! body when the routine is created (`check_function_bodies = on` by default),
//! so a function it calls must exist first — the call is a creation-order
//! dependency. A PL/pgSQL body resolves names at run time, so its calls are not.
//! Collecting calls from a PL/pgSQL body would put phantom edges in the apply
//! graph; omitting them from a `LANGUAGE sql` body would drop real ones.

use crate::entity::{Entity, REF_TYPE_FUNCTION, Reference};
use crate::error::Result;

use super::common;

/// Parse a function or procedure DDL file.
pub(crate) fn parse_proc(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Before any early return: references are qualified against the search path,
    // and an errored entity reporting `[]` instead of the `["public"]` default is
    // an invariant break the enum parser already hit once.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    let Some(routine) = routine_body(&parsed) else {
        entity
            .errors
            .push("this file declares no `CREATE FUNCTION` or `CREATE PROCEDURE`".to_string());
        return Ok(entity);
    };

    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    let (reads, writes, functions) = match routine.language.as_str() {
        // A SQL body is itself SQL: parse it directly. Called functions count,
        // because Postgres resolves them when the routine is created.
        "sql" => {
            let Ok(body) = pg_query::parse(&routine.body) else {
                // The body text itself is not valid SQL — e.g. a file mid-edit,
                // or one created with `check_function_bodies = off`. Every
                // Postgres-accepted `LANGUAGE sql` body re-parses standalone
                // (verified against TABLE/VALUES shorthands, positional params,
                // empty and multi-statement bodies), so this is reached only for
                // genuinely invalid body text — not for any construct real
                // Postgres would accept. libpg_query never looks inside the
                // dollar-quoted string, so the outer CREATE FUNCTION is still
                // valid; fall back to the PL/pgSQL walker rather than erroring,
                // since we just cannot read this body's refs.
                let (r, w) = common::extract_proc_refs_via_pg_query(sql, &default_schema).unwrap_or_default();
                return Ok(finish(entity, r, w, Vec::new()));
            };
            (
                qualify_all(body.select_tables(), &default_schema),
                qualify_all(body.dml_tables(), &default_schema),
                qualify_all(body.call_functions(), &default_schema),
            )
        }
        // A PL/pgSQL body resolves names at run time, so its calls are NOT
        // creation-order dependencies — `functions` stays empty deliberately.
        _ => {
            let (r, w) = common::extract_proc_refs_via_pg_query(sql, &default_schema).unwrap_or_default();
            (r, w, Vec::new())
        }
    };

    Ok(finish(entity, reads, writes, functions))
}

/// Fill the entity's reference fields.
///
/// Mirrors `parser::apply_proc_refs`: reads and writes become hard references,
/// called functions become soft ones tagged [`REF_TYPE_FUNCTION`], which
/// `references::resolve_references` keeps only when they name a known entity.
fn finish(mut entity: Entity, reads: Vec<String>, writes: Vec<String>, functions: Vec<String>) -> Entity {
    let mut references: Vec<Reference> = reads
        .iter()
        .chain(writes.iter())
        .map(|name| Reference {
            name: name.clone(),
            ref_type: None,
        })
        .collect();
    for name in functions {
        if references.iter().any(|r| r.name == name) {
            continue;
        }
        references.push(Reference {
            name,
            ref_type: Some(REF_TYPE_FUNCTION.to_string()),
        });
    }
    entity.refers = references.iter().map(|r| r.name.clone()).collect();
    entity.references = references;
    entity.reads = reads;
    entity.writes = writes;
    entity
}

/// Qualify libpg_query's bare names and sort them.
///
/// Sorted because `select_tables`/`dml_tables`/`call_functions` are built from a
/// `HashSet`, so their order differs on every process run — see the determinism
/// fix in `common::extract_view_refs_via_pg_query`.
fn qualify_all(names: Vec<String>, default_schema: &str) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .filter_map(|n| common::qualify_name_str(n, default_schema))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The `LANGUAGE` and body text of the first routine in the file.
struct Routine {
    language: String,
    body: String,
}

fn routine_body(parsed: &pg_query::ParseResult) -> Option<Routine> {
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::CreateFunctionStmt(create)) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        let (mut language, mut body) = (None, None);
        for opt in &create.options {
            let Some(pg_query::NodeEnum::DefElem(def)) = opt.node.as_ref() else {
                continue;
            };
            match def.defname.as_str() {
                "language" => {
                    if let Some(pg_query::NodeEnum::String(s)) = def.arg.as_ref().and_then(|a| a.node.as_ref()) {
                        language = Some(s.sval.to_lowercase());
                    }
                }
                "as" => {
                    if let Some(pg_query::NodeEnum::List(list)) = def.arg.as_ref().and_then(|a| a.node.as_ref())
                        && let Some(pg_query::NodeEnum::String(s)) = list.items.first().and_then(|i| i.node.as_ref())
                    {
                        body = Some(s.sval.clone());
                    }
                }
                _ => {}
            }
        }
        return Some(Routine {
            language: language.unwrap_or_default(),
            body: body.unwrap_or_default(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, REF_TYPE_FUNCTION};

    fn parse(sql: &str) -> Entity {
        parse_proc(Entity::new(EntityType::Function, "app.f"), sql).unwrap()
    }

    #[test]
    fn sql_body_reads_are_captured() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language sql as $$ select count(*) from t $$;",
        );
        assert_eq!(e.reads, vec!["app.t".to_string()], "got {:?}", e.reads);
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn sql_body_writes_are_captured() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns void language sql as $$ insert into t(a) values (1) $$;",
        );
        assert_eq!(e.writes, vec!["app.t".to_string()], "got {:?}", e.writes);
    }

    /// Postgres validates a `LANGUAGE sql` body at creation, so a function it
    /// calls must exist first — the call IS a creation-order dependency.
    #[test]
    fn sql_body_calls_become_soft_function_references() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language sql as $$ select app.myfn(1) $$;",
        );
        let myfn = e
            .references
            .iter()
            .find(|r| r.name == "app.myfn")
            .expect("called function missing");
        assert_eq!(myfn.ref_type.as_deref(), Some(REF_TYPE_FUNCTION));
    }

    #[test]
    fn plpgsql_body_reads_and_writes_are_captured() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns void language plpgsql as $$ begin insert into t(a) values (1); end $$;",
        );
        assert_eq!(e.writes, vec!["app.t".to_string()], "got {:?}", e.writes);
    }

    /// A PL/pgSQL body resolves names at run time, so its calls are NOT
    /// creation-order dependencies. The incumbent omits them deliberately;
    /// collecting them here would add phantom edges to the apply graph.
    #[test]
    fn plpgsql_body_calls_are_not_collected() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language plpgsql as $$ begin return app.myfn(1); end $$;",
        );
        assert!(
            !e.refers.iter().any(|r| r == "app.myfn"),
            "plpgsql calls must not become dependencies, got {:?}",
            e.refers
        );
    }

    #[test]
    fn language_after_the_body_is_still_detected() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int as $$ select count(*) from t $$ language sql;",
        );
        assert_eq!(e.reads, vec!["app.t".to_string()], "got {:?}", e.reads);
    }

    #[test]
    fn a_procedure_is_parsed_like_a_function() {
        let e = parse_proc(
            Entity::new(EntityType::Procedure, "app.p"),
            "set search_path to app;\n\
             create procedure p() language plpgsql as $$ begin insert into t(a) values (1); end $$;",
        )
        .unwrap();
        assert_eq!(e.writes, vec!["app.t".to_string()], "got {:?}", e.writes);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate function f() returns int language sql as $$ select 1 $$;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create function f() returns int language sql as $$ select 1 $$;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create function f() returns int language sql as ;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    /// Mirrors the enum and view parsers.
    #[test]
    fn an_errored_routine_still_has_a_search_path() {
        let e = parse("create function f() returns int language sql as ;;;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn a_file_declaring_no_routine_records_an_error() {
        let e = parse("select 1;");
        assert!(
            !e.errors.is_empty(),
            "a routine file with no CREATE FUNCTION must error"
        );
    }

    /// A `LANGUAGE sql` body whose text is not itself valid SQL (a file mid-edit,
    /// or one created on a server with `check_function_bodies = off`) cannot be
    /// re-parsed standalone — confirmed empirically: every Postgres-accepted
    /// `LANGUAGE sql` body tried (`TABLE t`, `VALUES (1)`, positional params, an
    /// empty body, multi-statement bodies) re-parses fine, so this path is only
    /// reached for genuinely invalid body text. The outer `CREATE FUNCTION`
    /// syntax is still valid — libpg_query never looks inside the dollar-quoted
    /// string — so this must not error; it just cannot report the body's refs.
    #[test]
    fn a_body_that_is_not_standalone_parseable_falls_back_without_erroring() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language sql as $$ this is not valid sql at all $$;",
        );
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
        assert!(e.reads.is_empty(), "got {:?}", e.reads);
        assert!(e.writes.is_empty(), "got {:?}", e.writes);
    }
}
