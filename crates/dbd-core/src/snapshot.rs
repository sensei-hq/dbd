use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::entity::{ColumnDef, IndexDef, TableConstraint};
use crate::error::Result;

const SNAPSHOTS_DIR: &str = "snapshots";
const MIGRATIONS_DIR: &str = "migrations";
const VERSION_PAD: usize = 3;

/// Zero-pad a version number.
pub fn pad_version(n: u32) -> String {
    format!("{:0>width$}", n, width = VERSION_PAD)
}

/// SHA-256 hex digest of a string.
pub fn checksum_of(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ── Snapshot types ──────────────────────────────────────

/// A versioned schema snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub description: String,
    pub timestamp: String,
    pub tables: Vec<TableSnapshot>,
    #[serde(default)]
    pub enums: Vec<EnumSnapshot>,
}

/// Snapshot of a single enum type's structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumSnapshot {
    pub name: String,
    pub schema: String,
    pub values: Vec<String>,
}

/// Snapshot of a single table's structure.
/// Reuses ColumnDef, IndexDef, TableConstraint from entity module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub name: String,
    pub schema: String,
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub table_constraints: Vec<TableConstraint>,
}

/// Migration graph metadata (stored as graph.json in each migration folder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationGraph {
    #[serde(rename = "fromVersion")]
    pub from_version: u32,
    #[serde(rename = "toVersion")]
    pub to_version: u32,
    #[serde(default)]
    pub added: Vec<String>,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
}

/// A pending migration with its source directory and metadata.
#[derive(Debug, Clone)]
pub struct PendingMigration {
    pub from_version: u32,
    pub to_version: u32,
    pub migration_dir: PathBuf,
    pub added: Vec<String>,
    pub altered: Vec<String>,
    pub dropped: Vec<String>,
    pub checksum: String,
}

// ── Snapshot I/O ────────────────────────────────────────

/// List all snapshots in the snapshots directory, sorted by version.
pub fn list_snapshots(dir: &Path) -> Vec<SnapshotInfo> {
    let snapshots_dir = dir.join(SNAPSHOTS_DIR);
    if !snapshots_dir.exists() {
        return Vec::new();
    }

    let mut snapshots: Vec<SnapshotInfo> = std::fs::read_dir(&snapshots_dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".json") && name.len() == VERSION_PAD + 5)
        })
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let version: u32 = name.trim_end_matches(".json").parse().ok()?;
            let file = entry.path();
            let (description, timestamp) = match std::fs::read_to_string(&file) {
                Ok(content) => {
                    let snap: serde_json::Value = serde_json::from_str(&content).ok()?;
                    (
                        snap["description"].as_str().unwrap_or("").to_string(),
                        snap["timestamp"].as_str().unwrap_or("").to_string(),
                    )
                }
                Err(_) => (String::new(), String::new()),
            };
            Some(SnapshotInfo {
                version,
                file,
                description,
                timestamp,
            })
        })
        .collect();

    snapshots.sort_by_key(|s| s.version);
    snapshots
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub version: u32,
    pub file: PathBuf,
    pub description: String,
    pub timestamp: String,
}

/// Read a specific snapshot from disk.
pub fn read_snapshot(version: u32, dir: &Path) -> Result<Option<Snapshot>> {
    let file = dir
        .join(SNAPSHOTS_DIR)
        .join(format!("{}.json", pad_version(version)));
    if !file.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&file)?;
    let snapshot: Snapshot = serde_json::from_str(&content)?;
    Ok(Some(snapshot))
}

/// Get the latest snapshot, or None if none exist.
pub fn latest_snapshot(dir: &Path) -> Result<Option<Snapshot>> {
    let snapshots = list_snapshots(dir);
    match snapshots.last() {
        Some(info) => read_snapshot(info.version, dir),
        None => Ok(None),
    }
}

/// Determine the next snapshot version number.
pub fn next_version(dir: &Path) -> u32 {
    let snapshots = list_snapshots(dir);
    match snapshots.last() {
        Some(info) => info.version + 1,
        None => 1,
    }
}

/// Check if any snapshots exist.
pub fn has_snapshots(dir: &Path) -> bool {
    !list_snapshots(dir).is_empty()
}

// ── Pending migrations ──────────────────────────────────

