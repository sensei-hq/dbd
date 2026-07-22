use crate::config::DbmlDocConfig;
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
    /// Explicit `TableGroup` definitions emitted at the end of the document.
    /// Each tuple is `(group_name, table_qualified_names)`. Pairs an empty
    /// group is silently dropped.
    pub groups: Vec<DbmlGroup>,
    /// When `true`, additionally synthesise one `TableGroup <schema>` per
    /// distinct schema present in the filtered tables. Explicit `groups`
    /// take precedence — auto-groups are skipped for any schema already
    /// covered by an explicit group with the same name.
    pub auto_group_by_schema: bool,
}

/// An explicit DBML `TableGroup` definition.
#[derive(Debug, Clone)]
pub struct DbmlGroup {
    pub name: String,
    /// Qualified table names (`schema.name`). Tables not present after
    /// include/exclude filtering are silently dropped from the group.
    pub tables: Vec<String>,
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

    // External entity stub tables (for FK targets that are External entities)
    let external_stubs = emit_external_stubs(params.entities);
    if !external_stubs.is_empty() {
        sections.push(external_stubs);
    }

    // Table groups — explicit first, then auto-by-schema for any schema the
    // explicit groups didn't cover.
    let included_tables: Vec<&Entity> = filtered
        .iter()
        .copied()
        .filter(|e| e.entity_type == EntityType::Table)
        .collect();
    let table_groups = emit_table_groups(
        &included_tables,
        &params.groups,
        params.auto_group_by_schema,
    );
    if !table_groups.is_empty() {
        sections.push(table_groups);
    }

    DbmlDocument {
        file_name: "design.dbml".to_string(),
        content: sections.join("\n"),
    }
}

/// Inputs for `generate_all`. Mirrors `DbmlParams` but the per-doc filter
/// state comes from the `DesignConfig` rather than being repeated per call.
pub struct DbmlMultiParams<'a> {
    pub entities: &'a [Entity],
    pub project_name: &'a str,
    pub database_type: &'a str,
    pub project_note: Option<&'a str>,
    pub docs: &'a std::collections::HashMap<String, DbmlDocConfig>,
}

/// Generate one `DbmlDocument` per entry in `docs`. The document
/// `file_name` is the doc's configured `output`, or `<key>.dbml` when
/// no `output` is set. When `docs` is empty, returns a single
/// `design.dbml` document with no filters and no groups (the
/// no-config default).
pub fn generate_all(params: &DbmlMultiParams<'_>) -> Vec<DbmlDocument> {
    if params.docs.is_empty() {
        let doc = generate_dbml(&DbmlParams {
            entities: params.entities,
            project_name: params.project_name,
            database_type: params.database_type,
            project_note: params.project_note,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });
        return vec![doc];
    }

    // Sort keys so output ordering is deterministic.
    let mut keys: Vec<&String> = params.docs.keys().collect();
    keys.sort();

    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let cfg = &params.docs[key];
        let (inc_schemas, exc_schemas, inc_tables, exc_tables) = (
            cfg.include.as_ref().map(|f| f.schemas.clone()).unwrap_or_default(),
            cfg.exclude.as_ref().map(|f| f.schemas.clone()).unwrap_or_default(),
            cfg.include.as_ref().map(|f| f.tables.clone()).unwrap_or_default(),
            cfg.exclude.as_ref().map(|f| f.tables.clone()).unwrap_or_default(),
        );
        let groups: Vec<DbmlGroup> = cfg
            .groups
            .iter()
            .map(|g| DbmlGroup {
                name: g.name.clone(),
                tables: g.tables.clone(),
            })
            .collect();
        let mut doc = generate_dbml(&DbmlParams {
            entities: params.entities,
            project_name: params.project_name,
            database_type: params.database_type,
            project_note: params.project_note,
            include_schemas: inc_schemas,
            exclude_schemas: exc_schemas,
            include_tables: inc_tables,
            exclude_tables: exc_tables,
            groups,
            auto_group_by_schema: cfg.auto_group_by_schema,
        });
        doc.file_name = cfg
            .output
            .clone()
            .unwrap_or_else(|| format!("{key}.dbml"));
        out.push(doc);
    }
    out
}

