use std::path::{Path, PathBuf};

use crate::entity::{Entity, EntityType};
use crate::snapshot::PendingMigration;

/// Strategy for applying entities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// Fresh database — no previous version, apply everything.
    Fresh,
    /// Pending migrations exist — run each version whole, in order, then
    /// re-apply the run's entities.
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
    /// Run the `ALTER TYPE … ADD VALUE` statements of one version's migration
    /// files — `(entity, script)` pairs, from
    /// [`crate::script::enum_value_additions`] — ahead of the rest of that
    /// version, then commit the run's transaction and open the next.
    ///
    /// Postgres will not use an enum value inside the transaction that added
    /// it ("unsafe use of new value"), and the rest of the version may: a
    /// default, a CHECK, a data script. So a run that adds enum values is one
    /// transaction up to and including each version's additions, and another
    /// from there. The plan records the version before it first (a
    /// [`ExecutionStep::SetVersion`]), so a later failure leaves a database
    /// whose record says where it is.
    AddEnumValues {
        migration_version: u32,
        scripts: Vec<(String, String)>,
    },
    /// Run an altered entity's `.data.sql`. Its own step, after every schema
    /// change of the same version: a data script may read what another table
    /// gains in that version.
    MigrateData {
        entity_name: String,
        data_sql_path: PathBuf,
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
    RecordMigration { version: u32, checksum: String },
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
///
/// `entities` are the ones the run acts on, already narrowed to its scope —
/// [`crate::Design::apply`] passes what `entities_in_scope` keeps. Every
/// strategy plans exactly those. `scope_names`, the scope's working set, is
/// consulted only for what is no longer in the design: a migration's dropped
/// tables, which belong to the scope that occupies their schema.
///
/// The migrate plan used to filter `entities` by `scope_names` a second time.
/// That set names schemas and tables, and never the roles and extensions every
/// scope keeps, so a scoped migration skipped a new role and a new extension —
/// and failed on the first table using the extension's type.
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
    if !migrates(db_version, latest_version, pending_migrations) {
        return plan_current(&valid_entities);
    }

    // Migrate: db_version < latest and there are pending migrations
    plan_migrate(&valid_entities, pending_migrations, latest_version, scope_names)
}

