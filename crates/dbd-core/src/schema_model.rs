//! The `SchemaModel` — a dbd-native JSON model of a schema, consumed by the
//! diagram viewer. Serializes to the `DBD_SCHEMA` shape (see
//! docs/mockup/designs/schema-data.js). Boolean column flags (`pk`/`nn`/`en`)
//! are emitted only when true; the viewer reads them truthily.

use serde::{Deserialize, Serialize};

use crate::design::Design;
use crate::entity::{EntityType, FkAction, IdentityKind, SortOrder, TableConstraint};
use crate::error::Result;
use crate::scope::ResolvedScope;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaModel {
    /// Wire-format version. `2` added [`Self::entities`], [`Self::deps`], and
    /// `fk`/`uq` on [`Column`]; `3` added [`Self::history`] and [`Self::enums`],
    /// then — optional, so every earlier v3 payload still reads — [`Self::stubs`],
    /// sequences in [`Self::entities`], [`TableNode::checks`], [`Index::predicate`]
    /// and [`Column::identity`] / [`Column::generated`].
    ///
    /// This type is read by dbd's own viewer, by a shared component package,
    /// and by external consumers, so it is a cross-repo contract rather than
    /// an internal shape. Without a version each extension is a guess for
    /// everyone downstream.
    #[serde(default = "default_version")]
    pub version: u32,
    pub project: ProjectInfo,
    pub schemas: Vec<SchemaInfo>,
    /// Tables only — unchanged from v1, deliberately. Every other kind is in
    /// [`Self::entities`], so a consumer reading this as "the tables" stays
    /// correct.
    pub tables: Vec<TableNode>,
    /// Foreign keys only — unchanged from v1. The dependency graph is
    /// [`Self::deps`]; an ER renderer wants these and a call-graph renderer
    /// wants those.
    pub refs: Vec<Ref>,
    /// The tables [`Self::refs`] land on that [`Self::tables`] does not carry
    /// (v3, additive): one table-shaped node each, holding only the columns the
    /// refs land on, so every foreign key has somewhere to be drawn.
    ///
    /// `kind` says why it is a stub — `external` (declared under `external:`),
    /// `out_of_scope` (a table of this project the scope leaves out) or
    /// `unresolved` (defined nowhere in the project). Kept apart from `tables`
    /// so that a consumer counting or listing the model's tables is not handed
    /// tables it does not own. Absent when every ref lands inside the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stubs: Vec<TableNode>,
    /// Views, materialized views, functions, procedures and triggers (v2), and
    /// sequences (v3, additive).
    ///
    /// Separate from [`Self::tables`] rather than folded in under `kind`,
    /// because folding would silently change what every existing consumer of
    /// `tables` receives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityNode>,
    /// What reads, writes or calls what (v2) — projected from
    /// [`crate::entity::Entity::refs`], which is already resolved and
    /// deduplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<DepEdge>,
    /// What each snapshot changed, oldest first (v3) — see [`crate::history`].
    ///
    /// Not filled by [`build`], which reads the design and nothing else; the
    /// caller that knows the project directory attaches it. Absent when the
    /// project has no snapshots, so a v2 consumer reads a v3 payload unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<crate::history::HistoryEntry>,
    /// Enum types with their values (v3). Absent when the project has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumNode>,
}

fn default_version() -> u32 {
    3
}

/// A non-table entity: a view, materialized view, function, procedure, trigger
/// or sequence.
///
/// No columns. A parsed routine has none, and a view's are not read — what it
/// has is a body and the things it depends on, which are in
/// [`SchemaModel::deps`].
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityNode {
    pub schema: String,
    pub name: String,
    /// `view` | `materialized_view` | `function` | `procedure` | `trigger` |
    /// `sequence`
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "noteMd", skip_serializing_if = "Option::is_none")]
    pub note_md: Option<String>,
}

/// An enum type and its values, in declaration order (v3).
///
/// The model carried only a per-schema count, so a viewer could neither list an
/// enum nor show what it allows. Named like every other entity — by its file
/// stem — so it lines up with the snapshot history, which names enums the same way.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct EnumNode {
    pub schema: String,
    pub name: String,
    pub values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "noteMd", skip_serializing_if = "Option::is_none")]
    pub note_md: Option<String>,
}

/// One dependency edge: a view reading a table, a routine calling a routine.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct DepEdge {
    pub from: NodeRef,
    pub to: NodeRef,
    /// `reads` | `writes` | `calls` | `member`
    pub kind: String,
    /// The target did not resolve to anything in the project — a built-in, or
    /// a genuine dangling reference. A renderer should dim rather than drop
    /// it: the edge is real, the endpoint is not placeable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unresolved: bool,
}

