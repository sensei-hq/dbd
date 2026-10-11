use crate::config::DbmlDocConfig;
use crate::entity::{
    ColumnDef, Entity, EntityType, ForeignKey, IndexColumn, IndexDef, SchemaPath, TableConstraint, TableDef,
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

    let enums = EnumTypes::of(design);

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
                &enums.qualify_columns(table_def, &entity.schema_path),
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
    let stubs = emit_ref_target_stubs(&included_tables, design, &enums);
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

/// The enums the design declares, by qualified name — what a column's type is
/// checked against before it is written.
///
/// DBML finds a column's enum by name alone, while the SQL the column came from
/// found it along its file's `search_path`. Written as the file spelled it,
/// `mood` reached `init --from-dbml` with no path to resolve it on, and the
/// rebuilt table's apply failed with `type "mood" does not exist`. So a type
/// naming a declared enum is written qualified, as the table's own path
/// resolves it; a built-in, or a name already qualified, is written as is.
struct EnumTypes(std::collections::HashSet<String>);

impl EnumTypes {
    fn of(design: &[Entity]) -> Self {
        Self(
            design
                .iter()
                .filter(|e| e.entity_type == EntityType::Enum)
                .map(qualified_name)
                .collect(),
        )
    }

    /// `data_type`, qualified when it names a declared enum on `path`.
    fn column_type(&self, data_type: &str, path: &SchemaPath) -> String {
        // `mood[]` names the enum `mood`; the array suffix rides along.
        let (base, suffix) = data_type.split_at(data_type.find('[').unwrap_or(data_type.len()));
        if base.contains('.') {
            return data_type.to_string();
        }
        // `public` is searched last: apply appends it to every path a file
        // states (`script::keep_public_on_search_path`), and a file stating none
        // resolves there.
        path.schemas()
            .chain(std::iter::once(crate::reconcile::DEFAULT_SCHEMA))
            .map(|schema| format!("{schema}.{base}"))
            .find(|name| self.0.contains(name))
            .map_or_else(|| data_type.to_string(), |name| format!("{name}{suffix}"))
    }

    /// `table_def` with each column's type as [`Self::column_type`] writes it.
    fn qualify_columns(&self, table_def: &TableDef, path: &SchemaPath) -> TableDef {
        let mut table_def = table_def.clone();
        for column in &mut table_def.columns {
            column.data_type = self.column_type(&column.data_type, path);
        }
        table_def
    }
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

    // The primary key. A column's `[pk]` cannot carry a name, so a key with a
    // name of its own is a `pk` entry in the indexes block — DBML's one way to
    // name one — and its columns go unmarked. A name Postgres would give the
    // key anyway (`<table>_pkey`) is left out: it comes back by itself, and the
    // columns keep the `[pk]` dbdiagram.io draws.
    let named_pk = table_def.constraints.iter().find_map(|c| match c {
        TableConstraint::PrimaryKey {
            name: Some(name),
            columns,
        } if !columns.is_empty() && *name != format!("{base_name}_pkey") => Some((name.clone(), columns.clone())),
        _ => None,
    });
    let pk_columns: std::collections::HashSet<String> = if named_pk.is_some() {
        std::collections::HashSet::new()
    } else {
        table_def
            .constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::PrimaryKey { columns, .. } => Some(columns.clone()),
                _ => None,
            })
            .flatten()
            .chain(table_def.columns.iter().filter(|c| c.is_pk).map(|c| c.name.clone()))
            .collect()
    };

    for col in &table_def.columns {
        lines.push(emit_column(col, &pk_columns));
    }

    // Indexes block. A table-level UNIQUE is listed here first: DBML has no
    // constraint syntax for it, and a unique index is how DBML spells one.
    // Leaving it out made `init --from-dbml` rebuild a table that accepts the
    // duplicates the design refuses. Read back as an index it enforced the
    // same thing and still read as drift against the database the design
    // built — a constraint to drop, an index to create — so its note marks it
    // as a constraint (see `index_note`), and it reads back as one.
    let idx_block: Vec<String> = named_pk
        .iter()
        .map(|(name, columns)| pk_index_line(name, columns))
        .chain(unique_constraint_indexes(table_def).map(|ix| emit_index_line(&ix, true)))
        .chain(table_def.indexes.iter().map(|ix| emit_index_line(ix, false)))
        .collect();
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

