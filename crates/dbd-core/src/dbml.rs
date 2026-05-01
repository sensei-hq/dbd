use crate::entity::{
    ColumnDef, Entity, EntityType, FkAction, ForeignKey, IndexDef, TableConstraint, TableDef,
};

/// Parameters for DBML generation.
pub struct DbmlParams<'a> {
    pub entities: &'a [Entity],
    pub project_name: &'a str,
    pub database_type: &'a str,
    pub project_note: Option<&'a str>,
    pub include_schemas: Vec<String>,
    pub exclude_schemas: Vec<String>,
    pub include_tables: Vec<String>,
    pub exclude_tables: Vec<String>,
}

/// A generated DBML document.
pub struct DbmlDocument {
    pub file_name: String,
    pub content: String,
}

/// Generate DBML from parsed entities, applying include/exclude filters.
pub fn generate_dbml(params: &DbmlParams) -> DbmlDocument {
    let mut sections = Vec::new();

    // Project block
    sections.push(emit_project_block(
        params.project_name,
        params.database_type,
        params.project_note,
    ));

    // Filter entities by include/exclude rules
    let filtered: Vec<&Entity> = params.entities.iter()
        .filter(|e| matches!(e.entity_type, EntityType::Table | EntityType::Enum))
        .filter(|e| is_included(e, params))
        .collect();

    // Enums
    for entity in &filtered {
        if entity.entity_type == EntityType::Enum && !entity.enum_values.is_empty() {
            sections.push(emit_enum(entity));
        }
    }

    // Tables
    for entity in &filtered {
        if entity.entity_type == EntityType::Table
            && let Some(ref table_def) = entity.table_def {
                sections.push(emit_table(
                    &entity.name,
                    entity.schema.as_deref().unwrap_or("public"),
                    table_def,
                ));
            }
    }

    // Refs (standalone, from all FK constraints — only from filtered tables)
    let refs = emit_all_refs(&filtered.iter().copied().cloned().collect::<Vec<_>>());
    if !refs.is_empty() {
        sections.push(refs);
    }

    DbmlDocument {
        file_name: "design.dbml".to_string(),
        content: sections.join("\n"),
    }
}

/// Check if an entity passes the include/exclude filters.
fn is_included(entity: &Entity, params: &DbmlParams) -> bool {
    let schema = entity.schema.as_deref().unwrap_or("public");

    // If include_schemas is set, entity must be in one of them
    if !params.include_schemas.is_empty() && !params.include_schemas.iter().any(|s| s == schema) {
        return false;
    }

    // If include_tables is set, entity must be in the list
    if !params.include_tables.is_empty() && !params.include_tables.iter().any(|t| t == &entity.name) {
        return false;
    }

    // If entity's schema is in exclude list, skip it
    if params.exclude_schemas.iter().any(|s| s == schema) {
        return false;
    }

    // If entity is in exclude tables list, skip it
    if params.exclude_tables.iter().any(|t| t == &entity.name) {
        return false;
    }

    true
}

fn emit_project_block(name: &str, db_type: &str, note: Option<&str>) -> String {
    let mut block = format!("Project \"{}\" {{\n  database_type: '{}'", name, db_type);
    if let Some(n) = note {
        let n = n.trim();
        if !n.is_empty() {
            block.push_str(&format!("\n  Note: {}", quote_dbml_string(n)));
        }
    }
    block.push_str("\n}\n");
    block
}

fn emit_enum(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let base_name = entity.name.split('.').next_back().unwrap_or(&entity.name);
    let mut lines = vec![format!("Enum \"{}\".\"{}\" {{", schema, base_name)];

    for value in &entity.enum_values {
        match &value.note {
            Some(note) => lines.push(format!("  \"{}\" [note: '{}']", value.name, note)),
            None => lines.push(format!("  \"{}\"", value.name)),
        }
    }

    lines.push("}\n".to_string());
    lines.join("\n")
}