/// One end of a [`DepEdge`]: schema and name, no column.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeRef {
    /// Schema name
    pub s: String,
    /// Entity name
    pub n: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub db: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: usize,
    pub enums: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct TableNode {
    pub schema: String,
    pub name: String,
    /// `table` for every entry of [`SchemaModel::tables`]; a stub's reason —
    /// `external` | `out_of_scope` | `unresolved` — in [`SchemaModel::stubs`].
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "noteMd", skip_serializing_if = "Option::is_none")]
    pub note_md: Option<String>,
    pub columns: Vec<Column>,
    /// Table-level UNIQUE constraints + explicit indexes. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    /// CHECK constraints, inline ones first as the parser lifts them onto the
    /// table (v3, additive). Omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

/// One CHECK constraint: what it requires, and its name when it was given one.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The boolean expression, as Postgres's own grammar renders it back.
    pub expression: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Index {
    /// Column spec, e.g. "(email)" or "(status, placed_at DESC)".
    pub def: String,
    /// unique index / constraint
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A partial index's `WHERE` predicate, as authored (v3, additive) — which
    /// rows the index covers, and for a UNIQUE one which rows it constrains.
    #[serde(rename = "where", skip_serializing_if = "Option::is_none")]
    pub predicate: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// primary key
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pk: bool,
    /// not null
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nn: bool,
    /// column type is an enum
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub en: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    /// column is a foreign key (v2)
    ///
    /// Carried so a renderer can pick a glyph without scanning
    /// [`SchemaModel::refs`] for the column.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fk: bool,
    /// column is unique (v2) — inline `UNIQUE` or a single-column constraint
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uq: bool,
    /// `always` | `by default` — the column is `GENERATED … AS IDENTITY`
    /// (v3, additive). Its values come from a sequence, not from `def`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// The expression of a `GENERATED ALWAYS AS (…) STORED` column (v3,
    /// additive). Not a default: the column cannot be written at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Ref {
    pub from: RefEnd,
    pub to: RefEnd,
    /// FK on-delete action: cascade | restrict | set_null | set_default | no_action
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RefEnd {
    /// Schema name
    pub s: String,
    /// Table name
    pub t: String,
    /// Column name
    pub c: String,
}

/// Build a `SchemaModel` from a loaded design, optionally filtered to a scope.
///
/// Fails when the scope cannot be resolved to a working set — a `deps: include`
/// closure that needs an entity the scope excludes. Every other command refuses
/// that scope, and a model drawn from it would be empty: a diagram that says the
/// scope holds nothing, rather than that it contradicts itself.
pub fn build(design: &Design, scope: Option<&ResolvedScope>) -> Result<SchemaModel> {
    let entities = match scope {
        Some(s) => design.scoped_entities(s)?,
        None => design.entities().to_vec(),
    };

    let enum_names = enum_names(&entities);

    let mut tables = Vec::new();
    let mut refs = Vec::new();

    for e in entities.iter().filter(|e| e.entity_type == EntityType::Table) {
        let Some(def) = &e.table_def else { continue };
        tables.push(build_table_node(e, def, &enum_names));
        refs.extend(collect_table_refs(e, def));
    }
    let stubs = collect_stubs(design, &tables, &refs);

    let mut schema_set: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    // A schema is one of the model's when the model draws anything in it — a
    // schema of views or routines alone is listed with zero tables and enums,
    // not dropped while its entities are drawn under it. Externals, roles and
    // the like are not drawn, so they place no schema here.
    for e in &entities {
        let Some(s) = &e.schema else { continue };
        let drawn = matches!(e.entity_type, EntityType::Table | EntityType::Enum) || node_kind(e.entity_type).is_some();
        if !drawn {
            continue;
        }
        let counts = schema_set.entry(s.clone()).or_insert((0, 0));
        match e.entity_type {
            EntityType::Table => counts.0 += 1,
            EntityType::Enum => counts.1 += 1,
            _ => {}
        }
    }
    let schemas = schema_set
        .into_iter()
        .map(|(name, (tables, enums))| SchemaInfo { name, tables, enums })
        .collect();

    tables.sort_by(|a, b| (a.schema.as_str(), a.name.as_str()).cmp(&(b.schema.as_str(), b.name.as_str())));

    // v2: everything that is not a table, and what depends on what.
    let mut entities_out: Vec<EntityNode> = entities
        .iter()
        .filter_map(|e| {
            node_kind(e.entity_type).map(|kind| EntityNode {
                schema: e.schema.clone().unwrap_or_default(),
                name: e.name.rsplit('.').next().unwrap_or(&e.name).to_string(),
                kind: kind.to_string(),
                note: note_first_line(e.comment.as_deref()),
                note_md: e.comment.clone(),
            })
        })
        .collect();
    entities_out.sort_by(|a, b| (a.schema.as_str(), a.name.as_str()).cmp(&(b.schema.as_str(), b.name.as_str())));

    // v3: the enums themselves, not just their count.
    let mut enums: Vec<EnumNode> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .map(|e| EnumNode {
            schema: e.schema.clone().unwrap_or_default(),
            name: e.name.rsplit('.').next().unwrap_or(&e.name).to_string(),
            values: e.enum_values.iter().map(|v| v.name.clone()).collect(),
            note: note_first_line(e.comment.as_deref()),
            note_md: e.comment.clone(),
        })
        .collect();
    enums.sort_by(|a, b| (a.schema.as_str(), a.name.as_str()).cmp(&(b.schema.as_str(), b.name.as_str())));

    let known: std::collections::HashSet<&str> = entities.iter().map(|e| e.name.as_str()).collect();
    let mut deps: Vec<DepEdge> = Vec::new();
    for e in &entities {
        for r in &e.refs {
            deps.push(DepEdge {
                from: node_ref(&e.name, e.schema.as_deref()),
                to: node_ref(&r.name, None),
                kind: dep_kind(r.kind).to_string(),
                // A call that resolved to nothing is a built-in, and a read
                // that did is a genuine dangle. Either way the endpoint is
                // not placeable, so say so rather than drop the edge.
                unresolved: r.unresolved || !known.contains(r.name.as_str()),
            });
        }
    }

    Ok(SchemaModel {
        version: default_version(),
        entities: entities_out,
        deps,
        project: ProjectInfo {
            name: design.config().project.name.clone(),
            db: design.config().source.dialect.clone(),
            note: design.config().project.note.clone(),
        },
        schemas,
        tables,
        refs,
        stubs,
        history: Vec::new(),
        enums,
    })
}

/// Every name a column's type can use to mean one of these enums.
///
/// NOTE: matches c.data_type against the enum entity's name (file-stem, e.g.
/// "config.status" / "status"), not necessarily the CREATE TYPE identifier
/// ("status_type"). Works when they coincide; a stricter match is future work.
fn enum_names(entities: &[crate::entity::Entity]) -> std::collections::HashSet<String> {
    entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .flat_map(|e| {
            let bare = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();
            [e.name.clone(), bare]
        })
        .collect()
}

/// One stub per table that a ref in `refs` lands on but `tables` does not
/// carry, holding only the columns those refs land on, sorted like `tables`.
///
/// The ref is the fact — the column says `fk`, the constraint exists — so it is
/// kept, and the stub is what lets a renderer place its far end. Its `kind`
/// says why it is not a full table, most specific first: declared under
/// `external:`, a table of this project the scope leaves out, or nothing the
/// project defines at all.
fn collect_stubs(design: &Design, tables: &[TableNode], refs: &[Ref]) -> Vec<TableNode> {
    let carried: std::collections::HashSet<(&str, &str)> =
        tables.iter().map(|t| (t.schema.as_str(), t.name.as_str())).collect();
    // Referenced columns per missing table, in first-reference order. An empty
    // name is a `REFERENCES t` whose target column was never resolved; there is
    // nothing to draw for it.
    let mut wanted: std::collections::BTreeMap<(&str, &str), Vec<&str>> = Default::default();
    for r in refs {
        let key = (r.to.s.as_str(), r.to.t.as_str());
        if carried.contains(&key) {
            continue;
        }
        let cols = wanted.entry(key).or_default();
        if !r.to.c.is_empty() && !cols.contains(&r.to.c.as_str()) {
            cols.push(r.to.c.as_str());
        }
    }
    // A stub's columns are described against the whole design: an out-of-scope
    // table's enum may be out of scope with it.
    let all_enums = enum_names(design.entities());
    wanted
        .into_iter()
        .map(|((schema, name), cols)| stub_node(design, schema, name, &cols, &all_enums))
        .collect()
}

/// The stub for `schema.name`, carrying the columns named in `cols`.
fn stub_node(
    design: &Design,
    schema: &str,
    name: &str,
    cols: &[&str],
    enum_names: &std::collections::HashSet<String>,
) -> TableNode {
    let qualified = format!("{schema}.{name}");
    let untyped = |c: &str| Column {
        name: c.to_string(),
        ty: String::new(),
        pk: false,
        nn: false,
        en: false,
        def: None,
        fk: false,
        uq: false,
        identity: None,
        generated: None,
        note: None,
    };
    // Each referenced column, typed from `known` where it is described there and
    // left untyped where it is not — a ref to a column nobody declared is still a
    // ref, and its card still needs the row to land on.
    let pick = |mut known: Vec<Column>| -> Vec<Column> {
        cols.iter()
            .map(|c| match known.iter().position(|k| k.name == *c) {
                Some(i) => known.swap_remove(i),
                None => untyped(c),
            })
            .collect()
    };

    if let Some(ext) = design.config().external.iter().find(|x| x.name == qualified) {
        let declared = ext
            .columns
            .iter()
            .flat_map(|m| m.iter())
            .map(|(c, ty)| Column {
                ty: ty.clone(),
                ..untyped(c)
            })
            .collect();
        return TableNode {
            schema: schema.to_string(),
            name: name.to_string(),
            kind: "external".into(),
            note: note_first_line(ext.note.as_deref()),
            note_md: ext.note.clone(),
            columns: pick(declared),
            indexes: Vec::new(),
            checks: Vec::new(),
        };
    }

    let in_project = design
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Table && e.name == qualified)
        .find_map(|e| e.table_def.as_ref().map(|def| build_table_node(e, def, enum_names)));
    match in_project {
        Some(full) => TableNode {
            kind: "out_of_scope".into(),
            columns: pick(full.columns),
            indexes: Vec::new(),
            checks: Vec::new(),
            ..full
        },
        None => TableNode {
            schema: schema.to_string(),
            name: name.to_string(),
            kind: "unresolved".into(),
            note: None,
            note_md: None,
            columns: pick(Vec::new()),
            indexes: Vec::new(),
            checks: Vec::new(),
        },
    }
}

