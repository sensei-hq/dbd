//! The `SchemaModel` — a dbd-native JSON model of a schema, consumed by the
//! diagram viewer. Serializes to the `DBD_SCHEMA` shape (see
//! docs/mockup/designs/schema-data.js). Boolean column flags (`pk`/`nn`/`en`)
//! are emitted only when true; the viewer reads them truthily.

use serde::Serialize;

use crate::design::Design;
use crate::entity::{EntityType, FkAction, TableConstraint};
use crate::scope::ResolvedScope;

#[derive(Debug, PartialEq, Serialize)]
pub struct SchemaModel {
    pub project: ProjectInfo,
    pub schemas: Vec<SchemaInfo>,
    pub tables: Vec<TableNode>,
    pub refs: Vec<Ref>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct ProjectInfo {
    pub name: String,
    pub db: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: usize,
    pub enums: usize,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct TableNode {
    pub schema: String,
    pub name: String,
    /// "table" in v1; extension point for view/function/procedure later.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "noteMd", skip_serializing_if = "Option::is_none")]
    pub note_md: Option<String>,
    pub columns: Vec<Column>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// primary key
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pk: bool,
    /// not null
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub nn: bool,
    /// column type is an enum
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub en: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Ref {
    pub from: RefEnd,
    pub to: RefEnd,
    /// FK on-delete action: cascade | restrict | set_null | set_default | no_action
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct RefEnd {
    /// Schema name
    pub s: String,
    /// Table name
    pub t: String,
    /// Column name
    pub c: String,
}

/// Build a `SchemaModel` from a loaded design, optionally filtered to a scope.
/// v1 emits only tables + schemas + FK refs.
pub fn build(design: &Design, scope: Option<&ResolvedScope>) -> SchemaModel {
    let entities = match scope {
        Some(s) => design.scoped_entities(s).unwrap_or_default(),
        None => design.entities().to_vec(),
    };

    // NOTE: matches c.data_type against the enum entity's name (file-stem, e.g.
    // "config.status" / "status"), not necessarily the CREATE TYPE identifier
    // ("status_type"). Works when they coincide; a stricter match is future work.
    let enum_names: std::collections::HashSet<String> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .flat_map(|e| {
            let bare = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();
            [e.name.clone(), bare]
        })
        .collect();

    let table_ids: std::collections::HashSet<String> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Table)
        .map(|e| e.name.clone())
        .collect();

    let mut tables = Vec::new();
    let mut refs = Vec::new();

    for e in entities.iter().filter(|e| e.entity_type == EntityType::Table) {
        let Some(def) = &e.table_def else { continue };
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
                note: c.comment.clone().or_else(|| def.comments.columns.get(&c.name).cloned()),
            })
            .collect();

        tables.push(TableNode {
            schema,
            name,
            kind: "table".into(),
            note: note_first_line(def),
            note_md: def.comments.table.clone(),
            columns,
        });

        for fk in collect_fks(def) {
            let to_schema = fk.ref_schema.clone().unwrap_or_else(|| e.schema.clone().unwrap_or_default());
            let to_id = format!("{to_schema}.{}", fk.ref_table);
            if !table_ids.contains(&to_id) {
                continue;
            }
            let action = fk.on_delete.map(fk_action_str);
            let from_schema = e.schema.clone().unwrap_or_default();
            let from_table = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();
            for (i, local) in fk.columns.iter().enumerate() {
                let remote = fk.ref_columns.get(i).cloned().unwrap_or_default();
                refs.push(Ref {
                    from: RefEnd { s: from_schema.clone(), t: from_table.clone(), c: local.clone() },
                    to: RefEnd { s: to_schema.clone(), t: fk.ref_table.clone(), c: remote },
                    action: action.clone(),
                });
            }
        }
    }

    let mut schema_set: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for e in &entities {
        let Some(s) = &e.schema else { continue };
        match e.entity_type {
            EntityType::Table => schema_set.entry(s.clone()).or_insert((0, 0)).0 += 1,
            EntityType::Enum => schema_set.entry(s.clone()).or_insert((0, 0)).1 += 1,
            _ => continue,
        };
    }
    let schemas = schema_set
        .into_iter()
        .map(|(name, (tables, enums))| SchemaInfo { name, tables, enums })
        .collect();

    tables.sort_by(|a, b| (a.schema.as_str(), a.name.as_str()).cmp(&(b.schema.as_str(), b.name.as_str())));

    SchemaModel {
        project: ProjectInfo {
            name: design.config().project.name.clone(),
            db: design.config().source.dialect.clone(),
            note: design.config().project.note.clone(),
        },
        schemas,
        tables,
        refs,
    }
}

/// First non-empty first line of a table comment → the short `note` (or None).
fn note_first_line(def: &crate::entity::TableDef) -> Option<String> {
    def.comments.table.as_ref().and_then(|t| {
        let first = t.lines().next().unwrap_or("").trim();
        if first.is_empty() { None } else { Some(first.to_string()) }
    })
}

/// All foreign keys on a table: inline column FKs + table-level FK constraints.
fn collect_fks(def: &crate::entity::TableDef) -> Vec<crate::entity::ForeignKey> {
    let mut out: Vec<crate::entity::ForeignKey> = def
        .columns
        .iter()
        .filter_map(|c| c.inline_fk.clone())
        .collect();
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
        let m = build(&d, None);
        assert_eq!(m.project.name, "example");
        assert_eq!(m.project.db, "postgresql");
        assert!(m.schemas.iter().all(|s| s.name != "auth"), "external-only auth schema must not appear");
        let config = m.schemas.iter().find(|s| s.name == "config").expect("config schema");
        assert!(config.tables >= 2, "config has lookups + lookup_values");
        let lookups = m.tables.iter().find(|t| t.schema == "config" && t.name == "lookups").expect("lookups");
        assert_eq!(lookups.kind, "table");
        assert!(lookups.columns.iter().any(|c| c.name == "id" && c.pk), "id is pk");
        assert!(
            m.refs.iter().any(|r|
                r.from.s == "config" && r.from.t == "lookup_values"
                && r.to.s == "config" && r.to.t == "lookups"),
            "FK edge present: {:?}", m.refs
        );
        assert!(m.tables.iter().all(|t| t.kind == "table"));
    }

    #[test]
    fn build_scoped_filters_tables_and_refs() {
        let d = fixture_design();
        let scope = d.resolve_scope(Some("config_only"), None).unwrap();
        let m = build(&d, Some(&scope));
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
        let m = build(&d, None);
        let json = serde_json::to_string_pretty(&m).unwrap();
        insta::assert_snapshot!(json);
    }

    #[test]
    fn serializes_to_dbd_schema_shape() {
        let model = SchemaModel {
            project: ProjectInfo { name: "p".into(), db: "postgresql".into(), note: None },
            schemas: vec![SchemaInfo { name: "config".into(), tables: 1, enums: 0 }],
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
                }],
            }],
            refs: vec![],
        };
        let v: serde_json::Value = serde_json::to_value(&model).unwrap();
        assert_eq!(v["tables"][0]["columns"][0]["pk"], serde_json::json!(true));
        assert_eq!(v["tables"][0]["columns"][0]["type"], serde_json::json!("uuid"));
        assert!(v["tables"][0]["columns"][0].get("en").is_none(), "false flag omitted");
        assert_eq!(v["tables"][0]["columns"][0]["def"], serde_json::json!("gen_random_uuid()"));
        assert!(v["project"].get("note").is_none(), "None note omitted");
        assert_eq!(v["tables"][0]["columns"][0]["nn"], serde_json::json!(true));
    }
}
