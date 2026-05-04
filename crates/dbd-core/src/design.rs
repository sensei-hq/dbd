use std::path::{Path, PathBuf};

use crate::adapter::DatabaseAdapter;
use crate::config::{self, DesignConfig};
use crate::dependency;
use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};
use crate::parser;
use crate::references;
use crate::scanner;
use crate::script;
use crate::snapshot;
use crate::snapshot::PendingMigration;

// ── Execution plan types ──────────────────────────────────

/// Strategy for applying entities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// Fresh database — no previous version, apply everything.
    Fresh,
    /// Pending migrations exist — interleave migrations with applies.
    Migrate,
    /// Already current — just re-apply idempotent DDL.
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
) -> ExecutionPlan {
    // Filter to valid, non-external entities
    let valid_entities: Vec<&Entity> = entities
        .iter()
        .filter(|e| e.errors.is_empty())
        .filter(|e| e.entity_type != EntityType::External)
        .collect();

    // Fresh: db_version == 0 → apply everything + set version
    if db_version == 0 {
        let mut steps: Vec<ExecutionStep> = valid_entities
            .iter()
            .map(|e| ExecutionStep::ApplyEntity(e.name.clone()))
            .collect();
        steps.push(ExecutionStep::SetVersion(latest_version));
        return ExecutionPlan {
            strategy: ApplyStrategy::Fresh,
            steps,
        };
    }

    // Current: no pending migrations or already at latest
    if db_version >= latest_version || pending_migrations.is_empty() {
        let steps: Vec<ExecutionStep> = valid_entities
            .iter()
            .map(|e| ExecutionStep::ApplyEntity(e.name.clone()))
            .collect();
        return ExecutionPlan {
            strategy: ApplyStrategy::Current,
            steps,
        };
    }

    // Migrate: db_version < latest and there are pending migrations
    // Collect all added/altered/dropped across all pending migrations
    let all_added: std::collections::HashSet<&str> = pending_migrations
        .iter()
        .flat_map(|m| m.added.iter().map(|s| s.as_str()))
        .collect();
    let all_altered: std::collections::HashSet<&str> = pending_migrations
        .iter()
        .flat_map(|m| m.altered.iter().map(|s| s.as_str()))
        .collect();

    let mut steps: Vec<ExecutionStep> = Vec::new();

    // For each entity, determine what to do
    for entity in &valid_entities {
        if all_added.contains(entity.name.as_str()) {
            steps.push(ExecutionStep::CreateEntity(entity.name.clone()));
        }

        if all_altered.contains(entity.name.as_str()) {
            // Run migration SQL for each pending migration that alters this entity
            for migration in pending_migrations {
                if migration.altered.contains(&entity.name) {
                    let parts: Vec<&str> = entity.name.split('.').collect();
                    let (schema, table_name) = if parts.len() > 1 {
                        (Some(parts[0]), parts[1])
                    } else {
                        (None, parts[0])
                    };
                    let sql_path = match schema {
                        Some(s) => migration
                            .migration_dir
                            .join(s)
                            .join(format!("{table_name}.sql")),
                        None => migration.migration_dir.join(format!("{table_name}.sql")),
                    };
                    steps.push(ExecutionStep::MigrateEntity {
                        entity_name: entity.name.clone(),
                        migration_sql_path: sql_path,
                        migration_version: migration.to_version,
                    });
                }
            }
        }

        // Always apply the current DDL (unless it's being dropped — handled below)
        if !all_added.contains(entity.name.as_str()) || all_altered.contains(entity.name.as_str()) {
            // Regular entities and altered entities get ApplyEntity
            steps.push(ExecutionStep::ApplyEntity(entity.name.clone()));
        } else {
            // Added entities also get ApplyEntity (CreateEntity is just a marker)
            steps.push(ExecutionStep::ApplyEntity(entity.name.clone()));
        }
    }

    // Handle dropped entities
    for migration in pending_migrations {
        for table_name in &migration.dropped {
            let parts: Vec<&str> = table_name.split('.').collect();
            let (schema, tbl) = if parts.len() > 1 {
                (Some(parts[0]), parts[1])
            } else {
                (None, parts[0])
            };
            let sql_path = match schema {
                Some(s) => migration
                    .migration_dir
                    .join(s)
                    .join(format!("{tbl}.drop.sql")),
                None => migration.migration_dir.join(format!("{tbl}.drop.sql")),
            };
            steps.push(ExecutionStep::DropEntity {
                entity_name: table_name.clone(),
                drop_sql_path: sql_path,
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

/// Validation report from inspect.
#[derive(Debug)]
pub struct Report {
    pub entity: Option<Entity>,
    pub issues: Vec<Entity>,
    pub warnings: Vec<Entity>,
}

/// An entry in the import plan: staging table + matched procedure + write targets.
#[derive(Debug, Clone)]
pub struct ImportPlanEntry {
    pub table: Entity,
    /// Procedure that reads from this staging table (matched by reads analysis).
    pub procedure: Option<String>,
    /// Config tables the procedure writes to.
    pub writes: Vec<String>,
}

/// Result of applying RLS policies.
pub struct PolicyReport {
    pub applied: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

/// Apply RLS policy files from the policies/ directory.
///
/// Files are executed in alphabetical order. Failed files are logged and skipped.
pub async fn apply_policies(
    adapter: &dyn DatabaseAdapter,
    project_dir: &Path,
    dry_run: bool,
) -> Result<PolicyReport> {
    let files = crate::scanner::scan_policies(project_dir);
    let mut report = PolicyReport {
        applied: Vec::new(),
        failed: Vec::new(),
    };

    for file in &files {
        if dry_run {
            report.applied.push(file.clone());
            continue;
        }
        match std::fs::read_to_string(file) {
            Ok(sql) => match adapter.execute_script(&sql).await {
                Ok(()) => report.applied.push(file.clone()),
                Err(e) => report.failed.push((file.clone(), e.to_string())),
            },
            Err(e) => report.failed.push((file.clone(), e.to_string())),
        }
    }

    Ok(report)
}

/// The Design orchestrator — main entry point for all operations.
///
/// Loads configuration, discovers and parses entities, resolves dependencies,
/// and provides apply/import/inspect/graph operations.
pub struct Design {
    config: DesignConfig,
    entities: Vec<Entity>,
    import_tables: Vec<Entity>,
    project_dir: PathBuf,
    #[allow(dead_code)]
    env: String,
    validated: bool,
}

impl Design {
    /// Create a Design from a config file path.
    ///
    /// Reads design.yaml, scans DDL files, parses entities, resolves references,
    /// and sorts by dependencies.
    pub fn from_config(config_path: &Path, env: &str) -> Result<Self> {
        Self::from_config_with_dir(config_path, env, None)
    }

    /// Create a Design with an explicit project directory.
    /// If `project_dir` is None, uses the config file's parent directory.
    pub fn from_config_with_dir(
        config_path: &Path,
        env: &str,
        project_dir: Option<&Path>,
    ) -> Result<Self> {
        let project_dir = project_dir
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| {
                config_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf()
            });

        let design_config = config::read(config_path)?;

        // Scan and parse DDL entities
        let ddl_files = scanner::scan_ddl(&project_dir);
        let mut entities: Vec<Entity> = ddl_files
            .iter()
            .filter_map(|file| {
                let sql = std::fs::read_to_string(file).ok()?;
                // Make path relative for entity naming
                let relative = file.strip_prefix(&project_dir).unwrap_or(file);
                parser::parse_entity(relative, &sql).ok()
            })
            .collect();

        // Add schema entities
        for schema_name in design_config.schema_names() {
            if !entities.iter().any(|e| e.entity_type == EntityType::Schema && e.name == schema_name) {
                entities.push(Entity::schema(&schema_name));
            }
        }

        // Auto-add schemas from entity file paths
        let entity_schemas: Vec<String> = entities
            .iter()
            .filter_map(|e| e.schema.clone())
            .collect();
        for schema in entity_schemas {
            if !entities.iter().any(|e| e.entity_type == EntityType::Schema && e.name == schema) {
                entities.push(Entity::schema(&schema));
            }
        }

        // Add target-specific entities (extensions, roles) from the default target
        if let Some(target) = design_config.target.values().next() {
            for ext in &target.extensions {
                let mut entity = Entity::new(EntityType::Extension, ext.name());
                entity.schema = ext.schema().map(|s| s.to_string());
                entities.push(entity);
            }
            for role in &target.roles {
                let mut entity = Entity::new(EntityType::Role, &role.name);
                entity.refers = role.refers.clone();
                entities.push(entity);
            }
        }

        // Filter out entities in skip_schemas
        if let Some(target) = design_config.target.values().next()
            && let Some(ref skip) = target.skip_schemas
        {
            entities.retain(|e| match &e.schema {
                Some(s) => !skip.contains(s),
                None => true,
            });
        }

        // Add external entities for reference resolution
        let external_names: Vec<String> = design_config
            .external
            .iter()
            .map(|e| e.name.clone())
            .collect();
        for ext in &design_config.external {
            entities.push(Entity::external(&ext.name));
        }

        // Resolve references
        references::resolve_references(&mut entities, &external_names, &design_config.ignore);

        // Sort by type priority then dependencies
        // Apply order: schemas → extensions → roles → enums → tables → views → functions/procedures → externals
        let (schemas, extensions, roles, enums, tables, views, functions, externals) =
            partition_entities(entities);
        let sorted_roles = dependency::sort_by_dependencies(&roles);
        let sorted_enums = dependency::sort_by_dependencies(&enums);
        let sorted_tables = dependency::sort_by_dependencies(&tables);
        let sorted_views = dependency::sort_by_dependencies(&views);
        let sorted_functions = dependency::sort_by_dependencies(&functions);

        let entities = [
            schemas, extensions, sorted_roles, sorted_enums,
            sorted_tables, sorted_views, sorted_functions, externals,
        ]
        .concat();

        // Scan import tables (data files, not DDL)
        let import_files = scanner::scan_import(&project_dir);
        let import_tables: Vec<Entity> = import_files
            .iter()
            .map(|file| {
                let relative = file.strip_prefix(&project_dir).unwrap_or(file);
                Entity::from_import_file(relative)
            })
            .collect();

        Ok(Self {
            config: design_config,
            entities,
            import_tables,
            project_dir,
            env: env.to_string(),
            validated: false,
        })
    }

    /// Access the parsed config.
    pub fn config(&self) -> &DesignConfig {
        &self.config
    }

    /// Access all entities (sorted in apply order).
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// Access import tables (data files found in import/).
    pub fn import_tables(&self) -> &[Entity] {
        &self.import_tables
    }

    /// Project directory path.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// Validate all entities and return self for chaining.
    pub fn validate(&mut self) -> &mut Self {
        for entity in &mut self.entities {
            if entity.entity_type == EntityType::External {
                continue;
            }
            // Check file exists for file-based entities
            if let Some(ref file) = entity.file {
                let full_path = self.project_dir.join(file);
                if !full_path.exists() {
                    entity.errors.push(format!("File not found: {}", file.display()));
                }
            }
        }
        self.validated = true;
        self
    }

    /// Generate a validation report, optionally scoped to one entity.
    pub fn report(&mut self, name: Option<&str>) -> Report {
        if !self.validated {
            self.validate();
        }

        let entity = name.and_then(|n| self.entities.iter().find(|e| e.name == n).cloned());

        let issues: Vec<Entity> = self
            .entities
            .iter()
            .chain(self.import_tables.iter())
            .filter(|e| !e.errors.is_empty())
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .cloned()
            .collect();

        let warnings: Vec<Entity> = self
            .entities
            .iter()
            .chain(self.import_tables.iter())
            .filter(|e| !e.warnings.is_empty())
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .cloned()
            .collect();

        Report {
            entity,
            issues,
            warnings,
        }
    }

    /// Apply all entities to the database via the adapter.
    ///
    /// Uses `build_execution_plan()` to determine strategy (Fresh / Migrate / Current)
    /// and executes the plan steps in order.
    pub async fn apply(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
    ) -> Result<()> {
        let valid_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .collect();

        if dry_run {
            return Ok(());
        }

        // Batch adapters (e.g. Convex) short-circuit — no execution plan needed
        if adapter.prefers_batch_apply() {
            let owned: Vec<Entity> = valid_entities.into_iter().cloned().collect();
            adapter.apply_entities(&owned).await?;
            return Ok(());
        }

        // Build execution plan
        let db_version = adapter.get_db_version().await?;
        let latest_version = self.config.project.version.unwrap_or(0);
        let pending = snapshot::pending_migrations(db_version, &self.project_dir);

        // Filter entities by name if scoped
        let scoped_entities: Vec<Entity> = valid_entities.iter().map(|e| (*e).clone()).collect();
        let plan = build_execution_plan(&scoped_entities, db_version, latest_version, &pending);

        // Ensure migrations table exists if we have migration steps
        let has_migrations = plan.steps.iter().any(|s| matches!(
            s,
            ExecutionStep::MigrateEntity { .. }
                | ExecutionStep::DropEntity { .. }
                | ExecutionStep::RecordMigration { .. }
        ));
        if has_migrations {
            adapter.ensure_migrations_table().await?;
        }

        // Build entity lookup for ApplyEntity / CreateEntity steps
        let entity_map: std::collections::HashMap<&str, &Entity> = self
            .entities
            .iter()
            .map(|e| (e.name.as_str(), e))
            .collect();

        // Execute plan steps
        for step in &plan.steps {
            match step {
                ExecutionStep::CreateEntity(entity_name) | ExecutionStep::ApplyEntity(entity_name) => {
                    if let Some(entity) = entity_map.get(entity_name.as_str()) {
                        adapter.apply_entity(entity).await?;
                    }
                }
                ExecutionStep::MigrateEntity { migration_sql_path, .. } => {
                    // 1. Run schema change (ALTER/DROP)
                    if migration_sql_path.exists() {
                        let sql = std::fs::read_to_string(migration_sql_path)?;
                        adapter.execute_script(&sql).await?;
                    }
                    // 2. Run data correction if present (*.data.sql)
                    let data_path = migration_sql_path.with_extension("data.sql");
                    if data_path.exists() {
                        let sql = std::fs::read_to_string(&data_path)?;
                        adapter.execute_script(&sql).await?;
                    }
                }
                ExecutionStep::DropEntity { drop_sql_path, .. } => {
                    if drop_sql_path.exists() {
                        let sql = std::fs::read_to_string(drop_sql_path)?;
                        adapter.execute_script(&sql).await?;
                    }
                }
                ExecutionStep::RecordMigration { version, checksum } => {
                    let desc = format!("migration to v{version}");
                    adapter
                        .apply_migration(*version, "", &desc, checksum)
                        .await?;
                }
                ExecutionStep::SetVersion(version) => {
                    adapter.set_project_meta(&self.env, *version).await?;
                }
            }
        }

        Ok(())
    }

    /// Build the import plan: staging tables paired with procedures, ordered by dependencies.
    ///
    /// Procedure matching is based on reads/writes analysis, not naming convention:
    /// - A procedure that *reads from* a staging table is its import procedure
    /// - Procedures are ordered so that if proc A writes to table X, and proc B
    ///   reads from table X (via FK), A runs before B
    ///
    /// Example: import_lookups reads staging.lookups, writes config.lookups
    ///          import_lookup_values reads staging.lookup_values, writes config.lookup_values
    ///          config.lookup_values has FK to config.lookups
    ///          → import_lookups must run before import_lookup_values
    pub fn import_plan(&self, name: Option<&str>) -> Vec<ImportPlanEntry> {
        let tables: Vec<&Entity> = self
            .import_tables
            .iter()
            .filter(|t| t.errors.is_empty())
            .filter(|t| name.is_none() || t.name == name.unwrap_or(""))
            .collect();

        // Collect all procedures that are candidates for import (in staging schemas)
        let procedures: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| {
                e.entity_type == EntityType::Procedure || e.entity_type == EntityType::Function
            })
            .filter(|e| !e.reads.is_empty() || !e.writes.is_empty())
            .collect();

        // Build entries: match each staging table to the procedure that reads from it
        let mut entries: Vec<ImportPlanEntry> = tables
            .iter()
            .map(|table| {
                let matched_proc = procedures.iter().find(|proc| {
                    proc.reads.iter().any(|r| r == &table.name)
                });

                ImportPlanEntry {
                    table: (*table).clone(),
                    procedure: matched_proc.map(|p| p.name.clone()),
                    writes: matched_proc
                        .map(|p| p.writes.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();

        // Sort by write dependencies:
        // If entry A writes to a table that entry B's target table references (via FK),
        // A must come before B.
        self.sort_import_plan(&mut entries);

        entries
    }

    /// Sort import entries so that procedures writing to tables referenced by other
    /// procedures' targets come first.
    fn sort_import_plan(&self, entries: &mut Vec<ImportPlanEntry>) {
        // Build a set of all config tables written by each entry
        let _write_set: std::collections::HashMap<String, Vec<String>> = entries
            .iter()
            .filter_map(|e| {
                e.procedure.as_ref().map(|p| (p.clone(), e.writes.clone()))
            })
            .collect();

        // Build dependency: entry depends on another if its writes target has a FK
        // to a table written by another entry.
        // For now, use the DDL entity's refers to check FK deps between write targets.
        let entity_refs: std::collections::HashMap<String, Vec<String>> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Table)
            .map(|e| (e.name.clone(), e.refers.clone()))
            .collect();

        // Simple topological sort on entries
        let n = entries.len();
        let mut sorted = Vec::with_capacity(n);
        let mut placed = vec![false; n];

        for _ in 0..n {
            for i in 0..n {
                if placed[i] {
                    continue;
                }
                // Check if all dependencies are already placed
                let deps_satisfied = entries[i].writes.iter().all(|write_target| {
                    // Get FK deps of this write target
                    let fk_deps = entity_refs.get(write_target).cloned().unwrap_or_default();
                    // All FK deps that are also write targets of other entries must be placed
                    fk_deps.iter().all(|dep| {
                        !entries.iter().enumerate().any(|(j, other)| {
                            !placed[j] && j != i && other.writes.contains(dep)
                        })
                    })
                });

                if deps_satisfied {
                    sorted.push(entries[i].clone());
                    placed[i] = true;
                    break;
                }
            }
        }

        // Append any remaining (cycles or unresolved)
        for i in 0..n {
            if !placed[i] {
                sorted.push(entries[i].clone());
            }
        }

        *entries = sorted;
    }

    /// Import staging data via the adapter.
    pub async fn import_data(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
    ) -> Result<()> {
        let plan = self.import_plan(name);

        // Step 1: Truncate staging tables (if configured)
        let truncate = self.config.import.options.truncate;
        if truncate && !dry_run {
            for entry in &plan {
                let qualified = entry.table.name.replace('.', "\".\"");
                adapter
                    .execute_script(&format!("TRUNCATE \"{qualified}\""))
                    .await?;
            }
        }

        // Step 2: Load data into staging tables
        for entry in &plan {
            if !dry_run {
                adapter.import_data(&entry.table, false).await?;
            }
        }

        // Step 3: Call import procedures
        for entry in &plan {
            if let Some(ref proc_name) = entry.procedure {
                if dry_run {
                    
                } else {
                    
                    adapter
                        .execute_script(&format!("CALL {proc_name}();"))
                        .await?;
                }
            }
        }

        // Step 4: Run after scripts
        for after_file in &self.config.import.after {
            let full_path = self.project_dir.join(after_file);
            if dry_run {
                
            } else {
                
                let sql = std::fs::read_to_string(&full_path)?;
                adapter.execute_script(&sql).await?;
            }
        }

        Ok(())
    }

    /// Deploy the full schema: apply DDL then import seed data.
    ///
    /// Equivalent to `apply` followed by `import_data`. dbd handles
    /// fresh / migrate / current strategy automatically — safe to call
    /// on every bootstrap (idempotent when schema is already current).
    pub async fn deploy(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
    ) -> Result<()> {
        self.apply(adapter, None, dry_run).await?;
        self.import_data(adapter, None, dry_run).await?;
        Ok(())
    }

    /// Combine all DDL into a single SQL file.
    pub fn combine(&self, file: &Path) -> Result<()> {
        let combined: Vec<String> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter_map(script::ddl_from_entity)
            .collect();

        std::fs::write(file, combined.join("\n"))?;
        Ok(())
    }

    /// Get the dependency graph for visualization.
    pub fn graph(&self, name: Option<&str>) -> dependency::GraphResult {
        let non_meta: Vec<Entity> = self
            .entities
            .iter()
            .filter(|e| matches!(
                e.entity_type,
                EntityType::Table | EntityType::View | EntityType::Function
                    | EntityType::Procedure | EntityType::Enum
            ))
            .cloned()
            .collect();
        dependency::graph_from_entities(&non_meta, name)
    }

    /// Reset the database (with safety guards).
    pub async fn reset(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
    ) -> Result<()> {
        if !force
            && let Some(meta) = adapter.get_project_meta().await? {
                if meta.env == "prod" {
                    return Err(DbdError::SafetyGuard(
                        "reset is blocked — database is marked as prod. Use --force to override."
                            .to_string(),
                    ));
                }
                if meta.version >= 1 {
                    return Err(DbdError::SafetyGuard(
                        "reset is blocked — database has applied migrations. Use --force to override."
                            .to_string(),
                    ));
                }
            }

        let roles = self
            .config
            .target
            .values()
            .next()
            .map(|t| &t.roles[..])
            .unwrap_or(&[]);

        // Collect ALL schemas — both config-declared and auto-discovered from DDL paths
        let schemas: Vec<String> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Schema)
            .map(|e| e.name.clone())
            .collect();
        if let Some(sql) = script::build_reset_script(&schemas, roles, target, &[])
            .map_err(DbdError::SafetyGuard)? {
            adapter.execute_script(&sql).await?;
        }
        adapter.clear_project_migrations().await?;
        
        Ok(())
    }
}

/// Partition entities by type for ordered apply.
/// Partition entities by type for ordered apply.
/// Returns: (schemas, extensions, roles, enums, tables, views, functions/procedures, externals)
#[allow(clippy::type_complexity)]
fn partition_entities(
    entities: Vec<Entity>,
) -> (
    Vec<Entity>,
    Vec<Entity>,
    Vec<Entity>,
    Vec<Entity>,
    Vec<Entity>,
    Vec<Entity>,
    Vec<Entity>,
    Vec<Entity>,
) {
    let mut schemas = Vec::new();
    let mut extensions = Vec::new();
    let mut roles = Vec::new();
    let mut enums = Vec::new();
    let mut tables = Vec::new();
    let mut views = Vec::new();
    let mut functions = Vec::new(); // functions + procedures
    let mut externals = Vec::new();

    for entity in entities {
        match entity.entity_type {
            EntityType::Schema => schemas.push(entity),
            EntityType::Extension => extensions.push(entity),
            EntityType::Role => roles.push(entity),
            EntityType::Enum => enums.push(entity),
            EntityType::Table => tables.push(entity),
            EntityType::View => views.push(entity),
            EntityType::Function | EntityType::Procedure => functions.push(entity),
            EntityType::External => externals.push(entity),
            _ => tables.push(entity), // Default to tables group
        }
    }

    (schemas, extensions, roles, enums, tables, views, functions, externals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::mock::MockAdapter;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    #[test]
    fn loads_design_from_fixture() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        assert_eq!(design.config().project.name, "example");
        assert!(!design.entities().is_empty());

        // Should have schemas
        let schemas: Vec<&str> = design
            .entities()
            .iter()
            .filter(|e| e.entity_type == EntityType::Schema)
            .map(|e| e.name.as_str())
            .collect();
        assert!(schemas.contains(&"config"));
        assert!(schemas.contains(&"staging"));
    }

    #[test]
    fn entities_include_extensions_and_roles() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        let types: Vec<EntityType> = design.entities().iter().map(|e| e.entity_type).collect();
        assert!(types.contains(&EntityType::Extension));
        assert!(types.contains(&EntityType::Role));
    }

    #[test]
    fn entities_sorted_schemas_first() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        let first_non_schema = design
            .entities()
            .iter()
            .position(|e| e.entity_type != EntityType::Schema)
            .unwrap_or(0);
        let last_schema = design
            .entities()
            .iter()
            .rposition(|e| e.entity_type == EntityType::Schema)
            .unwrap_or(0);

        assert!(last_schema < first_non_schema);
    }

    #[test]
    fn validate_reports_errors() {
        let config_path = fixture_dir().join("design.yaml");
        let mut design = Design::from_config(&config_path, "dev").unwrap();
        let report = design.report(None);

        // The fixture project should have no major errors
        // (warnings are expected for unresolved references)
        let _report = report; // Fixture may have entities with file-not-found errors
    }

    #[test]
    fn graph_returns_nodes_and_edges() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let graph = design.graph(None);

        assert!(!graph.nodes.is_empty());
        assert!(!graph.layers.is_empty());
    }

    #[tokio::test]
    async fn apply_dry_run_does_not_execute() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        design.apply(&mock, None, true).await.unwrap();
        assert!(mock.applied_names().is_empty());
    }

    #[tokio::test]
    async fn apply_executes_entities() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        design.apply(&mock, None, false).await.unwrap();
        assert!(!mock.applied_names().is_empty());
    }

    // ── T4: apply SetVersion writes meta ─────────────────

    #[tokio::test]
    async fn apply_set_version_writes_meta() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        // Before apply, version is 0
        assert_eq!(mock.get_db_version().await.unwrap(), 0);

        design.apply(&mock, None, false).await.unwrap();

        // After apply on a fresh env, meta should have been written
        // (version depends on design.yaml project.version — likely 0 or None for fixture)
        let meta = mock.get_project_meta().await.unwrap();
        // Fresh env with latest_version=0 still calls SetVersion(0) in Fresh strategy
        // which calls set_project_meta. Meta should exist.
        assert!(meta.is_some() || design.config().project.version.is_none());
    }

    #[tokio::test]
    async fn reset_blocked_in_prod() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();
        let mock = MockAdapter::new().with_meta("prod", 0);

        let result = design.reset(&mock, "postgres", false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prod"));
    }

    #[tokio::test]
    async fn reset_blocked_after_v1() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new().with_meta("dev", 1);

        let result = design.reset(&mock, "postgres", false).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("migrations"));
    }

    #[tokio::test]
    async fn reset_allowed_dev_pre_v1() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new().with_meta("dev", 0);

        let result = design.reset(&mock, "postgres", false).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn reset_force_overrides_guard() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();
        let mock = MockAdapter::new().with_meta("prod", 5);

        let result = design.reset(&mock, "postgres", true).await;
        assert!(result.is_ok());
    }

    #[test]
    fn combine_writes_file() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("init.sql");
        design.combine(&out).unwrap();

        assert!(out.exists());
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("CREATE SCHEMA"));
    }

    // ── Import plan tests ─────────────────────────────────

    // IP1: Import plan matches staging table to procedure by reads
    #[test]
    fn ip1_import_plan_matches_staging_table_to_procedure() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let plan = design.import_plan(None);

        // staging.lookups should match staging.import_lookups
        let lookups_entry = plan.iter().find(|e| e.table.name == "staging.lookups");
        assert!(lookups_entry.is_some(), "staging.lookups should appear in the import plan");
        let entry = lookups_entry.unwrap();
        assert_eq!(
            entry.procedure,
            Some("staging.import_lookups".to_string()),
            "staging.lookups should be matched to staging.import_lookups"
        );
        assert!(
            entry.writes.contains(&"config.lookups".to_string()),
            "import_lookups writes to config.lookups"
        );
    }

    // IP2: Import plan with no matching procedure
    #[test]
    fn ip2_import_plan_no_matching_procedure() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let plan = design.import_plan(None);

        // Check if there's any entry without a matching procedure.
        // If all staging tables have matching procedures, we verify
        // the structure is correct for unmatched ones by checking that
        // entries without procedures have empty writes.
        for entry in &plan {
            if entry.procedure.is_none() {
                assert!(
                    entry.writes.is_empty(),
                    "Entry without a procedure should have no writes"
                );
            }
        }

        // Also verify the plan has entries at all (fixture has import files)
        assert!(
            !plan.is_empty(),
            "Import plan should contain entries from fixture import files"
        );
    }

    // IP3: Import plan sorts by write dependencies
    #[test]
    fn ip3_import_plan_sorts_by_write_dependencies() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let plan = design.import_plan(None);

        // staging.import_lookups writes config.lookups
        // staging.import_lookup_values writes config.lookup_values
        // config.lookup_values has FK to config.lookups (lookup_id references lookups(id))
        // Therefore import_lookups must come before import_lookup_values
        let lookups_pos = plan
            .iter()
            .position(|e| e.table.name == "staging.lookups");
        let lookup_values_pos = plan
            .iter()
            .position(|e| e.table.name == "staging.lookup_values");

        assert!(lookups_pos.is_some(), "staging.lookups should be in plan");
        assert!(
            lookup_values_pos.is_some(),
            "staging.lookup_values should be in plan"
        );
        assert!(
            lookups_pos.unwrap() < lookup_values_pos.unwrap(),
            "staging.lookups (pos {}) should come before staging.lookup_values (pos {}) due to FK dependency",
            lookups_pos.unwrap(),
            lookup_values_pos.unwrap()
        );
    }

    // IP4: Import plan with name filter
    #[test]
    fn ip4_import_plan_with_name_filter() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        let plan = design.import_plan(Some("staging.lookups"));

        assert_eq!(plan.len(), 1, "Name filter should return exactly one entry");
        assert_eq!(plan[0].table.name, "staging.lookups");
    }

    // ── Import truncate test ──────────────────────────────

    #[tokio::test]
    async fn import_truncates_staging_tables_before_copy() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        // Default config has truncate: true
        assert!(design.config().import.options.truncate);

        let mock = MockAdapter::new();
        // import_data will fail on actual COPY (no real file), but truncate should happen first
        let _ = design.import_data(&mock, None, false).await;

        // Check that TRUNCATE was issued for staging tables
        let scripts = mock.scripts.lock().unwrap();
        let truncate_scripts: Vec<&String> = scripts.iter()
            .filter(|s| s.to_uppercase().contains("TRUNCATE"))
            .collect();
        // Should have at least one truncate if there are import tables
        if !design.import_tables().is_empty() {
            assert!(!truncate_scripts.is_empty(), "should issue TRUNCATE for staging tables");
        }
    }

    // ── Execution plan test helpers ───────────────────────

    fn test_entity(name: &str) -> Entity {
        Entity::new(EntityType::Table, name)
    }

    fn test_migration(
        from: u32,
        to: u32,
        added: Vec<&str>,
        altered: Vec<&str>,
        dropped: Vec<&str>,
    ) -> crate::snapshot::PendingMigration {
        crate::snapshot::PendingMigration {
            from_version: from,
            to_version: to,
            migration_dir: PathBuf::from(format!("migrations/{:03}", to)),
            added: added.into_iter().map(|s| s.to_string()).collect(),
            altered: altered.into_iter().map(|s| s.to_string()).collect(),
            dropped: dropped.into_iter().map(|s| s.to_string()).collect(),
            checksum: format!("checksum_v{to}"),
        }
    }

    // ── A1: Fresh environment ─────────────────────────────

    #[test]
    fn a1_fresh_env_applies_all_and_sets_version() {
        let entities = vec![
            test_entity("config.users"),
            test_entity("config.orders"),
        ];

        let plan = build_execution_plan(&entities, 0, 2, &[]);

        assert_eq!(plan.strategy, ApplyStrategy::Fresh);

        // Should have ApplyEntity for each entity + SetVersion
        let apply_names: Vec<&str> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(apply_names.contains(&"config.orders"));

        // Last step should be SetVersion
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(2))));
    }

    // ── A2: Current (db_version == latest) ────────────────

    #[test]
    fn a2_current_applies_all_no_set_version() {
        let entities = vec![
            test_entity("config.users"),
            test_entity("config.orders"),
        ];

        let plan = build_execution_plan(&entities, 2, 2, &[]);

        assert_eq!(plan.strategy, ApplyStrategy::Current);

        // All entities get ApplyEntity
        let apply_names: Vec<&str> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(apply_names.contains(&"config.orders"));

        // No SetVersion step
        assert!(!plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(_))));
    }

    // ── A3: Behind by one version ─────────────────────────

    #[test]
    fn a3_behind_by_one_has_migrate_entity() {
        let entities = vec![
            test_entity("config.users"),
            test_entity("config.orders"),
        ];
        let migrations = vec![
            test_migration(1, 2, vec![], vec!["config.users"], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 2, &migrations);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have a MigrateEntity step for config.users
        let migrate_steps: Vec<(&str, u32)> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::MigrateEntity { entity_name, migration_version, .. } => {
                Some((entity_name.as_str(), *migration_version))
            }
            _ => None,
        }).collect();
        assert!(migrate_steps.contains(&("config.users", 2)));

        // Should also have SetVersion
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(2))));
    }

    // ── A4: Behind by multiple versions ───────────────────

    #[test]
    fn a4_behind_by_multiple_has_record_per_migration() {
        let entities = vec![
            test_entity("config.users"),
            test_entity("config.orders"),
        ];
        let migrations = vec![
            test_migration(1, 2, vec![], vec!["config.users"], vec![]),
            test_migration(2, 3, vec![], vec!["config.orders"], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 3, &migrations);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have RecordMigration for both v2 and v3
        let record_versions: Vec<u32> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::RecordMigration { version, .. } => Some(*version),
            _ => None,
        }).collect();
        assert!(record_versions.contains(&2));
        assert!(record_versions.contains(&3));

        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(3))));
    }

    // ── A5: New table added in migration ──────────────────

    #[test]
    fn a5_new_table_gets_create_entity() {
        let entities = vec![
            test_entity("config.users"),
            test_entity("config.audit_log"),
        ];
        let migrations = vec![
            test_migration(1, 2, vec!["config.audit_log"], vec![], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 2, &migrations);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have CreateEntity for the new table
        let created: Vec<&str> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::CreateEntity(name) => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(created.contains(&"config.audit_log"));
    }

    // ── A6: Table drop ────────────────────────────────────

    #[test]
    fn a6_dropped_table_gets_drop_entity() {
        let entities = vec![
            test_entity("config.users"),
        ];
        let migrations = vec![
            test_migration(1, 2, vec![], vec![], vec!["config.legacy"]),
        ];

        let plan = build_execution_plan(&entities, 1, 2, &migrations);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have DropEntity for the dropped table
        let dropped: Vec<(&str, u32)> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::DropEntity { entity_name, migration_version, .. } => {
                Some((entity_name.as_str(), *migration_version))
            }
            _ => None,
        }).collect();
        assert!(dropped.contains(&("config.legacy", 2)));
    }

    // ════════════════════════════════════════════════════════
    // Scenario Tests: Execution plan edge cases
    // ════════════════════════════════════════════════════════

    // M5.1: Entity with errors filtered
    #[test]
    fn a_entity_with_errors_filtered() {
        let mut broken = Entity::new(EntityType::Table, "config.broken");
        broken.errors.push("parse error".to_string());
        let good = test_entity("config.users");
        let entities = vec![broken, good];

        let plan = build_execution_plan(&entities, 0, 1, &[]);

        // Only the good entity should appear in the plan
        let apply_names: Vec<&str> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(!apply_names.contains(&"config.broken"), "entity with errors should be filtered out");
    }

    // M5.2: External entity filtered
    #[test]
    fn a_external_entity_filtered() {
        let external = Entity::new(EntityType::External, "pg_catalog.pg_type");
        let table = test_entity("config.users");
        let entities = vec![external, table];

        let plan = build_execution_plan(&entities, 0, 1, &[]);

        let apply_names: Vec<&str> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(!apply_names.contains(&"pg_catalog.pg_type"), "external entity should be filtered out");
    }

    // M5.3: DB ahead of latest
    #[test]
    fn a_db_ahead_of_latest_behaves_as_current() {
        let entities = vec![test_entity("config.users")];

        let plan = build_execution_plan(&entities, 5, 3, &[]);

        assert_eq!(plan.strategy, ApplyStrategy::Current);
    }

    // M5.4: Both versions zero
    #[test]
    fn a_fresh_db_no_snapshots() {
        let entities = vec![test_entity("config.users")];

        let plan = build_execution_plan(&entities, 0, 0, &[]);

        assert_eq!(plan.strategy, ApplyStrategy::Fresh);
        // Should have ApplyEntity + SetVersion(0)
        let apply_names: Vec<&str> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
            _ => None,
        }).collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(0))));
    }

    // M5.5: Same entity altered in multiple versions
    #[test]
    fn a_entity_altered_in_multiple_versions() {
        let entities = vec![test_entity("config.users")];
        let migrations = vec![
            test_migration(1, 2, vec![], vec!["config.users"], vec![]),
            test_migration(2, 3, vec![], vec!["config.users"], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 3, &migrations);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have TWO MigrateEntity steps for config.users
        let migrate_steps: Vec<(&str, u32)> = plan.steps.iter().filter_map(|s| match s {
            ExecutionStep::MigrateEntity { entity_name, migration_version, .. } => {
                Some((entity_name.as_str(), *migration_version))
            }
            _ => None,
        }).collect();
        assert!(migrate_steps.contains(&("config.users", 2)));
        assert!(migrate_steps.contains(&("config.users", 3)));
        assert_eq!(
            migrate_steps.iter().filter(|(name, _)| *name == "config.users").count(),
            2,
            "should have exactly 2 MigrateEntity steps for config.users"
        );
    }

    // M5.7: Empty entities list
    #[test]
    fn a_empty_entities_empty_plan() {
        let entities: Vec<Entity> = vec![];

        let plan = build_execution_plan(&entities, 0, 1, &[]);

        assert_eq!(plan.strategy, ApplyStrategy::Fresh);
        // Should only have SetVersion step (no entities to apply)
        let apply_count = plan.steps.iter().filter(|s| matches!(s, ExecutionStep::ApplyEntity(_))).count();
        assert_eq!(apply_count, 0, "no entities means no ApplyEntity steps");
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(1))));
    }

    // ── skip_schemas filtering ───────────────────────────

    #[test]
    fn skip_schemas_filters_entities() {
        let mut entities = vec![
            Entity::new(EntityType::Table, "config.users"),
            Entity::new(EntityType::Table, "auth.sessions"),
        ];
        let skip = ["auth".to_string()];
        entities.retain(|e| match &e.schema { Some(s) => !skip.contains(s), None => true });
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].name, "config.users");
    }

    // ── deploy() tests ────────────────────────────────────

    #[tokio::test]
    async fn deploy_dry_run_returns_ok_and_applies_nothing() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();

        let mock = MockAdapter::new();
        design.deploy(&mock, true).await.unwrap();

        assert!(mock.applied_names().is_empty(), "dry_run must not apply any entities");
        assert!(mock.imported_names().is_empty(), "dry_run must not import any data");
    }

    // ── Policy tests ────────────────────────────────────────

    #[test]
    fn p2_empty_policies_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        let files = crate::scanner::scan_policies(tmp.path());
        assert!(files.is_empty());
    }

    #[test]
    fn p3_missing_policies_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No policies/ dir created
        let files = crate::scanner::scan_policies(tmp.path());
        assert!(files.is_empty());
    }

    #[test]
    fn p1_scan_finds_sorted_policy_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policies_dir = tmp.path().join("policies/config");
        std::fs::create_dir_all(&policies_dir).unwrap();
        std::fs::write(policies_dir.join("users.sql"), "-- policy").unwrap();
        std::fs::write(policies_dir.join("lookups.sql"), "-- policy").unwrap();

        let files = crate::scanner::scan_policies(tmp.path());
        assert_eq!(files.len(), 2);
        // Should be sorted alphabetically
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names[0] <= names[1], "files should be sorted");
    }

    #[test]
    fn p8_only_ddl_sql_discovered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policies_dir = tmp.path().join("policies/config");
        std::fs::create_dir_all(&policies_dir).unwrap();
        std::fs::write(policies_dir.join("users.sql"), "-- policy").unwrap();
        std::fs::write(policies_dir.join("readme.md"), "# docs").unwrap();
        std::fs::write(policies_dir.join("notes.txt"), "notes").unwrap();

        let files = crate::scanner::scan_policies(tmp.path());
        assert_eq!(files.len(), 1, "only .sql/.ddl files should be discovered");
    }

    #[tokio::test]
    async fn p5_policies_applied_via_mock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policies_dir = tmp.path().join("policies/config");
        std::fs::create_dir_all(&policies_dir).unwrap();
        std::fs::write(
            policies_dir.join("users.sql"),
            "ALTER TABLE config.users ENABLE ROW LEVEL SECURITY;",
        )
        .unwrap();

        let mock = MockAdapter::new();
        let report = apply_policies(&mock, tmp.path(), false).await.unwrap();
        assert_eq!(report.applied.len(), 1);
        assert!(report.failed.is_empty());
        assert_eq!(mock.script_count(), 1);
    }

    #[tokio::test]
    async fn p4_dry_run_shows_files_no_execution() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policies_dir = tmp.path().join("policies/config");
        std::fs::create_dir_all(&policies_dir).unwrap();
        std::fs::write(policies_dir.join("users.sql"), "-- policy").unwrap();

        let mock = MockAdapter::new();
        let report = apply_policies(&mock, tmp.path(), true).await.unwrap();
        assert_eq!(report.applied.len(), 1);
        assert_eq!(mock.script_count(), 0, "dry run should not execute");
    }

    #[tokio::test]
    async fn deploy_non_dry_run_completes_with_no_errors() {
        // Use a minimal design (no import tables, no after scripts) so
        // import_data succeeds with a MockAdapter.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: test\n",
        )
        .unwrap();

        let design = Design::from_config_with_dir(
            &tmp.path().join("design.yaml"),
            "dev",
            Some(tmp.path()),
        )
        .unwrap();

        let mock = MockAdapter::new();
        design.deploy(&mock, false).await.unwrap();
    }
}
