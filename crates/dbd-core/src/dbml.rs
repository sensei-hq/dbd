use crate::config::DbmlDocConfig;
use crate::entity::{ColumnDef, Entity, EntityType, ForeignKey, IndexColumn, IndexDef, TableConstraint, TableDef};

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
    // The filters run in here, so `entities` is already the whole design.
    generate_dbml_in(params, params.entities)
}

/// [`generate_dbml`], typing stub tables from `design` — every entity in the
/// design, which is wider than `params.entities` when a scope narrowed them.
fn generate_dbml_in(params: &DbmlParams, design: &[Entity]) -> DbmlDocument {
    let mut sections = Vec::new();

    // Project block
    sections.push(emit_project_block(
        params.project_name,
        params.database_type,
        params.project_note,
    ));

    // Filter entities by include/exclude rules
    let filtered: Vec<&Entity> = params
        .entities
        .iter()
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
            && let Some(ref table_def) = entity.table_def
        {
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

    // Table groups — explicit first, then auto-by-schema for any schema the
    // explicit groups didn't cover.
    let included_tables: Vec<&Entity> = filtered
        .iter()
        .copied()
        .filter(|e| e.entity_type == EntityType::Table)
        .collect();

    // A stub for every table a Ref points at that this document does not
    // define: a Ref to an undefined table is a DBML error, whether the target
    // is an External, a table a scope or filter left out, or one the design
    // never declares.
    let stubs = emit_ref_target_stubs(&included_tables, design);
    if !stubs.is_empty() {
        sections.push(stubs);
    }
    let table_groups = emit_table_groups(&included_tables, &params.groups, params.auto_group_by_schema);
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
    /// The entities to document — under `--scope`, the scope's working set.
    pub entities: &'a [Entity],
    /// Every entity in the design, before a scope narrowed `entities`. Read
    /// only to give a stub table — the stand-in for a referenced table the
    /// document leaves out — the referenced columns' real types instead of a
    /// `varchar` guess. Pass `entities` again when nothing was narrowed.
    pub design_entities: &'a [Entity],
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
        let doc = generate_dbml_in(
            &DbmlParams {
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
            },
            params.design_entities,
        );
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
        let mut doc = generate_dbml_in(
            &DbmlParams {
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
            },
            params.design_entities,
        );
        doc.file_name = cfg.output.clone().unwrap_or_else(|| format!("{key}.dbml"));
        out.push(doc);
    }
    out
}

