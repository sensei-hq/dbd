mod extractors;
mod tables;

use std::path::Path;

use crate::entity::{Entity, EntityType, REF_TYPE_FUNCTION, Reference};
use crate::error::Result;

pub use extractors::extract_search_paths;

// Parse a DDL file and produce an Entity with extracted metadata.
//
// This is the main parser entry point. It reads the SQL, parses it with
// sqlparser-rs (PostgreSQL dialect), and extracts:
// - Entity identity (type, name, schema) from the file path
// - Search paths from SET search_path statements
// - References (FK targets, view dependencies)
// - Table structure (columns, constraints, indexes) into TableDef
// - Enum values
// ── sqlparser workarounds ────────────────────────────────────────────
//
// WORKAROUND_REGISTRY: sqlparser-rs 0.61 (Apache DataFusion)
//
// The workarounds below patch SQL text before feeding it to sqlparser.
// Each is annotated with the limitation it addresses and what to check
// when upgrading sqlparser or switching to an alternative parser.
//
// To test if a workaround is still needed after a parser upgrade:
//   1. Comment out the workaround
//   2. Run: cargo test
//   3. Run: dbd-rs inspect -s <project-with-procedures-and-views>
//   4. If no parse errors → remove the workaround
//
// Alternative parsers to evaluate:
//   - pg_query (Rust bindings to libpg_query / PostgreSQL's C parser)
//     Handles everything but requires C compilation.
//   - tree-sitter-sql — editor-focused CST, not suitable for DDL analysis.
// ─────────────────────────────────────────────────────────────────────

/// Preprocess SQL to work around known sqlparser limitations.
///
/// See WORKAROUND_REGISTRY above for details.
fn preprocess_sql(sql: &str) -> String {
    let mut result = std::borrow::Cow::Borrowed(sql);

    // WORKAROUND: sqlparser-comment-on-object-types
    // Limitation: sqlparser only supports COMMENT ON TABLE and COMMENT ON COLUMN.
    //             COMMENT ON VIEW, MATERIALIZED VIEW, FUNCTION, PROCEDURE, TRIGGER,
    //             INDEX, etc. fail.
    // Impact:     Parse error on any DDL file with non-table/column comments.
    // Check:      Parser::parse_sql("COMMENT ON VIEW foo IS 'bar';")
    // Tracking:   https://github.com/apache/datafusion-sqlparser-rs/issues
    {
        let re = regex::Regex::new(
            r"(?is)\bcomment\s+on\s+(?:materialized\s+view|view|function|procedure|trigger|index|schema|extension|type)\s+\S+\s+is\s+'[^']*(?:''[^']*)*'\s*;"
        ).unwrap();
        if re.is_match(&result) {
            result = std::borrow::Cow::Owned(re.replace_all(&result, "").to_string());
        }
    }

    // WORKAROUND: sqlparser-create-procedure
    // Limitation: sqlparser does not support CREATE [OR REPLACE] PROCEDURE.
    //             Only CREATE [OR REPLACE] FUNCTION is recognized.
    // Impact:     All procedure DDL files fail to parse.
    // Fix:        Rewrite PROCEDURE → FUNCTION before parsing. The AST structure
    //             is identical — we only need the body for reads/writes extraction.
    // Check:      Parser::parse_sql("CREATE PROCEDURE foo() LANGUAGE plpgsql AS $$ BEGIN END; $$")
    // Tracking:   https://github.com/apache/datafusion-sqlparser-rs/issues
    {
        let re = regex::Regex::new(
            r"(?i)\b(create\s+(?:or\s+replace\s+)?)procedure\b"
        ).unwrap();
        if re.is_match(&result) {
            result = std::borrow::Cow::Owned(re.replace_all(&result, "${1}FUNCTION").to_string());
        }
    }

    // WORKAROUND: sqlparser-materialized-view-with-data
    // Limitation: sqlparser parses CREATE MATERIALIZED VIEW into
    //             CreateView { materialized: true, .. }, but rejects the trailing
    //             PostgreSQL `WITH [NO] DATA` clause ("Expected: end of statement,
    //             found: WITH").
    // Impact:     Parse error on any materialized-view DDL file with a WITH [NO] DATA clause.
    // Fix:        Drop the trailing WITH [NO] DATA for AST extraction only; it
    //             carries no structure we read (the emitter writes the real
    //             clause). Scoped to files that declare a materialized view so a
    //             stray `WITH DATA` elsewhere is left untouched.
    // Check:      Parser::parse_sql("CREATE MATERIALIZED VIEW v AS SELECT 1 WITH DATA;")
    // Tracking:   https://github.com/apache/datafusion-sqlparser-rs/issues
    {
        let re = regex::Regex::new(r"(?is)\bcreate\s+materialized\s+view\b").unwrap();
        if re.is_match(&result) {
            let with_data = regex::Regex::new(r"(?is)\s+with\s+(?:no\s+)?data\s*(;|$)").unwrap();
            result = std::borrow::Cow::Owned(with_data.replace_all(&result, "$1").to_string());
        }
    }

    result.into_owned()
}

