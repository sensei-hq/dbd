use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{DatabaseAdapter, ProjectMeta, ReferenceClass};
use crate::entity::Entity;
use crate::error::Result;

/// A mock adapter for testing. Records all operations without executing SQL.
#[derive(Debug, Clone)]
pub struct MockAdapter {
    pub applied: Arc<Mutex<Vec<String>>>,
    pub scripts: Arc<Mutex<Vec<String>>>,
    pub imported: Arc<Mutex<Vec<String>>>,
    pub db_version: u32,
    pub meta: Option<ProjectMeta>,
    pub connected: bool,
}

impl MockAdapter {
    pub fn new() -> Self {
        Self {
            applied: Arc::new(Mutex::new(Vec::new())),
            scripts: Arc::new(Mutex::new(Vec::new())),
            imported: Arc::new(Mutex::new(Vec::new())),
            db_version: 0,
            meta: None,
            connected: false,
        }
    }

    pub fn with_version(mut self, version: u32) -> Self {
        self.db_version = version;
        self
    }

    pub fn with_meta(mut self, env: &str, version: u32) -> Self {
        self.meta = Some(ProjectMeta {
            project: "test".to_string(),
            env: env.to_string(),
            version,
        });
        self
    }

    pub fn applied_names(&self) -> Vec<String> {
        self.applied.lock().unwrap().clone()
    }

    pub fn script_count(&self) -> usize {
        self.scripts.lock().unwrap().len()
    }

    pub fn imported_names(&self) -> Vec<String> {
        self.imported.lock().unwrap().clone()
    }
}

#[async_trait]
impl DatabaseAdapter for MockAdapter {
    async fn connect(&mut self) -> Result<()> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.connected = false;
        Ok(())
    }

    async fn test_connection(&self) -> Result<bool> {
        Ok(self.connected)
    }

    async fn execute_script(&self, sql: &str) -> Result<()> {
        self.scripts.lock().unwrap().push(sql.to_string());
        Ok(())
    }

    async fn apply_entity(&self, entity: &Entity) -> Result<()> {
        self.applied.lock().unwrap().push(entity.name.clone());
        Ok(())
    }

    async fn import_data(&self, entity: &Entity, _dry_run: bool) -> Result<()> {
        self.imported.lock().unwrap().push(entity.name.clone());
        Ok(())
    }

    async fn export_data(&self, _entity: &Entity) -> Result<()> {
        Ok(())
    }

    async fn load_catalog(&mut self) -> Result<()> {
        Ok(())
    }

    fn classify_reference(&self, _name: &str, _installed: &[String]) -> ReferenceClass {
        ReferenceClass::UserDefined
    }

    async fn resolve_entity(&self, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        Ok(())
    }

    async fn get_db_version(&self) -> Result<u32> {
        Ok(self.db_version)
    }

    async fn apply_migration(
        &self,
        _version: u32,
        _sql: &str,
        _description: &str,
        _checksum: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn clear_project_migrations(&self) -> Result<()> {
        Ok(())
    }

    async fn ensure_meta_table(&self) -> Result<()> {
        Ok(())
    }

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        Ok(self.meta.clone())
    }

    async fn set_project_meta(&self, _env: &str, _version: u32) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    #[tokio::test]
    async fn mock_records_applied_entities() {
        let mock = MockAdapter::new();
        let e1 = Entity::new(EntityType::Schema, "config");
        let e2 = Entity::new(EntityType::Table, "config.lookups");

        mock.apply_entity(&e1).await.unwrap();
        mock.apply_entity(&e2).await.unwrap();

        assert_eq!(mock.applied_names(), vec!["config", "config.lookups"]);
    }

    #[tokio::test]
    async fn mock_records_scripts() {
        let mock = MockAdapter::new();
        mock.execute_script("CREATE TABLE foo (id int);").await.unwrap();
        assert_eq!(mock.script_count(), 1);
    }

    #[tokio::test]
    async fn mock_returns_configured_version() {
        let mock = MockAdapter::new().with_version(3);
        assert_eq!(mock.get_db_version().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn mock_returns_configured_meta() {
        let mock = MockAdapter::new().with_meta("prod", 2);
        let meta = mock.get_project_meta().await.unwrap().unwrap();
        assert_eq!(meta.env, "prod");
        assert_eq!(meta.version, 2);
    }

    #[tokio::test]
    async fn mock_batch_apply() {
        let mock = MockAdapter::new();
        let entities = vec![
            Entity::new(EntityType::Schema, "config"),
            Entity::new(EntityType::Table, "config.lookups"),
        ];
        mock.apply_entities(&entities).await.unwrap();
        assert_eq!(mock.applied_names().len(), 2);
    }
}
