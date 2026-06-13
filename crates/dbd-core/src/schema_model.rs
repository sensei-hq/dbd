//! The `SchemaModel` — a dbd-native JSON model of a schema, consumed by the
//! diagram viewer. Serializes to the `DBD_SCHEMA` shape (see
//! docs/mockup/designs/schema-data.js). Boolean column flags (`pk`/`nn`/`en`)
//! are emitted only when true; the viewer reads them truthily.

use serde::Serialize;

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
    pub s: String,
    pub t: String,
    pub c: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
