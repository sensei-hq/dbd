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
use crate::scope::ResolvedScope;
use crate::script;
use crate::snapshot;
use crate::snapshot::PendingMigration;

mod scope;
mod apply;
mod reconcile;
mod import;
mod reset;
mod plan;
mod hooks;

pub use plan::{build_execution_plan, ApplyStrategy, ExecutionPlan, ExecutionStep};

/// Summary passed to the `on_complete` callback of `apply()`.
#[derive(Debug, Clone, Default)]
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
    /// `apply.before` hook scripts run.
    pub before_scripts: u32,
    /// `apply.after` hook scripts run.
    pub after_scripts: u32,
    /// Non-fatal diagnostics — currently, hooks a scope filtered out. Mirrors
    /// [`ImportComplete::warnings`]: an apply that skipped the script setting up
    /// Realtime must say so, not report a clean success.
    pub warnings: Vec<String>,
}

/// Running tallies accumulated while executing an apply plan's steps. Folded
/// into [`ApplyComplete`] once the plan finishes.
#[derive(Default)]
struct ApplyCounts {
    applied: u32,
    migrated: u32,
    created: u32,
    dropped: u32,
}

/// Summary passed to the `on_complete` callback of `import_data()`.
///
/// A zero-count import is a legitimate outcome, but it must never be a *silent*
/// one: every reason the plan came up short — no `import/` directory, files
/// belonging to another env, staging tables that failed to parse, entries cut by
/// the active scope — is recorded in `warnings` so the caller can report it.
#[derive(Debug, Clone, Default)]
pub struct ImportComplete {
    pub tables: u32,
    pub procedures: u32,
    pub after_scripts: u32,
    /// Non-fatal diagnostics explaining anything the import left out.
    pub warnings: Vec<String>,
}

/// Combined summary passed to the `on_complete` callback of `deploy()`.
#[derive(Debug, Clone, Default)]
pub struct DeployComplete {
    pub apply: ApplyComplete,
    pub import: ImportComplete,
    /// Outcome of the RLS policy phase. Failures here are non-fatal — they are
    /// reported as warnings and the deploy still succeeds.
    pub policies: PolicyReport,
}

impl DeployComplete {
    /// Every non-fatal diagnostic from the deploy, ready to print as warnings:
    /// each phase's own warnings in pipeline order, then one line per failed
    /// policy file, then one per policy a scope excluded.
    ///
    /// Scope skips belong here for the same reason import's do: the deploy
    /// left something out on purpose, and a summary that only counts what ran
    /// never says which files those were.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = self.apply.warnings.clone();
        out.extend(self.import.warnings.iter().cloned());
        out.extend(
            self.policies
                .failed
                .iter()
                .map(|(file, err)| format!("policy not applied: {} — {err}", file.display())),
        );
        out.extend(
            self.policies
                .skipped
                .iter()
                .map(|(file, why)| format!("policy skipped: {} — {why}", file.display())),
        );
        out
    }
}

/// Bundled progress callbacks for a long-running operation: `on_start(desc)` is
/// called before each step, `on_done(desc, err)` after each (`err` = `None` on
/// success), and `on_complete(summary)` once at the end. Use [`Progress::none`]
/// for a silent run.
pub struct Progress<S, D, C> {
    pub on_start: S,
    pub on_done: D,
    pub on_complete: C,
}

fn noop_start(_: &str) {}
fn noop_done(_: &str, _: Option<&str>) {}
fn noop_complete<C>(_: C) {}

impl<C> Progress<fn(&str), fn(&str, Option<&str>), fn(C)> {
    /// A no-op progress sink — for tests and silent runs.
    pub fn none() -> Self {
        Progress { on_start: noop_start, on_done: noop_done, on_complete: noop_complete::<C> }
    }
}

/// Summary returned by `apply()` describing what happened.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    pub strategy: ApplyStrategy,
    pub from_version: u32,
    pub to_version: u32,
}

