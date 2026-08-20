use std::path::{Path, PathBuf};

use crate::entity::{Entity, EntityType};
use crate::snapshot::PendingMigration;

/// Strategy for applying entities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// Fresh database — no previous version, apply everything.
    Fresh,
    /// Pending migrations exist — interleave migrations with applies.
    Migrate,
    /// Already current — just re-apply idempotent DDL. Also the default for a
    /// summary describing a run that applied nothing.
    #[default]
    Current,
}

/// A single step in the execution plan.
#[derive(Debug, Clone)]
pub enum ExecutionStep {
    /// Create a brand-new entity (from a migration's `added` list).
    CreateEntity(String),
    /// Run a migration SQL file for an altered entity.
    MigrateEntity {
        entity_name: String,
        migration_sql_path: PathBuf,
        migration_version: u32,
    },
    /// Apply (or re-apply) an entity's current DDL idempotently.
    ApplyEntity(String),
    /// Drop an entity using a migration SQL file.
    DropEntity {
        entity_name: String,
        drop_sql_path: PathBuf,
        migration_version: u32,
    },
    /// Record that a migration version was applied.
    RecordMigration {
        version: u32,
        checksum: String,
    },
    /// Set the project version to the latest.
    SetVersion(u32),
}

/// A complete execution plan with strategy and ordered steps.
#[derive(Debug)]
pub struct ExecutionPlan {
    pub strategy: ApplyStrategy,
    pub steps: Vec<ExecutionStep>,
}

/// Build an execution plan based on entity state and pending migrations.
///
/// Pure logic — no I/O. The caller is responsible for executing the steps.
pub fn build_execution_plan(
    entities: &[Entity],
    db_version: u32,
    latest_version: u32,
    pending_migrations: &[PendingMigration],
    scope_names: Option<&std::collections::HashSet<String>>,
) -> ExecutionPlan {
    // Filter to valid, non-external entities
    let valid_entities: Vec<&Entity> = entities
        .iter()
        .filter(|e| e.errors.is_empty())
        .filter(|e| e.entity_type != EntityType::External)
        .collect();

    // Fresh: db_version == 0 → apply everything + set version
    if db_version == 0 {
        return plan_fresh(&valid_entities, latest_version);
    }

    // Current: no pending migrations or already at latest
    if db_version >= latest_version || pending_migrations.is_empty() {
        return plan_current(&valid_entities);
    }

    // Migrate: db_version < latest and there are pending migrations
    plan_migrate(&valid_entities, pending_migrations, latest_version, scope_names)
}

/// Fresh install: apply every entity's DDL, then stamp the latest version.
fn plan_fresh(valid_entities: &[&Entity], latest_version: u32) -> ExecutionPlan {
    let mut steps: Vec<ExecutionStep> = valid_entities
        .iter()
        .map(|e| ExecutionStep::ApplyEntity(e.name.clone()))
        .collect();
    steps.push(ExecutionStep::SetVersion(latest_version));
    ExecutionPlan {
        strategy: ApplyStrategy::Fresh,
        steps,
    }
}

/// Up to date: (re)apply current DDL for every entity, no version change.
fn plan_current(valid_entities: &[&Entity]) -> ExecutionPlan {
    let steps: Vec<ExecutionStep> = valid_entities
        .iter()
        .map(|e| ExecutionStep::ApplyEntity(e.name.clone()))
        .collect();
    ExecutionPlan {
        strategy: ApplyStrategy::Current,
        steps,
    }
}

/// Migrate forward: create/alter/apply in-scope entities, run drop scripts,
/// record each pending migration, and stamp the latest version.
fn plan_migrate(
    valid_entities: &[&Entity],
    pending_migrations: &[PendingMigration],
    latest_version: u32,
    scope_names: Option<&std::collections::HashSet<String>>,
) -> ExecutionPlan {
    let in_scope = |n: &str| scope_names.is_none_or(|s| s.contains(n));

    // Collect all added/altered across all pending migrations
    let all_added: std::collections::HashSet<&str> = pending_migrations
        .iter()
        .flat_map(|m| m.added.iter().map(|s| s.as_str()))
        .collect();
    let all_altered: std::collections::HashSet<&str> = pending_migrations
        .iter()
        .flat_map(|m| m.altered.iter().map(|s| s.as_str()))
        .collect();

    let mut steps: Vec<ExecutionStep> = Vec::new();

    for entity in valid_entities {
        if !in_scope(entity.name.as_str()) {
            continue;
        }
        if all_added.contains(entity.name.as_str()) {
            steps.push(ExecutionStep::CreateEntity(entity.name.clone()));
        }
        if all_altered.contains(entity.name.as_str()) {
            push_migrate_steps_for(&entity.name, pending_migrations, &mut steps);
        }
        // Every in-scope entity (added, altered, or unchanged) re-applies its DDL.
        steps.push(ExecutionStep::ApplyEntity(entity.name.clone()));
    }

    // Handle dropped entities
    for migration in pending_migrations {
        for table_name in &migration.dropped {
            if !in_scope(table_name) {
                continue;
            }
            steps.push(ExecutionStep::DropEntity {
                entity_name: table_name.clone(),
                drop_sql_path: migration_entity_sql_path(
                    &migration.migration_dir,
                    table_name,
                    ".drop.sql",
                ),
                migration_version: migration.to_version,
            });
        }
    }

    // Record each migration
    for migration in pending_migrations {
        steps.push(ExecutionStep::RecordMigration {
            version: migration.to_version,
            checksum: migration.checksum.clone(),
        });
    }

    // Set version to latest
    steps.push(ExecutionStep::SetVersion(latest_version));

    ExecutionPlan {
        strategy: ApplyStrategy::Migrate,
        steps,
    }
}

/// Push a `MigrateEntity` step for every pending migration that alters `entity_name`.
fn push_migrate_steps_for(
    entity_name: &str,
    pending_migrations: &[PendingMigration],
    steps: &mut Vec<ExecutionStep>,
) {
    for migration in pending_migrations {
        if migration.altered.iter().any(|a| a == entity_name) {
            steps.push(ExecutionStep::MigrateEntity {
                entity_name: entity_name.to_string(),
                migration_sql_path: migration_entity_sql_path(
                    &migration.migration_dir,
                    entity_name,
                    ".sql",
                ),
                migration_version: migration.to_version,
            });
        }
    }
}

/// Path to a per-entity migration SQL file: `<dir>/<schema>/<table><suffix>`
/// (or `<dir>/<table><suffix>` when the entity name is unqualified).
fn migration_entity_sql_path(migration_dir: &Path, entity_name: &str, suffix: &str) -> PathBuf {
    let (schema, table) = crate::entity::split_qualified_name(entity_name);
    match schema {
        Some(s) => migration_dir.join(s).join(format!("{table}{suffix}")),
        None => migration_dir.join(format!("{table}{suffix}")),
    }
}