/// Emit `TableGroup` blocks for the given filtered tables. Returns an
/// empty string when no groups would have any tables.
fn emit_table_groups(included_tables: &[&Entity], explicit_groups: &[DbmlGroup], auto_group_by_schema: bool) -> String {
    let included_names: std::collections::HashSet<&str> = included_tables.iter().map(|e| e.name.as_str()).collect();

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
        let mut by_schema: std::collections::BTreeMap<String, Vec<&String>> = std::collections::BTreeMap::new();
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
            Some(note) => lines.push(format!("  \"{}\" [note: {}]", value.name, dbml_string(note))),
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

    // Indexes block. A table-level UNIQUE is listed here first: DBML has no
    // constraint syntax for it, and a named unique index is how DBML spells
    // one. Leaving it out made `init --from-dbml` rebuild a table that accepts
    // the duplicates the design refuses. It reads back as a unique index —
    // DBML cannot tell the two apart — which enforces the same thing under the
    // same name.
    let indexes: Vec<IndexDef> = unique_constraint_indexes(table_def)
        .chain(table_def.indexes.iter().cloned())
        .collect();
    let idx_block = emit_indexes(&indexes);
    if !idx_block.is_empty() {
        lines.push(String::new());
        lines.push("  indexes {".to_string());
        for idx_line in idx_block {
            lines.push(format!("    {}", idx_line));
        }
        lines.push("  }".to_string());
    }

    // Checks block. DBML has had `checks { … }` since @dbml/core v5; without
    // it every CHECK the design enforces was missing from the document and
    // from the table `init --from-dbml` rebuilt. The parser has already hoisted
    // column-level CHECKs into table constraints, so they all land here.
    let checks: Vec<String> = table_def
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::Check { name, expression } => Some(check_line(name.as_deref(), expression)),
            _ => None,
        })
        .collect();
    if !checks.is_empty() {
        lines.push(String::new());
        lines.push("  checks {".to_string());
        for check in checks {
            lines.push(format!("    {check}"));
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

/// One entry of a `checks { … }` block: the expression in backticks, named
/// when the constraint is. DBML reads a backticked expression raw and to the
/// next backtick — there is no escape — so a line break is written as a space
/// (the parser on the other side is line-based) and an expression containing a
/// backtick cannot be written faithfully at all.
fn check_line(name: Option<&str>, expression: &str) -> String {
    let expression = expression.replace(['\r', '\n'], " ");
    match name {
        Some(name) => format!("`{expression}` [name: {}]", dbml_string(name)),
        None => format!("`{expression}`"),
    }
}

/// Each table-level `UNIQUE` constraint as the unique index DBML writes it as.
fn unique_constraint_indexes(table_def: &TableDef) -> impl Iterator<Item = IndexDef> + '_ {
    table_def.constraints.iter().filter_map(|c| match c {
        TableConstraint::Unique {
            name,
            columns,
            nulls_not_distinct,
        } => Some(IndexDef {
            name: name.clone(),
            columns: columns
                .iter()
                .map(|column| IndexColumn {
                    name: column.clone(),
                    ..Default::default()
                })
                .collect(),
            unique: true,
            nulls_not_distinct: *nulls_not_distinct,
            ..Default::default()
        }),
        _ => None,
    })
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
        let inline = comment.trim().replace('\n', " ");
        settings.push(format!("note: {}", dbml_string(&inline)));
    }

    let settings_str = if settings.is_empty() {
        String::new()
    } else {
        format!(" [{}]", settings.join(", "))
    };

    format!("  \"{}\" {}{}", col.name, data_type, settings_str)
}

