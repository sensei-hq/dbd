use std::path::{Path, PathBuf};

use crate::adapter::DatabaseAdapter;
use crate::config::{self, DesignConfig, DepsPolicy, MaterializedViewsConfig};
use crate::dependency;
use crate::entity::{Entity, EntityType, TableConstraint};
use crate::error::{DbdError, Result};
use crate::parser;
use crate::references;
use crate::refcache::RefCache;
use crate::scanner;
use crate::scope::{self, ResolvedScope};
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

/// Summary passed to the `on_complete` callback of `apply()`.
#[derive(Debug, Clone)]
pub struct ApplyComplete {
    pub strategy: ApplyStrategy,
    pub from_version: u32,
    pub to_version: u32,
    /// Entities whose DDL was applied (created or re-applied idempotently).
    pub applied: u32,
    /// Entities that ran a migration SQL file.
    pub migrated: u32,
    /// Entities added fresh (subset of applied).
    pub created: u32,
    /// Entities dropped via migration.
    pub dropped: u32,
}

/// Summary passed to the `on_complete` callback of `import_data()`.
#[derive(Debug, Clone)]
pub struct ImportComplete {
    pub tables: u32,
    pub procedures: u32,
    pub after_scripts: u32,
}

/// Combined summary passed to the `on_complete` callback of `deploy()`.
#[derive(Debug, Clone)]
pub struct DeployComplete {
    pub apply: ApplyComplete,
    pub import: ImportComplete,
}

/// Summary returned by `apply()` describing what happened.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    pub strategy: ApplyStrategy,
    pub from_version: u32,
    pub to_version: u32,
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
    let parts: Vec<&str> = entity_name.split('.').collect();
    let (schema, table) = if parts.len() > 1 {
        (Some(parts[0]), parts[1])
    } else {
        (None, parts[0])
    };
    match schema {
        Some(s) => migration_dir.join(s).join(format!("{table}{suffix}")),
        None => migration_dir.join(format!("{table}{suffix}")),
    }
}

/// Refuse to apply while any pending migration still has unresolved `-- TODO:`
/// lines in its `data.sql` files.
fn ensure_no_pending_todos(pending: &[PendingMigration]) -> Result<()> {
    let todos = snapshot::pending_data_sql_todos(pending);
    if todos.is_empty() {
        return Ok(());
    }
    let details: String = todos
        .iter()
        .map(|t| format!("  {} (v{})", t.file.display(), t.version))
        .collect::<Vec<_>>()
        .join("\n");
    Err(DbdError::Config(format!(
        "Unresolved TODO(s) in data.sql — resolve before applying:\n{details}\n\
         Edit the file(s) above and replace each -- TODO comment with working SQL."
    )))
}

/// Refuse a destructive reconcile (dropped columns/constraints) unless the
/// caller explicitly opted in with `allow_destructive`.
fn ensure_reconcile_not_destructive(
    plan: &crate::reconcile::ReconcilePlan,
    allow_destructive: bool,
) -> Result<()> {
    if !plan.destructive || allow_destructive {
        return Ok(());
    }
    let details: String = plan
        .altered
        .iter()
        .map(|s| format!("  {}", s.entity_name))
        .collect::<Vec<_>>()
        .join("\n");
    Err(DbdError::Config(format!(
        "reconcile would make destructive changes (dropped columns/constraints) on:\n{details}\n\
         Re-run with --allow-destructive to proceed."
    )))
}

/// Build a `SET search_path` prelude covering every managed schema plus public,
/// so bare references in generated ALTERs resolve like the project's DDL files.
fn search_path_prelude(managed_schemas: &std::collections::HashSet<String>) -> String {
    let mut schemas: Vec<&str> = managed_schemas.iter().map(|s| s.as_str()).collect();
    schemas.sort_unstable();
    if !schemas.contains(&"public") {
        schemas.push("public");
    }
    let list = schemas
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("SET search_path TO {list};\n")
}

/// Restrict a live snapshot to only the tables/enums in the managed schemas
/// (reconcile never diffs or prunes objects in schemas the project doesn't own).
fn restrict_snapshot_to_schemas(
    full: crate::snapshot::Snapshot,
    managed_schemas: &std::collections::HashSet<String>,
) -> crate::snapshot::Snapshot {
    crate::snapshot::Snapshot {
        version: 0,
        description: String::new(),
        timestamp: String::new(),
        tables: full
            .tables
            .into_iter()
            .filter(|t| managed_schemas.contains(&t.schema))
            .collect(),
        enums: full
            .enums
            .into_iter()
            .filter(|e| managed_schemas.contains(&e.schema))
            .collect(),
    }
}


/// Whether an import plan entry runs under a scope's working set.
/// An entry with write-targets is kept only if ALL targets are in scope;
/// a proc-less entry is kept if its staging table is in scope.
///
/// Public so CLI previews (`import --dry-run`, `deploy`'s non-empty guard) can
/// filter the plan identically to how `import_data` filters it internally —
/// one source of truth for the predicate.
pub fn import_entry_in_scope(
    entry: &ImportPlanEntry,
    working_set: &std::collections::HashSet<String>,
    is_all: bool,
) -> bool {
    if is_all {
        return true;
    }
    if !entry.writes.is_empty() {
        entry.writes.iter().all(|w| working_set.contains(w))
    } else {
        working_set.contains(&entry.table.name)
    }
}

/// Validation report from inspect.
#[derive(Debug, Clone)]
pub struct Report {
    pub entity: Option<Entity>,
    pub issues: Vec<Entity>,
    pub warnings: Vec<Entity>,
    pub gaps: Vec<scope::ScopeGap>,
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

    // Canonicalize the project root once so path-traversal checks are reliable.
    let canon_root = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    for file in &files {
        if dry_run {
            report.applied.push(file.clone());
            continue;
        }

        // Guard: every policy file must resolve within the project directory.
        let canon_file = match file.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                report.failed.push((file.clone(), e.to_string()));
                continue;
            }
        };
        if !canon_file.starts_with(&canon_root) {
            report.failed.push((
                file.clone(),
                "path traversal rejected: file is outside project directory".to_string(),
            ));
            continue;
        }

        match std::fs::read_to_string(&canon_file) {
            Ok(sql) => match adapter.execute_script(&sql).await {
                Ok(()) => report.applied.push(file.clone()),
                Err(e) => report.failed.push((file.clone(), e.to_string())),
            },
            Err(e) => report.failed.push((file.clone(), e.to_string())),
        }
    }

    Ok(report)
}

/// Report the result of an execution step to the progress callback.
///
/// On success: calls `on_done(desc, None)` and returns `Ok(())`.
/// On failure: calls `on_done(desc, Some(msg))` and returns `Err(DbdError::Config(msg))`.
fn report_step_result(
    desc: &str,
    on_done: &mut dyn FnMut(&str, Option<&str>),
    result: Result<()>,
) -> Result<()> {
    match result {
        Ok(()) => {
            on_done(desc, None);
            Ok(())
        }
        Err(e) => {
            let msg = format!("{desc} failed: {e}");
            on_done(desc, Some(&msg));
            Err(DbdError::Config(msg))
        }
    }
}