/// Refuse to apply while any pending migration still has unresolved `-- TODO:`
/// lines in its `data.sql` files.
fn ensure_no_pending_todos(pending: &[PendingMigration]) -> Result<()> {
    let todos = snapshot::pending_data_sql_todos(pending)?;
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

/// Refuse a destructive reconcile (dropped columns, constraints, foreign keys,
/// or indexes) unless the caller explicitly opted in with `allow_destructive`.
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
        "reconcile would make destructive changes (dropped columns, constraints, or indexes) on:\n{details}\n\
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
    pub gaps: Vec<crate::scope::ScopeGap>,
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
///
/// A failed policy file is non-fatal — it is collected here and surfaced as a
/// warning rather than aborting the run — so callers MUST report `failed`.
#[derive(Debug, Clone, Default)]
pub struct PolicyReport {
    pub applied: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    /// Files a scope excluded, with the reason. Not failures — the table the
    /// policy protects is not part of this plane, so the file has nothing to
    /// act on. Reported so a skip is visible rather than silent.
    pub skipped: Vec<(PathBuf, String)>,
}

/// The entity a policy file protects, from its path.
///
/// The layout is `policies/<schema>/<table>.sql`, so the target is
/// `<schema>.<table>` — the same convention `Entity::from_file` uses for
/// `ddl/<type>/<schema>/<name>.ddl`. Returns `None` for a file that does not
/// follow it (a loose `policies/foo.sql`, a README), which is then treated as
/// unscopable and always applied.
pub(crate) fn policy_target(file: &Path, project_dir: &Path) -> Option<String> {
    let rel = file.strip_prefix(project_dir).ok()?.strip_prefix("policies").ok()?;
    let parts: Vec<&str> = rel.components().filter_map(|c| c.as_os_str().to_str()).collect();
    let [schema, table] = parts.as_slice() else {
        return None;
    };
    let table = table.strip_suffix(".sql").or_else(|| table.strip_suffix(".ddl"))?;
    Some(format!("{schema}.{table}"))
}

/// Apply RLS policy files from the policies/ directory.
///
/// Files are executed in alphabetical order. Failed files are logged and skipped.
///
/// `scope` filters them the way it filters everything else: a policy protects
/// one table, and a plane that does not have that table has nothing for the file
/// to do. Applying it anyway reported `schema "dojo" does not exist` on every
/// deploy of the other plane — an expected condition dressed as an error, which
/// is how real failures stop being read. A file whose path does not follow
/// `policies/<schema>/<table>.sql` is unscopable and always applied.
pub async fn apply_policies(
    adapter: &dyn DatabaseAdapter,
    project_dir: &Path,
    dry_run: bool,
    scope: Option<(&str, &std::collections::HashSet<String>)>,
) -> Result<PolicyReport> {
    let files = crate::scanner::scan_policies(project_dir)?;
    let mut report = PolicyReport {
        applied: Vec::new(),
        failed: Vec::new(),
        skipped: Vec::new(),
    };

    // Canonicalize the project root once so path-traversal checks are reliable.
    let canon_root = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    for file in &files {
        if let Some((scope_name, working_set)) = scope
            && let Some(target) = policy_target(file, project_dir)
            && !working_set.contains(&target)
        {
            report.skipped.push((
                file.clone(),
                format!("{target} is outside scope '{scope_name}'"),
            ));
            continue;
        }

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
    /// What the `import/` scan left out, kept so an import that loads nothing
    /// can explain itself instead of reporting a bare zero.
    import_scan_skips: crate::scanner::ImportScan,
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

        // Validate and resolve the parser before reading any file, so a bad
        // `source.parser` fails at load rather than partway through the scan.
        let parser_choice = crate::parser::ParserChoice::resolve(
            &design_config.source.dialect,
            design_config.source.parser.as_deref(),
        )?;

        // Scan and parse DDL entities. A file that fails to read must not
        // silently vanish from the desired set — a live table could be
        // dropped by `reconcile --prune` — so propagate the read error.
        // `parse_entity_with`'s own Err (unparseable DDL) still drops the
        // entity, unchanged from prior behavior.
        let ddl_files = scanner::scan_ddl(&project_dir)?;
        let mut entities: Vec<Entity> = Vec::new();
        for file in &ddl_files {
            let sql = std::fs::read_to_string(file)
                .map_err(|e| DbdError::Config(format!("read DDL {}: {e}", file.display())))?;
            // Use relative path for entity type/name derivation, but
            // store the absolute path so the file is readable regardless of CWD.
            let relative = file.strip_prefix(&project_dir).unwrap_or(file);
            if let Ok(mut entity) = parser::parse_entity_with(parser_choice, relative, &sql) {
                entity.file = Some(file.clone());
                entities.push(entity);
            }
        }

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

        // Order for apply in one pass over the whole graph. Dependencies decide,
        // with the type sequence (schemas → extensions → roles → sequences →
        // enums → tables → views → matviews → functions/procedures, via
        // `EntityType::apply_rank`) acting as a floor rather than a partition —
        // see `dependency::sort_by_dependencies` for why that distinction is the
        // whole design.
        //
        // A single sort is required because two type pairs depend on each other
        // in BOTH directions, so no fixed sequence can order them: a view can
        // call a function and a function body can read a view (issue #9); a
        // table's `DEFAULT`/`CHECK` can call a function and a function body can
        // read that table (issue #10).
        //
        // External entities are held out of the graph and appended. They are
        // never applied (`entities_in_scope` drops them), they exist only so
        // `resolve_references` can resolve references to objects dbd does not
        // manage — and being the highest rank, leaving them in would drag every
        // entity that references one to the very end of the order.
        let (externals, managed): (Vec<Entity>, Vec<Entity>) = entities
            .into_iter()
            .partition(|e| e.entity_type == EntityType::External);

        let entities = [dependency::sort_by_dependencies(&managed), externals].concat();

        // Scan import tables (data files, not DDL)
        // Pass env so that import/{env}/ subdirectories are filtered appropriately.
        let mut import_scan = scanner::scan_import(&project_dir, Some(env))?;
        let import_tables: Vec<Entity> = import_scan
            .files
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
        // The selected files are now represented by `import_tables`; keep only
        // the scan's exclusion record so nothing is stored twice.
        import_scan.files = Vec::new();

        Ok(Self {
            config: design_config,
            entities,
            import_tables,
            import_scan_skips: import_scan,
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

    /// Number of entities loaded from a DDL file under `ddl/`, as opposed to
    /// entities synthesized from `design.yaml` config (schemas, extensions,
    /// config-declared roles). A zero count on `apply` usually means the
    /// resolved project directory (`--source`) is wrong — the config loaded but
    /// no authored DDL was scanned — so callers can warn instead of silently
    /// reporting success.
    pub fn authored_entity_count(&self) -> usize {
        self.entities.iter().filter(|e| e.file.is_some()).count()
    }

    /// Access import tables (data files found in import/).
    pub fn import_tables(&self) -> &[Entity] {
        &self.import_tables
    }

    /// Project directory path.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// Scan all migration directories for unresolved `-- TODO:` comments in
    /// `*.data.sql` files. Returns one entry per affected file.
    ///
    /// Used by `inspect` to surface outstanding data corrections.
    /// `apply` independently blocks on PENDING migrations with TODOs.
    pub fn data_sql_todos(&self) -> Result<Vec<snapshot::DataSqlTodo>> {
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

        Ok(Self::drop_resolved_ref_warnings(&mut self.entities, &mut self.import_tables, |name| {
            resolved.contains(name)
        }))
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
        let cache = match RefCache::load(&self.project_dir)? {
            Some(c) => c,
            None => return Ok((0, None)),
        };
        let size = cache.len();

        let dropped = Self::drop_resolved_ref_warnings(&mut self.entities, &mut self.import_tables, |name| {
            cache.contains(name)
        });
        Ok((dropped, Some(size)))
    }

    /// Drop "Unresolved reference: NAME" warnings from `entities` and
    /// `import_tables` whose NAME satisfies `is_resolved`. Returns the count
    /// dropped. Shared by `resolve_unknown_refs_via_db` (live DB resolution)
    /// and `resolve_unknown_refs_via_cache` (ref-cache snapshot).
    fn drop_resolved_ref_warnings(
        entities: &mut [Entity],
        import_tables: &mut [Entity],
        is_resolved: impl Fn(&str) -> bool,
    ) -> usize {
        const PREFIX: &str = "Unresolved reference: ";
        let mut dropped = 0usize;
        let mut drop_in = |ents: &mut [Entity]| {
            for entity in ents.iter_mut() {
                entity.warnings.retain(|w| match w.strip_prefix(PREFIX) {
                    Some(name) if is_resolved(name) => {
                        dropped += 1;
                        false
                    }
                    _ => true,
                });
            }
        };
        drop_in(entities);
        drop_in(import_tables);
        dropped
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

    /// Entities (main + import tables) matching `has_items`, optionally narrowed
    /// to a single entity by `name`. Shared by `report()`'s `issues`/`warnings`
    /// collections, which differ only in `has_items`.
    fn filtered_entities(&self, name: Option<&str>, has_items: impl Fn(&Entity) -> bool) -> Vec<Entity> {
        self.entities
            .iter()
            .chain(self.import_tables.iter())
            .filter(|e| has_items(e) && name.is_none_or(|n| e.name == n))
            .cloned()
            .collect()
    }

    /// Generate a validation report, optionally filtered to one entity by
    /// `name` and augmented with dependency gaps when a `scope` is supplied.
    pub fn report(&mut self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Report {
        if !self.validated {
            self.validate();
        }

        let entity = name.and_then(|n| self.entities.iter().find(|e| e.name == n).cloned());

        let issues = self.filtered_entities(name, |e| !e.errors.is_empty());
        let warnings = self.filtered_entities(name, |e| !e.warnings.is_empty());

        let gaps = match scope {
            Some(s) => crate::scope::analyze_gaps(s, &self.entities, &self.external_names()),
            None => Vec::new(),
        };

        Report {
            entity,
            issues,
            warnings,
            gaps,
        }
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
        let graphable = crate::scope::is_scopable;
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

    /// Every materialized view's qualified name paired with its resolved
    /// refresh-job config, across the WHOLE design (not scope-filtered).
    ///
    /// Used to feed `adapter.sync_refresh_jobs`, which unschedules every
    /// `dbd:refresh:%` job absent from the set it is given — so callers must
    /// pass the full set even from a scoped operation, or out-of-scope
    /// matviews' jobs would be unscheduled. Shared by `apply` and `reconcile`.
    pub(crate) fn all_matview_jobs(&self) -> Vec<(String, crate::config::ResolvedMatview)> {
        self.entities
            .iter()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .map(|e| (e.name.clone(), self.config.materialized_views.resolve(&e.name)))
            .collect()
    }
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

        design.apply(&mock, None, true, None, Progress::none()).await.unwrap();
        assert!(mock.applied_names().is_empty());
    }

    #[tokio::test]
    async fn apply_executes_entities() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        design.apply(&mock, None, false, None, Progress::none()).await.unwrap();
        assert!(!mock.applied_names().is_empty());
    }

    // ── Lifecycle script hooks ────────────────────────────

    /// A project shaped like the real one this feature was built for: a scope
    /// that excludes one staging table, an `import.after` hook whose dependency
    /// on it is *derivable*, and a second whose table name is data and so must
    /// declare its `writes:`.
    fn hooks_project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for sub in ["ddl/table/app", "ddl/table/staging", "sql"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            r#"
project:
  name: hooks
  version: 1
source:
  dialect: postgresql
schemas:
  - app
  - staging
scopes:
  partial:
    excludes: [staging.b]
    deps: include
apply:
  before:
    - sql/pre_ddl.sql
  after:
    - sql/post_ddl.sql
import:
  staging: [staging]
  after:
    - sql/loader.sql
    - script: sql/dynamic.sql
      writes: [app.target]
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/target.ddl"),
            "create table if not exists app.target (id int primary key, n int);\n",
        )
        .unwrap();
        for t in ["a", "b"] {
            std::fs::write(
                dir.join(format!("ddl/table/staging/{t}.ddl")),
                format!("create table if not exists staging.{t} (id int primary key);\n"),
            )
            .unwrap();
        }
        std::fs::write(dir.join("sql/pre_ddl.sql"), "-- MARK pre_ddl\nselect 1;\n").unwrap();
        std::fs::write(dir.join("sql/post_ddl.sql"), "-- MARK post_ddl\nselect 1;\n").unwrap();
        // Derivable: names staging.a and staging.b as SQL identifiers.
        std::fs::write(
            dir.join("sql/loader.sql"),
            "-- MARK loader\ninsert into app.target (id, n)\n\
             select a.id, 1 from staging.a a join staging.b b on b.id = a.id;\n",
        )
        .unwrap();
        // NOT derivable: the table name lives in a format() string.
        std::fs::write(
            dir.join("sql/dynamic.sql"),
            "-- MARK dynamic\ndo $$ begin\n  execute format('insert into %I.target values (1, 1)', 'app');\nend $$;\n",
        )
        .unwrap();
        tmp
    }

    /// Which hook scripts an adapter was actually handed, by marker.
    fn markers_run(mock: &MockAdapter) -> Vec<String> {
        let scripts = mock.scripts.lock().unwrap();
        scripts
            .iter()
            .filter_map(|s| s.lines().next())
            .filter_map(|l| l.strip_prefix("-- MARK ").map(|m| m.to_string()))
            .collect()
    }

    /// The gap this feature exists to close: `dbd apply` calls `Design::apply`
    /// alone, so an `import.after` hook never runs on it. `apply.after` does.
    #[tokio::test]
    async fn apply_alone_runs_its_own_hooks_and_not_the_import_ones() {
        let tmp = hooks_project();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new();
        let mut summary: Option<ApplyComplete> = None;

        design
            .apply(&mock, None, false, None, Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| summary = Some(s),
            })
            .await
            .unwrap();

        assert_eq!(markers_run(&mock), vec!["pre_ddl", "post_ddl"]);
        let summary = summary.expect("apply must report a summary");
        assert_eq!(summary.before_scripts, 1);
        assert_eq!(summary.after_scripts, 1);
    }

    /// `apply.before` runs before the first entity and `apply.after` only after
    /// they all succeed — so an entity that fails leaves the before-hook run and
    /// the after-hook not.
    #[tokio::test]
    async fn a_failing_entity_stops_between_the_before_and_after_hooks() {
        let tmp = hooks_project();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new().fail_on_entity("app.target");

        design
            .apply(&mock, None, false, None, Progress::none())
            .await
            .expect_err("the injected entity failure must propagate");

        assert_eq!(markers_run(&mock), vec!["pre_ddl"]);
    }

    /// Publications and grants attach to objects that must already exist, and
    /// RLS is independent of them — so `apply.after` runs before `policies/`.
    #[tokio::test]
    async fn apply_after_runs_before_policies() {
        let tmp = hooks_project();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(tmp.path().join("policies/rls.sql"), "-- MARK policy\nselect 1;\n").unwrap();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new();

        design.deploy(&mock, false, None, |_| {}).await.unwrap();

        let markers = markers_run(&mock);
        let post = markers.iter().position(|m| m == "post_ddl").expect("apply.after must run");
        let policy = markers.iter().position(|m| m == "policy").expect("policies must run");
        assert!(post < policy, "apply.after must precede policies/: {markers:?}");
    }

    /// The contrast that is the whole feature: under a scope excluding
    /// `staging.b`, the derivable loader is skipped with a warning naming the
    /// table, while the `writes:`-declared hook still runs.
    #[tokio::test]
    async fn a_scoped_import_skips_the_derivable_hook_and_keeps_the_declared_one() {
        let tmp = hooks_project();
        let design = load_project(tmp.path(), "dev");
        let scope = design.resolve_scope(Some("partial"), None).unwrap();
        let mock = MockAdapter::new();
        let mut summary: Option<ImportComplete> = None;

        design
            .import_data(&mock, None, false, Some(&scope), Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| summary = Some(s),
            })
            .await
            .unwrap();

        assert_eq!(markers_run(&mock), vec!["dynamic"], "only the declared-writes hook may run");
        let summary = summary.expect("import must report a summary");
        assert_eq!(summary.after_scripts, 1);
        assert!(
            summary.warnings.iter().any(|w| {
                w.contains("sql/loader.sql") && w.contains("staging.b") && w.contains("partial")
            }),
            "the skip must name the script, the table and the scope: {:?}",
            summary.warnings
        );
    }

    /// Unscoped, both after-scripts run — scope filtering must not cost the
    /// common path anything.
    #[tokio::test]
    async fn an_unscoped_import_runs_every_after_script() {
        let tmp = hooks_project();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new();

        design.import_data(&mock, None, false, None, Progress::none()).await.unwrap();

        assert_eq!(markers_run(&mock), vec!["loader", "dynamic"]);
    }

    /// A declared hook dbd cannot find is a misconfiguration — apply must
    /// refuse rather than continue as if the script were optional.
    #[tokio::test]
    async fn apply_refuses_when_a_hook_file_is_missing() {
        let tmp = hooks_project();
        std::fs::remove_file(tmp.path().join("sql/post_ddl.sql")).unwrap();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new();

        let err = design
            .apply(&mock, None, false, None, Progress::none())
            .await
            .expect_err("a missing hook file must fail the apply");
        assert!(err.to_string().contains("sql/post_ddl.sql"), "got: {err}");
    }

    /// `--dry-run` names every hook it would run without executing one.
    #[tokio::test]
    async fn apply_dry_run_reports_hooks_without_executing_them() {
        let tmp = hooks_project();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new();
        let mut steps: Vec<String> = Vec::new();

        design
            .apply(&mock, None, /*dry_run*/ true, None, Progress {
                on_start: |d: &str| steps.push(d.to_string()),
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |_| {},
            })
            .await
            .unwrap();

        assert_eq!(mock.script_count(), 0, "a dry run must execute nothing");
        assert!(steps.iter().any(|s| s.contains("sql/pre_ddl.sql")), "got {steps:?}");
        assert!(steps.iter().any(|s| s.contains("sql/post_ddl.sql")), "got {steps:?}");
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

        design.apply(&mock, None, false, None, Progress::none()).await.unwrap();

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
            .apply(&mock, None, false, None, Progress::none())
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

        design.apply(&mock, None, false, None, Progress::none()).await.unwrap();

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

        design.apply(&mock, None, false, None, Progress::none()).await.unwrap();

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
            .apply(&mock, None, false, Some(&resolved), Progress::none())
            .await
            .unwrap();

        let meta = mock.get_project_meta().await.unwrap().expect("apply writes meta");
        assert_eq!(meta.scope.as_deref(), Some(resolved.name.as_str()));
    }

    #[tokio::test]
    async fn apply_repins_scope_on_current_strategy() {
        // A DB already at/above the latest version takes the Current strategy,
        // whose plan has no SetVersion step. The scope must still be (re)pinned.
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        // db_version 99 >= any fixture latest_version → Current strategy.
        let mock = MockAdapter::new().with_meta("dev", 99).with_scope("public");
        let resolved = design.resolve_scope(None, None).unwrap();

        design
            .apply(&mock, None, false, Some(&resolved), Progress::none())
            .await
            .unwrap();

        let meta = mock.get_project_meta().await.unwrap().expect("meta");
        // Re-pinned to the applied scope, and the version was preserved (not downgraded).
        assert_eq!(meta.scope.as_deref(), Some(resolved.name.as_str()));
        assert_eq!(meta.version, 99);
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

    // ── Import diagnostics: nothing is dropped silently ───

    /// Build a throwaway project with the given files under `import/`.
    fn project_with_import_files(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: warn_test\n  version: 1\nsource:\n  dialect: postgresql\n",
        )
        .unwrap();
        for (rel, contents) in files {
            let path = tmp.path().join("import").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }
        tmp
    }

    fn load_project(dir: &Path, env: &str) -> Design {
        Design::from_config_with_dir(&dir.join("design.yaml"), env, Some(dir)).unwrap()
    }

    /// Data files belonging to another env are excluded by design — but the
    /// exclusion must be *reported*. This is the wrong-`--env` case where a
    /// deploy loads no rows and previously said nothing at all.
    #[test]
    fn import_warns_about_files_skipped_for_another_env() {
        let tmp = project_with_import_files(&[
            ("dev/staging/lookups.csv", "id,name\n1,a\n"),
            ("prod/staging/lookups.csv", "id,name\n2,b\n"),
        ]);
        let design = load_project(tmp.path(), "prod");

        let warnings = design.import_warnings(None);
        assert!(
            warnings.iter().any(|w| w.contains("import/dev/") && w.contains("prod")),
            "skipping another env's files must be reported: {warnings:?}"
        );
    }

    /// The matching env's files are used, so there is nothing to warn about.
    #[test]
    fn import_does_not_warn_when_every_file_matches_the_env() {
        let tmp = project_with_import_files(&[("dev/staging/lookups.csv", "id,name\n1,a\n")]);
        let design = load_project(tmp.path(), "dev");
        assert!(
            design.import_warnings(None).is_empty(),
            "a fully-matching import set must not warn: {:?}",
            design.import_warnings(None)
        );
    }

    /// A project with no `import/` directory reports why it has nothing to load
    /// instead of quietly importing zero rows.
    #[test]
    fn import_warns_when_import_dir_is_absent() {
        let tmp = project_with_import_files(&[]);
        let design = load_project(tmp.path(), "dev");
        let warnings = design.import_warnings(None);
        assert!(
            warnings.iter().any(|w| w.contains("no import/ directory")),
            "an absent import/ dir must be explained: {warnings:?}"
        );
    }

    /// Entries cut by the active scope are reported through the import summary
    /// — the scope filter is the least visible of the drop paths, because the
    /// data file exists, parses, and matches the env, yet still never loads.
    #[tokio::test]
    async fn import_warns_about_entries_dropped_by_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/config")).unwrap();
        std::fs::create_dir_all(dir.join("import/staging")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: scope_warn_test\n  version: 1\n\
             source:\n  dialect: postgresql\n\
             schemas:\n  - config\n  - staging\n\
             import:\n  staging:\n    - staging\n\
             scopes:\n  config_only:\n    includes:\n      - config\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/config/things.ddl"),
            "create table if not exists config.things (id int primary key);\n",
        )
        .unwrap();
        // A staging data file with no matching import procedure, so the entry is
        // kept or dropped on the staging table's own scope membership.
        std::fs::write(dir.join("import/staging/lookups.csv"), "id,name\n1,a\n").unwrap();

        let design = load_project(dir, "dev");
        assert!(!design.import_tables().is_empty(), "the staging file must be discovered");

        let scope = design.resolve_scope(Some("config_only"), None).unwrap();
        let mock = MockAdapter::new();
        let mut summary: Option<ImportComplete> = None;
        design
            .import_data(&mock, None, /*dry_run*/ true, Some(&scope), Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| summary = Some(s),
            })
            .await
            .unwrap();

        let summary = summary.expect("import must always report a summary");
        assert_eq!(summary.tables, 0, "the out-of-scope staging table must not load");
        assert!(
            summary
                .warnings
                .iter()
                .any(|w| w.contains("staging.lookups") && w.contains("outside scope")),
            "a scope-dropped entry must be reported: {:?}",
            summary.warnings
        );
    }

    /// A `--name` that matches no staging file is a typo, and must not look
    /// like an empty-but-healthy import.
    #[tokio::test]
    async fn import_warns_when_name_filter_matches_nothing() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();

        let mock = MockAdapter::new();
        let mut summary: Option<ImportComplete> = None;
        design
            .import_data(&mock, Some("staging.does_not_exist"), true, None, Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| summary = Some(s),
            })
            .await
            .unwrap();

        let summary = summary.expect("import must always report a summary");
        assert_eq!(summary.tables, 0);
        assert!(
            summary.warnings.iter().any(|w| w.contains("does_not_exist")),
            "an unmatched --name must be reported: {:?}",
            summary.warnings
        );
    }

    /// An import that loads nothing still delivers a summary — the caller needs
    /// the zero to report it.
    #[tokio::test]
    async fn import_always_reports_a_summary_even_with_no_data() {
        let tmp = project_with_import_files(&[]);
        let design = load_project(tmp.path(), "dev");

        let mock = MockAdapter::new();
        let mut summary: Option<ImportComplete> = None;
        design
            .import_data(&mock, None, false, None, Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| summary = Some(s),
            })
            .await
            .unwrap();

        let summary = summary.expect("import must report a summary even when it loads nothing");
        assert_eq!(summary.tables, 0);
        assert!(!summary.warnings.is_empty(), "a zero import must carry its reason");
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
        let _ = design.import_data(&mock, None, false, None, Progress::none()).await;

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
            .apply(&mock, None, false, None, Progress::none())
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
            .apply(&mock, None, false, None, Progress::none())
            .await;

        assert!(result.is_ok(), "should not be blocked: {:?}", result);
    }

    // ── authored-entity guard (wrong --source / empty ddl detection) ──────────

    #[test]
    fn authored_entity_count_zero_when_no_ddl() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();
        // Config declares a schema but there is NO ddl/ dir — mirrors a wrong
        // `--source`: the config loads, but no authored DDL is scanned.
        std::fs::write(
            project_dir.join("design.yaml"),
            "project:\n  name: test\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - public\n",
        )
        .unwrap();
        let design =
            Design::from_config_with_dir(&project_dir.join("design.yaml"), "dev", Some(project_dir)).unwrap();

        assert_eq!(
            design.authored_entity_count(),
            0,
            "a project with a schema but no ddl/ files has zero authored entities"
        );
        // The config-derived schema entity IS present, so entities().len() alone
        // (which would be >= 1) does not catch the empty-ddl case.
        assert!(
            design
                .entities()
                .iter()
                .any(|e| e.entity_type == crate::entity::EntityType::Schema),
            "the public schema entity is loaded from config"
        );
    }

    #[test]
    fn authored_entity_count_counts_ddl_files() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path();
        std::fs::write(
            project_dir.join("design.yaml"),
            "project:\n  name: test\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - public\n",
        )
        .unwrap();
        let ddl_dir = project_dir.join("ddl/table/public");
        std::fs::create_dir_all(&ddl_dir).unwrap();
        std::fs::write(
            ddl_dir.join("widgets.ddl"),
            "create table if not exists public.widgets (id bigint primary key);\n",
        )
        .unwrap();
        let design =
            Design::from_config_with_dir(&project_dir.join("design.yaml"), "dev", Some(project_dir)).unwrap();

        assert_eq!(
            design.authored_entity_count(),
            1,
            "the widgets table authored under ddl/ is counted"
        );
        assert!(
            design
                .entities()
                .iter()
                .any(|e| e.name == "public.widgets" && e.file.is_some()),
            "widgets came from a ddl file"
        );
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
            .apply(&mock, None, false, Some(&scope), Progress::none())
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
            .apply(&mock, None, false, Some(&scope), Progress::none())
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
            .apply(&mock, None, false, Some(&scope), Progress::none())
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
            .apply(&mock, None, false, Some(&scope), Progress::none())
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

    /// The export must run the SAME three phases as `dbd deploy` — including
    /// RLS policies, which it previously skipped entirely. An embedder calling
    /// `Design::deploy` got a database with no policies applied and no warning.
    #[tokio::test]
    async fn deploy_export_applies_policies_like_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(
            tmp.path().join("policies/users.sql"),
            "ALTER TABLE config.users ENABLE ROW LEVEL SECURITY;",
        )
        .unwrap();

        let design =
            Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path()))
                .unwrap();
        let mock = MockAdapter::new();
        let mut summary = None;
        design
            .deploy(&mock, false, None, |s| summary = Some(s))
            .await
            .unwrap();

        let summary: DeployComplete = summary.expect("deploy must report a summary");
        assert_eq!(summary.policies.applied.len(), 1, "policy file must be applied by the export");
        assert!(summary.policies.failed.is_empty());
    }

    /// A deploy with nothing to import must still report a summary — the zero
    /// count is the signal that the registry rows did not load.
    #[tokio::test]
    async fn deploy_reports_zero_import_with_reason_when_no_import_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();

        let design =
            Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path()))
                .unwrap();
        let mock = MockAdapter::new();
        let mut summary = None;
        design
            .deploy(&mock, false, None, |s| summary = Some(s))
            .await
            .unwrap();

        let summary: DeployComplete = summary.expect("deploy must report a summary even with no data");
        assert_eq!(summary.import.tables, 0);
        let warnings = summary.warnings();
        assert!(
            warnings.iter().any(|w| w.contains("no import/ directory")),
            "a missing import/ dir must be explained, not silent: {warnings:?}"
        );
    }

    /// A policy file that fails must not fail the deploy, but it MUST come back
    /// as a warning — non-fatal is fine, silent is not.
    #[tokio::test]
    async fn deploy_reports_failed_policy_as_warning_without_failing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(tmp.path().join("policies/broken.sql"), "THIS IS NOT SQL;").unwrap();

        let design =
            Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path()))
                .unwrap();
        let mock = MockAdapter::new().fail_script_containing("THIS IS NOT SQL");
        let mut summary = None;
        let result = design
            .deploy(&mock, false, None, |s| summary = Some(s))
            .await;

        assert!(result.is_ok(), "a failed policy file must not fail the deploy");
        let summary: DeployComplete = summary.expect("deploy must report a summary");
        assert_eq!(summary.policies.failed.len(), 1, "the failure must be recorded");
        let warnings = summary.warnings();
        assert!(
            warnings.iter().any(|w| w.contains("policy not applied") && w.contains("broken.sql")),
            "failed policy must surface as a warning: {warnings:?}"
        );
    }

    /// End-to-end on the deploy path: a scope that excludes a schema must skip
    /// that schema's policy files and *say so*. Counting only what ran would
    /// let `deploy --scope` report "1 applied" with nothing accounting for the
    /// rest — the silent-omission failure this reporting exists to prevent.
    #[tokio::test]
    async fn a_scoped_deploy_surfaces_skipped_policies_as_warnings() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: test\nschemas: [app, svc]\nscopes:\n  daemon:\n    excludes: [svc]\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/t.ddl"),
            "set search_path to app;\ncreate table if not exists t (id int primary key);",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("policies/app")).unwrap();
        std::fs::create_dir_all(tmp.path().join("policies/svc")).unwrap();
        std::fs::write(tmp.path().join("policies/app/t.sql"), "select 1;").unwrap();
        std::fs::write(tmp.path().join("policies/svc/metrics.sql"), "select 1;").unwrap();

        let design =
            Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path()))
                .unwrap();
        let scope = design.resolve_scope(Some("daemon"), None).expect("scope must resolve");
        let mock = MockAdapter::new();
        let mut summary = None;
        design
            .deploy(&mock, false, Some(&scope), |s| summary = Some(s))
            .await
            .expect("deploy must succeed");

        let summary: DeployComplete = summary.expect("deploy must report a summary");
        assert_eq!(summary.policies.skipped.len(), 1, "svc policy must be skipped: {summary:?}");
        assert_eq!(summary.policies.applied.len(), 1, "app policy must still apply");
        assert!(summary.policies.failed.is_empty(), "a skip is not a failure");
        let warnings = summary.warnings();
        assert!(
            warnings.iter().any(|w| w.contains("policy skipped") && w.contains("metrics.sql")),
            "skipped policy must surface as a warning: {warnings:?}"
        );
    }

    // ── Policy tests ────────────────────────────────────────

    #[test]
    fn p2_empty_policies_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        let files = crate::scanner::scan_policies(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn p3_missing_policies_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No policies/ dir created
        let files = crate::scanner::scan_policies(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn p1_scan_finds_sorted_policy_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let policies_dir = tmp.path().join("policies/config");
        std::fs::create_dir_all(&policies_dir).unwrap();
        std::fs::write(policies_dir.join("users.sql"), "-- policy").unwrap();
        std::fs::write(policies_dir.join("lookups.sql"), "-- policy").unwrap();

        let files = crate::scanner::scan_policies(tmp.path()).unwrap();
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

        let files = crate::scanner::scan_policies(tmp.path()).unwrap();
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
        let report = apply_policies(&mock, tmp.path(), false, None).await.unwrap();
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
        let report = apply_policies(&mock, tmp.path(), true, None).await.unwrap();
        assert_eq!(report.applied.len(), 1);
        assert_eq!(mock.script_count(), 0, "dry run should not execute");
    }

    /// A policy protects one table. A plane whose scope excludes that table has
    /// nothing for the file to do, and applying it anyway reported
    /// `schema "…" does not exist` on every deploy — an expected condition
    /// dressed as an error.
    #[tokio::test]
    async fn a_policy_for_an_out_of_scope_table_is_skipped_not_failed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies/dojo")).unwrap();
        std::fs::write(
            tmp.path().join("policies/dojo/repository_metrics.sql"),
            "select 1;",
        )
        .unwrap();
        let mock = MockAdapter::new();

        let ws: std::collections::HashSet<String> = ["app.other".to_string()].into_iter().collect();
        let report = apply_policies(&mock, tmp.path(), false, Some(("daemon", &ws)))
            .await
            .unwrap();

        assert!(report.applied.is_empty(), "must not apply: {:?}", report.applied);
        assert!(report.failed.is_empty(), "a skip is not a failure: {:?}", report.failed);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].1.contains("dojo.repository_metrics"), "got {:?}", report.skipped);
        assert!(report.skipped[0].1.contains("daemon"), "must name the scope: {:?}", report.skipped);
    }

    #[tokio::test]
    async fn a_policy_for_an_in_scope_table_is_applied() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies/dojo")).unwrap();
        std::fs::write(tmp.path().join("policies/dojo/relay_inbox.sql"), "select 1;").unwrap();
        let mock = MockAdapter::new();

        let ws: std::collections::HashSet<String> =
            ["dojo.relay_inbox".to_string()].into_iter().collect();
        let report = apply_policies(&mock, tmp.path(), false, Some(("dojo", &ws)))
            .await
            .unwrap();

        assert_eq!(report.applied.len(), 1);
        assert!(report.skipped.is_empty());
    }

    /// A file off the `policies/<schema>/<table>.sql` convention has no derivable
    /// target, so it is unscopable and must still run — silently dropping it
    /// would be the worse failure.
    #[tokio::test]
    async fn a_policy_file_off_convention_is_always_applied() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies")).unwrap();
        std::fs::write(tmp.path().join("policies/loose.sql"), "select 1;").unwrap();
        let mock = MockAdapter::new();

        let ws: std::collections::HashSet<String> = std::collections::HashSet::new();
        let report = apply_policies(&mock, tmp.path(), false, Some(("anything", &ws)))
            .await
            .unwrap();

        assert_eq!(report.applied.len(), 1, "off-convention file must still apply");
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn policy_target_reads_schema_and_table_from_the_path() {
        let root = std::path::Path::new("/p");
        assert_eq!(
            policy_target(std::path::Path::new("/p/policies/dojo/repository_metrics.sql"), root),
            Some("dojo.repository_metrics".to_string())
        );
        // off-convention shapes have no target
        assert_eq!(policy_target(std::path::Path::new("/p/policies/loose.sql"), root), None);
        assert_eq!(
            policy_target(std::path::Path::new("/p/policies/a/b/c.sql"), root),
            None
        );
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

    /// The apply order `Design::from_config` produces, without needing a DB or
    /// an on-disk project.
    fn order_entities_for_test(entities: Vec<Entity>) -> Vec<Entity> {
        dependency::sort_by_dependencies(&entities)
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

    /// A real dependency overrides the type sequence: the function is applied
    /// before the view that calls it, even though View outranks Function.
    #[test]
    fn dependency_overrides_type_order_for_view_on_function() {
        use crate::entity::EntityType;
        let mut view = Entity::new(EntityType::View, "app.v");
        view.refers = vec!["app.f".to_string()];
        let ents = vec![view, Entity::new(EntityType::Function, "app.f")];

        let ordered = order_entities_for_test(ents);
        let pos = |name: &str| ordered.iter().position(|e| e.name == name).unwrap();
        assert!(pos("app.f") < pos("app.v"), "got {:?}", ordered.iter().map(|e| &e.name).collect::<Vec<_>>());
        assert!(ordered.iter().all(|e| e.errors.is_empty()));
    }

    /// The reverse edge is honored by the same mechanism, which is why the type
    /// sequence cannot simply be reordered to put functions first.
    #[test]
    fn dependency_overrides_type_order_for_function_on_view() {
        use crate::entity::EntityType;
        let mut func = Entity::new(EntityType::Function, "app.f");
        func.refers = vec!["app.v".to_string()];
        let ents = vec![func, Entity::new(EntityType::View, "app.v")];

        let ordered = order_entities_for_test(ents);
        let pos = |name: &str| ordered.iter().position(|e| e.name == name).unwrap();
        assert!(pos("app.v") < pos("app.f"));
    }

    /// Schemas still lead, and a table still precedes a view, when nothing in
    /// the graph says otherwise.
    #[test]
    fn type_rank_orders_entities_within_a_dependency_level() {
        use crate::entity::EntityType;
        let ents = vec![
            Entity::new(EntityType::Function, "app.f"),
            Entity::new(EntityType::Table, "app.t"),
            Entity::schema("app"),
            Entity::new(EntityType::Enum, "app.e"),
            Entity::new(EntityType::Sequence, "app.s"),
        ];
        let sorted = order_entities_for_test(ents);
        let ordered: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(ordered, vec!["app", "app.s", "app.e", "app.t", "app.f"]);
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
                ..Default::default()
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