/// List pending migrations (versions after current_db_version).
pub fn pending_migrations(current_db_version: u32, dir: &Path) -> Vec<PendingMigration> {
    let migrations_dir = dir.join(MIGRATIONS_DIR);
    if !migrations_dir.exists() {
        return Vec::new();
    }

    let mut migrations: Vec<PendingMigration> = std::fs::read_dir(&migrations_dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let to_version: u32 = name.parse().ok()?;
            if to_version <= current_db_version {
                return None;
            }
            let migration_dir = entry.path();
            let graph_file = migration_dir.join("graph.json");
            if !graph_file.exists() {
                return None;
            }
            let content = std::fs::read_to_string(&graph_file).ok()?;
            let graph: MigrationGraph = serde_json::from_str(&content).ok()?;
            let checksum = checksum_of(&content);
            Some(PendingMigration {
                from_version: graph.from_version,
                to_version,
                migration_dir,
                added: graph.added,
                altered: graph.altered,
                dropped: graph.dropped,
                checksum,
            })
        })
        .collect();

    migrations.sort_by_key(|m| m.to_version);
    migrations
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_snapshot_dir(tmp: &TempDir) {
        let dir = tmp.path().join("snapshots");
        fs::create_dir_all(&dir).unwrap();

        let snap1 = Snapshot {
            version: 1,
            description: "initial".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tables: vec![],
            enums: vec![],
        };
        fs::write(
            dir.join("001.json"),
            serde_json::to_string_pretty(&snap1).unwrap(),
        )
        .unwrap();

        let snap2 = Snapshot {
            version: 2,
            description: "add notes column".to_string(),
            timestamp: "2026-02-01T00:00:00Z".to_string(),
            tables: vec![],
            enums: vec![],
        };
        fs::write(
            dir.join("002.json"),
            serde_json::to_string_pretty(&snap2).unwrap(),
        )
        .unwrap();
    }

    fn create_migration_dir(tmp: &TempDir) {
        let dir = tmp.path().join("migrations/002");
        fs::create_dir_all(&dir).unwrap();

        let graph = MigrationGraph {
            from_version: 1,
            to_version: 2,
            added: vec![],
            altered: vec!["config.lookup_values".to_string()],
            dropped: vec![],
        };
        fs::write(
            dir.join("graph.json"),
            serde_json::to_string_pretty(&graph).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pad_version_formats() {
        assert_eq!(pad_version(1), "001");
        assert_eq!(pad_version(42), "042");
        assert_eq!(pad_version(100), "100");
    }

    #[test]
    fn checksum_is_deterministic() {
        let a = checksum_of("hello");
        let b = checksum_of("hello");
        assert_eq!(a, b);
        assert_ne!(a, checksum_of("world"));
    }

    #[test]
    fn list_snapshots_finds_all() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        let snapshots = list_snapshots(tmp.path());

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].version, 1);
        assert_eq!(snapshots[0].description, "initial");
        assert_eq!(snapshots[1].version, 2);
        assert_eq!(snapshots[1].description, "add notes column");
    }

    #[test]
    fn list_snapshots_empty_when_no_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(list_snapshots(tmp.path()).is_empty());
    }

    #[test]
    fn read_snapshot_by_version() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        let snap = read_snapshot(1, tmp.path()).unwrap().unwrap();
        assert_eq!(snap.version, 1);
        assert_eq!(snap.description, "initial");
    }

    #[test]
    fn read_snapshot_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        assert!(read_snapshot(99, tmp.path()).unwrap().is_none());
    }

    #[test]
    fn latest_snapshot_returns_highest_version() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        let snap = latest_snapshot(tmp.path()).unwrap().unwrap();
        assert_eq!(snap.version, 2);
    }

    #[test]
    fn next_version_increments() {
        let tmp = TempDir::new().unwrap();
        create_snapshot_dir(&tmp);
        assert_eq!(next_version(tmp.path()), 3);
    }

    #[test]
    fn next_version_starts_at_1() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(next_version(tmp.path()), 1);
    }

    #[test]
    fn has_snapshots_detects_presence() {
        let tmp = TempDir::new().unwrap();
        assert!(!has_snapshots(tmp.path()));
        create_snapshot_dir(&tmp);
        assert!(has_snapshots(tmp.path()));
    }

    #[test]
    fn pending_migrations_filters_by_version() {
        let tmp = TempDir::new().unwrap();
        create_migration_dir(&tmp);

        // DB at version 0 → migration 002 is pending
        let pending = pending_migrations(0, tmp.path());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].to_version, 2);
        assert_eq!(pending[0].altered, vec!["config.lookup_values"]);
        assert!(!pending[0].checksum.is_empty());

        // DB at version 2 → nothing pending
        let pending = pending_migrations(2, tmp.path());
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_migrations_empty_when_no_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(pending_migrations(0, tmp.path()).is_empty());
    }

    #[test]
    fn migration_graph_without_added_field_deserializes_with_empty_vec() {
        let json = r#"{
            "fromVersion": 1,
            "toVersion": 2,
            "altered": ["config.users"],
            "dropped": []
        }"#;
        let graph: MigrationGraph = serde_json::from_str(json).unwrap();
        assert!(graph.added.is_empty());
        assert_eq!(graph.altered, vec!["config.users"]);
        assert_eq!(graph.from_version, 1);
        assert_eq!(graph.to_version, 2);
    }
}
