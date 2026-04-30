use std::path::Path;

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

    async fn execute_file(&self, path: &Path) -> Result<()> {
        let sql = std::fs::read_to_string(path)?;
        self.execute_script(&sql).await
    }

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

    // ── Meta tracking (environment, safety guards) ─────

    async fn ensure_meta_table(&self) -> Result<()>;
    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>>;
    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()>;
}

pub mod mock;
#[cfg(feature = "postgres")]
pub mod postgres;

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
}
