mod extractors;
mod tables;

use std::path::Path;

use crate::entity::{Entity, EntityType, Reference};
use crate::error::Result;

pub use extractors::extract_search_paths;

/// Parse a DDL file and produce an Entity with extracted metadata.
///
/// This is the main parser entry point. It reads the SQL, parses it with
/// sqlparser-rs (PostgreSQL dialect), and extracts:
/// - Entity identity (type, name, schema) from the file path
/// - Search paths from SET search_path statements
/// - References (FK targets, view dependencies)
/// - Table structure (columns, constraints, indexes) into TableDef
/// - Enum values
/// - Procedure reads/writes
/// Preprocess SQL to work around sqlparser 0.61 limitations:
///
/// 1. Strip unsupported COMMENT ON types (VIEW, FUNCTION, PROCEDURE, etc.)
///    sqlparser only handles COMMENT ON TABLE and COMMENT ON COLUMN.
///
/// 2. Rewrite CREATE [OR REPLACE] PROCEDURE → CREATE [OR REPLACE] FUNCTION
///    sqlparser doesn't support PROCEDURE. Since we only need the body for
///    reads/writes extraction, FUNCTION parsing produces identical results.
fn preprocess_sql(sql: &str) -> String {
    // Strip unsupported COMMENT ON types
    let comment_re = regex::Regex::new(
        r"(?is)\bcomment\s+on\s+(?:view|function|procedure|trigger|index|schema|extension|type)\s+\S+\s+is\s+'[^']*(?:''[^']*)*'\s*;"
    ).unwrap();
    let result = comment_re.replace_all(sql, "");

    // Rewrite PROCEDURE → FUNCTION for sqlparser compatibility
    let proc_re = regex::Regex::new(
        r"(?i)\b(create\s+(?:or\s+replace\s+)?)procedure\b"
    ).unwrap();
    proc_re.replace_all(&result, "${1}FUNCTION").to_string()
}

pub fn parse_entity(file: &Path, sql: &str) -> Result<Entity> {
    let mut entity = Entity::from_file(file);

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
}