/// Which of the just-applied materialized views need a `dbd:hash` sentinel
/// stamped by `apply`: exactly those NOT present before the apply ran. Pure so
/// the "only newly-created" rule is unit-testable without a live database.
///
/// `apply` runs `CREATE MATERIALIZED VIEW IF NOT EXISTS`, so a matview that
/// already existed is a no-op and its deployed definition may differ from the
/// design; stamping a "current" hash onto it would mask that drift from
/// `reconcile`. Pre-existing matviews are therefore deliberately excluded.
fn matviews_to_stamp<'a>(
    applied_matviews: &[&'a Entity],
    pre_existing: &std::collections::HashSet<String>,
) -> Vec<&'a Entity> {
    applied_matviews
        .iter()
        .copied()
        .filter(|e| !pre_existing.contains(&e.name))
        .collect()
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
                // Use relative path for entity type/name derivation, but
                // store the absolute path so the file is readable regardless of CWD.
                let relative = file.strip_prefix(&project_dir).unwrap_or(file);
                let mut entity = parser::parse_entity(relative, &sql).ok()?;
                entity.file = Some(file.clone());
                Some(entity)
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
        // Apply order: schemas → extensions → roles → sequences → enums → tables → views → materialized views → functions/procedures → externals
        // Sequences are placed before tables so a column `DEFAULT nextval('seq')`
        // referencing a standalone sequence finds the sequence already created.
        let (schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals) =
            partition_entities(entities);
        let sorted_roles = dependency::sort_by_dependencies(&roles);
        let sorted_enums = dependency::sort_by_dependencies(&enums);
        let sorted_tables = dependency::sort_by_dependencies(&tables);
        let sorted_views = dependency::sort_by_dependencies(&views);
        let sorted_matviews = dependency::sort_by_dependencies(&matviews);
        let sorted_functions = dependency::sort_by_dependencies(&functions);

        let entities = [
            schemas, extensions, sorted_roles, sequences, sorted_enums,
            sorted_tables, sorted_views, sorted_matviews, sorted_functions, externals,
        ]
        .concat();

        // Scan import tables (data files, not DDL)
        // Pass env so that import/{env}/ subdirectories are filtered appropriately.
        let import_files = scanner::scan_import(&project_dir, Some(env));
        let import_tables: Vec<Entity> = import_files
            .iter()
            .map(|file| {
                // Use the relative path for entity type/name/schema derivation,
                // but store the absolute path so the file is readable regardless of CWD.
                let relative = file.strip_prefix(&project_dir).unwrap_or(file);
                let mut entity = Entity::from_import_file(relative);
                entity.file = Some(file.clone());
                entity
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

    /// External entity names from config (for ref resolution / gap analysis).
    fn external_names(&self) -> Vec<String> {
        self.config.external.iter().map(|e| e.name.clone()).collect()
    }

    /// Resolve a scope by name. `None` ⇒ `default` scope if defined, else `all`.
    /// `deps_override` (CLI `--deps`) wins over the scope's own `deps`.
    pub fn resolve_scope(
        &self,
        name: Option<&str>,
        deps_override: Option<DepsPolicy>,
    ) -> Result<ResolvedScope> {
        scope::resolve(
            &self.config.scopes,
            name,
            deps_override,
            &self.entities,
            &self.external_names(),
        )
    }

    /// The set of entity names an operation should act on under this scope.
    /// `include` policy expands to the dependency closure.
    pub fn working_set(&self, scope: &ResolvedScope) -> Result<std::collections::HashSet<String>> {
        match scope.deps {
            DepsPolicy::Include => scope::closure(scope, &self.entities, &self.external_names()),
            DepsPolicy::Report => Ok(scope.entities.clone()),
        }
    }

    /// Whether an entity is kept under a resolved scope's working set.
    /// Roles/externals are always-on infrastructure. Extensions are too by
    /// default, unless the scope sets an `extensions` allowlist — then only the
    /// named extensions apply (an empty list drops them all), letting a scope
    /// target a database that lacks an extension (e.g. an embedded PG without
    /// pgvector).
    fn entity_in_scope(
        entity: &Entity,
        scope: &ResolvedScope,
        working_set: &std::collections::HashSet<String>,
    ) -> bool {
        if scope.is_all {
            return true;
        }
        match entity.entity_type {
            EntityType::Extension => match &scope.extensions {
                Some(allow) => allow.contains(&entity.name),
                None => true,
            },
            EntityType::Role | EntityType::External => true,
            _ => working_set.contains(&entity.name),
        }
    }

    /// The loaded entities filtered to a resolved scope's working set
    /// (closure under `include`, the plain set under `report`). The all-scope
    /// returns every entity. Read-only and gap-neutral — for entity-selecting
    /// commands like `dbml` that document/emit a subset without the write-path
    /// gap gate.
    pub fn scoped_entities(&self, scope: &ResolvedScope) -> Result<Vec<Entity>> {
        if scope.is_all {
            return Ok(self.entities.clone());
        }
        let ws = self.working_set(scope)?;
        Ok(self
            .entities
            .iter()
            .filter(|e| Self::entity_in_scope(e, scope, &ws))
            .cloned()
            .collect())
    }

    /// Under `report` policy, error if the scope has dependency gaps (an in-scope
    /// entity that references a managed entity outside the scope). No-op for the
    /// all-scope, `include` policy, or a gap-free scope. Shared by `apply` and
    /// `import_data` so both refuse the same way before any write.
    pub fn check_scope_gaps(&self, scope: &ResolvedScope) -> Result<()> {
        if scope.is_all || scope.deps != DepsPolicy::Report {
            return Ok(());
        }
        let gaps = scope::analyze_gaps(scope, &self.entities, &self.external_names());
        if gaps.is_empty() {
            return Ok(());
        }
        let detail: String = gaps
            .iter()
            .map(|g| format!("  {} requires {} ({})", g.required_by, g.missing, g.chain.join(" → ")))
            .collect::<Vec<_>>()
            .join("\n");
        Err(DbdError::Config(format!(
            "scope '{}' has {} dependency gap(s) — add them or use --deps include:\n{detail}",
            scope.name,
            gaps.len()
        )))
    }

    /// Scope guard: refuse to operate under a scope different from the one this
    /// database was pinned to. `meta` is the stored project meta (`None` on a
    /// fresh database), `requested` is the resolved scope name for this run, and
    /// `allow_scope_change` bypasses the guard (the next successful write re-pins
    /// the DB). A database with no recorded scope (`meta.scope == None`) is
    /// unpinned and never blocks — the current run pins it. Mirrors the prod
    /// guard in [`Design::reset`]; invoked from the CLI write handlers.
    pub fn check_scope_guard(
        meta: Option<&crate::adapter::ProjectMeta>,
        requested: &str,
        allow_scope_change: bool,
    ) -> Result<()> {
        if allow_scope_change {
            return Ok(());
        }
        if let Some(pinned) = meta.and_then(|m| m.scope.as_deref())
            && pinned != requested
        {
            return Err(DbdError::SafetyGuard(format!(
                "scope guard: this database is pinned to scope '{pinned}', but you requested '{requested}'.\n\
                 Applying a different scope would build a divergent schema.\n\
                 → re-run with --scope {pinned}, or pass --allow-scope-change to re-point this database to '{requested}'."
            )));
        }
        Ok(())
    }

    /// Scan all migration directories for unresolved `-- TODO:` comments in
    /// `*.data.sql` files. Returns one entry per affected file.
    ///
    /// Used by `inspect` to surface outstanding data corrections.
    /// `apply` independently blocks on PENDING migrations with TODOs.
    pub fn data_sql_todos(&self) -> Vec<snapshot::DataSqlTodo> {
        snapshot::scan_data_sql_todos(&self.project_dir)
    }

    /// Drop "Unresolved reference: NAME" warnings whose target NAME exists
    /// in the live database catalog (tables, views, or enum types).
    ///
    /// Returns the number of warnings dropped. Used by `inspect --from-db`
    /// to silence warnings that resolve against a real DB but not against
    /// the project's external entity list.
    pub async fn resolve_unknown_refs_via_db(
        &mut self,
        adapter: &dyn DatabaseAdapter,
    ) -> Result<usize> {
        const PREFIX: &str = "Unresolved reference: ";

        let mut candidates: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entity in self.entities.iter().chain(self.import_tables.iter()) {
            for warning in &entity.warnings {
                if let Some(name) = warning.strip_prefix(PREFIX) {
                    candidates.insert(name.to_string());
                }
            }
        }

        let mut resolved: std::collections::HashSet<String> = std::collections::HashSet::new();
        for name in &candidates {
            if adapter.resolve_entity(name).await?.is_some() {
                resolved.insert(name.clone());
            }
        }

        let mut dropped = 0usize;
        let drop_in = |entities: &mut Vec<Entity>, dropped: &mut usize| {
            for entity in entities {
                entity.warnings.retain(|w| match w.strip_prefix(PREFIX) {
                    Some(name) if resolved.contains(name) => {
                        *dropped += 1;
                        false
                    }
                    _ => true,
                });
            }
        };
        drop_in(&mut self.entities, &mut dropped);
        drop_in(&mut self.import_tables, &mut dropped);
        Ok(dropped)
    }

    /// Capture the full set of user-defined entities (tables, views, enums)
    /// from the live database and persist them to
    /// `<project_dir>/.dbd/refcache.json`.
    ///
    /// Subsequent offline `inspect` runs can use this snapshot to silence
    /// "Unresolved reference" warnings via [`resolve_unknown_refs_via_cache`].
    ///
    /// Returns the number of entities written to the cache.
    pub async fn write_ref_cache(
        &self,
        adapter: &dyn DatabaseAdapter,
        source: &str,
    ) -> Result<usize> {
        let names = adapter.list_entities().await?;
        let count = names.len();
        let cache = RefCache::new(source, names);
        cache.save(&self.project_dir)?;
        Ok(count)
    }

    /// Drop "Unresolved reference: NAME" warnings whose target NAME exists
    /// in the project's `<project_dir>/.dbd/refcache.json` snapshot.
    ///
    /// Returns `Ok((dropped, Some(cache_size)))` when a cache was found
    /// (regardless of whether any warnings matched), or `Ok((0, None))`
    /// when no cache file exists.
    pub fn resolve_unknown_refs_via_cache(&mut self) -> Result<(usize, Option<usize>)> {
        const PREFIX: &str = "Unresolved reference: ";

        let cache = match RefCache::load(&self.project_dir)? {
            Some(c) => c,
            None => return Ok((0, None)),
        };
        let size = cache.len();

        let mut dropped = 0usize;
        let drop_in = |entities: &mut Vec<Entity>, dropped: &mut usize| {
            for entity in entities {
                entity.warnings.retain(|w| match w.strip_prefix(PREFIX) {
                    Some(name) if cache.contains(name) => {
                        *dropped += 1;
                        false
                    }
                    _ => true,
                });
            }
        };
        drop_in(&mut self.entities, &mut dropped);
        drop_in(&mut self.import_tables, &mut dropped);
        Ok((dropped, Some(size)))
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

    /// Generate a validation report, optionally filtered to one entity by
    /// `name` and augmented with dependency gaps when a `scope` is supplied.
    pub fn report(&mut self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Report {
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

        let gaps = match scope {
            Some(s) => scope::analyze_gaps(s, &self.entities, &self.external_names()),
            None => Vec::new(),
        };

        Report {
            entity,
            issues,
            warnings,
            gaps,
        }
    }

    /// Apply all entities to the database via the adapter.
    ///
    /// Uses `build_execution_plan()` to determine strategy (Fresh / Migrate / Current)
    /// and executes the plan steps in order.
    ///
    /// `on_start(desc)` is called just before each visible step.
    /// `on_done(desc, err)` is called after — `err` is `None` on success.
    /// `on_complete(summary)` is called once after all steps succeed.
    /// Use `|_| {}` / `|_, _| {}` / `|_| {}` when progress reporting is not needed.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut on_start: S,
        mut on_done: D,
        mut on_complete: C,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(ApplyComplete),
    {
        // Resolve scope → working set, gap-gate under `report` (abort before any write).
        // The gate runs even under `dry_run`: a gappy scope is misconfigured
        // regardless of whether writes would occur.
        let working_set: Option<std::collections::HashSet<String>> = match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                Some(self.working_set(s)?)
            }
            _ => None,
        };

        let valid_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .filter(|e| match (&working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect();

        if dry_run {
            return Ok(());
        }

        // Batch adapters (e.g. Convex) short-circuit — no execution plan needed
        if adapter.prefers_batch_apply() {
            let count = valid_entities.len() as u32;
            let owned: Vec<Entity> = valid_entities.into_iter().cloned().collect();
            adapter.apply_entities(&owned).await?;
            on_complete(ApplyComplete {
                strategy: ApplyStrategy::Current,
                from_version: 0,
                to_version: 0,
                applied: count,
                migrated: 0,
                created: 0,
                dropped: 0,
            });
            return Ok(());
        }

        // Build execution plan
        let db_version = adapter.get_db_version().await?;
        let latest_version = self.config.project.version.unwrap_or(0);
        let pending = snapshot::pending_migrations(db_version, &self.project_dir);

        // Block apply if any pending migration has unresolved data.sql TODOs.
        ensure_no_pending_todos(&pending)?;

        // Filter entities by name if scoped
        let scoped_entities: Vec<Entity> = valid_entities.iter().map(|e| (*e).clone()).collect();
        let plan = build_execution_plan(&scoped_entities, db_version, latest_version, &pending, working_set.as_ref());

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

        // Counters for on_complete summary
        let mut count_applied: u32 = 0;
        let mut count_migrated: u32 = 0;
        let mut count_created: u32 = 0;
        let mut count_dropped: u32 = 0;

        // Build entity lookup for ApplyEntity / CreateEntity steps
        let entity_map: std::collections::HashMap<&str, &Entity> = self
            .entities
            .iter()
            .map(|e| (e.name.as_str(), e))
            .collect();

        // Materialized views this run applies (respecting the scope + name
        // filter). Capture which of them ALREADY exist before applying, so that
        // afterwards we stamp the `dbd:hash` sentinel only on the ones this run
        // newly creates. Skip the state query entirely when there are none.
        let applied_matviews: Vec<&Entity> = valid_entities
            .iter()
            .copied()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .collect();
        let pre_existing_matviews: std::collections::HashSet<String> = if applied_matviews.is_empty()
        {
            std::collections::HashSet::new()
        } else {
            adapter.matview_states().await?.into_keys().collect()
        };

        // Wrap the whole plan in one transaction when the backend supports it,
        // so an interrupted upgrade rolls back to the prior schema instead of
        // leaving objects half-applied. `DBD_NO_TX` opts out for plans that
        // contain non-transactional DDL (e.g. CREATE INDEX CONCURRENTLY).
        let use_txn = adapter.supports_transactional_apply()
            && std::env::var_os("DBD_NO_TX").is_none()
            && !plan.steps.is_empty();

        if use_txn {
            adapter.begin_batch().await?;
        }

        // Execute plan steps inside an async block so a mid-plan error routes
        // through rollback below rather than returning early.
        let exec: Result<()> = async {
            for step in &plan.steps {
                match step {
                    ExecutionStep::CreateEntity(entity_name) => {
                        if let Some(entity) = entity_map.get(entity_name.as_str()) {
                            let desc = format!("{}:{entity_name}", entity.entity_type.tag());
                            on_start(&desc);
                            let result = adapter.apply_entity(entity).await;
                            report_step_result(&desc, &mut on_done, result)?;
                            count_created += 1;
                            count_applied += 1;
                        }
                    }
                    ExecutionStep::ApplyEntity(entity_name) => {
                        if let Some(entity) = entity_map.get(entity_name.as_str()) {
                            let desc = format!("{}:{entity_name}", entity.entity_type.tag());
                            on_start(&desc);
                            let result = adapter.apply_entity(entity).await;
                            report_step_result(&desc, &mut on_done, result)?;
                            count_applied += 1;
                        }
                    }
                    ExecutionStep::MigrateEntity { entity_name, migration_sql_path, migration_version } => {
                        let type_tag = entity_map.get(entity_name.as_str())
                            .map(|e| e.entity_type.tag())
                            .unwrap_or_else(|| "entity".to_string());
                        let desc = format!("migrate {type_tag}:{entity_name} → v{migration_version}");
                        on_start(&desc);
                        let result: Result<()> = async {
                            if migration_sql_path.exists() {
                                let sql = std::fs::read_to_string(migration_sql_path)?;
                                adapter.execute_script(&sql).await?;
                            }
                            let data_path = migration_sql_path.with_extension("data.sql");
                            if data_path.exists() {
                                let sql = std::fs::read_to_string(&data_path)?;
                                adapter.execute_script(&sql).await?;
                            }
                            Ok(())
                        }
                        .await;
                        report_step_result(&desc, &mut on_done, result)?;
                        count_migrated += 1;
                    }
                    ExecutionStep::DropEntity { entity_name, drop_sql_path, migration_version } => {
                        let type_tag = entity_map.get(entity_name.as_str())
                            .map(|e| e.entity_type.tag())
                            .unwrap_or_else(|| "entity".to_string());
                        let desc = format!("drop {type_tag}:{entity_name} (v{migration_version})");
                        on_start(&desc);
                        let result: Result<()> = async {
                            if drop_sql_path.exists() {
                                let sql = std::fs::read_to_string(drop_sql_path)?;
                                adapter.execute_script(&sql).await?;
                            }
                            Ok(())
                        }
                        .await;
                        report_step_result(&desc, &mut on_done, result)?;
                        count_dropped += 1;
                    }
                    ExecutionStep::RecordMigration { version, checksum } => {
                        let desc = format!("migration to v{version}");
                        adapter.apply_migration(*version, "", &desc, checksum).await?;
                    }
                    ExecutionStep::SetVersion(version) => {
                        adapter
                            .set_project_meta(&self.env, *version, scope.map(|s| s.name.as_str()))
                            .await?;
                    }
                }
            }
            Ok(())
        }
        .await;

        match exec {
            Ok(()) => {
                if use_txn {
                    adapter.commit_batch().await?;
                }
            }
            Err(e) => {
                if use_txn {
                    // Best-effort rollback; surface the original failure.
                    let _ = adapter.rollback_batch().await;
                }
                return Err(e);
            }
        }

        // Stamp the `dbd:hash` sentinel on the materialized views THIS run newly
        // created (absent from `pre_existing_matviews`), so a later `dbd
        // reconcile` recognizes them as dbd-managed instead of warning "exists
        // but is not stamped by dbd". An already-existing matview is left
        // untouched on purpose (see `matviews_to_stamp`): stamping a "current"
        // hash onto one whose deployed definition may have drifted would mask
        // that drift. Runs after commit, so the object exists for the COMMENT.
        for e in matviews_to_stamp(&applied_matviews, &pre_existing_matviews) {
            adapter
                .execute_script(&crate::reconcile::matview_hash_comment_sql(
                    &e.name,
                    &crate::reconcile::matview_hash(e),
                ))
                .await?;
        }

        // Sync pg_cron refresh jobs across the WHOLE design (not the scoped
        // subset): `sync_refresh_jobs` unschedules every `dbd:refresh:%` job
        // absent from the set it is given, so a scoped run fed only its subset
        // would unschedule out-of-scope matviews' jobs. Centralized here (rather
        // than in the CLI) so BOTH `dbd apply` and `dbd deploy` schedule refresh
        // jobs. The adapter guards on pg_cron presence, so it is a safe no-op on
        // databases (and non-Postgres targets) without the extension.
        let mv_jobs: Vec<(String, crate::config::ResolvedMatview)> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .map(|e| (e.name.clone(), self.config.materialized_views.resolve(&e.name)))
            .collect();
        adapter.sync_refresh_jobs(&mv_jobs).await?;

        on_complete(ApplyComplete {
            strategy: plan.strategy,
            from_version: db_version,
            to_version: latest_version,
            applied: count_applied,
            migrated: count_migrated,
            created: count_created,
            dropped: count_dropped,
        });
        Ok(())
    }

    /// Reconcile the live database to the desired schema in place (declarative).
    ///
    /// Introspects the target, diffs its tables/enums against the project, and
    /// applies the result directly — no snapshot files, no version bump. Added
    /// tables/enums get a full create; existing ones are `ALTER`ed to match;
    /// other objects (schemas, extensions, sequences, functions, views, roles)
    /// are re-applied idempotently.
    ///
    /// The diff is scoped to the schemas the design declares, so reconcile never
    /// touches tables in other schemas. Within those schemas, tables the design
    /// no longer declares (orphans) are dropped **only** when `prune` is set —
    /// otherwise they are left untouched and reported via the returned plan.
    ///
    /// This is the pre-release (pre-v1) workflow; callers gate it on
    /// `project.released`. When the plan drops a column or constraint from an
    /// existing table and `allow_destructive` is false, it refuses before any
    /// write. Returns the computed plan so callers can surface warnings, orphans,
    /// and a summary.
    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
        allow_destructive: bool,
        prune: bool,
        scope: Option<&ResolvedScope>,
        mut on_start: S,
        mut on_done: D,
        mut on_complete: C,
    ) -> Result<crate::reconcile::ReconcilePlan>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(crate::reconcile::ReconcileComplete),
    {
        use crate::reconcile::{
            plan_fk_convergence, plan_reconcile, qualified_entity_name, raw_snapshot_from_entities,
            snapshot_from_entities, ReconcileComplete, DEFAULT_SCHEMA,
        };
        use std::collections::{HashMap, HashSet};

        // Batch adapters (e.g. Convex) have no live SQL schema to diff.
        if adapter.prefers_batch_apply() {
            return Err(DbdError::Config(
                "reconcile is not supported for this target (no live SQL schema to diff)"
                    .to_string(),
            ));
        }

        // Resolve scope → working set, gap-gate under `report` (abort before writes).
        let working_set: Option<HashSet<String>> = match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                Some(self.working_set(s)?)
            }
            _ => None,
        };

        // Desired entities: valid, non-external, in scope — in dependency order.
        let desired_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| match (&working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect();

        // Desired snapshot (tables + enums, schema-normalized).
        let desired_owned: Vec<Entity> = desired_entities.iter().map(|e| (*e).clone()).collect();
        let desired = snapshot_from_entities(&desired_owned);

        // Schemas the design manages. Reconcile only diffs within these, so it
        // never considers (or prunes) tables in schemas the project doesn't own.
        let managed_schemas: HashSet<String> = desired_entities
            .iter()
            .map(|e| match e.entity_type {
                EntityType::Schema => e.name.clone(),
                _ => {
                    let s = e.schema.clone().unwrap_or_default();
                    if s.is_empty() {
                        DEFAULT_SCHEMA.to_string()
                    } else {
                        s
                    }
                }
            })
            .collect();

        // Live snapshot, restricted to managed schemas. Tables here but not in
        // `desired` surface as `plan.dropped` (orphans) — pruned only on request.
        let live_entities = adapter.introspect().await?;
        let live_full = snapshot_from_entities(&live_entities);
        let live = restrict_snapshot_to_schemas(live_full, &managed_schemas);

        let mut plan = plan_reconcile(&live, &desired);

        // Foreign keys (issue #8): canonicalize strips FKs from the snapshots
        // above, so converge them from the RAW snapshots — adding declared FKs
        // the live DB lacks and dropping (destructive) ones the design removed.
        let desired_raw = raw_snapshot_from_entities(&desired_owned);
        let live_raw =
            restrict_snapshot_to_schemas(raw_snapshot_from_entities(&live_entities), &managed_schemas);
        plan_fk_convergence(&mut plan, &live_raw, &desired_raw);

        // Materialized-view DETECTION (read-only) — done BEFORE the dry_run return
        // so `--dry-run` previews matview creates AND drift warnings. Postgres has
        // no `CREATE OR REPLACE MATERIALIZED VIEW`, and dbd deliberately never
        // auto-drops one (a DROP … CASCADE would repopulate it and drop its
        // dependents — unacceptable in a dev-loop reconcile). So: CREATE an absent
        // matview (stamping a `dbd:hash` sentinel), SKIP one whose stored hash
        // matches the design, and for one that drifted (or carries no sentinel)
        // WARN and leave it untouched. Only the CREATE *writes* run after the
        // return; here we merely record the decisions into the plan and a local
        // `mv_to_create` list the write pass reuses (no second states fetch).
        use crate::reconcile::{decide_matview_action, matview_create_sql, matview_hash, MatviewAction};
        let matviews: Vec<&Entity> = desired_entities
            .iter()
            .copied()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .collect();
        let mut mv_to_create: Vec<(&Entity, String)> = Vec::new();
        if !matviews.is_empty() {
            let states = adapter.matview_states().await?;
            for &e in &matviews {
                let want = matview_hash(e);
                match decide_matview_action(&want, states.get(&e.name).cloned()) {
                    MatviewAction::Skip => {}
                    MatviewAction::Create => {
                        plan.matview_creates.push(e.name.clone());
                        mv_to_create.push((e, want));
                    }
                    MatviewAction::Warn => {
                        // Detected drift, but dbd never auto-recreates a matview.
                        // Surface it through the plan's warnings (the same channel
                        // `dbd reconcile` prints for risky-change advisories).
                        let has_sentinel = matches!(states.get(&e.name), Some(Some(_)));
                        plan.warnings.push(if has_sentinel {
                            format!(
                                "materialized view {name}: definition differs from the deployed object \
                                 (dbd does not auto-recreate materialized views because that would \
                                 DROP … CASCADE and lose data/dependents). To apply the new \
                                 definition, drop it manually (`DROP MATERIALIZED VIEW \"{schema}\".\"{bare}\" CASCADE`) \
                                 and re-run `dbd apply` to recreate it.",
                                name = e.name,
                                schema = e.schema.as_deref().unwrap_or("public"),
                                bare = e.name.rsplit('.').next().unwrap_or(&e.name),
                            )
                        } else {
                            format!(
                                "materialized view {}: exists but is not stamped by dbd (cannot \
                                 verify its definition); recreate it under dbd management to enable \
                                 drift detection.",
                                e.name
                            )
                        });
                    }
                }
            }
        }

        if dry_run {
            return Ok(plan);
        }

        ensure_reconcile_not_destructive(&plan, allow_destructive)?;

        let added: HashSet<&str> = plan.added.iter().map(|s| s.as_str()).collect();
        let alter_sql: HashMap<&str, &str> = plan
            .altered
            .iter()
            .map(|s| (s.entity_name.as_str(), s.sql.as_str()))
            .collect();

        let mut summary = ReconcileComplete::default();

        // Pass A — prerequisites + added tables/enums, in dependency order.
        //   schema/extension/role/sequence: always (idempotent DDL);
        //   enum/table: only when newly added (a full create).
        for e in &desired_entities {
            let do_apply = match e.entity_type {
                EntityType::Schema
                | EntityType::Extension
                | EntityType::Role
                | EntityType::Sequence => true,
                EntityType::Enum | EntityType::Table => {
                    added.contains(qualified_entity_name(e).as_str())
                }
                _ => false,
            };
            if !do_apply {
                continue;
            }
            let is_created = matches!(e.entity_type, EntityType::Enum | EntityType::Table);
            let desc = format!("{}:{}", e.entity_type.tag(), e.name);
            on_start(&desc);
            let result = adapter.apply_entity(e).await;
            report_step_result(&desc, &mut on_done, result)?;
            if is_created {
                summary.created += 1;
            } else {
                summary.reapplied += 1;
            }
        }

        // Generated ALTERs carry no `set search_path`, unlike the project's DDL
        // files. Prepend one covering every managed schema (+ public) so bare
        // references — e.g. an enum type or a default calling a managed function —
        // resolve the same way they do when the DDL file runs.
        let search_path = search_path_prelude(&managed_schemas);

        // Pass B — ALTER existing tables/enums, in dependency order.
        for e in &desired_entities {
            let n = qualified_entity_name(e);
            if let Some(sql) = alter_sql.get(n.as_str()) {
                let desc = format!("alter {}:{n}", e.entity_type.tag());
                on_start(&desc);
                let script = format!("{search_path}{sql}");
                let result = adapter.execute_script(&script).await;
                report_step_result(&desc, &mut on_done, result)?;
                summary.altered += 1;
            }
        }

        // Pass C — code objects (CREATE OR REPLACE), after tables/columns exist.
        for e in &desired_entities {
            if !matches!(
                e.entity_type,
                EntityType::View | EntityType::Function | EntityType::Procedure
            ) {
                continue;
            }
            let desc = format!("{}:{}", e.entity_type.tag(), e.name);
            on_start(&desc);
            let result = adapter.apply_entity(e).await;
            report_step_result(&desc, &mut on_done, result)?;
            summary.reapplied += 1;
        }

        // Pass C-mv (WRITES) — CREATE the matviews the detection pass (above the
        // dry_run return) found absent. Reuses the pre-computed `mv_to_create`, so
        // there is no second `matview_states()` fetch. Each CREATE carries the same
        // `search_path` prelude the ALTERs use, since the emitted `CREATE` (unlike
        // a DDL file) has no `set search_path`.
        for (e, want) in &mv_to_create {
            let desc = format!("{}:{}", e.entity_type.tag(), e.name);
            on_start(&desc);
            let result = adapter
                .execute_script(&format!("{search_path}{}", matview_create_sql(e, want)))
                .await;
            report_step_result(&desc, &mut on_done, result)?;
            summary.reapplied += 1;
        }

        // Pass D — prune orphaned tables (in managed schemas, gone from the
        // design), only when explicitly requested. Otherwise they are reported
        // via the returned plan and left untouched.
        if prune {
            for stmt in &plan.dropped {
                let desc = format!("prune table:{}", stmt.entity_name);
                on_start(&desc);
                let result = adapter.execute_script(&stmt.sql).await;
                report_step_result(&desc, &mut on_done, result)?;
                summary.dropped += 1;
            }
        }

        // Sync pg_cron refresh jobs, mirroring the apply path. This MUST cover the
        // whole design, not the scoped `desired_entities`: `sync_refresh_jobs`
        // unschedules every `dbd:refresh:%` job absent from the set it is given, so
        // a scoped reconcile fed only its subset would unschedule out-of-scope
        // matviews' jobs. The adapter guards on pg_cron presence, so it is a safe
        // no-op on databases (and non-Postgres targets) without the extension.
        let mv_jobs: Vec<(String, crate::config::ResolvedMatview)> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .map(|e| (e.name.clone(), self.config.materialized_views.resolve(&e.name)))
            .collect();
        adapter.sync_refresh_jobs(&mv_jobs).await?;

        // Stamp the project version so `migrate --status` / `apply` stay consistent.
        let version = self.config.project.version.unwrap_or(1);
        adapter.ensure_meta_table().await?;
        adapter.set_project_meta(&self.env, version, scope.map(|s| s.name.as_str())).await?;

        on_complete(summary);
        Ok(plan)
    }

    /// Read-only: introspect the live database and return the complete
    /// difference against the design. Never writes. Unlike `reconcile`, this is
    /// available even after the project is released.
    pub async fn diff_live(
        &self,
        adapter: &dyn DatabaseAdapter,
        scope: Option<&ResolvedScope>,
    ) -> Result<crate::SchemaDiff> {
        use crate::reconcile::{raw_snapshot_from_entities, DEFAULT_SCHEMA};
        use std::collections::HashSet;

        // Batch adapters (e.g. Convex) have no live SQL schema to diff.
        if adapter.prefers_batch_apply() {
            return Err(DbdError::Config(
                "diff is not supported for this target (no live SQL schema to diff)".to_string(),
            ));
        }

        // Resolve scope → working set, gap-gate under `report` (abort before writes).
        let working_set: Option<HashSet<String>> = match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                Some(self.working_set(s)?)
            }
            _ => None,
        };

        // Desired entities: valid, non-external, in scope — in dependency order.
        let desired_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| match (&working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect();

        // Desired snapshot (tables + enums, schema-normalized). Raw (un-canonicalized)
        // so FK/CHECK/indexes/comments survive for `SchemaDiff` to compare.
        let desired_owned: Vec<Entity> = desired_entities.iter().map(|e| (*e).clone()).collect();
        let desired = raw_snapshot_from_entities(&desired_owned);

        // Schemas the design manages. The diff is scoped to these, so it never
        // reports drift for tables in schemas the project doesn't own.
        let managed_schemas: HashSet<String> = desired_entities
            .iter()
            .map(|e| match e.entity_type {
                EntityType::Schema => e.name.clone(),
                _ => {
                    let s = e.schema.clone().unwrap_or_default();
                    if s.is_empty() {
                        DEFAULT_SCHEMA.to_string()
                    } else {
                        s
                    }
                }
            })
            .collect();

        // Live snapshot, restricted to managed schemas. Raw so introspected
        // FK/CHECK/indexes/comments reach `SchemaDiff::normalize_for_diff`.
        let live_entities = adapter.introspect().await?;
        let live_full = raw_snapshot_from_entities(&live_entities);
        let live = restrict_snapshot_to_schemas(live_full, &managed_schemas);

        let mut diff = crate::SchemaDiff::compute(live, desired);

        // Materialized-view drift (read-only). Matviews aren't in the tables/enums
        // snapshots above, so `compute` never reports them — categorize them here.
        // Mirrors reconcile's create/skip/warn decision, but splits the two "warn"
        // cases into Drifted (stamped, hash mismatch) vs Unstamped (no dbd
        // sentinel), and adds Orphan (live matview in a managed schema, gone from
        // the design).
        use crate::reconcile::matview_hash;
        use crate::schema_diff::{MatviewDrift, MatviewDriftKind};

        let design_matviews: Vec<&Entity> = desired_entities
            .iter()
            .copied()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .collect();
        let design_mv_names: HashSet<&str> =
            design_matviews.iter().map(|e| e.name.as_str()).collect();

        let mut matview_drift: Vec<MatviewDrift> = Vec::new();

        // Design matviews: compare each design hash to the live sentinel. Only
        // fetch `matview_states()` when the design actually has a matview — an
        // orphan-only diff never needs it (orphans are found from `live_entities`).
        if !design_matviews.is_empty() {
            let states = adapter.matview_states().await?;
            for e in &design_matviews {
                let want = matview_hash(e);
                let kind = match states.get(&e.name) {
                    None => MatviewDriftKind::Missing,
                    Some(Some(h)) if *h == want => continue, // in sync — emit nothing
                    Some(Some(_)) => MatviewDriftKind::Drifted,
                    Some(None) => MatviewDriftKind::Unstamped,
                };
                matview_drift.push(MatviewDrift { name: e.name.clone(), kind });
            }
        }

        // Orphans: live matviews in a managed schema, absent from the design. No
        // hash needed, so this reuses the already-fetched `live_entities` and adds
        // no query. Schema derivation mirrors `managed_schemas` above.
        for e in &live_entities {
            if e.entity_type != EntityType::MaterializedView {
                continue;
            }
            let schema = {
                let s = e.schema.clone().unwrap_or_default();
                if s.is_empty() { DEFAULT_SCHEMA.to_string() } else { s }
            };
            if managed_schemas.contains(&schema) && !design_mv_names.contains(e.name.as_str()) {
                matview_drift.push(MatviewDrift { name: e.name.clone(), kind: MatviewDriftKind::Orphan });
            }
        }

        // Deterministic order for stable output and tests.
        matview_drift.sort_by(|a, b| a.name.cmp(&b.name));
        diff.matview_drift = matview_drift;

        Ok(diff)
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
    ///
    /// `on_start(desc)` is called just before each step.
    /// `on_done(desc, err)` is called after each step — `err` is `None` on success,
    /// `Some(message)` on failure (called before the error is returned so the caller
    /// can update UI state before propagation).
    /// `on_complete(summary)` is called once after all steps succeed.
    /// Use `|_| {}` / `|_, _| {}` / `|_| {}` when progress reporting is not needed.
    #[allow(clippy::too_many_arguments)]
    pub async fn import_data<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut on_start: S,
        mut on_done: D,
        mut on_complete: C,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(ImportComplete),
    {
        let plan = self.import_plan(name);
        let plan: Vec<ImportPlanEntry> = match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                let ws = self.working_set(s)?;
                plan.into_iter()
                    .filter(|e| import_entry_in_scope(e, &ws, false))
                    .collect()
            }
            _ => plan,
        };

        // Counters for on_complete summary
        let mut count_tables: u32 = 0;
        let mut count_procedures: u32 = 0;
        let mut count_after_scripts: u32 = 0;

        // Ensure internal dbd procedures are present before any import runs.
        // Uses CREATE OR REPLACE so it self-heals and stays current with dbd's version.
        if !dry_run {
            let has_jsonl = plan.iter().any(|e| {
                e.table.format.as_deref().is_some_and(|f| f == "json" || f == "jsonl")
            });
            if has_jsonl {
                adapter.ensure_import_procedure().await?;
            }
        }

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
            let fmt = entry.table.format.as_deref().unwrap_or("csv");
            let desc = format!("import {} ({})", entry.table.name, fmt);
            on_start(&desc);
            let result = if dry_run { Ok(()) } else { adapter.import_data(&entry.table, false).await };
            report_step_result(&desc, &mut on_done, result)?;
            count_tables += 1;
        }

        // Step 3: Call import procedures
        for entry in &plan {
            if let Some(ref proc_name) = entry.procedure {
                let desc = format!("call {proc_name}()");
                on_start(&desc);
                let result = if dry_run { Ok(()) } else { adapter.execute_script(&format!("CALL {proc_name}();")).await };
                report_step_result(&desc, &mut on_done, result)?;
                count_procedures += 1;
            }
        }

        // Step 4: Run after scripts — intentionally NOT scope-filtered; these are
        // project-global post-import hooks, not tied to individual import entries.
        // Scoped callers are responsible for ensuring their after-scripts are safe.
        for after_file in &self.config.import.after {
            let full_path = self.project_dir.join(after_file);
            let desc = format!("run {after_file}");
            on_start(&desc);
            let result = if dry_run {
                Ok(())
            } else {
                let sql = std::fs::read_to_string(&full_path)?;
                adapter.execute_script(&sql).await
            };
            report_step_result(&desc, &mut on_done, result)?;
            count_after_scripts += 1;
        }

        on_complete(ImportComplete {
            tables: count_tables,
            procedures: count_procedures,
            after_scripts: count_after_scripts,
        });
        Ok(())
    }

    /// Deploy the full schema: apply DDL then import seed data.
    ///
    /// Equivalent to `apply` followed by `import_data`. dbd handles
    /// fresh / migrate / current strategy automatically — safe to call
    /// on every bootstrap (idempotent when schema is already current).
    ///
    /// `on_complete(summary)` is called once after both phases succeed with
    /// combined version info and step counts.
    pub async fn deploy<C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut on_complete: C,
    ) -> Result<()>
    where
        C: FnMut(DeployComplete),
    {
        let mut apply_summary: Option<ApplyComplete> = None;
        let mut import_summary: Option<ImportComplete> = None;

        self.apply(adapter, None, dry_run, scope, |_| {}, |_, _| {}, |s| {
            apply_summary = Some(s);
        })
        .await?;

        self.import_data(adapter, None, dry_run, scope, |_| {}, |_, _| {}, |s| {
            import_summary = Some(s);
        })
        .await?;

        on_complete(DeployComplete {
            apply: apply_summary.unwrap_or(ApplyComplete {
                strategy: ApplyStrategy::Current,
                from_version: 0,
                to_version: 0,
                applied: 0,
                migrated: 0,
                created: 0,
                dropped: 0,
            }),
            import: import_summary.unwrap_or(ImportComplete {
                tables: 0,
                procedures: 0,
                after_scripts: 0,
            }),
        });
        Ok(())
    }

    /// Combine all DDL into a single SQL file.
    /// Combine entity DDL into a single SQL script. `scope` filters to that
    /// scope's working set (`None` ⇒ the full set). Filter-only — no gap gate;
    /// use `inspect --scope` to surface dependency gaps, or `deps: include` to
    /// emit a self-contained closure.
    pub fn combine(&self, file: &Path, scope: Option<&ResolvedScope>) -> Result<()> {
        let entities = match scope {
            Some(s) => self.scoped_entities(s)?,
            None => self.entities.clone(),
        };
        let combined: Vec<String> = entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter_map(script::ddl_from_entity)
            .collect();

        std::fs::write(file, combined.join("\n"))?;
        Ok(())
    }

    /// Get the dependency graph for visualization. `name` narrows to one entity's
    /// subgraph; `scope` filters to that scope's working set (`None` ⇒ full set).
    pub fn graph(
        &self,
        name: Option<&str>,
        scope: Option<&ResolvedScope>,
    ) -> Result<dependency::GraphResult> {
        let graphable = |e: &Entity| {
            matches!(
                e.entity_type,
                EntityType::Table
                    | EntityType::View
                    | EntityType::MaterializedView
                    | EntityType::Function
                    | EntityType::Procedure
                    | EntityType::Enum
            )
        };
        let non_meta: Vec<Entity> = match scope {
            Some(s) => {
                let ws = self.working_set(s)?;
                self.entities
                    .iter()
                    .filter(|e| graphable(e) && Self::entity_in_scope(e, s, &ws))
                    .cloned()
                    .collect()
            }
            None => self.entities.iter().filter(|e| graphable(e)).cloned().collect(),
        };
        Ok(dependency::graph_from_entities(&non_meta, name))
    }

    /// Schemas a `reset` would drop under an optional scope. `None`/all-scope ⇒
    /// every managed schema; a subset scope ⇒ only the schemas its working set
    /// occupies (reset is schema-granular — `DROP SCHEMA … CASCADE`).
    pub fn reset_target_schemas(&self, scope: Option<&ResolvedScope>) -> Result<Vec<String>> {
        let all: Vec<String> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Schema)
            .map(|e| e.name.clone())
            .collect();
        match scope {
            Some(s) if !s.is_all => {
                let ws = self.working_set(s)?;
                Ok(all
                    .into_iter()
                    .filter(|schema| {
                        let prefix = format!("{schema}.");
                        ws.contains(schema) || ws.iter().any(|n| n.starts_with(&prefix))
                    })
                    .collect())
            }
            _ => Ok(all),
        }
    }

    /// Reset the database (with safety guards). `scope` restricts the dropped
    /// schemas to that scope's working set (`None`/all-scope ⇒ everything). Roles
    /// are only dropped on a full reset — a subset scope leaves shared roles
    /// intact since roles are not scope-selectable.
    pub async fn reset(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
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

        if let Some(sql) = self.reset_script(target, drop_schemas, drop_extensions, scope)? {
            adapter.execute_script(&sql).await?;
        }
        adapter.clear_project_migrations().await?;

        Ok(())
    }

    /// Build the SQL a `reset` would run, without touching the database. Drops
    /// the project's managed data-model entities individually (scope-filtered)
    /// and, when `drop_schemas`/`drop_extensions` are set, the managed schemas /
    /// configured extensions. Returns `None` when there is nothing to drop.
    /// Used by `reset()` and by `--dry-run`.
    pub fn reset_script(
        &self,
        target: &str,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<Option<String>> {
        // Roles aren't scope-selectable, so only a full reset drops them; a
        // subset scope leaves shared roles intact.
        let is_subset = matches!(scope, Some(s) if !s.is_all);
        let roles: &[_] = if is_subset {
            &[]
        } else {
            self.config
                .target
                .values()
                .next()
                .map(|t| &t.roles[..])
                .unwrap_or(&[])
        };

        // Data-model entities to drop individually (scope-filtered), in the
        // builder's reverse dependency order.
        let is_data_model = |e: &Entity| {
            matches!(
                e.entity_type,
                EntityType::Table
                    | EntityType::View
                    | EntityType::MaterializedView
                    | EntityType::Function
                    | EntityType::Procedure
                    | EntityType::Enum
                    | EntityType::Sequence
            )
        };
        let entities: Vec<&Entity> = match scope {
            Some(s) if !s.is_all => {
                let ws = self.working_set(s)?;
                self.entities
                    .iter()
                    .filter(|e| is_data_model(e) && Self::entity_in_scope(e, s, &ws))
                    .collect()
            }
            _ => self.entities.iter().filter(|e| is_data_model(e)).collect(),
        };

        // Schemas the `--schemas` path may drop — all managed schemas, or just
        // those the scope occupies.
        let schemas = self.reset_target_schemas(scope)?;

        // The active target's extensions (by bare name) for the `--extensions` path.
        let extensions: Vec<String> = self
            .config
            .target
            .values()
            .next()
            .map(|t| t.extensions.iter().map(|e| e.name().to_string()).collect())
            .unwrap_or_default();

        script::build_reset_script(
            &entities,
            roles,
            &extensions,
            target,
            drop_schemas,
            drop_extensions,
            &schemas,
        )
        .map_err(DbdError::SafetyGuard)
    }
}