/// Emit `TableGroup` blocks for the given filtered tables. Returns an
/// empty string when no groups would have any tables.
fn emit_table_groups(
    included_tables: &[&Entity],
    explicit_groups: &[DbmlGroup],
    auto_group_by_schema: bool,
) -> String {
    let included_names: std::collections::HashSet<&str> =
        included_tables.iter().map(|e| e.name.as_str()).collect();

    let mut blocks: Vec<String> = Vec::new();
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for group in explicit_groups {
        let tables: Vec<&String> = group
            .tables
            .iter()
            .filter(|t| included_names.contains(t.as_str()))
            .collect();
        if tables.is_empty() {
            continue;
        }
        blocks.push(format_table_group(&group.name, &tables));
        used_names.insert(group.name.clone());
    }

    if auto_group_by_schema {
        let mut by_schema: std::collections::BTreeMap<String, Vec<&String>> =
            std::collections::BTreeMap::new();
        for e in included_tables {
            let schema = e.schema.as_deref().unwrap_or("public").to_string();
            by_schema.entry(schema).or_default().push(&e.name);
        }
        for (schema, names) in &by_schema {
            if used_names.contains(schema) {
                continue;
            }
            blocks.push(format_table_group(schema, names));
        }
    }

    blocks.join("\n")
}

fn format_table_group(name: &str, tables: &[&String]) -> String {
    let mut sorted: Vec<&&String> = tables.iter().collect();
    sorted.sort();
    let mut out = format!("TableGroup \"{}\" {{\n", name);
    for t in sorted {
        let (schema, base) = match t.split_once('.') {
            Some((s, b)) => (s, b),
            None => ("public", t.as_str()),
        };
        out.push_str(&format!("  \"{schema}\".\"{base}\"\n"));
    }
    out.push_str("}\n");
    out
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
    if col.identity.is_some() {
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

/// Generate stub Table blocks for External entities that are FK targets.
///
/// Scans all Table entities for FK constraints (inline + table-level) pointing
/// to an External entity. For each such external, emits a minimal Table block
/// containing only the referenced columns plus an "[external]" note.
/// External entities with no FK references (e.g. functions like auth.uid) are skipped.
fn emit_external_stubs(entities: &[Entity]) -> String {
    // Collect external entity names for quick lookup
    let external_names: std::collections::HashSet<&str> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::External)
        .map(|e| e.name.as_str())
        .collect();

    if external_names.is_empty() {
        return String::new();
    }

    // For each external entity, collect referenced columns from FK constraints
    let mut external_refs: std::collections::HashMap<&str, std::collections::HashSet<String>> =
        std::collections::HashMap::new();

    for entity in entities {
        if entity.entity_type != EntityType::Table {
            continue;
        }
        let Some(ref table_def) = entity.table_def else {
            continue;
        };

        // Inline FKs from columns
        for col in &table_def.columns {
            if let Some(ref fk) = col.inline_fk {
                record_external_fk(fk, &external_names, &mut external_refs);
            }
        }

        // Table-level FK constraints
        for constraint in &table_def.constraints {
            if let TableConstraint::ForeignKey(fk) = constraint {
                record_external_fk(fk, &external_names, &mut external_refs);
            }
        }
    }

    if external_refs.is_empty() {
        return String::new();
    }

    let mut blocks = Vec::new();
    let mut sorted_names: Vec<&&str> = external_refs.keys().collect();
    sorted_names.sort();

    for ext_name in sorted_names {
        let cols = external_refs.get(ext_name).unwrap();
        blocks.push(emit_external_stub_block(ext_name, cols));
    }

    blocks.join("\n")
}

/// If `fk` targets a known external entity, record its referenced columns under
/// that entity's name in `external_refs`.
fn record_external_fk<'a>(
    fk: &crate::entity::ForeignKey,
    external_names: &std::collections::HashSet<&'a str>,
    external_refs: &mut std::collections::HashMap<&'a str, std::collections::HashSet<String>>,
) {
    let ref_schema = fk.ref_schema.as_deref().unwrap_or("public");
    let qualified = format!("{}.{}", ref_schema, fk.ref_table);
    if let Some(ext_name) = external_names.iter().find(|n| **n == qualified) {
        let entry = external_refs.entry(ext_name).or_default();
        for rc in &fk.ref_columns {
            entry.insert(rc.clone());
        }
    }
}

