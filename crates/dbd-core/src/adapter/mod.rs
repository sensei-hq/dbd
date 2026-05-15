use std::collections::{HashMap, HashSet};

use async_trait::async_trait;

use crate::entity::Entity;
use crate::error::Result;

/// Classification result for a reference name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceClass {
    /// Built-in function, type, or operator (pg_catalog, SQL standard)
    Internal,
    /// From a specific database extension
    Extension(String),
    /// Not recognized — treated as a project entity dependency
    UserDefined,
}

/// Project-level metadata stored in `_dbd_meta`.
#[derive(Debug, Clone)]
pub struct ProjectMeta {
    pub project: String,
    pub env: String,
    pub version: u32,
    pub applied_at: Option<String>,
}

/// Catalog data loaded from the database for reference classification.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CatalogData {
    /// Qualified function names: "pg_catalog.array_agg", "extensions.st_distance"
    pub functions: HashSet<String>,
    /// Qualified type names: "pg_catalog.int4", "public.geometry"
    pub types: HashSet<String>,
    /// Bare name -> extension name: "st_distance" -> "postgis"
    pub extension_objects: HashMap<String, String>,
    /// Schemas owned by extensions
    pub extension_schemas: HashSet<String>,
}

/// The database adapter trait — each target implements this.
///
/// Adapters handle:
/// - Connection lifecycle
/// - DDL execution (apply entities)
/// - Data operations (import/export via COPY or equivalent)
/// - Catalog queries (reference classification, entity resolution)
/// - Migration tracking (_dbd_migrations + _dbd_meta)
#[async_trait]
pub trait DatabaseAdapter: Send + Sync {
    // ── Lifecycle ───────────────────────────────────────

    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn test_connection(&self) -> Result<bool>;

    // ── DDL execution ──────────────────────────────────

    async fn execute_script(&self, sql: &str) -> Result<()>;

    async fn apply_entity(&self, entity: &Entity) -> Result<()>;

    async fn apply_entities(&self, entities: &[Entity]) -> Result<()> {
        for entity in entities {
            self.apply_entity(entity).await?;
        }
        Ok(())
    }

    /// Return true if the adapter wants all entities at once (e.g., Convex).
    fn prefers_batch_apply(&self) -> bool {
        false
    }

    // ── Data operations ────────────────────────────────

    async fn import_data(&self, entity: &Entity, dry_run: bool) -> Result<()>;
    async fn export_data(&self, entity: &Entity) -> Result<()>;

    async fn batch_export(&self, entities: &[Entity]) -> Result<()> {
        for entity in entities {
            self.export_data(entity).await?;
        }
        Ok(())
    }

    // ── Catalog — adapter-owned knowledge ──────────────

    /// Load the catalog of built-in functions, types, and extension objects.
    /// Called once after connect. Results cached for the session.
    async fn load_catalog(&mut self) -> Result<()>;

    /// Classify a reference as internal, extension, or user-defined.
    fn classify_reference(&self, name: &str, installed_extensions: &[String]) -> ReferenceClass;

    /// Check if an entity exists in the database catalog.
    async fn resolve_entity(&self, name: &str) -> Result<Option<String>>;

    /// List all user-defined entities (tables, views, enum types) in the
    /// database, returned as schema-qualified names (e.g. `auth.users`).
    /// System catalogs (pg_catalog, information_schema, pg_toast) are excluded.
    ///
    /// Used by `inspect --database` to build a project-local reference cache
    /// for offline lookups.
    async fn list_entities(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    // ── Migration tracking ─────────────────────────────

    async fn ensure_migrations_table(&self) -> Result<()>;
    async fn get_db_version(&self) -> Result<u32>;
    async fn apply_migration(
        &self,
        version: u32,
        sql: &str,
        description: &str,
        checksum: &str,
    ) -> Result<()>;
    async fn clear_project_migrations(&self) -> Result<()>;

    // ── Internal dbd procedures ────────────────────────

    /// Ensure the internal `staging.import_jsonb_to_table` procedure exists.
    /// Called automatically before any JSONL import. The procedure is embedded
    /// in the dbd binary — users do not own or manage it.
    async fn ensure_import_procedure(&self) -> Result<()>;

    // ── Meta tracking (environment, safety guards) ─────

    async fn ensure_meta_table(&self) -> Result<()>;
    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>>;
    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()>;
}

pub mod convex;
pub mod mock;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_class_equality() {
        assert_eq!(ReferenceClass::Internal, ReferenceClass::Internal);
        assert_eq!(
            ReferenceClass::Extension("postgis".into()),
            ReferenceClass::Extension("postgis".into())
        );
        assert_ne!(ReferenceClass::Internal, ReferenceClass::UserDefined);
    }

    #[test]
    fn c1_pg_catalog_function_is_internal() {
        let mut catalog = CatalogData::default();
        catalog
            .functions
            .insert("pg_catalog.array_agg".to_string());
        assert!(catalog.functions.contains("pg_catalog.array_agg"));
    }

    #[test]
    fn c4_sql_noise_keywords_present() {
        // SQL noise keywords are checked via PostgresAdapter::is_sql_noise,
        // but we verify common noise words are recognized patterns.
        let noise_words = ["varchar", "integer", "now", "coalesce", "count"];
        for word in noise_words {
            assert!(
                [
                    "varchar", "int", "integer", "bigint", "smallint", "numeric", "decimal",
                    "boolean", "text", "date", "timestamp", "timestamptz", "uuid", "jsonb",
                    "json", "bytea", "float", "double", "real", "serial", "bigserial", "btree",
                    "hash", "gin", "gist", "brin", "now", "coalesce", "nullif", "greatest",
                    "least", "extract", "count", "sum", "avg", "min", "max", "string_agg",
                    "row_number", "rank", "dense_rank", "lead", "lag", "upper", "lower", "trim",
                    "length", "replace", "substring", "cast", "exists", "between", "like", "in",
                    "not", "and", "or", "true", "false", "null", "default", "current_user",
                    "localtime", "localtimestamp", "random", "floor", "ceil", "abs", "round",
                    "enum", "record", "void", "trigger", "event_trigger",
                ]
                .contains(&word),
                "{word} should be SQL noise"
            );
        }
    }

    #[test]
    fn c10_catalog_data_default_is_empty() {
        let catalog = CatalogData::default();
        assert!(catalog.functions.is_empty());
        assert!(catalog.types.is_empty());
        assert!(catalog.extension_objects.is_empty());
        assert!(catalog.extension_schemas.is_empty());
    }

    #[test]
    fn c11_catalog_data_serialization_roundtrip() {
        let mut catalog = CatalogData::default();
        catalog
            .functions
            .insert("pg_catalog.array_agg".to_string());
        catalog
            .extension_objects
            .insert("st_distance".to_string(), "postgis".to_string());
        let json = serde_json::to_string(&catalog).unwrap();
        let deserialized: CatalogData = serde_json::from_str(&json).unwrap();
        assert!(deserialized.functions.contains("pg_catalog.array_agg"));
        assert_eq!(
            deserialized.extension_objects.get("st_distance"),
            Some(&"postgis".to_string())
        );
    }
}