/// Build the diagram `TableNode` for one table entity (columns + indexes + notes).
fn build_table_node(
    e: &crate::entity::Entity,
    def: &crate::entity::TableDef,
    enum_names: &std::collections::HashSet<String>,
) -> TableNode {
    let schema = e.schema.clone().unwrap_or_default();
    let name = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();

    let pk_cols: std::collections::HashSet<&str> = def
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::PrimaryKey { columns, .. } => Some(columns.iter().map(|s| s.as_str())),
            _ => None,
        })
        .flatten()
        .collect();

    // Foreign-key and unique columns, so a renderer never has to re-derive
    // them from `refs` and the index list to choose a glyph.
    let fk_cols: std::collections::HashSet<&str> = def
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::ForeignKey(fk) => Some(fk.columns.iter().map(|s| s.as_str())),
            _ => None,
        })
        .flatten()
        .chain(
            def.columns
                .iter()
                .filter(|c| c.inline_fk.is_some())
                .map(|c| c.name.as_str()),
        )
        .collect();
    // Single-column UNIQUE only: a composite constraint is a property of the
    // combination, and marking each member would claim something untrue.
    let uq_cols: std::collections::HashSet<&str> = def
        .constraints
        .iter()
        .filter_map(|c| match c {
            TableConstraint::Unique { columns, .. } if columns.len() == 1 => Some(columns[0].as_str()),
            _ => None,
        })
        .chain(def.columns.iter().filter(|c| c.is_unique).map(|c| c.name.as_str()))
        .collect();

    let columns = def
        .columns
        .iter()
        .map(|c| Column {
            name: c.name.clone(),
            ty: c.data_type.clone(),
            pk: c.is_pk || pk_cols.contains(c.name.as_str()),
            nn: !c.nullable,
            en: enum_names.contains(&c.data_type),
            def: c.default_value.clone(),
            fk: fk_cols.contains(c.name.as_str()),
            uq: uq_cols.contains(c.name.as_str()),
            identity: c.identity.map(|k| {
                match k {
                    IdentityKind::Always => "always",
                    IdentityKind::ByDefault => "by default",
                }
                .to_string()
            }),
            generated: c.generated.clone(),
            note: c.comment.clone().or_else(|| def.comments.columns.get(&c.name).cloned()),
        })
        .collect();

    TableNode {
        schema,
        name,
        kind: "table".into(),
        note: note_first_line(def.comments.table.as_deref()),
        note_md: def.comments.table.clone(),
        columns,
        indexes: collect_indexes(def),
        checks: def
            .constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::Check { name, expression } => Some(Check {
                    name: name.clone(),
                    expression: expression.clone(),
                }),
                _ => None,
            })
            .collect(),
    }
}

