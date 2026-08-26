//! Project-local cache of database entity names, used to resolve
//! "Unresolved reference" warnings without a live database connection.
//!
//! `inspect --from-db` writes the cache; subsequent offline `inspect`
//! runs (or runs in environments without `DATABASE_URL`) consult it to
//! silence warnings whose targets are known to exist in the captured
//! catalog snapshot.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{DbdError, Result};

/// Filename used inside `<project_dir>/.dbd/` for the reference cache.
pub const REFCACHE_FILE: &str = "refcache.json";

/// A snapshot of schema-qualified entity names captured from a live
/// database catalog (tables, views, enum types).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefCache {
    /// RFC3339 timestamp of when the snapshot was taken.
    pub captured_at: String,
    /// Adapter target name (e.g. "postgres", "supabase"), purely informational.
    #[serde(default)]
    pub source: String,
    /// Schema-qualified entity names (e.g. "auth.users", "public.user_role").
    pub entities: HashSet<String>,
}

impl RefCache {
    /// Build a new cache from a list of schema-qualified entity names.
    pub fn new<I, S>(source: impl Into<String>, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            captured_at: chrono::Utc::now().to_rfc3339(),
            source: source.into(),
            entities: names.into_iter().map(Into::into).collect(),
        }
    }

    /// Absolute path to the cache file for the given project directory.
    pub fn path(project_dir: &Path) -> PathBuf {
        project_dir.join(".dbd").join(REFCACHE_FILE)
    }

    /// Load the cache from `<project_dir>/.dbd/refcache.json`.
    /// Returns `Ok(None)` when the file does not exist.
    pub fn load(project_dir: &Path) -> Result<Option<Self>> {
        let path = Self::path(project_dir);
        if !path.exists() {
            return Ok(None);
        }
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let content =
            std::fs::read_to_string(&path).map_err(|e| DbdError::Config(format!("Read refcache failed: {e}")))?;
        let cache: Self =
            serde_json::from_str(&content).map_err(|e| DbdError::Config(format!("Parse refcache failed: {e}")))?;
        Ok(Some(cache))
    }

    /// Persist the cache to `<project_dir>/.dbd/refcache.json`,
    /// creating the parent directory if needed.
    pub fn save(&self, project_dir: &Path) -> Result<()> {
        let path = Self::path(project_dir);
        if let Some(parent) = path.parent() {
            // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            std::fs::create_dir_all(parent)
                .map_err(|e| DbdError::Config(format!("Create refcache dir failed: {e}")))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| DbdError::Config(format!("Serialize refcache failed: {e}")))?;
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        std::fs::write(&path, json).map_err(|e| DbdError::Config(format!("Write refcache failed: {e}")))?;
        Ok(())
    }

    /// Whether this name is in the captured catalog.
    pub fn contains(&self, name: &str) -> bool {
        self.entities.contains(name)
    }

    /// Number of entities in the snapshot.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// True when the snapshot has no entities.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempdir().unwrap();
        let cache = RefCache::new("postgres", ["auth.users", "public.lookups"]);
        cache.save(tmp.path()).unwrap();

        let loaded = RefCache::load(tmp.path()).unwrap().unwrap();
        assert_eq!(loaded.source, "postgres");
        assert!(loaded.contains("auth.users"));
        assert!(loaded.contains("public.lookups"));
        assert!(!loaded.contains("missing.thing"));
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let tmp = tempdir().unwrap();
        assert!(RefCache::load(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn save_creates_parent_dir() {
        let tmp = tempdir().unwrap();
        let cache = RefCache::new("postgres", ["public.t"]);
        cache.save(tmp.path()).unwrap();
        assert!(tmp.path().join(".dbd").join(REFCACHE_FILE).exists());
    }

    #[test]
    fn empty_cache_reports_empty() {
        let cache = RefCache::default();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }
}