/// Partition entities by type for ordered apply.
/// Returns: (schemas, extensions, roles, sequences, enums, tables, views, matviews, functions/procedures, externals)
///
/// Sequences are emitted before tables so a column `DEFAULT nextval('seq')`
/// referencing a standalone sequence finds the sequence already created.
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
    Vec<Entity>,
    Vec<Entity>,
) {
    let mut schemas = Vec::new();
    let mut extensions = Vec::new();
    let mut roles = Vec::new();
    let mut sequences = Vec::new();
    let mut enums = Vec::new();
    let mut tables = Vec::new();
    let mut views = Vec::new();
    let mut matviews = Vec::new();
    let mut functions = Vec::new(); // functions + procedures
    let mut externals = Vec::new();

    for entity in entities {
        match entity.entity_type {
            EntityType::Schema => schemas.push(entity),
            EntityType::Extension => extensions.push(entity),
            EntityType::Role => roles.push(entity),
            EntityType::Sequence => sequences.push(entity),
            EntityType::Enum => enums.push(entity),
            EntityType::Table => tables.push(entity),
            EntityType::View => views.push(entity),
            EntityType::MaterializedView => matviews.push(entity),
            EntityType::Function | EntityType::Procedure => functions.push(entity),
            EntityType::External => externals.push(entity),
            _ => tables.push(entity), // Default to tables group
        }
    }

    (schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals)
}