/// Whether a run from `db_version` migrates: the database records a version,
/// the design is past it, and a migration lies between. A fresh database is
/// built from the DDL instead, and an up-to-date one re-applies it.
pub(crate) fn migrates(db_version: u32, latest_version: u32, pending_migrations: &[PendingMigration]) -> bool {
    db_version != 0 && db_version < latest_version && !pending_migrations.is_empty()
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

/// Migrate forward, one pending version at a time, each run whole before the
/// next starts — as running them one release at a time would:
///
/// 0. the enum values it adds, committed before the rest of it runs (see
///    [`ExecutionStep::AddEnumValues`]);
/// 1. its creates and schema changes (`.sql`), in dependency order;
/// 2. its data scripts (`.data.sql`), in dependency order — after every schema
///    change of the version, since one table's script may read what another
///    table gains in the same version;
/// 3. its drops, of the tables in the run's scope;
/// 4. the record that it ran.
///
/// Then every entity of the run re-applies its DDL, and the latest version is
/// stamped.
///
/// The plan used to run entity by entity: every pending migration of one
/// table, then every one of the next. Jumping several versions ran a later
/// version's change to a table before an earlier version's change to another,
/// so a script reading what the earlier one adds failed on a jump that a
/// stepwise upgrade survived; and a table one version dropped and a later one
/// re-added was created first — a no-op over the old table — then dropped.
///
/// Only tables and enums have migrations. What they may need from the rest —
/// schemas, extensions, roles, and anything a created or altered table refers
/// to — is applied before the step that needs it; everything else re-applies
/// after the last version, on the migrated tables.
fn plan_migrate(
    valid_entities: &[&Entity],
    pending_migrations: &[PendingMigration],
    latest_version: u32,
    scope_names: Option<&std::collections::HashSet<String>>,
) -> ExecutionPlan {
    use std::collections::HashSet;

    let in_run: std::collections::HashMap<&str, &Entity> =
        valid_entities.iter().map(|e| (e.name.as_str(), *e)).collect();
    let mut applied: HashSet<&str> = HashSet::new();
    let mut steps: Vec<ExecutionStep> = Vec::new();

    // The prerequisites of every version: schemas, extensions, roles and
    // sequences that read no table. A version's first new table may be in a
    // new schema or use a new extension's type, and none of these can depend
    // on a version's state.
    for e in valid_entities {
        if !has_migrations(e)
            && e.entity_type.apply_rank() < EntityType::Enum.apply_rank()
            && !reaches_migrated(e, &in_run)
        {
            applied.insert(e.name.as_str());
            steps.push(ExecutionStep::ApplyEntity(e.name.clone()));
        }
    }

    // A dropped table is gone from the design, so it is in no working set: it
    // belongs to a scope that occupies its schema, as it did while it existed.
    // Apply runs the drop only where the table still exists.
    let drop_in_scope = |n: &str| {
        scope_names.is_none_or(|s| s.contains(n))
            || crate::entity::split_qualified_name(n)
                .0
                .is_some_and(|schema| scope_names.is_some_and(|s| s.contains(&schema)))
    };

    let mut previous: Option<u32> = None;
    for migration in pending_migrations {
        let names = |list: &[String], e: &Entity| list.contains(&e.name);

        // Enum values first, committed with everything before them.
        let additions: Vec<(String, String)> = valid_entities
            .iter()
            .filter(|e| names(&migration.altered, e))
            .filter_map(|e| {
                let path = migration_entity_sql_path(&migration.migration_dir, &e.name, ".sql");
                let sql = crate::source_text::read_to_string(&path).ok()?;
                crate::script::enum_value_additions(&sql).map(|script| (e.name.clone(), script))
            })
            .collect();
        if !additions.is_empty() {
            if let Some(version) = previous {
                steps.push(ExecutionStep::SetVersion(version));
            }
            steps.push(ExecutionStep::AddEnumValues {
                migration_version: migration.to_version,
                scripts: additions,
            });
        }

        // Creates and schema changes, in dependency order. What a table refers
        // to and has no migrations of its own — a function its default calls —
        // is applied just ahead of it, once.
        for e in valid_entities {
            let added = names(&migration.added, e);
            let altered = names(&migration.altered, e);
            if !added && !altered {
                continue;
            }
            for prerequisite in prerequisites_of(e, valid_entities, &in_run) {
                if applied.insert(prerequisite) {
                    steps.push(ExecutionStep::ApplyEntity(prerequisite.to_string()));
                }
            }
            if added {
                steps.push(ExecutionStep::CreateEntity(e.name.clone()));
            }
            if altered {
                steps.push(ExecutionStep::MigrateEntity {
                    entity_name: e.name.clone(),
                    migration_sql_path: migration_entity_sql_path(&migration.migration_dir, &e.name, ".sql"),
                    migration_version: migration.to_version,
                });
            }
        }

        // Data scripts, once the version's schema is in place.
        for e in valid_entities.iter().filter(|e| names(&migration.altered, e)) {
            let data_sql_path = migration_entity_sql_path(&migration.migration_dir, &e.name, ".data.sql");
            if data_sql_path.exists() {
                steps.push(ExecutionStep::MigrateData {
                    entity_name: e.name.clone(),
                    data_sql_path,
                    migration_version: migration.to_version,
                });
            }
        }

        // Drops, last: a data script may still read what goes.
        for table_name in migration.dropped.iter().filter(|t| drop_in_scope(t)) {
            // `dbd snapshot` writes a dropped table's statement where it writes
            // every per-table migration — `<schema>/<table>.sql`. A hand-written
            // `<table>.drop.sql` still wins; looking only for that name meant
            // no generated drop ever ran.
            let explicit = migration_entity_sql_path(&migration.migration_dir, table_name, ".drop.sql");
            let drop_sql_path = if explicit.exists() {
                explicit
            } else {
                migration_entity_sql_path(&migration.migration_dir, table_name, ".sql")
            };
            steps.push(ExecutionStep::DropEntity {
                entity_name: table_name.clone(),
                drop_sql_path,
                migration_version: migration.to_version,
            });
        }

        steps.push(ExecutionStep::RecordMigration {
            version: migration.to_version,
            checksum: migration.checksum.clone(),
        });
        previous = Some(migration.to_version);
    }

    // Every entity of the run re-applies its DDL, on the migrated objects. A
    // new one already ran exactly that DDL to be created, unless a pending
    // migration also alters it; one applied ahead of a version already ran.
    let in_any = |pick: fn(&PendingMigration) -> &Vec<String>, name: &str| {
        pending_migrations.iter().any(|m| pick(m).iter().any(|n| n == name))
    };
    for e in valid_entities {
        if applied.contains(e.name.as_str()) {
            continue;
        }
        if !in_any(|m| &m.added, &e.name) || in_any(|m| &m.altered, &e.name) {
            steps.push(ExecutionStep::ApplyEntity(e.name.clone()));
        }
    }

    steps.push(ExecutionStep::SetVersion(latest_version));

    ExecutionPlan {
        strategy: ApplyStrategy::Migrate,
        steps,
    }
}

/// Whether `entity` is of a kind snapshots track, and so has migrations.
fn has_migrations(entity: &Entity) -> bool {
    matches!(entity.entity_type, EntityType::Table | EntityType::Enum)
}

/// Whether `entity` refers — directly, or through entities with no migrations
/// of their own — to one that has migrations. Such an entity reads a state a
/// version changes, so it cannot run ahead of the versions.
fn reaches_migrated(entity: &Entity, in_run: &std::collections::HashMap<&str, &Entity>) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<&Entity> = vec![entity];
    while let Some(e) = stack.pop() {
        for name in e.refers() {
            let Some(target) = in_run.get(name) else { continue };
            if target.name == entity.name || !seen.insert(target.name.as_str()) {
                continue;
            }
            if has_migrations(target) {
                return true;
            }
            stack.push(target);
        }
    }
    false
}

/// The entities without migrations of their own that `entity` refers to,
/// directly or through one another, in `valid_entities` order — dependency
/// order, so each comes after what it needs.
fn prerequisites_of<'a>(
    entity: &Entity,
    valid_entities: &[&'a Entity],
    in_run: &std::collections::HashMap<&str, &'a Entity>,
) -> Vec<&'a str> {
    let mut needed = std::collections::HashSet::new();
    let mut stack: Vec<&Entity> = vec![entity];
    while let Some(e) = stack.pop() {
        for name in e.refers() {
            let Some(target) = in_run.get(name) else { continue };
            if has_migrations(target) || target.name == entity.name || !needed.insert(target.name.as_str()) {
                continue;
            }
            stack.push(target);
        }
    }
    valid_entities
        .iter()
        .filter(|e| needed.contains(e.name.as_str()))
        .map(|e| e.name.as_str())
        .collect()
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
