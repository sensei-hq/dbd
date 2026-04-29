use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// All entity types supported by dbd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Schema,
    Extension,
    Role,
    Enum,
    Table,
    View,
    Function,
    Procedure,
    External,
    Import,
    Export,
}

/// Entity types that live under a schema (file path: ddl/<type>/<schema>/<name>.ddl)
pub const TYPES_WITH_SCHEMA: &[EntityType] = &[
    EntityType::Enum,
    EntityType::Table,
    EntityType::View,
    EntityType::Function,
    EntityType::Procedure,
];

/// Entity types without schema qualification (file path: ddl/<type>/<name>.ddl)
pub const TYPES_WITHOUT_SCHEMA: &[EntityType] = &[
    EntityType::Role,
    EntityType::Schema,
    EntityType::Extension,
];

impl EntityType {
    /// Parse a type string from a folder name.
    pub fn from_folder_name(name: &str) -> Option<Self> {
        match name {
            "table" => Some(Self::Table),
            "view" => Some(Self::View),
            "function" => Some(Self::Function),
            "procedure" => Some(Self::Procedure),
            "enum" => Some(Self::Enum),
            "role" => Some(Self::Role),
            _ => None,
        }
    }

    /// Whether this type requires schema qualification.
    pub fn has_schema(&self) -> bool {
        TYPES_WITH_SCHEMA.contains(self)
    }
}

/// A parsed reference to another entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub ref_type: Option<String>,
}

/// Foreign key constraint with full detail.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: Option<FkAction>,
    pub on_update: Option<FkAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FkAction {
    Cascade,
    Restrict,
    SetNull,
    SetDefault,
    NoAction,
}

/// Table-level constraint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TableConstraint {
    PrimaryKey {
        name: Option<String>,
        columns: Vec<String>,
    },
    Unique {
        name: Option<String>,
        columns: Vec<String>,
    },
    ForeignKey(ForeignKey),
    Check {
        name: Option<String>,
        expression: String,
    },
}

/// Parsed column definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_pk: bool,
    pub is_unique: bool,
    pub is_identity: bool,
    pub comment: Option<String>,
    pub inline_fk: Option<ForeignKey>,
}

/// Index definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: Option<String>,
    pub columns: Vec<IndexColumn>,
    pub unique: bool,
    pub index_type: Option<IndexType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexColumn {
    pub name: String,
    pub order: Option<SortOrder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexType {
    Btree,
    Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    Desc,
}

/// Table and column comments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableComments {
    pub table: Option<String>,
    pub columns: HashMap<String, String>,
}

/// Full parsed table structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDef {
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub indexes: Vec<IndexDef>,
    pub comments: TableComments,
}

/// Enum variant with optional note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    pub name: String,
    pub note: Option<String>,
}

/// The central data structure. All DDL objects flow through this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_type: EntityType,
    pub name: String,
    pub schema: Option<String>,
    pub file: Option<PathBuf>,
    pub format: Option<String>,
    pub refers: Vec<String>,
    pub references: Vec<Reference>,
    pub search_paths: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub table_def: Option<TableDef>,
    pub enum_values: Vec<EnumValue>,
}

impl Entity {
    /// Create a new empty entity with the given type and name.
    pub fn new(entity_type: EntityType, name: &str) -> Self {
        let (schema, _) = split_qualified_name(name);
        Self {
            entity_type,
            name: name.to_string(),
            schema,
            file: None,
            format: None,
            refers: Vec::new(),
            references: Vec::new(),
            search_paths: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            reads: Vec::new(),
            writes: Vec::new(),
            table_def: None,
            enum_values: Vec::new(),
        }
    }

    /// Create an entity from a DDL file path.
    ///
    /// Path format: `ddl/<type>/<schema>/<name>.ddl` (schema-scoped)
    ///              `ddl/<type>/<name>.ddl` (non-schema types like role)
    pub fn from_file(path: &Path) -> Self {
        let parts: Vec<&str> = path.components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();

        // Strip leading "ddl" if present
        let parts = if parts.first() == Some(&"ddl") {
            &parts[1..]
        } else {
            &parts
        };

        let entity_type = parts
            .first()
            .and_then(|s| EntityType::from_folder_name(s))
            .unwrap_or(EntityType::Table);

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("ddl");

        let (name, schema) = if entity_type.has_schema() && parts.len() >= 3 {
            let schema = parts[1].to_string();
            let qualified = format!("{}.{}", schema, stem);
            (qualified, Some(schema))
        } else {
            (stem.to_string(), None)
        };

        Self {
            entity_type,
            name,
            schema,
            file: Some(path.to_path_buf()),
            format: Some(ext.to_string()),
            refers: Vec::new(),
            references: Vec::new(),
            search_paths: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            reads: Vec::new(),
            writes: Vec::new(),
            table_def: None,
            enum_values: Vec::new(),
        }
    }

    /// Create a schema entity.
    pub fn schema(name: &str) -> Self {
        Self::new(EntityType::Schema, name)
    }