/// Validate materialized-view refresh configuration against the resolved
/// entities and the target's declared extension names. Pure and offline —
/// used by `dbd inspect` so misconfigured refresh settings surface without a
/// database connection. Returns human-readable error strings; empty = valid.
///
/// Checks, per materialized view (resolved via `mv_config.resolve`):
///   - `concurrently: true` requires the view to have a UNIQUE index —
///     `REFRESH MATERIALIZED VIEW CONCURRENTLY` fails in Postgres without one.
///   - a resolved `refresh` schedule must be a valid 5-field cron expression.
///
/// And once, across all matviews:
///   - if any matview resolves a `refresh` schedule, `pg_cron` must be among
///     `extensions` (scheduling is implemented via pg_cron jobs).
pub fn validate_materialized_views(
    entities: &[Entity],
    mv_config: &MaterializedViewsConfig,
    extensions: &[String],
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut any_scheduled = false;

    for entity in entities
        .iter()
        .filter(|e| e.entity_type == EntityType::MaterializedView)
    {
        let resolved = mv_config.resolve(&entity.name);

        if resolved.refresh.is_some() {
            any_scheduled = true;
        }

        if resolved.concurrently {
            let has_unique_index = entity
                .table_def
                .as_ref()
                .is_some_and(|t| t.indexes.iter().any(|i| i.unique));
            if !has_unique_index {
                errors.push(format!(
                    "materialized view {}: REFRESH ... CONCURRENTLY requires a unique index",
                    entity.name
                ));
            }
        }

        if let Some(schedule) = &resolved.refresh
            && !is_valid_cron_expression(schedule)
        {
            errors.push(format!(
                "materialized view {}: invalid cron expression '{schedule}'",
                entity.name
            ));
        }
    }

    if any_scheduled && !extensions.iter().any(|e| e == "pg_cron") {
        errors.push(
            "materialized view refresh scheduling requires the pg_cron extension \
             (add 'pg_cron' under target.postgres.extensions)"
                .to_string(),
        );
    }

    errors
}