fn emit_indexes(indexes: &[IndexDef]) -> Vec<String> {
    let mut lines = Vec::new();

    for idx in indexes {
        let cols = if idx.columns.len() == 1 {
            index_key(&idx.columns[0])
        } else {
            format!("({})", idx.columns.iter().map(index_key).collect::<Vec<_>>().join(", "))
        };

        let mut settings = Vec::new();
        if idx.unique {
            settings.push("unique".to_string());
        }
        if let Some(ref name) = idx.name {
            settings.push(format!("name: {}", dbml_string(name)));
        }
        if let Some(note) = index_note::write(idx) {
            settings.push(format!("note: {}", dbml_string(&note)));
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

/// One index key as DBML writes it: a column by name, an expression in
/// backticks. Written bare, an expression reads back as a column literally
/// named `lower(email)`, and the DDL rebuilt from it quotes it as one.
fn index_key(key: &IndexColumn) -> String {
    if key.is_expression {
        format!("`{}`", key.name)
    } else {
        key.name.clone()
    }
}

/// The index `note:` convention: where dbd writes what DBML's index syntax
/// cannot hold, and where [`crate::dbml_parse`] reads it back.
///
/// DBML's index settings are `type` (btree or hash), `name`, `unique`, `pk` and
/// `note`; its reference parser rejects anything else. So a partial index's
/// `WHERE`, a key's sort order and `NULLS NOT DISTINCT` have nowhere to go but
/// the note — and dropping them is not cosmetic. A unique index on
/// `(customer_id) WHERE status = 'open'` came back from `init --from-dbml` as a
/// unique index on `(customer_id)`: one order per customer, ever.
///
/// The note holds one fact per line. These are the only lines read back, each
/// matched exactly at the start of a line, and anything else is ignored (an
/// index has no comment to keep it in):
///
/// - `where: <predicate>` — the partial-index predicate, SQL as written. A
///   line break inside it is written as a space, since lines separate facts.
/// - `order: <entry>, <entry>, …` — one entry per key, in key order: `asc` or
///   `desc`, optionally followed by `nulls first` or `nulls last`. Written only
///   when some key is not plain ascending. `asc` reads back as no stated
///   direction, which is what Postgres means by an unadorned key. A line whose
///   entries do not cover every key, or do not parse, is ignored whole rather
///   than applied to the wrong keys.
/// - `nulls not distinct` — the index treats NULLs as equal.
pub(crate) mod index_note {
    use crate::entity::{IndexColumn, IndexDef, SortOrder};

    const WHERE: &str = "where:";
    const ORDER: &str = "order:";
    const NULLS_NOT_DISTINCT: &str = "nulls not distinct";

    /// The note for `ix`, or `None` when DBML's own syntax already says it all.
    pub(crate) fn write(ix: &IndexDef) -> Option<String> {
        let mut lines = Vec::new();
        if let Some(predicate) = &ix.predicate {
            lines.push(format!("{WHERE} {}", predicate.replace(['\r', '\n'], " ")));
        }
        if ix
            .columns
            .iter()
            .any(|k| k.order == Some(SortOrder::Desc) || k.nulls_first.is_some())
        {
            let entries: Vec<String> = ix.columns.iter().map(order_entry).collect();
            lines.push(format!("{ORDER} {}", entries.join(", ")));
        }
        if ix.nulls_not_distinct {
            lines.push(NULLS_NOT_DISTINCT.to_string());
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Apply the convention's lines in `note` to `ix`, whose keys are already set.
    pub(crate) fn read(note: &str, ix: &mut IndexDef) {
        for line in note.lines().map(str::trim) {
            if let Some(predicate) = line.strip_prefix(WHERE).map(str::trim) {
                if !predicate.is_empty() {
                    ix.predicate = Some(predicate.to_string());
                }
            } else if let Some(entries) = line.strip_prefix(ORDER) {
                read_order(entries, &mut ix.columns);
            } else if line == NULLS_NOT_DISTINCT {
                ix.nulls_not_distinct = true;
            }
        }
    }

    fn order_entry(key: &IndexColumn) -> String {
        let direction = if key.order == Some(SortOrder::Desc) {
            "desc"
        } else {
            "asc"
        };
        match key.nulls_first {
            Some(true) => format!("{direction} nulls first"),
            Some(false) => format!("{direction} nulls last"),
            None => direction.to_string(),
        }
    }

    fn read_order(entries: &str, keys: &mut [IndexColumn]) {
        let parsed: Option<Vec<_>> = entries.split(',').map(parse_order_entry).collect();
        if let Some(parsed) = parsed.filter(|p| p.len() == keys.len()) {
            for (key, (order, nulls_first)) in keys.iter_mut().zip(parsed) {
                key.order = order;
                key.nulls_first = nulls_first;
            }
        }
    }

    fn parse_order_entry(entry: &str) -> Option<(Option<SortOrder>, Option<bool>)> {
        let words: Vec<String> = entry.split_whitespace().map(str::to_ascii_lowercase).collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let (order, rest) = match words.split_first()? {
            (&"asc", rest) => (None, rest),
            (&"desc", rest) => (Some(SortOrder::Desc), rest),
            _ => return None,
        };
        let nulls_first = match rest {
            [] => None,
            ["nulls", "first"] => Some(true),
            ["nulls", "last"] => Some(false),
            _ => return None,
        };
        Some((order, nulls_first))
    }
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

/// Render a DBML column reference: `"schema"."table"."col"` for a single
/// column, or `"schema"."table".("c1", "c2")` for a composite key.
fn qualify_columns(schema: &str, table: &str, cols: &[String]) -> String {
    if cols.len() == 1 {
        format!("\"{}\".\"{}\".\"{}\"", schema, table, cols[0])
    } else {
        format!(
            "\"{}\".\"{}\".({})",
            schema,
            table,
            cols.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ")
        )
    }
}

fn emit_ref(source_schema: &str, source_table: &str, fk: &ForeignKey) -> String {
    let ref_schema = fk.ref_schema.as_deref().unwrap_or("public");
    let ref_table = &fk.ref_table;

    let source_cols = qualify_columns(source_schema, source_table, &fk.columns);
    let target_cols = qualify_columns(ref_schema, ref_table, &fk.ref_columns);

    let mut settings = Vec::new();
    if let Some(action) = &fk.on_delete {
        settings.push(format!("delete: {}", action.as_dbml()));
    }
    if let Some(action) = &fk.on_update {
        settings.push(format!("update: {}", action.as_dbml()));
    }

    let settings_str = if settings.is_empty() {
        String::new()
    } else {
        format!(" [{}]", settings.join(", "))
    };

    format!("Ref: {} > {}{}", source_cols, target_cols, settings_str)
}

/// Stub `Table` blocks for every table a Ref from `documented` points at that
/// `documented` does not define, each holding only the referenced columns.
///
/// A Ref to a table the document does not define is a DBML error, and stubs
/// used to exist only for External entities — so `dbd dbml --scope` wrote Refs
/// to the scope's out-of-scope parents with nothing for them to land on. The
/// target is looked up in `design`: a table the design defines keeps the
/// referenced columns' real types; an External, or a table the design never
/// declares, has no types to give and gets `varchar`.
fn emit_ref_target_stubs(documented: &[&Entity], design: &[Entity]) -> String {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    let defined: HashSet<String> = documented.iter().map(|e| qualified_name(e)).collect();
    let mut targets: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for entity in documented {
        let Some(table_def) = &entity.table_def else {
            continue;
        };
        let inline = table_def.columns.iter().filter_map(|c| c.inline_fk.as_ref());
        let table_level = table_def.constraints.iter().filter_map(|c| match c {
            TableConstraint::ForeignKey(fk) => Some(fk),
            _ => None,
        });
        for fk in inline.chain(table_level) {
            // Named exactly as `emit_ref` names the target, so the stub is the
            // table the Ref line points at.
            let target = format!("{}.{}", fk.ref_schema.as_deref().unwrap_or("public"), fk.ref_table);
            if !defined.contains(&target) {
                targets
                    .entry(target)
                    .or_default()
                    .extend(fk.ref_columns.iter().map(String::as_str));
            }
        }
    }

    targets
        .iter()
        .map(|(target, columns)| {
            let known = design.iter().find(|e| {
                matches!(e.entity_type, EntityType::Table | EntityType::External) && qualified_name(e) == *target
            });
            emit_stub_block(target, columns, known)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `schema.table` for an entity, as the document's `Table` and `Ref` lines
/// name it.
fn qualified_name(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let base = entity.name.split('.').next_back().unwrap_or(&entity.name);
    format!("{schema}.{base}")
}

/// One stub `Table` block: the referenced `columns` of `target`, typed from
/// `known` when the design defines it, and a note saying why it is a stub.
fn emit_stub_block(target: &str, columns: &std::collections::BTreeSet<&str>, known: Option<&Entity>) -> String {
    let (schema, table) = target.split_once('.').unwrap_or(("public", target));
    let known_columns = known.and_then(|e| e.table_def.as_ref()).map(|td| td.columns.as_slice());

    let mut lines = vec![format!("Table \"{schema}\".\"{table}\" {{")];
    for column in columns {
        let data_type = known_columns
            .and_then(|cols| cols.iter().find(|c| c.name == *column))
            .map_or_else(|| "varchar".to_string(), |c| quote_type_if_needed(&c.data_type));
        lines.push(format!("  \"{column}\" {data_type}"));
    }
    let note = match known.map(|e| e.entity_type) {
        Some(EntityType::External) => "External entity — managed outside this project",
        Some(_) => "Defined in this project, outside this document — only the referenced columns are shown",
        None => "Referenced, but not defined in this project",
    };
    lines.push(String::new());
    lines.push(format!("  Note: {}", dbml_string(note)));
    lines.push("}\n".to_string());
    lines.join("\n")
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
    // String literal — a SQL `'…'` (whose `''` is one quote) or a bare word.
    // DBML gets the text itself, in DBML's escaping: SQL's doubled quote is
    // not a DBML escape, and written as-is it ends the string.
    let text = match trimmed.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        Some(inner) => inner.replace("''", "'"),
        None => trimmed.trim_matches('\'').to_string(),
    };
    dbml_string(&text)
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
        format!("'''\n{}\n'''", dbml_multiline_body(trimmed))
    } else {
        dbml_string(trimmed)
    }
}

/// A DBML single-quoted string literal, `'…'`.
///
/// DBML's lexer reads a backslash in a quoted string as an escape — `\\`,
/// `\'`, `\n`, and any other `\x` as plain `x` — and a single-quoted string
/// cannot span lines. So every backslash and quote is escaped and a line break
/// written as `\n`. Escaping only the quote made `C:\temp` read back as
/// `C:<tab>emp` and a regex `\d` as `d`; not escaping it at all ended the
/// string at the first apostrophe.
fn dbml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// The body of a DBML `'''` string. Backslashes are escapes there too, and
/// three quotes in a row end the string, so each `\` is doubled and a quote
/// that would open a `'''` run is escaped. Other quotes stay as written —
/// apostrophes are common in prose notes and DBML needs nothing done to them.
fn dbml_multiline_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' if text[i..].starts_with("'''") => out.push_str("\\'"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EnumValue, FkAction, TableComments};

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
            generated: None,
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
            EnumValue {
                name: "active".to_string(),
                note: Some("Currently active".to_string()),
            },
            EnumValue {
                name: "inactive".to_string(),
                note: None,
            },
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
        let mut entity = make_table_entity("config.lookups", vec![col("name", "VARCHAR(100)")], vec![]);
        entity.table_def.as_mut().unwrap().indexes = vec![IndexDef {
            name: Some("idx_lookups_name".to_string()),
            columns: vec![IndexColumn {
                name: "name".to_string(),
                order: None,
                ..Default::default()
            }],
            unique: true,
            index_type: None,
            ..Default::default()
        }];

        let table_def = entity.table_def.as_ref().unwrap();
        let block = emit_table("config.lookups", "config", table_def);
        assert!(block.contains("indexes {"));
        assert!(block.contains("name [unique, name: 'idx_lookups_name']"));
    }

    /// DBML has no table-constraint syntax for UNIQUE; its spelling is a unique
    /// index, single-column or composite, carrying the constraint's name.
    #[test]
    fn a_table_level_unique_constraint_is_written_as_a_named_unique_index() {
        let entity = make_table_entity(
            "shop.orders",
            vec![col("customer_id", "uuid"), col("ref_code", "text")],
            vec![
                TableConstraint::Unique {
                    name: Some("orders_ref_code_uq".to_string()),
                    columns: vec!["ref_code".to_string()],
                    nulls_not_distinct: false,
                },
                TableConstraint::Unique {
                    name: None,
                    columns: vec!["customer_id".to_string(), "ref_code".to_string()],
                    nulls_not_distinct: false,
                },
            ],
        );

        let block = emit_table("shop.orders", "shop", entity.table_def.as_ref().unwrap());
        assert!(
            block.contains("ref_code [unique, name: 'orders_ref_code_uq']"),
            "got:\n{block}"
        );
        assert!(block.contains("(customer_id, ref_code) [unique]"), "got:\n{block}");
    }

    /// DBML writes an index expression in backticks — bare, `lower(email)`
    /// reads as a column of that name.
    #[test]
    fn an_expression_index_key_is_written_in_backticks() {
        let mut entity = make_table_entity(
            "shop.customers",
            vec![col("tenant_id", "bigint"), col("email", "text")],
            vec![],
        );
        let expression = |name: &str| IndexColumn {
            name: name.to_string(),
            is_expression: true,
            ..Default::default()
        };
        entity.table_def.as_mut().unwrap().indexes = vec![
            IndexDef {
                name: Some("customers_email_lower_idx".to_string()),
                columns: vec![expression("lower(email)")],
                ..Default::default()
            },
            IndexDef {
                name: Some("customers_tenant_email_idx".to_string()),
                columns: vec![
                    IndexColumn {
                        name: "tenant_id".to_string(),
                        ..Default::default()
                    },
                    expression("lower(email)"),
                ],
                ..Default::default()
            },
        ];

        let block = emit_table("shop.customers", "shop", entity.table_def.as_ref().unwrap());
        assert!(
            block.contains("`lower(email)` [name: 'customers_email_lower_idx']"),
            "got:\n{block}"
        );
        assert!(
            block.contains("(tenant_id, `lower(email)`) [name: 'customers_tenant_email_idx']"),
            "got:\n{block}"
        );
    }

    /// DBML's index settings stop at `type`, `name`, `unique`, `pk` and `note`,
    /// so the partial predicate, key order and `NULLS NOT DISTINCT` ride in the
    /// note, one fact per line.
    #[test]
    fn index_facts_dbml_cannot_express_are_written_into_the_note() {
        let mut entity = make_table_entity(
            "shop.orders",
            vec![col("customer_id", "bigint"), col("created_at", "timestamptz")],
            vec![],
        );
        let key = |name: &str| IndexColumn {
            name: name.to_string(),
            ..Default::default()
        };
        entity.table_def.as_mut().unwrap().indexes = vec![
            IndexDef {
                name: Some("orders_one_open_per_customer".to_string()),
                columns: vec![key("customer_id")],
                unique: true,
                predicate: Some("status = 'open'".to_string()),
                ..Default::default()
            },
            IndexDef {
                name: Some("orders_recent_idx".to_string()),
                columns: vec![
                    key("customer_id"),
                    IndexColumn {
                        order: Some(crate::entity::SortOrder::Desc),
                        nulls_first: Some(false),
                        ..key("created_at")
                    },
                ],
                nulls_not_distinct: true,
                ..Default::default()
            },
        ];

        let block = emit_table("shop.orders", "shop", entity.table_def.as_ref().unwrap());
        assert!(
            block.contains(
                r"customer_id [unique, name: 'orders_one_open_per_customer', note: 'where: status = \'open\'']"
            ),
            "got:\n{block}"
        );
        assert!(
            block.contains(
                r"(customer_id, created_at) [name: 'orders_recent_idx', note: 'order: asc, desc nulls last\nnulls not distinct']"
            ),
            "got:\n{block}"
        );
    }

    /// A table's CHECK constraints go in DBML's `checks { … }` block, each
    /// expression in backticks, named when the constraint is.
    #[test]
    fn check_constraints_are_written_in_a_checks_block() {
        let entity = make_table_entity(
            "shop.orders",
            vec![col("qty", "integer"), col("total_cents", "integer")],
            vec![
                TableConstraint::Check {
                    name: Some("orders_total_positive".to_string()),
                    expression: "total_cents >= 0".to_string(),
                },
                TableConstraint::Check {
                    name: None,
                    expression: "qty > 0".to_string(),
                },
            ],
        );

        let block = emit_table("shop.orders", "shop", entity.table_def.as_ref().unwrap());
        assert!(
            block.contains("  checks {\n    `total_cents >= 0` [name: 'orders_total_positive']\n    `qty > 0`\n  }"),
            "got:\n{block}"
        );
    }

    /// DBML's lexer reads `\` in a quoted string as an escape, so a backslash
    /// is written `\\` and a quote `\'` — wherever dbd writes a string.
    #[test]
    fn strings_are_written_with_dbml_escapes() {
        let column = ColumnDef {
            comment: Some(r"C:\temp, the user's".to_string()),
            default_value: Some("'it''s'".to_string()),
            ..col("path", "text")
        };
        assert_eq!(
            emit_column(&column, &std::collections::HashSet::new()),
            r#"  "path" text [default: 'it\'s', note: 'C:\\temp, the user\'s']"#
        );

        let mut kind = Entity::new(EntityType::Enum, "app.kind");
        kind.enum_values = vec![EnumValue {
            name: "plain".to_string(),
            note: Some(r"the user's \d".to_string()),
        }];
        assert!(
            emit_enum(&kind).contains(r#""plain" [note: 'the user\'s \\d']"#),
            "got:\n{}",
            emit_enum(&kind)
        );
    }

    #[test]
    fn table_with_note() {
        let mut entity = make_table_entity("config.lookups", vec![col("id", "INT")], vec![]);
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
            ..Default::default()
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
            EnumValue {
                name: "active".to_string(),
                note: None,
            },
            EnumValue {
                name: "inactive".to_string(),
                note: None,
            },
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
            ..Default::default()
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
        assert!(line.contains("[delete: cascade]"), "expected delete action: {line}");
    }

    #[test]
    fn composite_fk_round_trips_through_full_generation() {
        // End-to-end: a TableConstraint::ForeignKey with multiple columns
        // should surface as a single composite Ref line.
        let mut child = make_table_entity(
            "shop.orders",
            vec![pk_col("id", "UUID"), col("user_id", "UUID"), col("tenant_id", "UUID")],
            vec![],
        );
        child
            .table_def
            .as_mut()
            .unwrap()
            .constraints
            .push(TableConstraint::ForeignKey(ForeignKey {
                name: None,
                columns: vec!["user_id".into(), "tenant_id".into()],
                ref_schema: Some("auth".into()),
                ref_table: "memberships".into(),
                ref_columns: vec!["user_id".into(), "tenant_id".into()],
                on_delete: None,
                on_update: None,
                ..Default::default()
            }));
        let parent = make_table_entity(
            "auth.memberships",
            vec![pk_col("user_id", "UUID"), pk_col("tenant_id", "UUID")],
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

        assert!(
            doc.content.contains("Table \"auth\".\"users\""),
            "should have stub table for auth.users"
        );
        assert!(
            doc.content.contains("\"id\" varchar"),
            "stub should contain referenced column"
        );
        assert!(
            doc.content.contains("External entity"),
            "stub should have external note"
        );
    }

    /// A filter that leaves a referenced table out of the document must still
    /// stub it — a Ref to an undefined table is a DBML error — and, since the
    /// table is in the design, the stub keeps the referenced column's type.
    #[test]
    fn a_referenced_table_left_out_of_the_document_is_stubbed_with_its_real_types() {
        let lookups = make_table_entity(
            "config.lookups",
            vec![pk_col("id", "uuid"), col("name", "text")],
            vec![],
        );
        let values = make_table_entity(
            "config.lookup_values",
            vec![pk_col("id", "uuid"), col("lookup_id", "uuid")],
            vec![TableConstraint::ForeignKey(ForeignKey {
                columns: vec!["lookup_id".to_string()],
                ref_schema: Some("config".to_string()),
                ref_table: "lookups".to_string(),
                ref_columns: vec!["id".to_string()],
                ..Default::default()
            })],
        );
        let entities = vec![lookups, values];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "Test",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec!["config.lookups".to_string()],
            groups: vec![],
            auto_group_by_schema: false,
        });

        let start = doc
            .content
            .find("Table \"config\".\"lookups\" {")
            .unwrap_or_else(|| panic!("no stub:\n{}", doc.content));
        let stub = &doc.content[start..];
        let stub = &stub[..stub.find('}').unwrap()];
        assert!(stub.contains("\"id\" uuid"), "got:\n{}", doc.content);
        assert!(!stub.contains("\"name\""), "got:\n{}", doc.content);
    }

    #[test]
    fn external_entity_without_fk_refs_skipped() {
        let table_entity = make_table_entity("config.profiles", vec![pk_col("id", "UUID")], vec![]);
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

        assert!(
            !doc.content.contains("auth.uid"),
            "external without FK refs should not appear"
        );
        assert!(
            !doc.content.contains("\"auth\".\"uid\""),
            "external without FK refs should not appear as table"
        );
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
            design_entities: &entities,
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
            design_entities: &entities,
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
        let entities = vec![make_table_entity("config.users", vec![col("id", "INT")], vec![])];
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