/// The custom-property key a generated column's expression travels under.
///
/// DBML has no generated-column syntax. Without one, `GENERATED ALWAYS AS
/// (total_cents / 100.0) STORED` came back from `init --from-dbml` as a plain
/// writable column. Since @dbml/core v9.1 a column may carry custom properties
/// — string-valued `key: 'value'` settings DBML keeps as metadata — so the
/// expression rides in one, read back by [`crate::dbml_parse`].
///
/// The key reads like the SQL it stands for and is spelled so a user's own
/// tag (`generated: "by-etl"`, `pii: "true"`) cannot be mistaken for it: a tag
/// read as an expression would become a real `GENERATED` clause.
pub(crate) const GENERATED_PROPERTY: &str = "generated always as";

/// The custom-property key an identity column's kind travels under, beside
/// `increment` — DBML's only identity vocabulary, which cannot tell ALWAYS
/// from BY DEFAULT. A bare `increment` reads as BY DEFAULT, so only ALWAYS
/// writes the property (`'always'`), and documents without one are unchanged.
/// Without it an ALWAYS identity came back BY DEFAULT: a column that silently
/// started accepting hand-written ids.
pub(crate) const IDENTITY_PROPERTY: &str = "generated as identity";

fn emit_column(col: &ColumnDef, pk_columns: &std::collections::HashSet<String>) -> String {
    let data_type = quote_type_if_needed(&col.data_type);
    let mut settings = Vec::new();

    if pk_columns.contains(&col.name) {
        settings.push("pk".to_string());
    }
    if let Some(kind) = col.identity {
        settings.push("increment".to_string());
        if kind == crate::entity::IdentityKind::Always {
            settings.push(format!("{IDENTITY_PROPERTY}: 'always'"));
        }
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
    if let Some(ref expression) = col.generated {
        settings.push(format!("{GENERATED_PROPERTY}: {}", dbml_string(expression)));
    }
    if let Some(ref comment) = col.comment {
        // A setting stays on its column's line; `dbml_string` writes each line
        // break as DBML's `\n` escape, which both parsers read back as one.
        settings.push(format!("note: {}", dbml_string(comment.trim())));
    }

    let settings_str = if settings.is_empty() {
        String::new()
    } else {
        format!(" [{}]", settings.join(", "))
    };

    format!("  \"{}\" {}{}", col.name, data_type, settings_str)
}

fn emit_indexes(indexes: &[IndexDef]) -> Vec<String> {
    indexes.iter().map(|idx| emit_index_line(idx, false)).collect()
}

/// The `pk` entry of an `indexes { … }` block for a primary key named `name`.
fn pk_index_line(name: &str, columns: &[String]) -> String {
    let keys = if columns.len() == 1 {
        dbml_column_name(&columns[0])
    } else {
        format!(
            "({})",
            columns
                .iter()
                .map(|c| dbml_column_name(c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("{keys} [pk, name: {}]", dbml_string(name))
}

/// A column name as an index key: quoted when it is not a plain word.
fn dbml_column_name(name: &str) -> String {
    let plain = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_string()
    } else {
        dbml_quoted_name(name)
    }
}

/// A DBML quoted name, `"…"`, its backslashes and quotes escaped.
fn dbml_quoted_name(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One entry of an `indexes { … }` block; `constraint` when it is a table-level
/// UNIQUE constraint rather than an index.
fn emit_index_line(idx: &IndexDef, constraint: bool) -> String {
    let cols = if idx.columns.len() == 1 {
        index_key(&idx.columns[0])
    } else {
        format!("({})", idx.columns.iter().map(index_key).collect::<Vec<_>>().join(", "))
    };

    let mut settings = Vec::new();
    if idx.unique {
        settings.push("unique".to_string());
    }
    // The access method is DBML's own `type:` — an identifier, which
    // @dbml/core accepts for any method (`gin`, `gist`, `brin`, `hnsw` …).
    if let Some(ref method) = idx.index_type {
        settings.push(format!("type: {}", method.amname()));
    }
    if let Some(ref name) = idx.name {
        settings.push(format!("name: {}", dbml_string(name)));
    }
    if let Some(note) = index_note::write(idx, constraint) {
        settings.push(format!("note: {}", dbml_string(&note)));
    }

    if settings.is_empty() {
        cols
    } else {
        format!("{} [{}]", cols, settings.join(", "))
    }
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
/// DBML's index settings are `type`, `name`, `unique`, `pk` and `note`; its
/// reference parser rejects anything else. `type` takes the access method — any
/// identifier, in @dbml/core 10.3.0, whatever its error message says about
/// btree and hash — so the method is written there. A partial index's `WHERE`,
/// a key's sort order and operator class, `NULLS NOT DISTINCT`, `INCLUDE` and
/// storage parameters have nowhere to go but the note — and dropping them is
/// not cosmetic. A unique index on `(customer_id) WHERE status = 'open'` came
/// back from `init --from-dbml` as a unique index on `(customer_id)`: one order
/// per customer, ever; a covering index came back without its payload.
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
/// - `opclass: <entry>, <entry>, …` — one entry per key, in key order: its
///   operator class, or `-` for the default. Written only when some key has
///   one; a line that does not cover every key is ignored whole.
/// - `nulls not distinct` — the index treats NULLs as equal.
/// - `include: <column>, <column>, …` — the `INCLUDE` payload columns.
/// - `with: <name> = <value>, …` — the access method's storage parameters.
/// - `unique constraint` — the entry is a table-level UNIQUE constraint, not an
///   index. A database holds the two as different objects, so a constraint
///   read back as an index reads as drift against the database the design
///   built. Honoured only on a `unique` entry of plain columns with no other
///   line; anything else stays an index.
pub(crate) mod index_note {
    use crate::entity::{IndexColumn, IndexDef, SortOrder};

    const WHERE: &str = "where:";
    const ORDER: &str = "order:";
    const OPCLASS: &str = "opclass:";
    const NULLS_NOT_DISTINCT: &str = "nulls not distinct";
    const INCLUDE: &str = "include:";
    const WITH: &str = "with:";
    const UNIQUE_CONSTRAINT: &str = "unique constraint";
    /// An `opclass:` entry for a key that uses its type's default.
    const DEFAULT_OPCLASS: &str = "-";

    /// The note for `ix` — `constraint` when it stands for a table-level UNIQUE
    /// — or `None` when DBML's own syntax already says it all.
    pub(crate) fn write(ix: &IndexDef, constraint: bool) -> Option<String> {
        let mut lines = Vec::new();
        if constraint {
            lines.push(UNIQUE_CONSTRAINT.to_string());
        }
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
        if ix.columns.iter().any(|k| k.opclass.is_some()) {
            let entries: Vec<&str> = ix
                .columns
                .iter()
                .map(|k| k.opclass.as_deref().unwrap_or(DEFAULT_OPCLASS))
                .collect();
            lines.push(format!("{OPCLASS} {}", entries.join(", ")));
        }
        if ix.nulls_not_distinct {
            lines.push(NULLS_NOT_DISTINCT.to_string());
        }
        if !ix.include.is_empty() {
            lines.push(format!("{INCLUDE} {}", ix.include.join(", ")));
        }
        if !ix.with_options.is_empty() {
            let options: Vec<String> = ix.with_options.iter().map(|(k, v)| format!("{k} = {v}")).collect();
            lines.push(format!("{WITH} {}", options.join(", ")));
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Whether `note` marks its entry as a table-level UNIQUE constraint.
    pub(crate) fn marks_a_constraint(note: &str) -> bool {
        note.lines().any(|line| line.trim() == UNIQUE_CONSTRAINT)
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
            } else if let Some(entries) = line.strip_prefix(OPCLASS) {
                read_opclasses(entries, &mut ix.columns);
            } else if line == NULLS_NOT_DISTINCT {
                ix.nulls_not_distinct = true;
            } else if let Some(columns) = line.strip_prefix(INCLUDE) {
                ix.include = list(columns);
            } else if let Some(options) = line.strip_prefix(WITH) {
                ix.with_options = list(options)
                    .iter()
                    .filter_map(|o| o.split_once('='))
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                    .collect();
            }
        }
    }

    /// The entries of a comma-separated line, trimmed, empty ones dropped.
    fn list(entries: &str) -> Vec<String> {
        entries
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn read_opclasses(entries: &str, keys: &mut [IndexColumn]) {
        let parsed: Vec<String> = entries.split(',').map(|e| e.trim().to_string()).collect();
        if parsed.len() == keys.len() && parsed.iter().all(|e| !e.is_empty()) {
            for (key, opclass) in keys.iter_mut().zip(parsed) {
                key.opclass = (opclass != DEFAULT_OPCLASS).then_some(opclass);
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

/// One `Ref:` for `fk`, held by `source_schema.source_table`.
///
/// A foreign key with a name of its own is a named Ref — `Ref "name": …`,
/// DBML's one place for it. A name Postgres would give the key anyway
/// (`<table>_<columns>_fkey`) is left out: it comes back by itself.
fn emit_ref(source_schema: &str, source_table: &str, fk: &ForeignKey) -> String {
    let default_name = format!("{source_table}_{}_fkey", fk.columns.join("_"));
    let named = match fk.name.as_deref() {
        Some(name) if name != default_name => format!(" {}", dbml_quoted_name(name)),
        _ => String::new(),
    };
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

    format!("Ref{named}: {} > {}{}", source_cols, target_cols, settings_str)
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
fn emit_ref_target_stubs(documented: &[&Entity], design: &[Entity], enums: &EnumTypes) -> String {
    use std::collections::{BTreeMap, HashSet};

    let defined: HashSet<String> = documented.iter().map(|e| qualified_name(e)).collect();
    let mut targets: BTreeMap<String, StubTarget> = BTreeMap::new();
    for entity in documented {
        let Some(table_def) = &entity.table_def else {
            continue;
        };
        let type_of = |column: &str| {
            table_def
                .columns
                .iter()
                .find(|c| c.name == column)
                .map(|c| enums.column_type(&c.data_type, &entity.schema_path))
        };
        // An inline FK's source is the column carrying it, whatever `columns` says.
        let inline = table_def
            .columns
            .iter()
            .filter_map(|c| c.inline_fk.as_ref().map(|fk| (fk, vec![type_of(&c.name)])));
        let table_level = table_def.constraints.iter().filter_map(|c| match c {
            TableConstraint::ForeignKey(fk) => Some((fk, fk.columns.iter().map(|col| type_of(col)).collect())),
            _ => None,
        });
        for (fk, source_types) in inline.chain(table_level) {
            // Named exactly as `emit_ref` names the target, so the stub is the
            // table the Ref line points at.
            let target = format!("{}.{}", fk.ref_schema.as_deref().unwrap_or("public"), fk.ref_table);
            if defined.contains(&target) {
                continue;
            }
            let stub = targets.entry(target).or_default();
            let key: Vec<&str> = fk.ref_columns.iter().map(String::as_str).collect();
            if !stub.keys.contains(&key) {
                stub.keys.push(key);
            }
            for (i, column) in fk.ref_columns.iter().enumerate() {
                let source_type = source_types.get(i).cloned().flatten();
                let entry = stub.columns.entry(column.as_str()).or_default();
                if entry.is_empty()
                    && let Some(source_type) = source_type
                {
                    *entry = source_type;
                }
            }
        }
    }

    targets
        .iter()
        .map(|(target, stub)| {
            let known = design.iter().find(|e| {
                matches!(e.entity_type, EntityType::Table | EntityType::External) && qualified_name(e) == *target
            });
            emit_stub_block(target, stub, known, enums)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the document's foreign keys say about a table it does not define.
#[derive(Default)]
struct StubTarget<'a> {
    /// Each referenced column, with the type of the first column seen
    /// referencing it — what the stub falls back to when nothing declares one.
    columns: std::collections::BTreeMap<&'a str, String>,
    /// Each distinct column set a foreign key references, in first-seen order.
    /// Postgres requires every one to be unique, so the stub declares it.
    keys: Vec<Vec<&'a str>>,
}

/// The note on a stub `Table` — one per reason the document does not define
/// the table. [`crate::dbml_parse`] reads a table carrying one back as
/// `external:`, not as a table to build: written as one, `init --from-dbml`
/// made a one-column copy of somebody else's table for the next apply.
pub(crate) const STUB_EXTERNAL_NOTE: &str = "External entity — managed outside this project";
pub(crate) const STUB_OUT_OF_DOCUMENT_NOTE: &str =
    "Defined in this project, outside this document — only the referenced columns are shown";
pub(crate) const STUB_UNDEFINED_NOTE: &str = "Referenced, but not defined in this project";

/// Whether `note` marks a stub `dbd dbml` wrote.
pub(crate) fn is_stub_note(note: &str) -> bool {
    [STUB_EXTERNAL_NOTE, STUB_OUT_OF_DOCUMENT_NOTE, STUB_UNDEFINED_NOTE].contains(&note.trim())
}

/// `schema.table` for an entity, as the document's `Table` and `Ref` lines
/// name it.
fn qualified_name(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let base = entity.name.split('.').next_back().unwrap_or(&entity.name);
    format!("{schema}.{base}")
}

/// One stub `Table` block: the referenced `columns` of `target`, and a note
/// saying why it is a stub.
///
/// A column is typed from `known` when the design defines the table, else
/// like the column referencing it — an `external:` declares no types, and the
/// `varchar` it used to get made `dbml2sql` write a `uuid` foreign key to a
/// `varchar` key, which Postgres refuses. `varchar` is left for a column with
/// neither.
fn emit_stub_block(target: &str, stub: &StubTarget, known: Option<&Entity>, enums: &EnumTypes) -> String {
    let (schema, table) = target.split_once('.').unwrap_or(("public", target));
    let known_columns = known.and_then(|e| e.table_def.as_ref().map(|td| (td.columns.as_slice(), &e.schema_path)));

    let mut lines = vec![format!("Table \"{schema}\".\"{table}\" {{")];
    for (column, referencing_type) in &stub.columns {
        let known_type = known_columns.and_then(|(cols, path)| {
            cols.iter()
                .find(|c| c.name == *column)
                .map(|c| enums.column_type(&c.data_type, path))
        });
        let data_type = known_type
            .or_else(|| (!referencing_type.is_empty()).then(|| referencing_type.clone()))
            .unwrap_or_else(|| "varchar".to_string());
        lines.push(format!("  \"{column}\" {}", quote_type_if_needed(&data_type)));
    }
    // A foreign key's target is unique; said here, `dbml2sql` writes the
    // constraint its foreign keys need instead of one Postgres refuses.
    let keys: Vec<IndexDef> = stub
        .keys
        .iter()
        .map(|key| IndexDef {
            columns: key
                .iter()
                .map(|column| IndexColumn {
                    name: column.to_string(),
                    ..Default::default()
                })
                .collect(),
            unique: true,
            ..Default::default()
        })
        .collect();
    lines.push(String::new());
    lines.push("  indexes {".to_string());
    lines.extend(emit_indexes(&keys).into_iter().map(|line| format!("    {line}")));
    lines.push("  }".to_string());
    let note = match known.map(|e| e.entity_type) {
        Some(EntityType::External) => STUB_EXTERNAL_NOTE,
        Some(_) => STUB_OUT_OF_DOCUMENT_NOTE,
        None => STUB_UNDEFINED_NOTE,
    };
    lines.push(String::new());
    lines.push(format!("  Note: {}", dbml_string(note)));
    lines.push("}\n".to_string());
    lines.join("\n")
}

/// A column default as DBML writes it: a number, `true`/`false`, `null`, a
/// `'string'`, or anything else as a `` `backticked` `` expression.
///
/// Decided by parsing the default as Postgres does, not by its spelling. A
/// word was taken for a string, so `current_date` — one of SQL's value
/// keywords, which the parser reads as a function of the clock or the session
/// — was written `'current_date'`, and `init --from-dbml` rebuilt
/// `DEFAULT 'current_date'`: refused on a date column, the literal text on a
/// text one.
fn quote_default(value: &str) -> String {
    let trimmed = value.trim();
    match default_kind(trimmed) {
        Some(DefaultKind::Number) => trimmed.to_string(),
        Some(DefaultKind::Bool(b)) => b.to_string(),
        Some(DefaultKind::Null) => "null".to_string(),
        Some(DefaultKind::Text(text)) => dbml_string(&text),
        Some(DefaultKind::Expression) => format!("`{trimmed}`"),
        None => quote_unparsed_default(trimmed),
    }
}

/// What a default is, read from libpg_query's tree.
enum DefaultKind {
    Number,
    Bool(bool),
    Null,
    /// A string literal's text, its SQL escaping already undone.
    Text(String),
    Expression,
}

/// The kind of default `expr` is, or `None` when it is not SQL Postgres reads.
///
/// A bare identifier is the one exception to "anything else is an expression":
/// Postgres refuses a column reference in a default, so a bare word can only
/// be text that came from elsewhere written without quotes, and is read as the
/// string it was before. Keywords are not identifiers here — `current_user`
/// parses as SQL's own function.
fn default_kind(expr: &str) -> Option<DefaultKind> {
    use pg_query::NodeEnum;
    use pg_query::protobuf::a_const::Val;

    let parsed = pg_query::parse(&format!("SELECT {expr}")).ok()?;
    let stmt = parsed.protobuf.stmts.first()?.stmt.as_ref()?.node.as_ref()?;
    let NodeEnum::SelectStmt(select) = stmt else {
        return None;
    };
    if select.target_list.len() != 1 {
        return None;
    }
    let NodeEnum::ResTarget(target) = select.target_list.first()?.node.as_ref()? else {
        return None;
    };
    Some(match target.val.as_deref()?.node.as_ref()? {
        NodeEnum::AConst(c) if c.isnull => DefaultKind::Null,
        NodeEnum::AConst(c) => match c.val.as_ref()? {
            Val::Ival(_) | Val::Fval(_) => DefaultKind::Number,
            Val::Boolval(b) => DefaultKind::Bool(b.boolval),
            Val::Sval(s) => DefaultKind::Text(s.sval.clone()),
            Val::Bsval(_) => DefaultKind::Expression,
        },
        NodeEnum::ColumnRef(r) if r.fields.len() == 1 && !expr.contains('"') => match r.fields[0].node.as_ref()? {
            NodeEnum::String(word) => DefaultKind::Text(word.sval.clone()),
            _ => DefaultKind::Expression,
        },
        _ => DefaultKind::Expression,
    })
}

/// A default Postgres does not read as SQL, written as it always was: a
/// number, a boolean or `null` bare, something with a call or a cast as an
/// expression, anything else as text.
fn quote_unparsed_default(trimmed: &str) -> String {
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        return trimmed.to_lowercase();
    }
    if trimmed.parse::<f64>().is_ok() {
        return trimmed.to_string();
    }
    if trimmed.eq_ignore_ascii_case("null") {
        return "null".to_string();
    }
    if trimmed.contains('(') || trimmed.contains("::") || trimmed.contains('+') {
        return format!("`{trimmed}`");
    }
    // A SQL `'…'` (whose `''` is one quote) or a bare word. DBML gets the text
    // itself, in DBML's escaping: SQL's doubled quote is not a DBML escape,
    // and written as-is it ends the string.
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
/// Multi-line → triple quotes, flush against the text: '''text'''
///
/// Flush, as @dbml/core's own exporter writes it: that parser drops a blank
/// first line but keeps a blank last one, so `'''`, newline, text, newline,
/// `'''` read back with a trailing newline — every multi-line note one
/// character longer than the comment it documents. The text is trimmed, so
/// its first line has no indent and @dbml/core's dedent leaves it whole.
fn quote_dbml_string(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.contains('\n') {
        format!("'''{}'''", dbml_multiline_body(trimmed))
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
/// that would open a `'''` run is escaped — as is every quote at the very end,
/// which would otherwise run into the closing `'''`. Other quotes stay as
/// written — apostrophes are common in prose notes and DBML needs nothing done
/// to them.
fn dbml_multiline_body(text: &str) -> String {
    let trailing_quotes = text.len() - text.trim_end_matches('\'').len();
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' if text[i..].starts_with("'''") || i >= text.len() - trailing_quotes => out.push_str("\\'"),
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
            collation: None,
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
    /// index, single-column or composite, carrying the constraint's name — and
    /// a note saying it is a constraint, which `dbml_parse` reads back as one.
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
            block.contains("ref_code [unique, name: 'orders_ref_code_uq', note: 'unique constraint']"),
            "got:\n{block}"
        );
        assert!(
            block.contains("(customer_id, ref_code) [unique, note: 'unique constraint']"),
            "got:\n{block}"
        );
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

    /// DBML has no generated-column syntax; the expression rides in a custom
    /// property whose key reads like the SQL it stands for.
    #[test]
    fn a_generated_column_carries_its_expression_as_a_custom_property() {
        let column = ColumnDef {
            generated: Some("coalesce(note, 'n/a')".to_string()),
            ..col("label", "text")
        };
        assert_eq!(
            emit_column(&column, &std::collections::HashSet::new()),
            r#"  "label" text [generated always as: 'coalesce(note, \'n/a\')']"#
        );
    }

    /// `increment` is DBML's whole vocabulary for identity; ALWAYS adds a
    /// custom property, BY DEFAULT stays a bare `increment`.
    #[test]
    fn an_always_identity_says_so_in_a_custom_property() {
        let identity = |kind| ColumnDef {
            identity: Some(kind),
            nullable: false,
            ..col("id", "bigint")
        };
        let none = std::collections::HashSet::new();
        assert_eq!(
            emit_column(&identity(crate::entity::IdentityKind::Always), &none),
            r#"  "id" bigint [increment, generated as identity: 'always', not null]"#
        );
        assert_eq!(
            emit_column(&identity(crate::entity::IdentityKind::ByDefault), &none),
            r#"  "id" bigint [increment, not null]"#
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

    /// SQL's value keywords are expressions, not text: `current_date` was
    /// written as the string `'current_date'`, which `init --from-dbml` turned
    /// into `DEFAULT 'current_date'` — refused on a date column, and on a text
    /// column the literal text `current_user` in place of the user's name.
    /// Each is written as DBML writes an expression, in backticks. A string
    /// that only spells one stays a string.
    #[test]
    fn a_sql_value_keyword_default_is_an_expression_not_a_string() {
        for keyword in [
            "current_date",
            "current_time",
            "current_timestamp",
            "localtime",
            "localtimestamp",
            "current_user",
            "session_user",
            "user",
            "current_role",
            "current_catalog",
            "current_schema",
            "CURRENT_TIMESTAMP(3)",
        ] {
            assert_eq!(quote_default(keyword), format!("`{keyword}`"));
        }
        assert_eq!(quote_default("'current_date'"), "'current_date'");
        assert_eq!(quote_default("'it''s'"), r"'it\'s'");
        assert_eq!(quote_default("-1"), "-1");
        assert_eq!(quote_default("1.5"), "1.5");
        assert_eq!(quote_default("'x'::text"), "`'x'::text`");
    }

    /// @dbml/core reads `'''`, a line break, the text, a line break and `'''`
    /// as the text plus a trailing newline — it drops a blank first line, not
    /// a blank last one — so every multi-line note dbd wrote read back one
    /// character longer, a comment that no longer matched its table's. Its own
    /// exporter writes the text flush against the quotes, which reads back
    /// exactly in both parsers. A quote that would run into the closing `'''`
    /// is escaped.
    #[test]
    fn a_multi_line_note_is_written_flush_against_its_quotes() {
        assert_eq!(quote_dbml_string("line one\nline two"), "'''line one\nline two'''");
        assert_eq!(quote_dbml_string("say\n'hi'"), "'''say\n'hi\\''''");
        assert_eq!(quote_dbml_string("two\nquotes ''"), "'''two\nquotes \\'\\''''");
        assert_eq!(quote_dbml_string("  padded\n  lines  "), "'''padded\n  lines'''");
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

    /// A stub's referenced columns are declared unique, once per distinct set
    /// a foreign key names. Postgres requires a foreign key's target to be
    /// unique, so the reference itself says they are; without it `dbml2sql`
    /// wrote a foreign key Postgres refuses with "there is no unique constraint
    /// matching given keys".
    #[test]
    fn a_stub_declares_each_referenced_column_set_unique() {
        let fk = |columns: &[&str], ref_columns: &[&str]| {
            TableConstraint::ForeignKey(ForeignKey {
                columns: columns.iter().map(|c| c.to_string()).collect(),
                ref_schema: Some("auth".to_string()),
                ref_table: "users".to_string(),
                ref_columns: ref_columns.iter().map(|c| c.to_string()).collect(),
                ..Default::default()
            })
        };
        let profiles = make_table_entity(
            "app.profiles",
            vec![pk_col("id", "int"), col("user_id", "uuid"), col("tenant", "int")],
            vec![fk(&["user_id"], &["id"]), fk(&["tenant", "user_id"], &["tenant", "id"])],
        );
        let entities = vec![profiles, Entity::new(EntityType::External, "auth.users")];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "t",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        // Read from the stub's own block: dbd's reader takes a stub back as
        // `external:`, so this is what `dbml2sql` — which builds it — sees.
        let start = doc.content.find("Table \"auth\".\"users\" {").expect("a stub");
        let stub = &doc.content[start..];
        let stub = &stub[..stub.find("\n}").unwrap()];
        let unique: Vec<&str> = stub
            .lines()
            .map(str::trim)
            .filter(|l| l.ends_with("[unique]"))
            .collect();
        assert_eq!(unique, ["id [unique]", "(tenant, id) [unique]"], "{}", doc.content);
    }

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
        // Typed like the column that references it: nothing declares an
        // external's columns, and the `varchar` guess made `dbml2sql` write a
        // `uuid` foreign key to a `varchar` key, which Postgres refuses.
        let start = doc.content.find("Table \"auth\".\"users\" {").unwrap();
        let stub = &doc.content[start..];
        let stub = &stub[..stub.find('}').unwrap()];
        assert!(
            stub.contains("\"id\" UUID"),
            "stub should type the referenced column like its foreign key:\n{}",
            doc.content
        );
        assert!(
            doc.content.contains("External entity"),
            "stub should have external note"
        );
    }

    /// A name holding a quote or a backslash round-trips. The writer quoted
    /// names without escaping them — `"we"ird"` is not DBML — and the reader
    /// ended a quoted name at its first quote, escaped or not, so a column
    /// came back as `we\` and the rebuilt table file did not parse.
    #[test]
    fn a_name_with_a_quote_or_a_backslash_round_trips() {
        let mut mood = Entity::new(EntityType::Enum, "app.mood");
        mood.enum_values = ["it's", "say \"hi\""]
            .iter()
            .map(|v| EnumValue {
                name: v.to_string(),
                note: None,
            })
            .collect();
        let mut table = make_table_entity(
            "app.t",
            vec![pk_col("we\"ird", "int"), col("back\\slash", "text")],
            vec![],
        );
        table.table_def.as_mut().unwrap().indexes.push(IndexDef {
            name: Some("t_weird_idx".to_string()),
            columns: vec![IndexColumn {
                name: "we\"ird".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let entities = vec![mood, table];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "t",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        let parsed = crate::dbml_parse::parse_dbml(&doc.content).unwrap_or_else(|e| panic!("{e}\n{}", doc.content));
        let values: Vec<&str> = parsed
            .iter()
            .find(|e| e.name == "app.mood")
            .unwrap()
            .enum_values
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(values, ["it's", "say \"hi\""], "{}", doc.content);
        let def = parsed
            .iter()
            .find(|e| e.name == "app.t")
            .unwrap()
            .table_def
            .clone()
            .unwrap();
        let columns: Vec<&str> = def.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(columns, ["we\"ird", "back\\slash"], "{}", doc.content);
        assert_eq!(def.indexes[0].columns[0].name, "we\"ird", "{}", doc.content);
    }

    /// A column comment keeps its line breaks. They were flattened to spaces
    /// before the note was escaped, so a markdown comment came back from
    /// `init --from-dbml` as one run-on line. DBML's `\n` escape keeps the note
    /// on the one line both parsers read settings from.
    #[test]
    fn a_multi_line_column_comment_round_trips_with_its_line_breaks() {
        let mut code = col("code", "text");
        code.comment = Some("line one\n- line two".to_string());
        let entities = vec![make_table_entity("app.t", vec![pk_col("id", "int"), code], vec![])];
        let doc = generate_dbml(&DbmlParams {
            entities: &entities,
            project_name: "t",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        let parsed = crate::dbml_parse::parse_dbml(&doc.content).unwrap();
        let table = parsed.iter().find(|e| e.name == "app.t").expect("table read back");
        let comment = table.table_def.as_ref().unwrap().columns[1].comment.as_deref();
        assert_eq!(comment, Some("line one\n- line two"), "{}", doc.content);
    }

    /// An enum column's type is written the way the table's own `search_path`
    /// resolves it. Written bare, `init --from-dbml` built `mood` unqualified,
    /// and the apply failed with `type "mood" does not exist` — the enum lives
    /// in `types`, which nothing on the new file's path names.
    #[test]
    fn an_enum_column_round_trips_with_the_schema_its_search_path_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/enum/types")).unwrap();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - types\n  - app\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/enum/types/mood.ddl"),
            "set search_path to types;\ncreate type mood as enum ('calm', 'busy');\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/t.ddl"),
            "set search_path to app, types;\n\
             create table if not exists t (id int primary key, m mood not null, ms mood[]);\n",
        )
        .unwrap();
        let design = crate::Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let doc = generate_dbml(&DbmlParams {
            entities: design.entities(),
            project_name: "t",
            database_type: "PostgreSQL",
            project_note: None,
            include_schemas: vec![],
            exclude_schemas: vec![],
            include_tables: vec![],
            exclude_tables: vec![],
            groups: vec![],
            auto_group_by_schema: false,
        });

        let parsed = crate::dbml_parse::parse_dbml(&doc.content).unwrap();
        let table = parsed.iter().find(|e| e.name == "app.t").expect("table read back");
        let types: Vec<(&str, &str)> = table
            .table_def
            .as_ref()
            .unwrap()
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str()))
            .collect();
        assert_eq!(
            types,
            [("id", "int"), ("m", "types.mood"), ("ms", "types.mood[]")],
            "{}",
            doc.content
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