/// Render a DBML stub `Table` block for one external entity and its referenced
/// columns (typed generically as `varchar`, since the real types are unknown).
fn emit_external_stub_block(
    ext_name: &str,
    cols: &std::collections::HashSet<String>,
) -> String {
    let (schema, table_name) = match ext_name.split_once('.') {
        Some((s, t)) => (s, t),
        None => ("public", ext_name),
    };

    let mut lines = vec![format!("Table \"{}\".\"{}\" {{", schema, table_name)];
    let mut sorted_cols: Vec<&String> = cols.iter().collect();
    sorted_cols.sort();
    for col_name in sorted_cols {
        lines.push(format!("  \"{}\" varchar", col_name));
    }
    lines.push(String::new());
    lines.push("  Note: 'External entity — managed outside this project'".to_string());
    lines.push("}\n".to_string());
    lines.join("\n")
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
            identity: None,
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
            groups: vec![],
            auto_group_by_schema: false,
        });

        assert!(doc.content.contains("Project \"TestProject\""));
        assert!(doc.content.contains("Enum \"config\".\"status\""));
        assert!(doc.content.contains("Table \"config\".\"users\""));
        assert!(doc.content.contains("Ref:"));
    }

    #[test]
    fn composite_fk_emits_dbml_tuple_syntax() {
        // DBML spec: `Ref: table.(col1, col2) > other.(col1, col2)`.
        // Schema-qualified composite refs keep the tuple on the right of the dot.
        let fk = ForeignKey {
            name: Some("orders_user_tenant_fk".into()),
            columns: vec!["user_id".into(), "tenant_id".into()],
            ref_schema: Some("auth".into()),
            ref_table: "memberships".into(),
            ref_columns: vec!["user_id".into(), "tenant_id".into()],
            on_delete: Some(FkAction::Cascade),
            on_update: None,
        };
        let line = emit_ref("shop", "orders", &fk);
        assert!(
            line.contains("\"shop\".\"orders\".(\"user_id\", \"tenant_id\")"),
            "source side missing composite tuple: {line}"
        );
        assert!(
            line.contains("\"auth\".\"memberships\".(\"user_id\", \"tenant_id\")"),
            "target side missing composite tuple: {line}"
        );
        assert!(
            line.contains("[delete: cascade]"),
            "expected delete action: {line}"
        );
    }

    #[test]
    fn composite_fk_round_trips_through_full_generation() {
        // End-to-end: a TableConstraint::ForeignKey with multiple columns
        // should surface as a single composite Ref line.
        let mut child = make_table_entity(
            "shop.orders",
            vec![
                pk_col("id", "UUID"),
                col("user_id", "UUID"),
                col("tenant_id", "UUID"),
            ],
            vec![],
        );
        child.table_def.as_mut().unwrap().constraints.push(
            TableConstraint::ForeignKey(ForeignKey {
                name: None,
                columns: vec!["user_id".into(), "tenant_id".into()],
                ref_schema: Some("auth".into()),
                ref_table: "memberships".into(),
                ref_columns: vec!["user_id".into(), "tenant_id".into()],
                on_delete: None,
                on_update: None,
            }),
        );
        let parent = make_table_entity(
            "auth.memberships",
            vec![
                pk_col("user_id", "UUID"),
                pk_col("tenant_id", "UUID"),
            ],
            vec![],
        );
        let entities = vec![child, parent];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Composite",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });
        let refs: Vec<&str> = doc.content.lines().filter(|l| l.starts_with("Ref:")).collect();
        assert_eq!(refs.len(), 1, "expected one composite Ref line, got {refs:?}");
        assert!(refs[0].contains("(\"user_id\", \"tenant_id\")"));
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
            groups: vec![],
            auto_group_by_schema: false,
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
            groups: vec![],
            auto_group_by_schema: false,
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
            groups: vec![],
            auto_group_by_schema: false,
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
            groups: vec![],
            auto_group_by_schema: false,
        });
        assert!(doc.content.contains("config"));
        assert!(doc.content.contains("staging"));
    }

    // ── External entity stub tests ───────────────────────

    #[test]
    fn external_entity_renders_as_stub_table() {
        let table_entity = make_table_entity(
            "config.profiles",
            vec![
                pk_col("id", "UUID"),
                ColumnDef {
                    inline_fk: Some(ForeignKey {
                        columns: vec!["user_id".to_string()],
                        ref_schema: Some("auth".to_string()),
                        ref_table: "users".to_string(),
                        ref_columns: vec!["id".to_string()],
                        ..Default::default()
                    }),
                    ..col("user_id", "UUID")
                },
            ],
            vec![],
        );
        let external_entity = Entity::new(EntityType::External, "auth.users");

        let entities = vec![table_entity, external_entity];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        assert!(doc.content.contains("Table \"auth\".\"users\""), "should have stub table for auth.users");
        assert!(doc.content.contains("\"id\" varchar"), "stub should contain referenced column");
        assert!(doc.content.contains("External entity"), "stub should have external note");
    }

    #[test]
    fn external_entity_without_fk_refs_skipped() {
        let table_entity = make_table_entity(
            "config.profiles",
            vec![pk_col("id", "UUID")],
            vec![],
        );
        // auth.uid is a function, not a FK target
        let external_entity = Entity::new(EntityType::External, "auth.uid");

        let entities = vec![table_entity, external_entity];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        assert!(!doc.content.contains("auth.uid"), "external without FK refs should not appear");
        assert!(!doc.content.contains("\"auth\".\"uid\""), "external without FK refs should not appear as table");
    }

    // ── TableGroup tests ───────────────────────────────

    #[test]
    fn auto_group_by_schema_emits_one_group_per_schema() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("config.roles", vec![col("id", "INT")], vec![]),
            make_table_entity("audit.logs", vec![col("id", "INT")], vec![]),
        ];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "G",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: true,
        });
        // One group per schema, alphabetical (audit before config).
        let audit = doc.content.find("TableGroup \"audit\"").expect(&doc.content);
        let config = doc.content.find("TableGroup \"config\"").expect(&doc.content);
        assert!(audit < config);
        // Each group lists its schema's tables.
        let g_audit = &doc.content[audit..config];
        assert!(g_audit.contains("\"audit\".\"logs\""), "audit group: {g_audit}");
        let g_config = &doc.content[config..];
        assert!(g_config.contains("\"config\".\"roles\""), "config group: {g_config}");
        assert!(g_config.contains("\"config\".\"users\""), "config group: {g_config}");
    }

    #[test]
    fn explicit_group_takes_precedence_over_auto_for_same_name() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("config.roles", vec![col("id", "INT")], vec![]),
            make_table_entity("audit.logs", vec![col("id", "INT")], vec![]),
        ];
        let explicit = DbmlGroup {
            name: "config".into(),
            // Only one of config's tables in the explicit group.
            tables: vec!["config.users".into()],
        };
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "G",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![explicit],
            auto_group_by_schema: true,
        });
        // Explicit "config" group present and contains only users — roles is
        // NOT auto-injected because the explicit name claimed the slot.
        let config_pos = doc.content.find("TableGroup \"config\"").expect(&doc.content);
        let next_group_or_end = doc.content[config_pos + 1..]
            .find("TableGroup ")
            .map(|p| config_pos + 1 + p)
            .unwrap_or(doc.content.len());
        let group_body = &doc.content[config_pos..next_group_or_end];
        assert!(group_body.contains("\"config\".\"users\""), "{group_body}");
        assert!(!group_body.contains("\"config\".\"roles\""), "{group_body}");
        // audit auto-group still appears (its name wasn't claimed).
        assert!(doc.content.contains("TableGroup \"audit\""));
    }

    #[test]
    fn explicit_group_drops_tables_not_in_filtered_set() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            // staging.temp is filtered out below — must not surface in the group.
            make_table_entity("staging.temp", vec![col("id", "INT")], vec![]),
        ];
        let explicit = DbmlGroup {
            name: "core".into(),
            tables: vec!["config.users".into(), "staging.temp".into()],
        };
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "G",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec!["staging".into()],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![explicit],
            auto_group_by_schema: false,
        });
        assert!(doc.content.contains("TableGroup \"core\""));
        assert!(doc.content.contains("\"config\".\"users\""));
        assert!(!doc.content.contains("\"staging\".\"temp\""));
    }

    // ── Multi-document tests ───────────────────────────

    #[test]
    fn generate_all_with_empty_docs_falls_back_to_single_document() {
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT")], vec![])];
        let docs_cfg: std::collections::HashMap<String, crate::config::DbmlDocConfig> =
            std::collections::HashMap::new();
        let docs = generate_all(&DbmlMultiParams {
            entities: &entities,
            project_name: "P",
            database_type: "PostgreSQL",
            project_note: None,
            docs: &docs_cfg,
        });
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].file_name, "design.dbml");
        assert!(docs[0].content.contains("Table \"config\".\"users\""));
    }

    #[test]
    fn generate_all_emits_one_document_per_configured_key() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
            make_table_entity("audit.logs", vec![col("id", "INT")], vec![]),
        ];
        let mut docs_cfg: std::collections::HashMap<String, crate::config::DbmlDocConfig> =
            std::collections::HashMap::new();
        docs_cfg.insert(
            "core".to_string(),
            crate::config::DbmlDocConfig {
                include: Some(crate::config::DbmlFilter {
                    schemas: vec!["config".into()],
                    tables: vec![],
                }),
                exclude: None,
                output: Some("core.dbml".into()),
                auto_group_by_schema: false,
                groups: vec![],
            },
        );
        docs_cfg.insert(
            "audit".to_string(),
            crate::config::DbmlDocConfig {
                include: Some(crate::config::DbmlFilter {
                    schemas: vec!["audit".into()],
                    tables: vec![],
                }),
                exclude: None,
                output: None,
                auto_group_by_schema: true,
                groups: vec![],
            },
        );

        let docs = generate_all(&DbmlMultiParams {
            entities: &entities,
            project_name: "P",
            database_type: "PostgreSQL",
            project_note: None,
            docs: &docs_cfg,
        });
        assert_eq!(docs.len(), 2);
        // Sorted by key: "audit" before "core".
        assert_eq!(docs[0].file_name, "audit.dbml");
        assert_eq!(docs[1].file_name, "core.dbml");
        // Each doc only contains its filtered schema.
        assert!(docs[0].content.contains("audit"));
        assert!(!docs[0].content.contains("config.users"));
        assert!(docs[1].content.contains("config"));
        assert!(!docs[1].content.contains("audit.logs"));
        // The audit doc had auto_group_by_schema = true.
        assert!(docs[0].content.contains("TableGroup \"audit\""));
    }

    #[test]
    fn no_groups_when_neither_explicit_nor_auto() {
        let entities = vec![
            make_table_entity("config.users", vec![col("id", "INT")], vec![]),
        ];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "G",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });
        assert!(!doc.content.contains("TableGroup"));
    }
}