/// Collect the diagram `Ref`s for every one of a table's foreign keys (one `Ref`
/// per referencing column), wherever the target lives — see [`collect_stubs`].
fn collect_table_refs(e: &crate::entity::Entity, def: &crate::entity::TableDef) -> Vec<Ref> {
    let mut refs = Vec::new();
    for fk in collect_fks(def) {
        let to_schema = fk
            .ref_schema
            .clone()
            .unwrap_or_else(|| e.schema.clone().unwrap_or_default());
        let action = fk.on_delete.map(fk_action_str);
        let from_schema = e.schema.clone().unwrap_or_default();
        let from_table = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();
        for (i, local) in fk.columns.iter().enumerate() {
            let remote = fk.ref_columns.get(i).cloned().unwrap_or_default();
            refs.push(Ref {
                from: RefEnd {
                    s: from_schema.clone(),
                    t: from_table.clone(),
                    c: local.clone(),
                },
                to: RefEnd {
                    s: to_schema.clone(),
                    t: fk.ref_table.clone(),
                    c: remote,
                },
                action: action.clone(),
            });
        }
    }
    refs
}

/// First non-empty first line of a comment → the short `note` (or None).
///
/// Takes the text rather than a `TableDef`, because a view or routine's comment
/// lives on [`crate::entity::Entity::comment`] and must render the same way: the
/// entity description table draws `note` as the one-line summary and `noteMd` as
/// the full prose, and a view whose summary was the whole essay would wreck the
/// column it sits in.
fn note_first_line(text: Option<&str>) -> Option<String> {
    let first = text?.lines().next().unwrap_or("").trim();
    (!first.is_empty()).then(|| first.to_string())
}