/// Scan role DDL for `GRANT <parent> TO <member>` membership lines.
///
/// sqlparser cannot handle `DO $$ … $$` blocks or `GRANT role TO role`, so we
/// use a simple, non-backtracking regex on the raw SQL instead.
///
/// The pattern matches role-membership grants (identifiers only, no privilege
/// keywords like `SELECT`). The `… ON …` form (object grants such as
/// `GRANT SELECT ON TABLE … TO …`) will not match because an `ON` clause
/// appears between the privilege list and the target — the regex requires the
/// `TO` to follow immediately after the identifier, so object grants are
/// correctly excluded.
///
/// Captured group 1 (the granted/parent role) is recorded as a `Reference`.
/// Duplicates are deduplicated while preserving first-seen order.
fn extract_role_memberships(sql: &str, entity: &mut Entity) {
    let re = regex::Regex::new(
        r#"(?i)\bGRANT\s+"?([A-Za-z_][A-Za-z0-9_$]*)"?\s+TO\s+"?([A-Za-z_][A-Za-z0-9_$]*)"?"#,
    )
    .expect("role membership regex is valid");
    let mut seen = std::collections::HashSet::new();
    for cap in re.captures_iter(sql) {
        let granted = cap[1].to_string();
        if seen.insert(granted.clone()) {
            entity.references.push(Reference {
                name: granted,
                ref_type: None,
            });
        }
    }
    entity.refers = entity
        .references
        .iter()
        .map(|r| r.name.clone())
        .collect();
}

/// Store a routine's extracted dependencies on the entity.
///
/// `reads`/`writes` stay table sets — import planning and scope analysis read
/// them that way — and become hard references. Called functions are added as
/// soft references ([`REF_TYPE_FUNCTION`]): a body's built-in calls look exactly
/// like calls to a project-managed function here, so the resolver keeps the ones
/// that name a known entity and drops the rest without warning.
fn apply_proc_refs(entity: &mut Entity, refs: extractors::ProcRefs) {
    entity.references = refs
        .reads
        .iter()
        .chain(refs.writes.iter())
        .map(|name| Reference {
            name: name.clone(),
            ref_type: None,
        })
        .chain(refs.functions.iter().map(|name| Reference {
            name: name.clone(),
            ref_type: Some(REF_TYPE_FUNCTION.to_string()),
        }))
        .collect();
    entity.reads = refs.reads;
    entity.writes = refs.writes;
}

