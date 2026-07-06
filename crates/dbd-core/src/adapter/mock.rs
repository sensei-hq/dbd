use std::collections::HashSet;
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
    pub meta: Arc<Mutex<Option<ProjectMeta>>>,
    pub connected: bool,
    /// Entity names that `resolve_entity` should report as existing.
    pub known_entities: Arc<Mutex<HashSet<String>>>,
    /// Ordered log of batch-transaction lifecycle calls: "begin"/"commit"/"rollback".
    pub txn: Arc<Mutex<Vec<String>>>,
    /// Whether this mock reports transactional-apply support.
    pub supports_txn: bool,
    /// If set, `apply_entity` errors for this entity name (fault injection).
    pub fail_on: Arc<Mutex<Option<String>>>,
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAdapter {
    pub fn new() -> Self {
        Self {
            applied: Arc::new(Mutex::new(Vec::new())),
            scripts: Arc::new(Mutex::new(Vec::new())),
            imported: Arc::new(Mutex::new(Vec::new())),
            meta: Arc::new(Mutex::new(None)),
            connected: false,
            known_entities: Arc::new(Mutex::new(HashSet::new())),
            txn: Arc::new(Mutex::new(Vec::new())),
            supports_txn: false,
            fail_on: Arc::new(Mutex::new(None)),
        }
    }

    /// Report transactional-apply support so `Design::apply` wraps the batch.
    pub fn with_transactions(mut self) -> Self {
        self.supports_txn = true;
        self
    }

    /// Make `apply_entity` fail when it reaches `name`, to exercise rollback.
    pub fn fail_on_entity(self, name: &str) -> Self {
        *self.fail_on.lock().unwrap() = Some(name.to_string());
        self
    }

    /// Ordered batch-transaction lifecycle log ("begin"/"commit"/"rollback").
    pub fn txn_log(&self) -> Vec<String> {
        self.txn.lock().unwrap().clone()
    }

    /// Configure `resolve_entity` to report these names as existing in the DB.
    pub fn with_known_entities<I, S>(self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut set = self.known_entities.lock().unwrap();
        for n in names {
            set.insert(n.into());
        }
        drop(set);
        self
    }

    pub fn with_version(self, version: u32) -> Self {
        self.with_meta("dev", version)
    }

    pub fn with_meta(self, env: &str, version: u32) -> Self {
        *self.meta.lock().unwrap() = Some(ProjectMeta {
            project: "test".to_string(),
            env: env.to_string(),
            version,
            applied_at: Some("2026-01-01T00:00:00Z".to_string()),
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
        if self.fail_on.lock().unwrap().as_deref() == Some(entity.name.as_str()) {
            return Err(crate::error::DbdError::Config(format!(
                "mock: injected failure applying {}",
                entity.name
            )));
        }
        self.applied.lock().unwrap().push(entity.name.clone());
        Ok(())
    }

    fn supports_transactional_apply(&self) -> bool {
        self.supports_txn
    }

    async fn begin_batch(&self) -> Result<()> {
        self.txn.lock().unwrap().push("begin".to_string());
        Ok(())
    }

    async fn commit_batch(&self) -> Result<()> {
        self.txn.lock().unwrap().push("commit".to_string());
        Ok(())
    }

    async fn rollback_batch(&self) -> Result<()> {
        self.txn.lock().unwrap().push("rollback".to_string());
        Ok(())
    }

    async fn import_data(&self, entity: &Entity, _dry_run: bool) -> Result<()> {
        self.imported.lock().unwrap().push(entity.name.clone());
        Ok(())
    }

    async fn export_data(&self, _entity: &Entity, _out_dir: Option<&std::path::Path>) -> Result<()> {
        Ok(())
    }

    async fn load_catalog(&mut self) -> Result<()> {
        Ok(())
    }

    fn classify_reference(&self, _name: &str, _installed: &[String]) -> ReferenceClass {
        ReferenceClass::UserDefined
    }

    async fn resolve_entity(&self, name: &str) -> Result<Option<String>> {
        if self.known_entities.lock().unwrap().contains(name) {
            Ok(Some(name.to_string()))
        } else {
            Ok(None)
        }
    }

    async fn list_entities(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self.known_entities.lock().unwrap().iter().cloned().collect();
        names.sort();
        Ok(names)
    }

    async fn ensure_import_procedure(&self) -> Result<()> {
        Ok(())
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        Ok(())
    }

    async fn get_db_version(&self) -> Result<u32> {
        Ok(self.meta.lock().unwrap().as_ref().map(|m| m.version).unwrap_or(0))
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
        Ok(self.meta.lock().unwrap().clone())
    }

    async fn set_project_meta(&self, env: &str, version: u32) -> Result<()> {
        *self.meta.lock().unwrap() = Some(ProjectMeta {
            project: "test".to_string(),
            env: env.to_string(),
            version,
            applied_at: Some("2026-01-01T00:00:00Z".to_string()),
        });
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

    // ── T1: get_db_version returns 0 when no meta ───────

    #[tokio::test]
    async fn t1_get_db_version_returns_0_when_no_meta() {
        let mock = MockAdapter::new();
        assert_eq!(mock.get_db_version().await.unwrap(), 0);
    }

    // ── T2: get_db_version returns meta version ────────

    #[tokio::test]
    async fn t2_get_db_version_returns_meta_version() {
        let mock = MockAdapter::new().with_meta("dev", 3);
        assert_eq!(mock.get_db_version().await.unwrap(), 3);
    }

    // ── T3: set_project_meta updates version and env ───

    #[tokio::test]
    async fn t3_set_project_meta_updates_state() {
        let mock = MockAdapter::new();
        assert_eq!(mock.get_db_version().await.unwrap(), 0);

        mock.set_project_meta("prod", 5).await.unwrap();

        assert_eq!(mock.get_db_version().await.unwrap(), 5);
        let meta = mock.get_project_meta().await.unwrap().unwrap();
        assert_eq!(meta.env, "prod");
        assert_eq!(meta.version, 5);
        assert!(meta.applied_at.is_some());
    }

    // ── T6: ProjectMeta includes applied_at ────────────

    #[tokio::test]
    async fn t6_project_meta_includes_applied_at() {
        let mock = MockAdapter::new().with_meta("dev", 1);
        let meta = mock.get_project_meta().await.unwrap().unwrap();
        assert!(meta.applied_at.is_some());
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