fn emit_table(name: &str, schema: &str, table_def: &TableDef) -> String {
    let base_name = name.split('.').next_back().unwrap_or(name);
    let mut lines = vec![format!("Table \"{}\".\"{}\" {{", schema, base_name)];

    // Collect PK columns from table-level constraints
    let pk_columns: std::collections::HashSet<String> = table_def
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::PrimaryKey { columns, .. } => Some(columns.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    for col in &table_def.columns {
        lines.push(emit_column(col, &pk_columns));
    }

    // Indexes block
    let idx_block = emit_indexes(&table_def.indexes, &pk_columns);
    if !idx_block.is_empty() {
        lines.push(String::new());
        lines.push("  indexes {".to_string());
        for idx_line in idx_block {
            lines.push(format!("    {}", idx_line));
        }
        lines.push("  }".to_string());
    }

    // Table note
    if let Some(ref note) = table_def.comments.table {
        lines.push(String::new());
        lines.push(format!("  Note: {}", quote_dbml_string(note)));
    }

    lines.push("}\n".to_string());
    lines.join("\n")
}

fn emit_column(col: &ColumnDef, pk_columns: &std::collections::HashSet<String>) -> String {
    let data_type = quote_type_if_needed(&col.data_type);
    let mut settings = Vec::new();

    if col.is_pk || pk_columns.contains(&col.name) {
        settings.push("pk".to_string());
    }
    if col.is_identity {
        settings.push("increment".to_string());
    }
    if !col.nullable {
        settings.push("not null".to_string());
    }
    if col.is_unique {
        settings.push("unique".to_string());
    }
    if let Some(ref default) = col.default_value {
        settings.push(format!("default: {}", quote_default(default)));
    }
    if let Some(ref comment) = col.comment {
        // Inline notes must be single-line — collapse newlines
        let inline = comment.trim().replace('\n', " ").replace('\'', "\\'");
        settings.push(format!("note: '{}'", inline));
    }

    let settings_str = if settings.is_empty() {
        String::new()
    } else {
        format!(" [{}]", settings.join(", "))
    };

    format!("  \"{}\" {}{}", col.name, data_type, settings_str)
}

fn emit_indexes(indexes: &[IndexDef], _pk_columns: &std::collections::HashSet<String>) -> Vec<String> {
    let mut lines = Vec::new();

    for idx in indexes {
        let cols = if idx.columns.len() == 1 {
            idx.columns[0].name.clone()
        } else {
            format!(
                "({})",
                idx.columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let mut settings = Vec::new();
        if idx.unique {
            settings.push("unique".to_string());
        }
        if let Some(ref name) = idx.name {
            settings.push(format!("name: '{}'", name));
        }

        let settings_str = if settings.is_empty() {
            String::new()
        } else {
            format!(" [{}]", settings.join(", "))
        };

        lines.push(format!("{}{}", cols, settings_str));
    }

    lines
}

fn emit_all_refs(entities: &[Entity]) -> String {
    let mut lines = Vec::new();

    for entity in entities {
        if entity.entity_type != EntityType::Table {
            continue;
        }
        let Some(ref table_def) = entity.table_def else {
            continue;
        };

        let schema = entity.schema.as_deref().unwrap_or("public");
        let base_name = entity.name.split('.').next_back().unwrap_or(&entity.name);

        // Inline FKs from columns
        for col in &table_def.columns {
            if let Some(ref fk) = col.inline_fk {
                lines.push(emit_ref(schema, base_name, fk));
            }
        }

        // Table-level FKs from constraints
        for constraint in &table_def.constraints {
            if let TableConstraint::ForeignKey(fk) = constraint {
                lines.push(emit_ref(schema, base_name, fk));
            }
        }
    }

    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    }
}

fn emit_ref(source_schema: &str, source_table: &str, fk: &ForeignKey) -> String {
    let ref_schema = fk.ref_schema.as_deref().unwrap_or("public");
    let ref_table = &fk.ref_table;

    let source_cols = if fk.columns.len() == 1 {
        format!(
            "\"{}\".\"{}\".\"{}\"",
            source_schema, source_table, fk.columns[0]
        )
    } else {
        format!(
            "\"{}\".\"{}\".({})",
            source_schema,
            source_table,
            fk.columns
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let target_cols = if fk.ref_columns.len() == 1 {
        format!(
            "\"{}\".\"{}\".\"{}\"",
            ref_schema, ref_table, fk.ref_columns[0]
        )
    } else {
        format!(
            "\"{}\".\"{}\".({})",
            ref_schema,
            ref_table,
            fk.ref_columns
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let mut settings = Vec::new();
    if let Some(action) = &fk.on_delete {
        settings.push(format!("delete: {}", fk_action_str(action)));
    }
    if let Some(action) = &fk.on_update {
        settings.push(format!("update: {}", fk_action_str(action)));
    }

    let settings_str = if settings.is_empty() {
        String::new()
    } else {
        format!(" [{}]", settings.join(", "))
    };

    format!("Ref: {} > {}{}", source_cols, target_cols, settings_str)
}

fn fk_action_str(action: &FkAction) -> &'static str {
    match action {
        FkAction::Cascade => "cascade",
        FkAction::Restrict => "restrict",
        FkAction::SetNull => "set null",
        FkAction::SetDefault => "set default",
        FkAction::NoAction => "no action",
    }
}

fn quote_default(value: &str) -> String {
    let trimmed = value.trim();
    // Booleans
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        return trimmed.to_lowercase();
    }
    // Numbers
    if trimmed.parse::<f64>().is_ok() {
        return trimmed.to_string();
    }
    // NULL
    if trimmed.eq_ignore_ascii_case("null") {
        return "null".to_string();
    }
    // Expression (function call or complex expression)
    if trimmed.contains('(') || trimmed.contains("::") || trimmed.contains('+') {
        return format!("`{}`", trimmed);
    }
    // String literal
    format!("'{}'", trimmed.trim_matches('\''))
}

fn quote_type_if_needed(data_type: &str) -> String {
    if data_type.contains(' ') {
        format!("\"{}\"", data_type)
    } else {
        data_type.to_string()
    }
}

/// Format a string for DBML.
/// Single-line → single quotes: 'text'
/// Multi-line → triple quotes: '''text'''
fn quote_dbml_string(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.contains('\n') {
        format!("'''\n{}\n'''", trimmed)
    } else {
        format!("'{}'", trimmed.replace('\'', "\\'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EnumValue, IndexColumn, TableComments};

    fn make_table_entity(name: &str, columns: Vec<ColumnDef>, constraints: Vec<TableConstraint>) -> Entity {
        let mut entity = Entity::new(EntityType::Table, name);
        entity.table_def = Some(TableDef {
            columns,
            constraints,
            indexes: vec![],
            comments: TableComments::default(),
        });
        entity
    }

    fn col(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            is_identity: false,
            comment: None,
            inline_fk: None,
        }
    }

    fn pk_col(name: &str, data_type: &str) -> ColumnDef {
        ColumnDef {
            is_pk: true,
            nullable: false,
            ..col(name, data_type)
        }
    }

    #[test]
    fn project_block() {
        let block = emit_project_block("MyProject", "PostgreSQL", Some("Test project"));
        assert!(block.contains("Project \"MyProject\""));
        assert!(block.contains("database_type: 'PostgreSQL'"));
        assert!(block.contains("Note: 'Test project'"));
    }

    #[test]
    fn enum_block() {
        let mut entity = Entity::new(EntityType::Enum, "config.status");
        entity.enum_values = vec![
            EnumValue { name: "active".to_string(), note: Some("Currently active".to_string()) },
            EnumValue { name: "inactive".to_string(), note: None },
        ];
        let block = emit_enum(&entity);
        assert!(block.contains("Enum \"config\".\"status\""));
        assert!(block.contains("\"active\" [note: 'Currently active']"));
        assert!(block.contains("\"inactive\""));
    }

    #[test]
    fn table_with_columns() {
        let entity = make_table_entity(
            "config.users",
            vec![
                pk_col("id", "UUID"),
                ColumnDef {
                    nullable: false,
                    is_unique: true,
                    ..col("email", "VARCHAR(255)")
                },
                ColumnDef {
                    default_value: Some("true".to_string()),
                    ..col("is_active", "BOOLEAN")
                },
            ],
            vec![],
        );

        let table_def = entity.table_def.as_ref().unwrap();
        let block = emit_table("config.users", "config", table_def);

        assert!(block.contains("Table \"config\".\"users\""));
        assert!(block.contains("\"id\" UUID [pk, not null]"));
        assert!(block.contains("\"email\" VARCHAR(255) [not null, unique]"));
        assert!(block.contains("\"is_active\" BOOLEAN [default: true]"));
    }

    #[test]
    fn table_with_function_default() {
        let entity = make_table_entity(
            "config.items",
            vec![ColumnDef {
                default_value: Some("uuid_generate_v4()".to_string()),
                ..pk_col("id", "UUID")
            }],
            vec![],
        );

        let table_def = entity.table_def.as_ref().unwrap();
        let block = emit_table("config.items", "config", table_def);
        assert!(block.contains("default: `uuid_generate_v4()`"));
    }

    #[test]
    fn table_with_indexes() {
        let mut entity = make_table_entity(
            "config.lookups",
            vec![col("name", "VARCHAR(100)")],
            vec![],
        );
        entity.table_def.as_mut().unwrap().indexes = vec![IndexDef {
            name: Some("idx_lookups_name".to_string()),
            columns: vec![IndexColumn {
                name: "name".to_string(),
                order: None,
            }],
            unique: true,
            index_type: None,
        }];

        let table_def = entity.table_def.as_ref().unwrap();
        let block = emit_table("config.lookups", "config", table_def);
        assert!(block.contains("indexes {"));
        assert!(block.contains("name [unique, name: 'idx_lookups_name']"));
    }

    #[test]
    fn table_with_note() {
        let mut entity = make_table_entity(
            "config.lookups",
            vec![col("id", "INT")],
            vec![],
        );
        entity.table_def.as_mut().unwrap().comments.table = Some("Lookup categories".to_string());

        let table_def = entity.table_def.as_ref().unwrap();
        let block = emit_table("config.lookups", "config", table_def);
        assert!(block.contains("Note: 'Lookup categories'"));
    }

    #[test]
    fn ref_with_actions() {
        let fk = ForeignKey {
            name: None,
            columns: vec!["user_id".to_string()],
            ref_schema: Some("config".to_string()),
            ref_table: "users".to_string(),
            ref_columns: vec!["id".to_string()],
            on_delete: Some(FkAction::Cascade),
            on_update: Some(FkAction::NoAction),
        };

        let ref_line = emit_ref("config", "orders", &fk);
        assert!(ref_line.contains("Ref:"));
        assert!(ref_line.contains("\"config\".\"orders\".\"user_id\""));
        assert!(ref_line.contains("> \"config\".\"users\".\"id\""));
        assert!(ref_line.contains("[delete: cascade, update: no action]"));
    }

    #[test]
    fn ref_without_actions() {
        let fk = ForeignKey {
            columns: vec!["lookup_id".to_string()],
            ref_schema: Some("config".to_string()),
            ref_table: "lookups".to_string(),
            ref_columns: vec!["id".to_string()],
            ..Default::default()
        };

        let ref_line = emit_ref("config", "lookup_values", &fk);
        assert!(!ref_line.contains("["));
    }

    #[test]
    fn full_generation() {
        let mut enum_entity = Entity::new(EntityType::Enum, "config.status");
        enum_entity.enum_values = vec![
            EnumValue { name: "active".to_string(), note: None },
            EnumValue { name: "inactive".to_string(), note: None },
        ];

        let table_entity = make_table_entity(
            "config.users",
            vec![
                pk_col("id", "UUID"),
                ColumnDef {
                    inline_fk: Some(ForeignKey {
                        columns: vec!["status".to_string()],
                        ref_schema: Some("config".to_string()),
                        ref_table: "status".to_string(),
                        ref_columns: vec!["id".to_string()],
                        ..Default::default()
                    }),
                    ..col("status", "INT")
                },
            ],
            vec![],
        );

        let entities = vec![enum_entity, table_entity];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "TestProject",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
        });

        assert!(doc.content.contains("Project \"TestProject\""));
        assert!(doc.content.contains("Enum \"config\".\"status\""));
        assert!(doc.content.contains("Table \"config\".\"users\""));
        assert!(doc.content.contains("Ref:"));
    }

    #[test]
    fn quote_default_values() {
        assert_eq!(quote_default("true"), "true");
        assert_eq!(quote_default("false"), "false");
        assert_eq!(quote_default("42"), "42");
        assert_eq!(quote_default("3.14"), "3.14");
        assert_eq!(quote_default("null"), "null");
        assert_eq!(quote_default("now()"), "`now()`");
        assert_eq!(quote_default("uuid_generate_v4()"), "`uuid_generate_v4()`");
        assert_eq!(quote_default("'hello'"), "'hello'");
    }

    #[test]
    fn types_with_spaces_are_quoted() {
        assert_eq!(quote_type_if_needed("INT"), "INT");
        assert_eq!(
            quote_type_if_needed("TIMESTAMP WITH TIME ZONE"),
            "\"TIMESTAMP WITH TIME ZONE\""
        );
    }

    // ── Filter tests ───────────────────────────────────

    #[test]
    fn exclude_schema_filters_tables() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("staging.temp", vec![col("id", "INT")], vec![]),
        ];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec!["staging".to_string()],
            include_tables: vec![],
            exclude_tables: vec![],
        });
        assert!(doc.content.contains("config"));
        assert!(!doc.content.contains("staging"), "staging should be excluded");
    }

    #[test]
    fn include_schema_filters_to_only_included() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("staging.temp", vec![col("id", "INT")], vec![]),
        ];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec!["config".to_string()],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
        });
        assert!(doc.content.contains("config"));
        assert!(!doc.content.contains("staging"), "staging should not be included");
    }

    #[test]
    fn exclude_table_by_name() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("config.secret", vec![col("id", "INT")], vec![]),
        ];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec!["config.secret".to_string()],
        });
        assert!(doc.content.contains("users"));
        assert!(!doc.content.contains("secret"), "secret table should be excluded");
    }

    #[test]
    fn no_filters_includes_everything() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("staging.temp", vec![col("id", "INT")], vec![]),
        ];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
        });
        assert!(doc.content.contains("config"));
        assert!(doc.content.contains("staging"));
    }
}