/// Table-level UNIQUE constraints + explicit indexes, formatted for the viewer's
/// Indexes section. `def` is the column spec ("(a, b DESC)"); `unique` drives the
/// UNIQUE badge; `name` is the constraint/index name when known.
fn collect_indexes(def: &crate::entity::TableDef) -> Vec<Index> {
    let cols = |columns: &[String]| format!("({})", columns.join(", "));
    let mut out = Vec::new();
    for c in &def.constraints {
        if let TableConstraint::Unique { name, columns, .. } = c {
            out.push(Index {
                def: cols(columns),
                unique: true,
                name: name.clone(),
                predicate: None,
            });
        }
    }
    for ix in &def.indexes {
        let spec = ix
            .columns
            .iter()
            .map(|c| match c.order {
                Some(SortOrder::Desc) => format!("{} DESC", c.name),
                _ => c.name.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Index {
            def: format!("({spec})"),
            unique: ix.unique,
            name: ix.name.clone(),
            predicate: ix.predicate.clone(),
        });
    }
    out
}

/// All foreign keys on a table: inline column FKs + table-level FK constraints.
fn collect_fks(def: &crate::entity::TableDef) -> Vec<crate::entity::ForeignKey> {
    let mut out: Vec<crate::entity::ForeignKey> = def.columns.iter().filter_map(|c| c.inline_fk.clone()).collect();
    for c in &def.constraints {
        if let TableConstraint::ForeignKey(fk) = c {
            out.push(fk.clone());
        }
    }
    out
}

/// Map an `FkAction` to the lowercase string used in the `action` field.
fn fk_action_str(a: FkAction) -> String {
    match a {
        FkAction::Cascade => "cascade",
        FkAction::Restrict => "restrict",
        FkAction::SetNull => "set_null",
        FkAction::SetDefault => "set_default",
        FkAction::NoAction => "no_action",
    }
    .to_string()
}

/// The `kind` string for a non-table entity, or `None` for one that belongs
/// elsewhere in the model (tables) or nowhere in it (schemas, roles).
fn node_kind(t: EntityType) -> Option<&'static str> {
    match t {
        EntityType::View => Some("view"),
        EntityType::MaterializedView => Some("materialized_view"),
        EntityType::Function => Some("function"),
        EntityType::Procedure => Some("procedure"),
        EntityType::Trigger => Some("trigger"),
        EntityType::Sequence => Some("sequence"),
        _ => None,
    }
}