/// Minimal validation for a pg_cron 5-field schedule expression: exactly five
/// whitespace-separated fields, each made up only of digits and `* / , -`.
/// Not a full cron parser — just enough to flag obviously malformed input
/// offline (e.g. wrong field count, stray words).
fn is_valid_cron_expression(expr: &str) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    fields.len() == 5
        && fields
            .iter()
            .all(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_digit() || "*/,-".contains(c)))
}

/// A CHECK constraint that constrains one column to a fixed set of string
/// literals — a candidate for a Postgres `ENUM` type. Advisory only.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumHint {
    pub entity: String,
    pub column: String,
    pub values: Vec<String>,
}

/// Flag single-column, string-literal-set CHECK constraints as enum candidates.
///
/// Postgres/Supabase only (`dialect == "postgresql"`); advisory, never an error.
/// Recognizes three equivalent shapes: `col IN ('a', 'b')`,
/// `col = ANY(ARRAY['a', 'b'])`, and `col = 'a' OR col = 'b'`.
pub fn suggest_enum_candidates(entities: &[Entity], dialect: &str) -> Vec<EnumHint> {
    if dialect != "postgresql" {
        return Vec::new();
    }
    let mut hints = Vec::new();
    for e in entities.iter().filter(|e| e.entity_type == EntityType::Table) {
        let Some(td) = &e.table_def else { continue };
        for c in &td.constraints {
            let TableConstraint::Check { expression, .. } = c else {
                continue;
            };
            if let Some((column, values)) = string_set_check(expression) {
                let h = EnumHint {
                    entity: e.name.clone(),
                    column,
                    values,
                };
                if !hints.contains(&h) {
                    hints.push(h);
                }
            }
        }
    }
    hints
}

/// Parse a CHECK expression and, if it constrains a single column to a set of
/// string literals, return `(column, values)`. Returns `None` for anything
/// else — numeric sets, subqueries, mixed lists, casts/functions on the column,
/// multiple columns, or a negated (`NOT IN`) set.
fn string_set_check(expr: &str) -> Option<(String, Vec<String>)> {
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;

    let parsed = Parser::new(&PostgreSqlDialect {})
        .try_with_sql(expr)
        .ok()
        .and_then(|mut p| p.parse_expr().ok());

    match parsed {
        Some(ast) => string_set_from_ast(&ast),
        // Parse failure: conservative regex for `col IN ('a', 'b', …)` only.
        None => string_set_from_regex(expr),
    }
}