    /// Create an external entity (FK stub).
    pub fn external(name: &str) -> Self {
        Self::new(EntityType::External, name)
    }

    /// Whether this entity has validation errors.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Split "schema.name" into (Some("schema"), "name"), or (None, "name").
pub fn split_qualified_name(name: &str) -> (Option<String>, String) {
    match name.split_once('.') {
        Some((schema, entity)) => (Some(schema.to_string()), entity.to_string()),
        None => (None, name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn entity_type_from_folder_name() {
        assert_eq!(EntityType::from_folder_name("table"), Some(EntityType::Table));
        assert_eq!(EntityType::from_folder_name("view"), Some(EntityType::View));
        assert_eq!(EntityType::from_folder_name("function"), Some(EntityType::Function));
        assert_eq!(EntityType::from_folder_name("procedure"), Some(EntityType::Procedure));
        assert_eq!(EntityType::from_folder_name("enum"), Some(EntityType::Enum));
        assert_eq!(EntityType::from_folder_name("role"), Some(EntityType::Role));
        assert_eq!(EntityType::from_folder_name("unknown"), None);
    }

    #[test]
    fn entity_type_has_schema() {
        assert!(EntityType::Table.has_schema());
        assert!(EntityType::View.has_schema());
        assert!(EntityType::Enum.has_schema());
        assert!(!EntityType::Role.has_schema());
        assert!(!EntityType::Schema.has_schema());
    }

    #[test]
    fn entity_from_table_file() {
        let entity = Entity::from_file(Path::new("ddl/table/config/lookups.ddl"));
        assert_eq!(entity.entity_type, EntityType::Table);
        assert_eq!(entity.name, "config.lookups");
        assert_eq!(entity.schema, Some("config".to_string()));
        assert_eq!(entity.file, Some(PathBuf::from("ddl/table/config/lookups.ddl")));
        assert_eq!(entity.format, Some("ddl".to_string()));
    }

    #[test]
    fn entity_from_view_file() {
        let entity = Entity::from_file(Path::new("ddl/view/config/genders.ddl"));
        assert_eq!(entity.entity_type, EntityType::View);
        assert_eq!(entity.name, "config.genders");
        assert_eq!(entity.schema, Some("config".to_string()));
    }

    #[test]
    fn entity_from_procedure_file() {
        let entity = Entity::from_file(Path::new("ddl/procedure/staging/import_lookups.ddl"));
        assert_eq!(entity.entity_type, EntityType::Procedure);
        assert_eq!(entity.name, "staging.import_lookups");
        assert_eq!(entity.schema, Some("staging".to_string()));
    }

    #[test]
    fn entity_from_enum_file() {
        let entity = Entity::from_file(Path::new("ddl/enum/config/status.sql"));
        assert_eq!(entity.entity_type, EntityType::Enum);
        assert_eq!(entity.name, "config.status");
        assert_eq!(entity.schema, Some("config".to_string()));
        assert_eq!(entity.format, Some("sql".to_string()));
    }

    #[test]
    fn entity_from_role_file() {
        let entity = Entity::from_file(Path::new("ddl/role/admin.ddl"));
        assert_eq!(entity.entity_type, EntityType::Role);
        assert_eq!(entity.name, "admin");
        assert_eq!(entity.schema, None);
    }

    #[test]
    fn entity_new_with_qualified_name() {
        let entity = Entity::new(EntityType::Table, "config.lookups");
        assert_eq!(entity.name, "config.lookups");
        assert_eq!(entity.schema, Some("config".to_string()));
    }

    #[test]
    fn entity_new_with_unqualified_name() {
        let entity = Entity::new(EntityType::Role, "admin");
        assert_eq!(entity.name, "admin");
        assert_eq!(entity.schema, None);
    }

    #[test]
    fn entity_schema_constructor() {
        let entity = Entity::schema("config");
        assert_eq!(entity.entity_type, EntityType::Schema);
        assert_eq!(entity.name, "config");
    }

    #[test]
    fn split_qualified_name_with_schema() {
        let (schema, name) = split_qualified_name("config.lookups");
        assert_eq!(schema, Some("config".to_string()));
        assert_eq!(name, "lookups");
    }

    #[test]
    fn split_qualified_name_without_schema() {
        let (schema, name) = split_qualified_name("admin");
        assert_eq!(schema, None);
        assert_eq!(name, "admin");
    }

    #[test]
    fn entity_has_errors() {
        let mut entity = Entity::new(EntityType::Table, "test");
        assert!(!entity.has_errors());
        entity.errors.push("missing file".to_string());
        assert!(entity.has_errors());
    }

    #[test]
    fn fk_action_serializes() {
        let fk = ForeignKey {
            on_delete: Some(FkAction::Cascade),
            on_update: Some(FkAction::NoAction),
            ..Default::default()
        };
        let json = serde_json::to_string(&fk).unwrap();
        assert!(json.contains("cascade"));
        assert!(json.contains("no_action"));
    }
}
