mod extractors;
mod tables;

use std::path::Path;

use crate::entity::{Entity, EntityType, Reference};
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
    //             COMMENT ON VIEW, FUNCTION, PROCEDURE, TRIGGER, INDEX, etc. fail.
    // Impact:     Parse error on any DDL file with non-table/column comments.
    // Check:      Parser::parse_sql("COMMENT ON VIEW foo IS 'bar';")
    // Tracking:   https://github.com/apache/datafusion-sqlparser-rs/issues
    {
        let re = regex::Regex::new(
            r"(?is)\bcomment\s+on\s+(?:view|function|procedure|trigger|index|schema|extension|type)\s+\S+\s+is\s+'[^']*(?:''[^']*)*'\s*;"
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
            entity.errors.push(format!("Parse error: {e}"));
            // For procedures/functions, we can still extract reads/writes from
            // the raw SQL even when sqlparser can't parse the full statement
            // (e.g. CREATE OR REPLACE PROCEDURE is not fully supported).
            if matches!(entity.entity_type, EntityType::Function | EntityType::Procedure) {
                let (reads, writes) = extractors::extract_proc_reads_writes(sql);
                entity.reads = reads;
                entity.writes = writes;
                entity.references = entity
                    .reads
                    .iter()
                    .chain(entity.writes.iter())
                    .map(|name| Reference {
                        name: name.clone(),
                        ref_type: None,
                    })
                    .collect();
                entity.refers = entity
                    .references
                    .iter()
                    .map(|r| r.name.clone())
                    .collect();
            }
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
            let (refs, _columns) = extractors::extract_view_info(&statements, &entity.search_paths);
            entity.references = refs;
        }
        EntityType::Enum => {
            entity.enum_values = extractors::extract_enum_values(&statements);
        }
        EntityType::Function | EntityType::Procedure => {
            let (reads, writes) = extractors::extract_proc_reads_writes(sql);
            entity.reads = reads;
            entity.writes = writes;
            // References from reads/writes
            entity.references = entity
                .reads
                .iter()
                .chain(entity.writes.iter())
                .map(|name| Reference {
                    name: name.clone(),
                    ref_type: None,
                })
                .collect();
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
}