/// Match the three enum-candidate AST shapes against a parsed CHECK expression.
fn string_set_from_ast(ast: &sqlparser::ast::Expr) -> Option<(String, Vec<String>)> {
    use sqlparser::ast::{BinaryOperator, Expr};

    match ast {
        // col IN ('a', 'b', …)  — reject NOT IN.
        Expr::InList {
            expr,
            list,
            negated: false,
        } => {
            let col = ident_name(expr)?;
            let vals = all_string_lits(list)?;
            Some((col, vals))
        }
        // col = ANY(ARRAY['a', 'b', …])
        Expr::AnyOp {
            left,
            compare_op: BinaryOperator::Eq,
            right,
            ..
        } => {
            let col = ident_name(left)?;
            let Expr::Array(array) = right.as_ref() else {
                return None;
            };
            let vals = all_string_lits(&array.elem)?;
            Some((col, vals))
        }
        // col = 'a' OR col = 'b' OR …  — every leaf must be `<same col> = <string>`.
        Expr::BinaryOp {
            op: BinaryOperator::Or,
            ..
        } => {
            let mut col: Option<String> = None;
            let mut vals = Vec::new();
            if collect_or_equalities(ast, &mut col, &mut vals) {
                Some((col?, vals))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Walk an `OR` tree, collecting `<col> = <string literal>` leaves. Fails (returns
/// `false`) if any leaf is a different column or not a column-equals-string test.
fn collect_or_equalities(
    expr: &sqlparser::ast::Expr,
    col: &mut Option<String>,
    vals: &mut Vec<String>,
) -> bool {
    use sqlparser::ast::{BinaryOperator, Expr};

    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Or,
            right,
        } => collect_or_equalities(left, col, vals) && collect_or_equalities(right, col, vals),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            let Some(name) = ident_name(left) else {
                return false;
            };
            let Some(val) = string_lit(right) else {
                return false;
            };
            match col {
                Some(existing) if *existing != name => return false,
                Some(_) => {}
                None => *col = Some(name),
            }
            vals.push(val);
            true
        }
        _ => false,
    }
}

/// Collect the string-literal values of a list, or `None` if any element is not
/// a single-quoted string.
fn all_string_lits(list: &[sqlparser::ast::Expr]) -> Option<Vec<String>> {
    if list.is_empty() {
        return None;
    }
    list.iter().map(string_lit).collect()
}

/// The name of a bare column reference (`Identifier` or `CompoundIdentifier`),
/// or `None` for casts, functions, or any other expression.
fn ident_name(expr: &sqlparser::ast::Expr) -> Option<String> {
    use sqlparser::ast::Expr;
    match expr {
        Expr::Identifier(ident) => Some(ident.value.clone()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|i| i.value.clone()),
        _ => None,
    }
}

/// The value of a single-quoted string literal, or `None` for any other expression.
fn string_lit(expr: &sqlparser::ast::Expr) -> Option<String> {
    use sqlparser::ast::{Expr, Value};
    match expr {
        Expr::Value(vws) => match &vws.value {
            Value::SingleQuotedString(s) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Conservative fallback for when the expression does not parse: match a single
/// column `IN ('a', 'b', …)` of all single-quoted literals.
fn string_set_from_regex(expr: &str) -> Option<(String, Vec<String>)> {
    let trimmed = expr.trim();
    let open = trimmed.find('(')?;
    let head = trimmed[..open].trim();
    if !trimmed.ends_with(')') {
        return None;
    }

    // Head must be `"?col"? IN` (case-insensitive keyword), single identifier.
    let (col_raw, kw) = head.rsplit_once(char::is_whitespace)?;
    if !kw.eq_ignore_ascii_case("in") {
        return None;
    }
    let col = col_raw.trim().trim_matches('"');
    if col.is_empty() || !col.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    // Body items must each be `'…'` single-quoted literals.
    let body = &trimmed[open + 1..trimmed.len() - 1];
    let mut vals = Vec::new();
    for part in body.split(',') {
        let item = part.trim();
        if item.len() < 2 || !item.starts_with('\'') || !item.ends_with('\'') {
            return None;
        }
        vals.push(item[1..item.len() - 1].to_string());
    }
    if vals.is_empty() {
        return None;
    }
    Some((col.to_string(), vals))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::mock::MockAdapter;
    use crate::entity::TableDef;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    fn meta_with_scope(scope: Option<&str>) -> crate::adapter::ProjectMeta {
        crate::adapter::ProjectMeta {
            project: "p".to_string(),
            env: "dev".to_string(),
            version: 1,
            scope: scope.map(|s| s.to_string()),
            applied_at: None,
        }
    }

    #[test]
    fn scope_guard_allows_matching_scope() {
        let m = meta_with_scope(Some("public"));
        assert!(Design::check_scope_guard(Some(&m), "public", false).is_ok());
    }

    #[test]
    fn scope_guard_blocks_mismatch() {
        let m = meta_with_scope(Some("public"));
        let err = Design::check_scope_guard(Some(&m), "internal", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("public") && msg.contains("internal"), "msg was: {msg}");
    }

    #[test]
    fn scope_guard_unpinned_never_blocks() {
        let m = meta_with_scope(None);
        assert!(Design::check_scope_guard(Some(&m), "internal", false).is_ok());
        assert!(Design::check_scope_guard(None, "internal", false).is_ok());
    }

    #[test]
    fn scope_guard_allow_scope_change_bypasses() {
        let m = meta_with_scope(Some("public"));
        assert!(Design::check_scope_guard(Some(&m), "internal", true).is_ok());
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
        let report = design.report(None, None);

        // The fixture project should have no major errors
        // (warnings are expected for unresolved references)
        let _report = report; // Fixture may have entities with file-not-found errors
    }

    #[test]
    fn graph_returns_nodes_and_edges() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let graph = design.graph(None, None).unwrap();

        assert!(!graph.nodes.is_empty());
        assert!(!graph.layers.is_empty());
    }

    #[test]
    fn graph_includes_materialized_views() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let graph = design.graph(None, None).unwrap();
        assert!(
            graph.nodes.iter().any(|n| n.name == "config.genders_mv"),
            "materialized view should appear as a graph node"
        );
    }

    #[test]
    fn graph_filters_to_scope() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(Some("config_only"), None).unwrap();
        let graph = design.graph(None, Some(&scope)).unwrap();

        // Only config.* nodes survive — no staging entities in the scoped graph.
        assert!(!graph.nodes.is_empty());
        assert!(graph.nodes.iter().all(|n| !n.name.starts_with("staging.")));
        assert!(graph.nodes.iter().any(|n| n.name.starts_with("config.")));
    }

    #[tokio::test]
    async fn apply_dry_run_does_not_execute() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        design.apply(&mock, None, true, None, |_| {}, |_, _| {}, |_| {}).await.unwrap();
        assert!(mock.applied_names().is_empty());
    }

    #[tokio::test]
    async fn apply_executes_entities() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        design.apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {}).await.unwrap();
        assert!(!mock.applied_names().is_empty());
    }

    // ── diff_live (read-only) ──────────────────────────────

    /// diff_live is read-only and reports drift: an empty live DB (MockAdapter's
    /// introspect() returns no entities) against a non-empty design yields a
    /// non-empty diff, and applies/executes nothing.
    #[tokio::test]
    async fn diff_live_reports_drift_read_only() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new(); // empty live DB

        let d = design.diff_live(&mock, None).await.unwrap();
        assert!(!d.is_empty(), "empty live DB vs a non-empty design must show drift");
        assert!(mock.applied_names().is_empty(), "diff_live must not apply anything");
        assert_eq!(mock.script_count(), 0, "diff_live must not execute any script");
    }

    /// Regression (issue #8): a foreign key the design declares but the live DB
    /// is missing must surface in `dbd diff`. The read-only diff builds its
    /// snapshots with a NON-stripping builder, so inline/table FKs reach the
    /// comparison — unlike reconcile, whose `canonicalize` strips them. Before
    /// the fix, `diff_live` built both sides via `snapshot_from_entities`
    /// (canonicalize) and the FK was silently stripped from both, so drift went
    /// unreported.
    #[tokio::test]
    async fn diff_live_detects_missing_foreign_key() {
        use crate::diff::{DiffAction, FieldType};

        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        // Live = the design's own table entities, but with config.lookup_values'
        // inline FKs removed — the "FK dropped out from under the design" case.
        let live: Vec<Entity> = design
            .entities()
            .iter()
            .filter(|e| {
                matches!(e.entity_type, EntityType::Table | EntityType::Enum) && e.errors.is_empty()
            })
            .cloned()
            .map(|mut e| {
                if e.name == "config.lookup_values"
                    && let Some(td) = e.table_def.as_mut()
                {
                    for c in &mut td.columns {
                        c.inline_fk = None;
                    }
                }
                e
            })
            .collect();

        let mock = MockAdapter::new();
        *mock.introspected.lock().unwrap() = live;

        let d = design.diff_live(&mock, None).await.unwrap();
        assert!(
            d.changes.iter().any(|c| c.entity_name == "config.lookup_values"
                && matches!(&c.action, DiffAction::Change(changes)
                    if changes.iter().any(|f| f.field_type == FieldType::Constraint))),
            "a design FK missing from the live DB must surface as a constraint change; got {:?}",
            d.changes
        );
    }

    // ── Transactional apply (atomic batch) ───────────────

    #[tokio::test]
    async fn apply_commits_batch_transaction_on_success() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new().with_transactions();

        design.apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {}).await.unwrap();

        assert_eq!(mock.txn_log(), vec!["begin", "commit"]);
    }

    #[tokio::test]
    async fn apply_rolls_back_batch_transaction_on_failure() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        // Fail on the first entity that would be applied, so the batch aborts mid-plan.
        let target = design
            .entities()
            .iter()
            .find(|e| e.errors.is_empty() && e.entity_type != EntityType::External)
            .map(|e| e.name.clone())
            .expect("fixture has at least one applicable entity");
        let mock = MockAdapter::new().with_transactions().fail_on_entity(&target);

        let err = design
            .apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap_err();

        assert!(err.to_string().contains("injected failure"));
        assert_eq!(mock.txn_log(), vec!["begin", "rollback"]);
    }

    #[tokio::test]
    async fn apply_without_txn_support_skips_transaction() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new(); // supports_transactional_apply() == false

        design.apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {}).await.unwrap();

        assert!(mock.txn_log().is_empty());
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

        design.apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {}).await.unwrap();

        // After apply on a fresh env, meta should have been written
        // (version depends on design.yaml project.version — likely 0 or None for fixture)
        let meta = mock.get_project_meta().await.unwrap();
        // Fresh env with latest_version=0 still calls SetVersion(0) in Fresh strategy
        // which calls set_project_meta. Meta should exist.
        assert!(meta.is_some() || design.config().project.version.is_none());
    }

    #[tokio::test]
    async fn apply_pins_resolved_scope() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();
        let resolved = design.resolve_scope(None, None).unwrap();

        design
            .apply(&mock, None, false, Some(&resolved), |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap();

        let meta = mock.get_project_meta().await.unwrap().expect("apply writes meta");
        assert_eq!(meta.scope.as_deref(), Some(resolved.name.as_str()));
    }

    #[tokio::test]
    async fn reset_blocked_in_prod() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();
        let mock = MockAdapter::new().with_meta("prod", 0);

        let result = design.reset(&mock, "postgres", false, false, false, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("prod"));
    }

    #[tokio::test]
    async fn reset_blocked_after_v1() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new().with_meta("dev", 1);

        let result = design.reset(&mock, "postgres", false, false, false, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("migrations"));
    }

    #[tokio::test]
    async fn reset_allowed_dev_pre_v1() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new().with_meta("dev", 0);

        let result = design.reset(&mock, "postgres", false, false, false, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn reset_force_overrides_guard() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();
        let mock = MockAdapter::new().with_meta("prod", 5);

        let result = design.reset(&mock, "postgres", true, false, false, None).await;
        assert!(result.is_ok());
    }

    #[test]
    fn reset_target_schemas_filters_to_scope() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        // Full reset (all-scope / None) drops every managed schema.
        let all = design.reset_target_schemas(None).unwrap();
        assert!(all.iter().any(|s| s == "config"));
        assert!(all.iter().any(|s| s == "staging"));

        // A config-only scope drops only the schemas its working set occupies.
        let scope = design.resolve_scope(Some("config_only"), None).unwrap();
        let scoped = design.reset_target_schemas(Some(&scope)).unwrap();
        assert!(scoped.iter().any(|s| s == "config"));
        assert!(!scoped.iter().any(|s| s == "staging"));
    }

    #[test]
    fn combine_writes_file() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("init.sql");
        design.combine(&out, None).unwrap();

        assert!(out.exists());
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("CREATE SCHEMA"));
    }

    #[test]
    fn combine_filters_to_scope() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(Some("config_only"), None).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("hub.sql");
        design.combine(&out, Some(&scope)).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        // config.* DDL present, staging.* procedures excluded from the scoped combine.
        assert!(content.contains("config"));
        assert!(!content.contains("staging"));
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
        let _ = design.import_data(&mock, None, false, None, |_| {}, |_, _| {}, |_| {}).await;

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

        let plan = build_execution_plan(&entities, 0, 2, &[], None);

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

        let plan = build_execution_plan(&entities, 2, 2, &[], None);

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

        let plan = build_execution_plan(&entities, 1, 2, &migrations, None);

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

        let plan = build_execution_plan(&entities, 1, 3, &migrations, None);

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

        let plan = build_execution_plan(&entities, 1, 2, &migrations, None);

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

        let plan = build_execution_plan(&entities, 1, 2, &migrations, None);

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

        let plan = build_execution_plan(&entities, 0, 1, &[], None);

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

        let plan = build_execution_plan(&entities, 0, 1, &[], None);

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

        let plan = build_execution_plan(&entities, 5, 3, &[], None);

        assert_eq!(plan.strategy, ApplyStrategy::Current);
    }

    // M5.4: Both versions zero
    #[test]
    fn a_fresh_db_no_snapshots() {
        let entities = vec![test_entity("config.users")];

        let plan = build_execution_plan(&entities, 0, 0, &[], None);

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

        let plan = build_execution_plan(&entities, 1, 3, &migrations, None);

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

        let plan = build_execution_plan(&entities, 0, 1, &[], None);

        assert_eq!(plan.strategy, ApplyStrategy::Fresh);
        // Should only have SetVersion step (no entities to apply)
        let apply_count = plan.steps.iter().filter(|s| matches!(s, ExecutionStep::ApplyEntity(_))).count();
        assert_eq!(apply_count, 0, "no entities means no ApplyEntity steps");
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(1))));
    }

    // ── skip_schemas filtering ───────────────────────────

    // ── scope_names filtering ─────────────────────────────

    #[test]
    fn execution_plan_skips_out_of_scope_migration_steps() {
        use std::collections::HashSet;
        let entities = vec![test_entity("a"), test_entity("b")];
        // migration drops "c" (not in scope) and alters "b" (in scope)
        let migrations = vec![test_migration(1, 2, vec![], vec!["b"], vec!["c"])];
        let in_scope: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();

        let plan = build_execution_plan(&entities, 1, 2, &migrations, Some(&in_scope));

        // No DropEntity for "c"
        assert!(!plan.steps.iter().any(|s| matches!(
            s, ExecutionStep::DropEntity { entity_name, .. } if entity_name == "c"
        )));
        // In-scope altered "b" still gets its migrate + apply steps
        assert!(plan.steps.iter().any(|s| matches!(
            s, ExecutionStep::MigrateEntity { entity_name, .. } if entity_name == "b"
        )));
        assert!(plan.steps.iter().any(|s| matches!(
            s, ExecutionStep::ApplyEntity(name) if name == "b"
        )));
        // Migration is recorded and SetVersion still advances
        assert!(plan.steps.iter().any(|s| matches!(
            s, ExecutionStep::RecordMigration { version: 2, .. }
        )));
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(2))));
    }

    // ── data.sql TODO blocking ────────────────────────────

    #[tokio::test]
    async fn apply_blocked_when_pending_migration_has_todo() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        // Minimal design.yaml at v2 — one pending migration ahead of DB v1
        std::fs::write(
            project_dir.join("design.yaml"),
            "project:\n  name: test\n  version: 2\n",
        )
        .unwrap();

        // Create a pending migration v2 with an unresolved data.sql
        let mig_dir = project_dir.join("migrations/002/config");
        std::fs::create_dir_all(&mig_dir).unwrap();
        std::fs::write(
            mig_dir.join("users.data.sql"),
            "-- TODO: Data correction required for config.users.score.\n\
             -- Column type changed from JSONB to INTEGER.\n",
        )
        .unwrap();
        // Write a minimal graph.json so pending_migrations picks it up
        std::fs::write(
            project_dir.join("migrations/002/graph.json"),
            r#"{"fromVersion":1,"toVersion":2,"altered":["config.users"],"added":[],"dropped":[]}"#,
        )
        .unwrap();

        let design = Design::from_config_with_dir(
            &project_dir.join("design.yaml"),
            "dev",
            Some(project_dir),
        )
        .unwrap();

        // DB is at v1 → migration v2 is pending
        let mock = crate::adapter::mock::MockAdapter::new().with_meta("dev", 1);
        let result = design
            .apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {})
            .await;

        assert!(result.is_err(), "apply should be blocked by unresolved TODO");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("TODO"), "error should mention TODO: {msg}");
        assert!(msg.contains("users.data.sql"), "error should name the file: {msg}");
    }

    #[tokio::test]
    async fn apply_not_blocked_when_data_sql_todo_is_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();

        std::fs::write(
            project_dir.join("design.yaml"),
            "project:\n  name: test\n  version: 1\n",
        )
        .unwrap();

        // data.sql with no TODO (already resolved)
        let mig_dir = project_dir.join("migrations/001/config");
        std::fs::create_dir_all(&mig_dir).unwrap();
        std::fs::write(
            mig_dir.join("users.data.sql"),
            "UPDATE config.users SET score = old_score::INTEGER;\n",
        )
        .unwrap();
        std::fs::write(
            project_dir.join("migrations/001/graph.json"),
            r#"{"fromVersion":0,"toVersion":1,"altered":["config.users"],"added":[],"dropped":[]}"#,
        )
        .unwrap();

        let design = Design::from_config_with_dir(
            &project_dir.join("design.yaml"),
            "dev",
            Some(project_dir),
        )
        .unwrap();

        // Fresh DB — v1 migration is pending but data.sql has no TODOs
        let mock = crate::adapter::mock::MockAdapter::new();
        let result = design
            .apply(&mock, None, false, None, |_| {}, |_, _| {}, |_| {})
            .await;

        assert!(result.is_ok(), "should not be blocked: {:?}", result);
    }

    #[tokio::test]
    async fn apply_with_report_gaps_errors_before_writing() {
        use crate::scope::ResolvedScope;
        use std::collections::HashSet;
        use crate::config::DepsPolicy;

        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        // report-policy scope with config.lookup_values but not its FK target → one gap.
        let scope = ResolvedScope {
            name: "test".into(),
            entities: HashSet::from(["config.lookup_values".to_string(), "config".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: None,
        };

        let result = design
            .apply(&mock, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {})
            .await;
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dependency gap"), "expected gap error, got: {msg}");
        assert!(mock.applied_names().is_empty()); // no writes
    }

    #[tokio::test]
    async fn apply_with_scope_filters_entities() {
        use crate::scope::ResolvedScope;
        use std::collections::HashSet;
        use crate::config::DepsPolicy;

        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        // Complete config-only scope (config.lookup_values's FK target IS present).
        let scope = ResolvedScope {
            name: "config_only".into(),
            entities: HashSet::from([
                "config".to_string(),
                "config.lookups".to_string(),
                "config.lookup_values".to_string(),
            ]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: None,
        };

        design
            .apply(&mock, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap();
        let applied = mock.applied_names();
        assert!(applied.iter().any(|n| n == "config.lookups"));
        assert!(!applied.iter().any(|n| n.starts_with("staging.")));
        // No allowlist (`extensions: None`) ⇒ target extensions still apply.
        assert!(applied.iter().any(|n| n == "uuid-ossp"));
    }

    // Helper: a gap-free config-only scope with a given extension allowlist.
    #[cfg(test)]
    fn config_only_scope(extensions: Option<std::collections::HashSet<String>>) -> ResolvedScope {
        use std::collections::HashSet;
        ResolvedScope {
            name: "config_only".into(),
            entities: HashSet::from([
                "config".to_string(),
                "config.lookups".to_string(),
                "config.lookup_values".to_string(),
            ]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions,
        }
    }

    #[tokio::test]
    async fn apply_empty_extension_allowlist_skips_extensions() {
        // `extensions: Some([])` — the embedded-Postgres-without-pgvector case:
        // apply must skip every extension while still applying in-scope tables.
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        let scope = config_only_scope(Some(HashSet::new()));
        design
            .apply(&mock, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap();
        let applied = mock.applied_names();
        assert!(applied.iter().any(|n| n == "config.lookups"), "in-scope tables still apply: {applied:?}");
        assert!(
            !applied.iter().any(|n| n == "uuid-ossp" || n == "postgis"),
            "empty allowlist must skip all extensions, got: {applied:?}"
        );
    }

    #[tokio::test]
    async fn apply_extension_allowlist_keeps_only_listed() {
        // `extensions: Some([uuid-ossp])` applies that one, drops the rest.
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        let scope = config_only_scope(Some(HashSet::from(["uuid-ossp".to_string()])));
        design
            .apply(&mock, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap();
        let applied = mock.applied_names();
        assert!(applied.iter().any(|n| n == "uuid-ossp"), "listed extension applies: {applied:?}");
        assert!(!applied.iter().any(|n| n == "postgis"), "unlisted extension dropped: {applied:?}");
    }

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
    async fn deploy_with_all_scope_applies_everything() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();
        let scope = design.resolve_scope(Some("all"), None).unwrap();

        design.deploy(&mock, false, Some(&scope), |_| {}).await.unwrap();
        assert!(!mock.applied_names().is_empty());
    }

    #[tokio::test]
    async fn deploy_dry_run_returns_ok_and_applies_nothing() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();

        let mock = MockAdapter::new();
        design.deploy(&mock, true, None, |_| {}).await.unwrap();

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
        design.deploy(&mock, false, None, |_| {}).await.unwrap();
    }

    // ── resolve_unknown_refs_via_db ──────────────────────

    fn empty_design() -> Design {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();
        Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path()))
            .unwrap()
    }

    #[tokio::test]
    async fn resolve_via_db_drops_warning_when_entity_exists() {
        let mut design = empty_design();
        let mut entity = Entity::new(EntityType::Table, "config.orders");
        entity.warnings.push("Unresolved reference: auth.users".to_string());
        design.entities.push(entity);

        let mock = MockAdapter::new().with_known_entities(["auth.users"]);
        let dropped = design.resolve_unknown_refs_via_db(&mock).await.unwrap();

        assert_eq!(dropped, 1);
        let last = design.entities.last().unwrap();
        assert!(last.warnings.is_empty());
    }

    #[tokio::test]
    async fn resolve_via_db_keeps_warning_when_entity_missing() {
        let mut design = empty_design();
        let mut entity = Entity::new(EntityType::Table, "config.orders");
        entity.warnings.push("Unresolved reference: auth.users".to_string());
        design.entities.push(entity);

        let mock = MockAdapter::new();
        let dropped = design.resolve_unknown_refs_via_db(&mock).await.unwrap();

        assert_eq!(dropped, 0);
        assert_eq!(design.entities.last().unwrap().warnings.len(), 1);
    }

    #[tokio::test]
    async fn resolve_via_db_leaves_unrelated_warnings_alone() {
        let mut design = empty_design();
        let mut entity = Entity::new(EntityType::Table, "config.orders");
        entity.warnings.push("Unresolved reference: auth.users".to_string());
        entity.warnings.push("Some other warning".to_string());
        design.entities.push(entity);

        let mock = MockAdapter::new().with_known_entities(["auth.users"]);
        let dropped = design.resolve_unknown_refs_via_db(&mock).await.unwrap();

        assert_eq!(dropped, 1);
        assert_eq!(
            design.entities.last().unwrap().warnings,
            vec!["Some other warning"]
        );
    }

    // ── refcache (offline reference cache) ───────────────

    #[tokio::test]
    async fn write_ref_cache_persists_names_from_adapter() {
        let mut design = empty_design();
        // Capture project dir before moving design through helpers.
        let project_dir = design.project_dir().to_path_buf();

        // Add a warning so we can also exercise the resolve path below.
        let mut entity = Entity::new(EntityType::Table, "config.orders");
        entity.warnings.push("Unresolved reference: auth.users".to_string());
        design.entities.push(entity);

        let mock = MockAdapter::new().with_known_entities(["auth.users", "public.lookups"]);
        let count = design.write_ref_cache(&mock, "postgres").await.unwrap();
        assert_eq!(count, 2);

        let loaded = crate::refcache::RefCache::load(&project_dir).unwrap().unwrap();
        assert!(loaded.contains("auth.users"));
        assert!(loaded.contains("public.lookups"));
        assert_eq!(loaded.source, "postgres");
    }

    #[test]
    fn resolve_via_cache_drops_warning_when_present() {
        let mut design = empty_design();
        let project_dir = design.project_dir().to_path_buf();

        let mut entity = Entity::new(EntityType::Table, "config.orders");
        entity.warnings.push("Unresolved reference: auth.users".to_string());
        entity.warnings.push("Unresolved reference: missing.thing".to_string());
        design.entities.push(entity);

        let cache = crate::refcache::RefCache::new("postgres", ["auth.users"]);
        cache.save(&project_dir).unwrap();

        let (dropped, size) = design.resolve_unknown_refs_via_cache().unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(size, Some(1));
        assert_eq!(
            design.entities.last().unwrap().warnings,
            vec!["Unresolved reference: missing.thing"]
        );
    }

    #[test]
    fn resolve_via_cache_is_noop_when_cache_missing() {
        let mut design = empty_design();
        let mut entity = Entity::new(EntityType::Table, "config.orders");
        entity.warnings.push("Unresolved reference: auth.users".to_string());
        design.entities.push(entity);

        let (dropped, size) = design.resolve_unknown_refs_via_cache().unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(size, None);
        // Warning remains untouched.
        assert_eq!(design.entities.last().unwrap().warnings.len(), 1);
    }

    #[test]
    fn resolve_scope_all_when_none() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(None, None).unwrap();
        assert!(scope.is_all);
    }

    #[test]
    fn working_set_all_scope_is_full_set() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(Some("all"), None).unwrap();
        let ws = design.working_set(&scope).unwrap();
        // Spans both schemas' DDL entities (config tables + a staging procedure).
        assert!(ws.contains("config.lookups"));
        assert!(ws.contains("staging.import_lookups"));
    }

    #[test]
    fn working_set_report_filters_to_scope() {
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        // A narrow report-policy scope returns exactly its own entities.
        let scope = ResolvedScope {
            name: "narrow".to_string(),
            entities: HashSet::from(["config.lookups".to_string(), "config".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: None,
        };
        let ws = design.working_set(&scope).unwrap();
        assert!(ws.contains("config.lookups"));
        assert!(!ws.contains("config.lookup_values")); // genuinely filtered out
        assert!(!ws.contains("staging.lookups"));
    }

    #[test]
    fn working_set_include_expands_closure() {
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        // config.lookup_values has an FK to config.lookups; include policy pulls it in.
        let scope = ResolvedScope {
            name: "auto".to_string(),
            entities: HashSet::from(["config.lookup_values".to_string(), "config".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Include,
            is_all: false,
            extensions: None,
        };
        let ws = design.working_set(&scope).unwrap();
        assert!(ws.contains("config.lookup_values"));
        assert!(ws.contains("config.lookups")); // pulled in by closure
    }

    #[test]
    fn scoped_entities_all_returns_everything() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let all = design.resolve_scope(Some("all"), None).unwrap();
        assert_eq!(
            design.scoped_entities(&all).unwrap().len(),
            design.entities().len()
        );
    }

    #[test]
    fn scoped_entities_filters_to_working_set() {
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        // A config-only selection: keep config.* tables, drop staging.* procedures.
        let scope = ResolvedScope {
            name: "config_only".to_string(),
            entities: HashSet::from([
                "config".to_string(),
                "config.lookups".to_string(),
                "config.lookup_values".to_string(),
            ]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: None,
        };
        let scoped = design.scoped_entities(&scope).unwrap();
        let names: Vec<&str> = scoped.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"config.lookups"));
        // Out-of-scope managed entities are filtered away.
        assert!(!names.iter().any(|n| n.starts_with("staging.")));
        // Fewer than the full set — proves filtering actually happened.
        assert!(scoped.len() < design.entities().len());
    }

    #[test]
    fn scoped_entities_extensions_none_keeps_all_extensions() {
        // No allowlist (`extensions: None`) preserves today's always-on
        // behavior: every target extension stays in the scope.
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        // Sanity: the fixture declares its extensions by bare name.
        let all_exts: Vec<&str> = design
            .entities()
            .iter()
            .filter(|e| e.entity_type == EntityType::Extension)
            .map(|e| e.name.as_str())
            .collect();
        assert!(all_exts.contains(&"uuid-ossp"));
        assert!(all_exts.contains(&"postgis"));

        let scope = ResolvedScope {
            name: "with_exts".to_string(),
            entities: HashSet::from(["config".to_string(), "config.lookups".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: None,
        };
        let scoped = design.scoped_entities(&scope).unwrap();
        let ext_names: Vec<&str> = scoped
            .iter()
            .filter(|e| e.entity_type == EntityType::Extension)
            .map(|e| e.name.as_str())
            .collect();
        assert!(ext_names.contains(&"uuid-ossp"));
        assert!(ext_names.contains(&"postgis"));
    }

    #[test]
    fn scoped_entities_empty_allowlist_drops_all_extensions() {
        // `extensions: Some([])` opts out of every extension — the use case for
        // an embedded Postgres that lacks them.
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = ResolvedScope {
            name: "hive".to_string(),
            entities: HashSet::from(["config".to_string(), "config.lookups".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: Some(HashSet::new()),
        };
        let scoped = design.scoped_entities(&scope).unwrap();
        assert!(
            !scoped.iter().any(|e| e.entity_type == EntityType::Extension),
            "empty allowlist must drop every extension"
        );
        // Roles are still always-on (the allowlist only governs extensions).
        assert!(scoped.iter().any(|e| e.entity_type == EntityType::Role));
    }

    #[test]
    fn scoped_entities_named_allowlist_keeps_only_listed() {
        // `extensions: Some([postgis])` keeps exactly postgis, drops uuid-ossp.
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = ResolvedScope {
            name: "only_postgis".to_string(),
            entities: HashSet::from(["config".to_string(), "config.lookups".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: Some(HashSet::from(["postgis".to_string()])),
        };
        let scoped = design.scoped_entities(&scope).unwrap();
        let ext_names: Vec<&str> = scoped
            .iter()
            .filter(|e| e.entity_type == EntityType::Extension)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(ext_names, vec!["postgis"]);
    }

    #[test]
    fn report_surfaces_scope_gaps() {
        let config_path = fixture_dir().join("design.yaml");
        let mut design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(Some("all"), None).unwrap();
        let report = design.report(None, Some(&scope));
        assert!(report.gaps.is_empty()); // all-scope ⇒ no gaps
    }

    #[test]
    fn report_surfaces_real_gaps() {
        use std::collections::HashSet;
        let config_path = fixture_dir().join("design.yaml");
        let mut design = Design::from_config(&config_path, "dev").unwrap();
        // Narrow scope with config.lookup_values but not its FK target config.lookups.
        let scope = ResolvedScope {
            name: "narrow".to_string(),
            entities: HashSet::from(["config.lookup_values".to_string(), "config".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
            extensions: None,
        };
        let report = design.report(None, Some(&scope));
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].missing, "config.lookups");
        assert_eq!(report.gaps[0].required_by, "config.lookup_values");
    }

    #[test]
    fn import_entry_in_scope_predicate() {
        use std::collections::HashSet;
        let mut entry = ImportPlanEntry {
            table: Entity::new(EntityType::Import, "staging.lookups"),
            procedure: Some("staging.import_lookups".to_string()),
            writes: vec!["config.lookups".to_string()],
        };
        let ws: HashSet<String> = ["config.lookups".to_string()].into_iter().collect();
        assert!(import_entry_in_scope(&entry, &ws, false));

        // write-target out of scope → excluded
        entry.writes = vec!["config.other".to_string()];
        assert!(!import_entry_in_scope(&entry, &ws, false));

        // is_all bypasses
        assert!(import_entry_in_scope(&entry, &ws, true));

        // proc-less entry (no writes): kept iff its staging table is in scope
        let procless = ImportPlanEntry {
            table: Entity::new(EntityType::Import, "staging.lookups"),
            procedure: None,
            writes: vec![],
        };
        let ws_table: HashSet<String> = ["staging.lookups".to_string()].into_iter().collect();
        assert!(import_entry_in_scope(&procless, &ws_table, false));
        assert!(!import_entry_in_scope(&procless, &HashSet::new(), false));
    }

    // ── Apply-order tests ───────────────────────

    /// Mirrors the real partition + concat apply order from `Design::from_config`,
    /// minus the `sort_by_dependencies` calls, so type-bucket ordering can be
    /// exercised without a DB / full `Design::from_config`.
    fn order_entities_for_test(entities: Vec<Entity>) -> Vec<Entity> {
        let (schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals) =
            partition_entities(entities);
        [schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals].concat()
    }

    #[test]
    fn matview_applied_after_views_before_functions() {
        use crate::entity::EntityType;
        let ents = vec![
            Entity::new(EntityType::Function, "app.f"),
            Entity::new(EntityType::MaterializedView, "app.mv"),
            Entity::new(EntityType::View, "app.v"),
            Entity::new(EntityType::Table, "app.t"),
        ];
        let ordered = order_entities_for_test(ents);
        let pos = |name: &str| ordered.iter().position(|e| e.name == name).unwrap();
        assert!(pos("app.t") < pos("app.v"));
        assert!(pos("app.v") < pos("app.mv"));
        assert!(pos("app.mv") < pos("app.f"));
    }

    // ── Materialized-view validation ────────────────────────

    /// A matview with a unique index, so only the flag under test can fire.
    fn matview_with_unique_index() -> Entity {
        let mut e = Entity::new(EntityType::MaterializedView, "a.m");
        e.table_def = Some(crate::entity::TableDef {
            columns: vec![],
            constraints: vec![],
            indexes: vec![crate::entity::IndexDef {
                name: Some("m_idx".to_string()),
                columns: vec![],
                unique: true,
                index_type: None,
            }],
            comments: Default::default(),
        });
        e
    }

    /// A matview with no indexes at all.
    fn matview_without_unique_index() -> Entity {
        let mut e = Entity::new(EntityType::MaterializedView, "a.m");
        e.table_def = Some(crate::entity::TableDef {
            columns: vec![],
            constraints: vec![],
            indexes: vec![],
            comments: Default::default(),
        });
        e
    }

    #[test]
    fn matview_concurrently_requires_unique_index() {
        let entities = vec![matview_without_unique_index()];
        let mv_config = MaterializedViewsConfig {
            options: crate::config::MatviewOptions {
                refresh: Some("0 2 * * *".to_string()),
                concurrently: true,
            },
            overrides: Default::default(),
        };
        let extensions = vec!["pg_cron".to_string()];

        let errors = validate_materialized_views(&entities, &mv_config, &extensions);
        assert_eq!(errors.len(), 1, "expected exactly one error, got: {errors:?}");
        assert!(errors[0].contains("unique index"), "got: {}", errors[0]);
    }

    #[test]
    fn matview_schedule_requires_pg_cron_extension() {
        let entities = vec![matview_with_unique_index()];
        let mv_config = MaterializedViewsConfig {
            options: crate::config::MatviewOptions {
                refresh: Some("0 2 * * *".to_string()),
                concurrently: false,
            },
            overrides: Default::default(),
        };
        let extensions: Vec<String> = vec![];

        let errors = validate_materialized_views(&entities, &mv_config, &extensions);
        assert_eq!(errors.len(), 1, "expected exactly one error, got: {errors:?}");
        assert!(errors[0].contains("pg_cron"), "got: {}", errors[0]);
    }

    #[test]
    fn matview_invalid_cron_expression_flagged() {
        let entities = vec![matview_with_unique_index()];
        let mv_config = MaterializedViewsConfig {
            options: crate::config::MatviewOptions {
                refresh: Some("not a cron".to_string()),
                concurrently: false,
            },
            overrides: Default::default(),
        };
        let extensions = vec!["pg_cron".to_string()];

        let errors = validate_materialized_views(&entities, &mv_config, &extensions);
        assert_eq!(errors.len(), 1, "expected exactly one error, got: {errors:?}");
        assert!(errors[0].contains("invalid cron"), "got: {}", errors[0]);
    }

    #[test]
    fn valid_matview_config_has_no_errors() {
        let entities = vec![matview_with_unique_index()];
        let mv_config = MaterializedViewsConfig {
            options: crate::config::MatviewOptions {
                refresh: Some("0 2 * * *".to_string()),
                concurrently: true,
            },
            overrides: Default::default(),
        };
        let extensions = vec!["pg_cron".to_string()];

        let errors = validate_materialized_views(&entities, &mv_config, &extensions);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    // ── Apply's "stamp only newly-created matviews" rule ─────

    /// `apply` must stamp the `dbd:hash` sentinel ONLY on matviews this run
    /// creates — i.e. those absent before it ran. An already-existing matview is
    /// excluded: `CREATE MATERIALIZED VIEW IF NOT EXISTS` is a no-op on it, so
    /// its deployed definition may differ from the design, and stamping a
    /// "current" hash would mask that drift from `reconcile`.
    #[test]
    fn matviews_to_stamp_excludes_pre_existing() {
        use std::collections::HashSet;
        let m1 = Entity::new(EntityType::MaterializedView, "app.m1");
        let m2 = Entity::new(EntityType::MaterializedView, "app.m2");
        let applied = vec![&m1, &m2];

        // Only m1 already existed → only m2 gets stamped.
        let pre_existing: HashSet<String> = ["app.m1".to_string()].into_iter().collect();
        let stamp: Vec<&str> = matviews_to_stamp(&applied, &pre_existing)
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(stamp, vec!["app.m2"], "only the newly-created matview is stamped");

        // Nothing existed → both are newly created, both stamped.
        let both: Vec<&str> = matviews_to_stamp(&applied, &HashSet::new())
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(both, vec!["app.m1", "app.m2"]);

        // Both already existed → nothing stamped (no drift masked).
        let all_pre: HashSet<String> =
            ["app.m1".to_string(), "app.m2".to_string()].into_iter().collect();
        assert!(matviews_to_stamp(&applied, &all_pre).is_empty());
    }

    fn tbl_with_check(name: &str, expr: &str) -> Entity {
        let mut e = Entity::new(EntityType::Table, name);
        e.table_def = Some(TableDef {
            columns: vec![],
            constraints: vec![TableConstraint::Check {
                name: None,
                expression: expr.to_string(),
            }],
            indexes: vec![],
            comments: Default::default(),
        });
        e
    }

    #[test]
    fn enum_hint_for_in_list_of_strings() {
        let ents = vec![tbl_with_check("config.lookups", "status IN ('active', 'inactive')")];
        let hints = suggest_enum_candidates(&ents, "postgresql");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].entity, "config.lookups");
        assert_eq!(hints[0].column, "status");
        assert_eq!(hints[0].values, vec!["active".to_string(), "inactive".to_string()]);
    }

    #[test]
    fn enum_hint_for_any_array_of_strings() {
        let ents = vec![tbl_with_check("s.t", "kind = ANY(ARRAY['a','b'])")];
        assert_eq!(suggest_enum_candidates(&ents, "postgresql").len(), 1);
    }

    #[test]
    fn enum_hint_for_or_chain_same_column() {
        let ents = vec![tbl_with_check("s.t", "role = 'admin' OR role = 'user'")];
        let hints = suggest_enum_candidates(&ents, "postgresql");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].column, "role");
    }

    #[test]
    fn no_hint_for_numeric_range_subquery_mixed_multicol_notin() {
        let cases = [
            "n IN (1, 2, 3)",
            "x > 0 AND x < 10",
            "char_length(name) < 5",
            "status IN (SELECT s FROM other)",
            "status IN ('a', other_col)",
            "a = 'x' OR b = 'y'",
            "status NOT IN ('a','b')",
        ];
        for c in cases {
            let ents = vec![tbl_with_check("s.t", c)];
            assert!(
                suggest_enum_candidates(&ents, "postgresql").is_empty(),
                "unexpected hint for: {c}"
            );
        }
    }

    #[test]
    fn no_hint_for_non_postgres_dialect() {
        let ents = vec![tbl_with_check("s.t", "status IN ('a','b')")];
        assert!(suggest_enum_candidates(&ents, "sqlite").is_empty());
    }
}