pub fn parse_entity(file: &Path, sql: &str) -> Result<Entity> {
    let mut entity = Entity::from_file(file);

    // Role DDL contains DO $$ … $$ blocks that sqlparser cannot parse.
    // Scan the raw SQL directly with a regex and return early — no sqlparser needed.
    if entity.entity_type == EntityType::Role {
        extract_role_memberships(sql, &mut entity);
        return Ok(entity);
    }

    let cleaned = preprocess_sql(sql);
    let dialect = sqlparser::dialect::PostgreSqlDialect {};
    let statements = match sqlparser::parser::Parser::parse_sql(&dialect, &cleaned) {
        Ok(stmts) => stmts,
        Err(e) => {
            // An enum guarded by `DO $$ … $$` — the only idiom Postgres offers
            // for a conditional CREATE TYPE — is valid SQL that sqlparser can't
            // read. Recovering it here matters more than the missing AST: an
            // entity carrying a parse error is filtered out of apply/reconcile's
            // desired set entirely (`design::scope::entities_in_scope`), so the
            // type was never created and the first table using it failed with a
            // bare `type "…" does not exist` that named neither the file nor the
            // real cause. libpg_query reads the block, so no error is recorded.
            if entity.entity_type == EntityType::Enum {
                let values = extractors::extract_enum_values_via_pg_query(&cleaned);
                if !values.is_empty() {
                    entity.enum_values = values;
                    return Ok(entity);
                }
            }
            // sqlparser reimplements the SQL grammar and lags Postgres, so its
            // rejection alone says nothing about the file. When libpg_query —
            // Postgres's own parser — accepts it, the file is valid and the
            // limitation is ours: record no error, and recover what the entity
            // actually needs from the raw text.
            //
            // Only for the types dbd applies as raw SQL and needs no structural
            // AST for. A TABLE must keep its error: without `table_def` it is
            // filtered out of the desired snapshot (`reconcile::
            // raw_snapshot_from_entities`), which makes the live table read as
            // an orphan that `--prune` would DROP. A MATERIALIZED VIEW must too:
            // its emitter rebuilds the CREATE from `writes[0]`, which only the
            // sqlparser path populates.
            let ast_optional = matches!(
                entity.entity_type,
                EntityType::Function | EntityType::Procedure | EntityType::View
            );
            let recovered = ast_optional && extractors::is_valid_postgres(&cleaned);
            if !recovered {
                entity.errors.push(format!("Parse error: {e}"));
            }

            // Reads and view references are qualified against the search path,
            // so recover it first — defaulting to `public` (as the sqlparser
            // path does) would re-point `t` at `public.t`, a plausibly-wrong
            // edge to a different table rather than an absent one.
            entity.search_paths = extractors::extract_search_paths_via_pg_query(&cleaned);
            let default_schema = entity
                .search_paths
                .first()
                .cloned()
                .unwrap_or_else(|| "public".to_string());

            match entity.entity_type {
                // libpg_query parses the body; the regex scanner is the last
                // resort. No parsed statements here, so the LANGUAGE sql AST
                // path is skipped.
                EntityType::Function | EntityType::Procedure => {
                    apply_proc_refs(
                        &mut entity,
                        extractors::extract_proc_refs(&[], &cleaned, &default_schema),
                    );
                }
                EntityType::View => {
                    entity.references =
                        extractors::extract_view_refs_via_pg_query(&cleaned, &default_schema);
                }
                _ => {}
            }
            entity.refers = entity
                .references
                .iter()
                .map(|r| r.name.clone())
                .collect();
            return Ok(entity);
        }
    };

    // Extract search paths
    entity.search_paths = extractors::extract_search_paths(&statements);

    // Extract entity-specific information based on type
    match entity.entity_type {
        EntityType::Table => {
            let (table_def, references) = tables::extract_table(&statements, &entity.search_paths);
            entity.table_def = Some(table_def);
            entity.references = references;
        }
        EntityType::View => {
            let refs = extractors::extract_view_info(&statements, &entity.search_paths);
            entity.references = refs;
        }
        EntityType::MaterializedView => {
            // Body + references like a view: the SELECT definition is stashed in
            // writes[0] (matching the introspector's view contract), and the
            // tables it reads from become references.
            //
            // Unlike the View arm, capture the body into writes[0]: emit_matview
            // (and reconcile) reconstruct the CREATE from writes[0], so a
            // file→emit round-trip needs it here.
            if let Some(body) = extractors::extract_view_body(&statements) {
                entity.writes = vec![body];
            }
            let refs = extractors::extract_view_info(&statements, &entity.search_paths);
            entity.references = refs;
            // Trailing CREATE INDEX statements land in table_def.indexes, exactly
            // like a table's indexes — reuse the table/index extractor (there is
            // no CREATE TABLE here, so only its indexes are populated).
            let (table_def, _refs) = tables::extract_table(&statements, &entity.search_paths);
            entity.table_def = Some(table_def);
        }
        EntityType::Enum => {
            entity.enum_values = extractors::extract_enum_values(&statements);
        }
        EntityType::Function | EntityType::Procedure => {
            let default_schema = entity
                .search_paths
                .first()
                .map(|s| s.as_str())
                .unwrap_or("public");
            let refs = extractors::extract_proc_refs(&statements, &cleaned, default_schema);
            apply_proc_refs(&mut entity, refs);
        }
        _ => {}
    }

    // Build refers list from references (entity names only)
    entity.refers = entity
        .references
        .iter()
        .map(|r| r.name.clone())
        .collect();

    Ok(entity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ddl")
            .join(name)
    }

    fn parse_fixture(path: &str) -> Entity {
        let file = fixture(path);
        let sql = std::fs::read_to_string(&file).unwrap();
        parse_entity(&file, &sql).unwrap()
    }

    #[test]
    fn parses_table_entity_type() {
        let entity = parse_fixture("table/config/lookups.ddl");
        assert_eq!(entity.entity_type, EntityType::Table);
        assert_eq!(entity.name, "config.lookups");
        assert_eq!(entity.schema, Some("config".to_string()));
    }

    #[test]
    fn extracts_search_paths() {
        let entity = parse_fixture("table/config/lookups.ddl");
        assert_eq!(entity.search_paths, vec!["config", "extensions"]);
    }

    #[test]
    fn extracts_table_columns() {
        let entity = parse_fixture("table/config/lookups.ddl");
        let table_def = entity.table_def.as_ref().unwrap();
        assert!(table_def.columns.len() >= 8);

        let id_col = table_def.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id_col.is_pk);
        assert!(!id_col.nullable);
        assert!(id_col.default_value.is_some());

        let name_col = table_def.columns.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name_col.data_type, "VARCHAR(30)");
    }

    #[test]
    fn extracts_table_with_fk_references() {
        let entity = parse_fixture("table/config/lookup_values.ddl");
        let refers: Vec<&str> = entity.refers.iter().map(|s| s.as_str()).collect();
        // Should reference lookups and categories via FK
        assert!(refers.contains(&"config.lookups") || refers.contains(&"lookups"));
    }

    #[test]
    fn extracts_view_references() {
        let entity = parse_fixture("view/config/genders.ddl");
        assert_eq!(entity.entity_type, EntityType::View);
        assert_eq!(entity.name, "config.genders");
        // View references tables it SELECTs from
        assert!(!entity.references.is_empty());
    }

    #[test]
    fn extracts_enum_values() {
        let entity = parse_fixture("enum/config/status.sql");
        assert_eq!(entity.entity_type, EntityType::Enum);
        assert_eq!(entity.name, "config.status");
        let values: Vec<&str> = entity.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(values, vec!["active", "inactive", "archived"]);
    }

    #[test]
    fn extracts_procedure_reads_writes() {
        let entity = parse_fixture("procedure/staging/import_lookups.ddl");
        assert_eq!(entity.entity_type, EntityType::Procedure);
        assert!(!entity.reads.is_empty());
        assert!(!entity.writes.is_empty());
        // Reads from staging.lookups, writes to config.lookups
        assert!(entity.reads.iter().any(|r| r.contains("staging.lookups")));
        assert!(entity.writes.iter().any(|w| w.contains("config.lookups")));
    }

    #[test]
    fn handles_parse_error_gracefully() {
        let entity = parse_entity(
            Path::new("ddl/table/config/broken.ddl"),
            "THIS IS NOT SQL AT ALL ;;;",
        )
        .unwrap();
        assert!(!entity.errors.is_empty());
    }

    // ── sqlparser 0.62 grammar gaps ──────────────────────────────────────────
    //
    // 0.61 rejected these three, and a parse error is not cosmetic: it drops the
    // entity from apply/reconcile's desired set, so `dbd apply` reported success
    // while never creating the object. Pin them so a future downgrade or a
    // regression in the dependency is caught here rather than in a user's
    // database. `returns setof` is the case that surfaced the whole class.

    #[test]
    fn setof_returning_function_parses_and_keeps_its_refs() {
        let entity = parse_entity(
            Path::new("ddl/function/app/srf.ddl"),
            "set search_path to app;\n\
             create function srf() returns setof t language sql as $$ select * from t $$;",
        )
        .unwrap();
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
        assert!(
            entity.reads.contains(&"app.t".to_string()),
            "a parsed function must resolve its read against the file's search_path, got {:?}",
            entity.reads
        );
    }

    #[test]
    fn variadic_function_parses() {
        let entity = parse_entity(
            Path::new("ddl/function/app/vf.ddl"),
            "set search_path to app;\n\
             create function vf(variadic a int[]) returns int language sql as $$ select 1 $$;",
        )
        .unwrap();
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
    }

    // ── libpg_query validation fallback ──────────────────────────────────────
    //
    // sqlparser is a convenience parser, not Postgres's. When it rejects a file
    // that libpg_query — Postgres's own grammar — accepts, the limitation is
    // ours and the file is valid SQL. Recording a parse error there is not
    // cosmetic: it drops the entity from apply/reconcile's desired set, so the
    // object is never created and the command still exits 0.
    //
    // The fallback is deliberately NOT applied to tables: a table with no
    // `table_def` is filtered out of the desired snapshot (`reconcile.rs:91`),
    // which makes the live table read as an orphan that `--prune` would DROP.
    // Erroring keeps it visible; silently accepting it risks data loss.

    #[test]
    fn window_function_valid_in_postgres_is_not_an_error() {
        let entity = parse_entity(
            Path::new("ddl/function/app/wf.ddl"),
            "set search_path to app;\n\
             create function wf() returns int language plpgsql as $$ begin perform 1 from t; end $$ window;",
        )
        .unwrap();
        assert!(
            entity.errors.is_empty(),
            "valid Postgres must not be reported as a user error: {:?}",
            entity.errors
        );
    }

    #[test]
    fn view_with_check_option_is_not_an_error_and_keeps_its_refs() {
        let entity = parse_entity(
            Path::new("ddl/view/app/v.ddl"),
            "set search_path to app;\n\
             create view v as select * from t where a > 1 with cascaded check option;",
        )
        .unwrap();
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);
        assert!(
            entity.refers.contains(&"app.t".to_string()),
            "a recovered view must keep its dependency edge, got {:?}",
            entity.refers
        );
    }

    // The wrong-schema trap: without search_path recovery the fallback would
    // qualify reads to `public`, turning a missing edge into a plausibly-wrong
    // one that points at a different table.

    #[test]
    fn fallback_recovers_search_paths_so_refs_resolve_to_the_right_schema() {
        let entity = parse_entity(
            Path::new("ddl/function/app/wf2.ddl"),
            "set search_path to app;\n\
             create function wf2() returns int language plpgsql as $$ begin perform 1 from t; end $$ window;",
        )
        .unwrap();
        assert_eq!(entity.search_paths, vec!["app".to_string()]);
        assert!(
            entity.reads.contains(&"app.t".to_string()),
            "read must qualify against the file's search_path, not `public`: {:?}",
            entity.reads
        );
    }

    #[test]
    fn table_sqlparser_cannot_read_still_errors() {
        let entity = parse_entity(
            Path::new("ddl/table/app/excl.ddl"),
            "set search_path to app;\n\
             create table excl (id int primary key, r int4range, exclude using gist (r with &&));",
        )
        .unwrap();
        assert!(
            !entity.errors.is_empty(),
            "a table with no table_def reads as an orphan that --prune would drop; \
             it must stay visible as an error"
        );
        assert!(entity.table_def.is_none());
    }

    #[test]
    fn sql_neither_parser_accepts_still_errors() {
        let entity = parse_entity(
            Path::new("ddl/view/app/broken.ddl"),
            "create view v as SELECT * FROM ;",
        )
        .unwrap();
        assert!(!entity.errors.is_empty(), "genuinely broken SQL must still error");
    }

    // ── Guarded enum: `DO $$ … $$` around CREATE TYPE ────────────────────────
    //
    // Postgres has no `CREATE TYPE IF NOT EXISTS`, so wrapping the CREATE in a
    // DO block that swallows `duplicate_object` is the only idiom for a
    // conditional enum. sqlparser rejects `DO` outright, and a parse error drops
    // the entity from apply/reconcile's desired set — so the type was never
    // created and the first table using it died with `type "…" does not exist`,
    // never mentioning the real cause. libpg_query reads the block.

    #[test]
    fn guarded_do_block_enum_is_parsed() {
        let sql = "set search_path to app;\n\
                   \n\
                   do $$ begin\n\
                     create type status_t as enum ('active', 'archived');\n\
                   exception when duplicate_object then null;\n\
                   end $$;\n";
        let entity = parse_entity(Path::new("ddl/enum/app/status_t.ddl"), sql).unwrap();

        assert_eq!(entity.entity_type, EntityType::Enum);
        assert_eq!(entity.name, "app.status_t");
        assert!(
            entity.errors.is_empty(),
            "a guarded enum is valid Postgres and must not report a parse error: {:?}",
            entity.errors
        );
        let values: Vec<&str> = entity.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(values, vec!["active", "archived"]);
    }

    // The fallback must not turn every unparseable enum file into a silent pass.

    #[test]
    fn unparseable_enum_still_reports_an_error() {
        let entity = parse_entity(
            Path::new("ddl/enum/app/broken.ddl"),
            "THIS IS NOT SQL AT ALL ;;;",
        )
        .unwrap();
        assert!(!entity.errors.is_empty(), "a broken enum file must still surface a parse error");
        assert!(entity.enum_values.is_empty());
    }

    #[test]
    fn do_block_declaring_no_enum_keeps_its_parse_error() {
        let entity = parse_entity(
            Path::new("ddl/enum/app/empty.ddl"),
            "do $$ begin perform 1; end $$;",
        )
        .unwrap();
        assert!(
            !entity.errors.is_empty(),
            "a DO block with no CREATE TYPE declares no enum — the error must stand"
        );
        assert!(entity.enum_values.is_empty());
    }

    // ── Role round-trip: emit → parse → refers ───────────────────────────────
    //
    // Proves that generate_role_script output is correctly parsed back, so that
    // role memberships survive a dbd apply cycle.
    #[test]
    fn role_membership_round_trip() {
        use crate::entity::EntityType;
        use crate::script::ddl_from_entity;

        // Build a role entity with two parent memberships.
        let mut role = crate::entity::Entity::new(EntityType::Role, "app_ro");
        role.refers = vec!["app_admin".to_string(), "other_parent".to_string()];

        // Emit DDL via the existing Role arm of ddl_from_entity.
        let emitted = ddl_from_entity(&role).expect("ddl_from_entity must return Some for Role");

        // The emitted text should contain the GRANT lines.
        assert!(
            emitted.contains("GRANT \"app_admin\" TO \"app_ro\""),
            "emitted DDL missing app_admin grant:\n{emitted}"
        );
        assert!(
            emitted.contains("GRANT \"other_parent\" TO \"app_ro\""),
            "emitted DDL missing other_parent grant:\n{emitted}"
        );

        // Now parse the emitted DDL back — this is the path `dbd apply` takes
        // when it re-reads a file written by `dbd merge --roles`.
        let parsed = parse_entity(Path::new("ddl/role/app_ro.ddl"), &emitted)
            .expect("parse_entity must not error on role DDL");

        assert_eq!(parsed.entity_type, EntityType::Role);
        assert_eq!(parsed.name, "app_ro");

        // Both parent roles must survive the round-trip.
        assert!(
            parsed.refers.contains(&"app_admin".to_string()),
            "app_admin missing from parsed refers: {:?}",
            parsed.refers
        );
        assert!(
            parsed.refers.contains(&"other_parent".to_string()),
            "other_parent missing from parsed refers: {:?}",
            parsed.refers
        );
        assert_eq!(parsed.refers.len(), 2, "unexpected extra refers: {:?}", parsed.refers);
    }

    #[test]
    fn role_with_no_grants_has_empty_refers() {
        use crate::entity::EntityType;
        use crate::script::ddl_from_entity;

        let role = crate::entity::Entity::new(EntityType::Role, "basic");
        let emitted = ddl_from_entity(&role).unwrap();

        let parsed = parse_entity(Path::new("ddl/role/basic.ddl"), &emitted).unwrap();
        assert!(
            parsed.refers.is_empty(),
            "role with no grants should have empty refers, got {:?}",
            parsed.refers
        );
    }

    #[test]
    fn role_bare_identifier_grant_parsed() {
        // Hand-authored files may omit double-quotes; the regex must handle bare identifiers.
        let sql = "DO $$ BEGIN\n  IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'child') THEN\n    CREATE ROLE \"child\";\n  END IF;\nEND $$;\nGRANT parent TO child;\n";
        let parsed = parse_entity(Path::new("ddl/role/child.ddl"), sql).unwrap();
        assert!(
            parsed.refers.contains(&"parent".to_string()),
            "bare-identifier grant not parsed; refers: {:?}",
            parsed.refers
        );
    }

    #[test]
    fn extracts_matview_body_and_indexes() {
        let sql = "CREATE MATERIALIZED VIEW analytics.daily_sales AS\n\
                   SELECT date_trunc('day', created_at) AS day, sum(total) AS revenue\n\
                   FROM shop.orders GROUP BY 1 WITH DATA;\n\
                   CREATE UNIQUE INDEX daily_sales_day_uidx ON analytics.daily_sales(day);";
        let entity =
            parse_entity(Path::new("ddl/materialized_view/analytics/daily_sales.ddl"), sql).unwrap();

        assert_eq!(entity.entity_type, EntityType::MaterializedView);
        assert!(entity.errors.is_empty(), "unexpected parse errors: {:?}", entity.errors);

        // Body captured the same way a view's body is (verbatim in writes[0]).
        let body = entity.writes.first().expect("matview body should be captured");
        assert!(
            body.to_lowercase().contains("from shop.orders"),
            "body missing source table: {body}"
        );

        // Trailing CREATE INDEX captured like a table's indexes.
        let indexes = &entity
            .table_def
            .as_ref()
            .expect("matview should have a table_def")
            .indexes;
        assert_eq!(indexes.len(), 1, "expected exactly one index: {indexes:?}");
        assert!(indexes[0].unique, "expected a UNIQUE index");
    }

    #[test]
    fn comment_on_materialized_view_is_stripped() {
        let sql = "COMMENT ON MATERIALIZED VIEW analytics.daily_sales IS 'daily rollup';";
        let cleaned = super::preprocess_sql(sql);
        assert!(!cleaned.to_lowercase().contains("comment on materialized view"),
            "expected COMMENT ON MATERIALIZED VIEW to be stripped, got: {cleaned}");
    }
}