/// [`crate::entity::RefKind`] as the wire string.
fn dep_kind(k: crate::entity::RefKind) -> &'static str {
    use crate::entity::RefKind;
    match k {
        RefKind::Reads => "reads",
        RefKind::Writes => "writes",
        RefKind::Calls => "calls",
        RefKind::Member => "member",
        RefKind::Uses => "uses",
    }
}

/// Split a possibly-qualified name into a [`NodeRef`], falling back to the
/// entity's own schema when the reference carries none.
fn node_ref(name: &str, fallback_schema: Option<&str>) -> NodeRef {
    match name.rsplit_once('.') {
        Some((s, n)) => NodeRef {
            s: s.to_string(),
            n: n.to_string(),
        },
        None => NodeRef {
            s: fallback_schema.unwrap_or_default().to_string(),
            n: name.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::Design;
    use std::path::PathBuf;

    fn fixture_design() -> Design {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/design.yaml");
        Design::from_config(&p, "dev").unwrap()
    }

    #[test]
    fn build_full_model_from_fixture() {
        let d = fixture_design();
        let m = build(&d, None).unwrap();
        assert_eq!(m.project.name, "example");
        assert_eq!(m.project.db, "postgresql");
        assert!(
            m.schemas.iter().all(|s| s.name != "auth"),
            "external-only auth schema must not appear"
        );
        let config = m.schemas.iter().find(|s| s.name == "config").expect("config schema");
        assert!(config.tables >= 2, "config has lookups + lookup_values");
        let lookups = m
            .tables
            .iter()
            .find(|t| t.schema == "config" && t.name == "lookups")
            .expect("lookups");
        assert_eq!(lookups.kind, "table");
        assert!(lookups.columns.iter().any(|c| c.name == "id" && c.pk), "id is pk");
        assert!(
            m.refs.iter().any(|r| r.from.s == "config"
                && r.from.t == "lookup_values"
                && r.to.s == "config"
                && r.to.t == "lookups"),
            "FK edge present: {:?}",
            m.refs
        );
        assert!(m.tables.iter().all(|t| t.kind == "table"));
    }

    #[test]
    fn build_scoped_filters_tables_and_refs() {
        let d = fixture_design();
        let scope = d.resolve_scope(Some("config_only"), None).unwrap();
        let m = build(&d, Some(&scope)).unwrap();
        assert!(m.tables.iter().all(|t| t.schema != "staging"), "staging dropped");
        assert!(m.tables.iter().any(|t| t.schema == "config"));
        assert!(
            m.refs.iter().all(|r| r.to.s != "staging" && r.from.s != "staging"),
            "no refs cross into the dropped schema"
        );
    }

    #[test]
    fn snapshot_fixture_model_json() {
        let d = fixture_design();
        let m = build(&d, None).unwrap();
        let json = serde_json::to_string_pretty(&m).unwrap();
        insta::assert_snapshot!(json);
    }

    #[test]
    fn serializes_to_dbd_schema_shape() {
        let model = SchemaModel {
            version: 2,
            entities: vec![],
            deps: vec![],
            history: vec![],
            enums: vec![],
            stubs: vec![],
            project: ProjectInfo {
                name: "p".into(),
                db: "postgresql".into(),
                note: None,
            },
            schemas: vec![SchemaInfo {
                name: "config".into(),
                tables: 1,
                enums: 0,
            }],
            tables: vec![TableNode {
                schema: "config".into(),
                name: "lookups".into(),
                kind: "table".into(),
                note: None,
                note_md: None,
                columns: vec![Column {
                    name: "id".into(),
                    ty: "uuid".into(),
                    pk: true,
                    nn: true,
                    en: false,
                    def: Some("gen_random_uuid()".into()),
                    note: None,
                    fk: false,
                    uq: false,
                    identity: None,
                    generated: None,
                }],
                indexes: vec![],
                checks: vec![],
            }],
            refs: vec![],
        };
        let v: serde_json::Value = serde_json::to_value(&model).unwrap();
        assert_eq!(v["tables"][0]["columns"][0]["pk"], serde_json::json!(true));
        assert_eq!(v["tables"][0]["columns"][0]["type"], serde_json::json!("uuid"));
        assert!(v["tables"][0]["columns"][0].get("en").is_none(), "false flag omitted");
        assert_eq!(
            v["tables"][0]["columns"][0]["def"],
            serde_json::json!("gen_random_uuid()")
        );
        assert!(v["project"].get("note").is_none(), "None note omitted");
        assert_eq!(v["tables"][0]["columns"][0]["nn"], serde_json::json!(true));
    }
}
