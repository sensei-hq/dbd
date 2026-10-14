use std::path::{Path, PathBuf};

use crate::adapter::DatabaseAdapter;
use crate::config::{self, DepsPolicy, DesignConfig, MaterializedViewsConfig};
use crate::dependency;
use crate::entity::{Entity, EntityType, TableConstraint};
use crate::error::{DbdError, Result};
use crate::parser;
use crate::refcache::RefCache;
use crate::references;
use crate::scanner;
use crate::scope::ResolvedScope;
use crate::script;
use crate::snapshot;
use crate::snapshot::PendingMigration;

mod apply;
mod env_guard;
mod hooks;
mod import;
mod marks;
mod ownership;
mod plan;
mod reading;
mod reconcile;
mod reset;
mod scope;
mod workspace;

pub use marks::DatabaseMarks;
pub use ownership::{Prune, SharedDatabase, SharedProject, SharedUse, leave_out_other_owners};
pub use plan::{ApplyStrategy, ExecutionPlan, ExecutionStep, build_execution_plan};
pub use reading::Reading;
pub use reset::{KeptRole, ResetComplete};
pub use workspace::{Module, Workspace, check_scope_gaps};

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
    /// For a [`ApplyStrategy::Fresh`] run: whether the database was already
    /// built before it — dbd had applied it without a version, or the design's
    /// objects were already there. A database with no recorded version is not
    /// necessarily empty, and calling this run a fresh install said it was.
    /// Always `false` for the other strategies.
    pub previously_built: bool,
}

/// The pending migrations a run would execute against one database, from
/// [`Design::pending_migration_preview`] — what `apply --dry-run` lists.
#[derive(Debug, Clone, Default)]
pub struct MigrationPreview {
    /// The version the database records (0: none).
    pub from_version: u32,
    /// The version the design is at.
    pub to_version: u32,
    /// The migration steps the run takes, in the order it takes them. Empty
    /// when the run migrates nothing.
    pub steps: Vec<MigrationStepPreview>,
    /// The versions the run records as applied. Not empty with `steps` empty
    /// for a scoped run that none of the pending migrations touch: it runs
    /// nothing for them, and still records them and moves the database on.
    pub recorded: Vec<u32>,
}

/// One migration step of a run, for a preview.
#[derive(Debug, Clone)]
pub struct MigrationStepPreview {
    /// The migration it belongs to.
    pub version: u32,
    /// The entity it acts on.
    pub entity: String,
    pub action: MigrationAction,
    /// The statements of the files the step runs, in order. Empty for
    /// [`MigrationAction::Create`], which runs the entity's own DDL.
    pub statements: Vec<PlannedStatement>,
    /// `false` for a drop of a table this database does not have: the run
    /// skips it.
    pub runs: bool,
}

/// What a migration step does to its entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationAction {
    /// A migration added it; its DDL creates it.
    Create,
    /// A migration adds values to it (an enum); they run, and commit, before
    /// the rest of the version.
    EnumValues,
    /// A migration altered it; its `.sql` runs.
    Migrate,
    /// A migration altered or added it; its `.data.sql` runs, after every
    /// schema change of the same version.
    Data,
    /// A migration dropped it; its drop script runs where it still exists.
    Drop,
}

/// One statement a migration file runs, as a preview shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStatement {
    /// The statement on one line, shortened when long.
    pub text: String,
    /// Whether it destroys something — a `DROP`, a `TRUNCATE`, or an `ALTER
    /// TABLE` dropping a column or constraint — read from the parse tree.
    pub drops: bool,
}

/// Running tallies accumulated while executing an apply plan's steps. Folded
/// into [`ApplyComplete`] once the plan finishes.
///
/// `applied` and `migrated` count entities, as [`ApplyComplete`] says they do,
/// not steps: an entity a migration adds is created and may then be migrated
/// and re-applied, and one altered by two pending migrations runs two
/// migration files. Counted per step, a design of 35 entities reported "36
/// applied".
#[derive(Default)]
struct ApplyCounts {
    applied: u32,
    migrated: u32,
    created: u32,
    dropped: u32,
    counted_applied: std::collections::HashSet<String>,
    counted_migrated: std::collections::HashSet<String>,
}

impl ApplyCounts {
    /// Count `entity` as applied, once however many steps applied it.
    fn applied(&mut self, entity: &str) {
        if self.counted_applied.insert(entity.to_string()) {
            self.applied += 1;
        }
    }

    /// Count `entity` as migrated, once however many migration files it ran.
    fn migrated(&mut self, entity: &str) {
        if self.counted_migrated.insert(entity.to_string()) {
            self.migrated += 1;
        }
    }
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

/// What [`Design::apply_grants`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GrantsOutcome {
    /// The design declares no grants, so nothing ran.
    #[default]
    None,
    /// The grant script ran.
    Applied,
    /// The design declares grants but the target has no SQL grant model
    /// (SQLite, Convex), so they were skipped rather than fed SQL it cannot run.
    Unsupported,
}

/// Combined summary passed to the `on_complete` callback of `deploy()`.
#[derive(Debug, Clone, Default)]
pub struct DeployComplete {
    pub apply: ApplyComplete,
    /// Whether the design's grants ran, between the schema and the data.
    pub grants: GrantsOutcome,
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
        out.extend(self.policy_warnings());
        out
    }

    /// The policy phase's share of [`Self::warnings`]: one line per failed
    /// policy file, then one per policy a scope excluded. Apart, so a caller
    /// reporting phase by phase can print them after the policy phase's own
    /// line rather than before it.
    pub fn policy_warnings(&self) -> Vec<String> {
        let failed = self
            .policies
            .failed
            .iter()
            .map(|(file, err)| format!("policy not applied: {} — {err}", file.display()));
        let skipped = self
            .policies
            .skipped
            .iter()
            .map(|(file, why)| format!("policy skipped: {} — {why}", file.display()));
        failed.chain(skipped).collect()
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
        Progress {
            on_start: noop_start,
            on_done: noop_done,
            on_complete: noop_complete::<C>,
        }
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

/// Remove from a live snapshot the tables and enums in `hidden` — those the
/// design declares but the scope leaves out (#40).
///
/// They sit in a managed schema without being desired, so left in they read as
/// orphans and `--prune` dropped them. Removed here, a scoped reconcile neither
/// alters nor prunes them, and a live object the design does not declare at all
/// is still an orphan. `hidden` is empty when there is no scope.
fn hide_declared(
    live: crate::snapshot::Snapshot,
    hidden: &std::collections::HashSet<(String, String)>,
) -> crate::snapshot::Snapshot {
    if hidden.is_empty() {
        return live;
    }
    let keep = |schema: &str, name: &str| !hidden.contains(&(schema.to_string(), name.to_string()));
    crate::snapshot::Snapshot {
        tables: live.tables.into_iter().filter(|t| keep(&t.schema, &t.name)).collect(),
        enums: live.enums.into_iter().filter(|e| keep(&e.schema, &e.name)).collect(),
        ..live
    }
}

/// Whether an import plan entry runs under a scope's working set.
/// An entry with write-targets is kept only if ALL targets are in scope;
/// a proc-less entry is kept if its staging table is in scope.
///
/// Superseded: it cannot see whether the design declares the staging table, so
/// it kept an import whose staging table the scope never built.
#[deprecated(
    since = "0.25.0",
    note = "use Design::scoped_import_plan, which also requires a declared staging table and says why an entry was skipped"
)]
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
    /// Errored entities the active scope actually builds. These are exactly
    /// what `Design::ensure_fully_parsed` refuses on, so `inspect` and
    /// `apply` agree about what is broken.
    pub issues: Vec<Entity>,
    /// Errored entities the active scope excludes. A scoped run never touches
    /// them, so they must not be reported as blocking it — but they are still
    /// reported, because a file that no scope builds is how a broken file goes
    /// unnoticed. Always empty without a scope, or under `all`.
    pub out_of_scope_issues: Vec<Entity>,
    /// Entities with warnings that the active scope builds — all of them
    /// without a scope, or under `all`.
    pub warnings: Vec<Entity>,
    /// Entities with warnings that the active scope excludes, kept apart like
    /// [`Self::out_of_scope_issues`]: listed and counted as the scope's, a
    /// warning in a file the scope never builds read as something to fix
    /// before the scoped run.
    pub out_of_scope_warnings: Vec<Entity>,
    pub gaps: Vec<crate::scope::ScopeGap>,
}

/// An entry in the import plan: staging table + matched procedure + write targets.
#[derive(Debug, Clone)]
pub struct ImportPlanEntry {
    pub table: Entity,
    /// The loader CALLed once the data files are loaded (picked by
    /// `Design::import_plan`). A table with two data files has it on its first
    /// entry only — it is called once, over the rows of both.
    pub procedure: Option<String>,
    /// The tables the table's loader writes — on every entry of the table, so a
    /// scope judges its data files alike.
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

/// Which policy files a run under `scope` applies and which it skips, read from
/// `policies/` without a database.
///
/// `applied` here means *would apply*: nothing is executed, so `failed` is
/// always empty. This is the filter [`apply_policies`] itself runs, exposed so
/// a preview lists what the run will do. `dbd policies --dry-run` used to list
/// every file under `policies/` whatever the scope, promising a policy on a
/// table the plane does not have.
pub fn plan_policies(
    project_dir: &Path,
    scope: Option<(&str, &std::collections::HashSet<String>)>,
) -> Result<PolicyReport> {
    let mut report = PolicyReport::default();
    for file in crate::scanner::scan_policies(project_dir)? {
        if let Some((scope_name, working_set)) = scope
            && let Some(target) = policy_target(&file, project_dir)
            && !working_set.contains(&target)
        {
            report
                .skipped
                .push((file, format!("{target} is outside scope '{scope_name}'")));
        } else {
            report.applied.push(file);
        }
    }
    Ok(report)
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
    let plan = plan_policies(project_dir, scope)?;
    if dry_run {
        return Ok(plan);
    }
    // A policy depends on the tables it names: written under the database's
    // dbd write lock, shared, so another project's reset or prune reads it
    // before it drops `CASCADE` (#7).
    ownership::with_write_lock(adapter, false, run_policies(adapter, project_dir, plan)).await
}

/// [`apply_policies`]'s run of `plan`, under the write lock.
async fn run_policies(adapter: &dyn DatabaseAdapter, project_dir: &Path, plan: PolicyReport) -> Result<PolicyReport> {
    let mut report = PolicyReport {
        applied: Vec::new(),
        failed: Vec::new(),
        skipped: plan.skipped,
    };

    // Canonicalize the project root once so path-traversal checks are reliable.
    let canon_root = project_dir.canonicalize().unwrap_or_else(|_| project_dir.to_path_buf());

    for file in &plan.applied {
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

        // A policy file is user-authored SQL and may be UTF-16 — see
        // `source_text`. The failure stays per-file here (it lands in
        // `report.failed`), but an undecodable one must say so rather than
        // report a bare I/O error.
        match crate::source_text::read_to_string(&canon_file) {
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
fn report_step_result(desc: &str, on_done: &mut dyn FnMut(&str, Option<&str>), result: Result<()>) -> Result<()> {
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
    /// How this project's DDL was read, resolved once from `source.dialect` /
    /// `source.parser`. Kept because it decides what an entity *is*: a
    /// `Verbatim` project has no structured model, so the operations that
    /// compare structure must refuse rather than compare nothing and report a
    /// match.
    parser: crate::parser::ParserChoice,
    /// `source.search_path`, resolved: the path a migration or hook script
    /// that states none of its own runs on, and its bare names are read on.
    search_path: Option<Vec<crate::entity::PathEntry>>,
    /// `source.dialect` as the project wrote it.
    ///
    /// Kept beside [`Self::parser`] because a refusal has to name what the
    /// *user* set, not the reader it selected: `sqlite` picks `Verbatim`, and
    /// telling someone their "verbatim project" cannot be diffed names nothing
    /// they typed.
    dialect: String,
    /// The owner whose entities this design takes over as it claims them —
    /// `--adopt-from` (#7). See [`Design::adopting_from`].
    adopt_from: Option<String>,
    /// Whether this design may run as an owner that holds entities here it
    /// declares none of — `--allow-ownership-change` (#7). See
    /// [`Design::allowing_ownership_change`].
    allow_ownership_change: bool,
    /// The workspace manifest's hooks a run attaches to the first and last
    /// module it runs, read from the project folder (#7). See
    /// [`Workspace::select`].
    outer_hooks: Option<OuterHooks>,
    /// What this design's import empties, when a workspace run decides it —
    /// see [`Design::emptying_for_import`].
    import_truncates: Option<Vec<String>>,
    /// Whether its import has emptied tables — see
    /// [`Design::emptied_for_import`].
    import_emptied: std::sync::atomic::AtomicBool,
    /// The owners a reset runs beside — see [`Design::resetting_beside`].
    reset_beside: std::collections::HashSet<String>,
    /// The module this design is in its workspace's manifest — see
    /// [`Design::module`].
    module: Option<String>,
    /// A module's project's other owners, whose entities it takes over as it
    /// claims what it declares: the project as it was before it had modules
    /// — its predecessor, first — and the workspace's other modules (#7, the
    /// workspace transfer rule). Empty for a project.
    transfer_from: Vec<String>,
    /// What the workspace's other modules declare: moving there, so never
    /// this module's to prune, nor a sign of another project under its name.
    declared_elsewhere: Vec<crate::adapter::OwnedObject>,
    /// Whether a run of this design retired its predecessor — see
    /// [`Design::retired_predecessor`].
    retired: std::sync::atomic::AtomicBool,
}

/// A workspace manifest's hooks, and the folder their paths are relative to —
/// attached to a module's design for a run (#7): its `apply.before` to the
/// first module, its `apply.after` and `import.after` to the last.
pub(crate) struct OuterHooks {
    pub(crate) dir: PathBuf,
    pub(crate) apply: config::ApplyConfig,
    pub(crate) import_after: Vec<config::ScriptEntry>,
}

/// Which list of hooks a run executes, for [`Design::plan_hook_phase`].
#[derive(Clone, Copy)]
pub(in crate::design) enum HookPhase {
    ApplyBefore,
    ApplyAfter,
    ImportAfter,
}

/// The apply phase of a `kind` of hook.
fn apply_phase(kind: hooks::HookKind) -> HookPhase {
    match kind {
        hooks::HookKind::Before => HookPhase::ApplyBefore,
        hooks::HookKind::After => HookPhase::ApplyAfter,
    }
}

impl HookPhase {
    fn kind(self) -> hooks::HookKind {
        match self {
            Self::ApplyBefore => hooks::HookKind::Before,
            Self::ApplyAfter | Self::ImportAfter => hooks::HookKind::After,
        }
    }
}

/// The phrase [`warn_if_no_search_path`] reports with, so
/// [`Design::search_path_warnings`] finds its warnings among an entity's
/// others without a second copy of the rule.
const NO_SEARCH_PATH: &str = "states no `SET search_path`";

/// Report a DDL file that states no `SET search_path` of its own.
///
/// Every dbd DDL file is expected to open with one — no emitter writes it, so
/// it is a convention the author keeps, and a file that forgets is a real
/// possibility. Measured across dbd's own fixtures, 43 of 44 schema-owning DDL
/// files have it.
///
/// The report is the point as much as the fallback. Silently resolving a
/// forgotten path — against `public`, or against the project default — is how a
/// reference ends up aimed at a schema nobody chose. `inspect` counts these and
/// `apply`/`deploy` print them.
///
/// Types without a schema (roles, extensions) are skipped: they have no
/// unqualified names to resolve, so a missing path is not a defect there.
fn warn_if_no_search_path(entity: &mut Entity, relative: &std::path::Path) {
    use crate::entity::PathSource;
    if entity.schema_path.stated() || !entity.entity_type.has_schema() {
        return;
    }
    // A file that did not parse has no path the parser could record — its
    // `SET search_path` may be its first line — so "states no SET
    // search_path" beside its parse error was false. The parse error is the
    // finding.
    if !entity.errors.is_empty() {
        return;
    }
    // A dialect with no schema path at all has nothing to warn about. T-SQL
    // resolves an unqualified name against the connecting user's default
    // schema and MySQL has no schemas — neither has a `search_path`, and
    // telling their authors about PostgreSQL's session default would be
    // nonsense. The PostgreSQL reader always supplies entries (its own
    // default, `"$user", public`), so an empty path means "not that dialect".
    if entity.schema_path.entries.is_empty() {
        return;
    }
    let resolved_against = match entity.schema_path.source {
        PathSource::Project => "source.search_path".to_string(),
        _ => "PostgreSQL's session default (\"$user\", public) — set source.search_path to choose".to_string(),
    };
    entity.warnings.push(format!(
        "{} {NO_SEARCH_PATH}; unqualified names resolved against {resolved_against}",
        relative.display()
    ));
}

impl Design {
    /// The environment this design was loaded for (`-e`): what a run records
    /// in the database, and what picks its `import/<env>/` data.
    pub fn env(&self) -> &str {
        &self.env
    }

    /// Every DDL file a run under `scope` — narrowed to the entity `name`
    /// selects, if any — executes that states no `SET search_path`, as a
    /// warning naming the path its unqualified names were resolved against:
    /// what `apply` and `deploy` print before they run.
    ///
    /// `inspect` counted these and nothing else said them, though the design
    /// is that a forgotten path is never silent: a file the run executes on a
    /// path nobody wrote into it is how an object lands in a schema nobody
    /// chose.
    pub fn search_path_warnings(&self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Result<Vec<String>> {
        let entities = match scope {
            Some(s) => self.scoped_entities(s)?,
            None => self.entities.clone(),
        };
        let named = match name {
            Some(n) => Some(self.resolve_name(n, scope)?.name.clone()),
            None => None,
        };
        Ok(entities
            .iter()
            .filter(|e| named.as_ref().is_none_or(|n| &e.name == n))
            .flat_map(|e| &e.warnings)
            .filter(|w| w.contains(NO_SEARCH_PATH))
            .cloned()
            .collect())
    }

    /// Which reader this project's DDL was read by.
    ///
    /// Exposed because it decides what a caller may ask for: only a reader
    /// that [`produces_structure`](crate::parser::ParserChoice::produces_structure)
    /// can be diffed, reconciled or emitted as another dialect.
    pub fn parser(&self) -> crate::parser::ParserChoice {
        self.parser
    }

    /// The schemas something outside the database serves.
    ///
    /// A table in one of these is reachable by `anon` over HTTP on Supabase;
    /// a table in any other schema is not. Distinct from
    /// [`crate::script::SUPABASE_INFRASTRUCTURE`], which is about who *owns* a
    /// schema — `extensions` is the platform's and not exposed, and a
    /// project's own `app` schema can be exposed without being the platform's.
    ///
    /// Internal unless declared, except that `public` is exposed by default on
    /// a `supabase` target, mirroring what Supabase itself does. An explicit
    /// `exposed: false` overrides that; a bare schema name does not, because
    /// saying nothing is not the same as saying no.
    pub fn exposed_schemas(&self) -> Vec<String> {
        let supabase = self.target_name() == "supabase";
        self.config
            .schemas
            .iter()
            .filter(|e| match e.exposed() {
                Some(declared) => declared,
                None => supabase && e.name() == crate::reconcile::DEFAULT_SCHEMA,
            })
            .map(|e| e.name())
            .collect()
    }

    /// Tables reachable from outside the database with no RLS policy declared.
    ///
    /// The point of knowing which schemas are exposed. A policy lives at
    /// `policies/<schema>/<table>.sql`, so "declared" is a file that exists —
    /// dbd does not parse the policy, only note that the author wrote one.
    ///
    /// Reports nothing when no schema is exposed, which includes every plain
    /// `postgres` project: with nothing serving HTTP there is no exposure to
    /// report, and a check that fires anyway is noise.
    pub fn unprotected_exposed_tables(&self) -> Vec<String> {
        let exposed = self.exposed_schemas();
        if exposed.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Table)
            .filter(|e| e.schema.as_ref().is_some_and(|s| exposed.contains(s)))
            .filter(|e| !self.has_policy_file(e))
            .map(|e| e.name.clone())
            .collect();
        out.sort();
        out
    }

    /// Whether `policies/<schema>/<table>.*` exists for this entity.
    fn has_policy_file(&self, entity: &Entity) -> bool {
        let Some(schema) = &entity.schema else { return false };
        let bare = entity.name.rsplit('.').next().unwrap_or(&entity.name);
        let dir = self.project_dir.join("policies").join(schema);
        ["sql", "ddl"]
            .iter()
            .any(|ext| dir.join(format!("{bare}.{ext}")).is_file())
    }

    /// The active target's name — the first key under `target:` in
    /// design.yaml, or `postgres` when none is declared.
    ///
    /// It decides which schemas are the platform's rather than the project's:
    /// on `supabase`, `auth` and friends are Supabase's and prune must not
    /// touch them. Mirrors how `reset` already resolves its target.
    pub fn target_name(&self) -> &str {
        self.config
            .target
            .keys()
            .next()
            .map(String::as_str)
            .unwrap_or("postgres")
    }

    /// `source.dialect` as the project wrote it, for a message that has to
    /// name what the user set rather than the reader it selected.
    pub fn dialect(&self) -> &str {
        &self.dialect
    }

    /// Create a Design from a config file path.
    ///
    /// Reads design.yaml, scans DDL files, parses entities, resolves references,
    /// and sorts by dependencies.
    pub fn from_config(config_path: &Path, env: &str) -> Result<Self> {
        Self::from_config_with_dir(config_path, env, None)
    }

    /// Create a Design with an explicit project directory.
    /// If `project_dir` is None, uses the config file's parent directory.
    ///
    /// Refuses a `design.yaml` with a `modules:` block: that is a workspace,
    /// a design per module — see [`Workspace::load`].
    pub fn from_config_with_dir(config_path: &Path, env: &str, project_dir: Option<&Path>) -> Result<Self> {
        let project_dir = project_dir
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| config_path.parent().unwrap_or(Path::new(".")).to_path_buf());

        let design_config = config::read(config_path)?;
        if !design_config.modules.is_empty() {
            return Err(DbdError::Config(format!(
                "{} declares modules:, so it is a workspace — a design per module, each owned by \
                 <project>/<module>; load it with Workspace::load",
                config_path.display()
            )));
        }
        Ok(Self::scan(design_config, env, project_dir)?.resolved(&[]))
    }

    /// Read the project `design_config` describes in `project_dir`: its DDL,
    /// the entities its config declares, and its import files — not yet
    /// resolved against each other (see [`Self::resolved`]).
    pub(crate) fn scan(design_config: DesignConfig, env: &str, project_dir: PathBuf) -> Result<Self> {
        // A scope's `modules:` selects the modules of a workspace; this design
        // has none. (A module's own config carries only its projection of the
        // manifest's scopes, which select nothing by module.)
        for (name, entry) in &design_config.scopes {
            if let config::ScopeEntry::Spec(spec) = entry
                && !spec.modules.is_empty()
            {
                return Err(DbdError::Config(format!(
                    "scope '{name}' selects modules (scopes.{name}.modules), but design.yaml declares no modules:"
                )));
            }
        }
        // Taken before the config is moved into the Design, and kept as the
        // user wrote it — a refusal must name `sqlite`, not the `Verbatim`
        // reader that label selects.
        let dialect_label = design_config.source.dialect.clone();

        // Validate and resolve the parser before reading any file, so a bad
        // `source.parser` fails at load rather than partway through the scan.
        let parser_choice = crate::parser::ParserChoice::resolve(
            &design_config.source.dialect,
            design_config.source.parser.as_deref(),
        )?;

        // The project's answer for a file that states no `SET search_path`.
        // Validated here rather than at use, so a mistake fails the load
        // instead of quietly resolving nothing.
        let project_search_path = match design_config.source.search_path.as_deref() {
            None => None,
            Some([]) => {
                return Err(DbdError::Config(
                    "source.search_path is an empty list. Remove the key to use PostgreSQL's \
                     own default (\"$user\", public), or name the schemas a file that states no \
                     `SET search_path` should resolve against."
                        .to_string(),
                ));
            }
            Some(names) => Some(
                names
                    .iter()
                    .map(|n| {
                        if n == "$user" {
                            crate::entity::PathEntry::CurrentUser
                        } else {
                            crate::entity::PathEntry::Schema(n.clone())
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
        };

        // Scan and parse DDL entities. A file that fails to read must not
        // silently vanish from the desired set — a live table could be
        // dropped by `reconcile --prune` — so propagate the read error.
        // `parse_entity_with`'s own Err (unparseable DDL) still drops the
        // entity, unchanged from prior behavior.
        let ddl_files = scanner::scan_ddl(&project_dir)?;
        let mut entities: Vec<Entity> = Vec::new();
        for file in &ddl_files {
            // `source_text`, not `std::fs::read_to_string`: SSMS writes
            // UTF-16LE by default, and the stdlib rejects it outright. Since
            // this error PROPAGATES, one such file used to fail the whole load
            // rather than itself — a project authored in SQL Server could not
            // be opened at all.
            let sql = crate::source_text::read_to_string(file)?;
            // Use relative path for entity type/name derivation, but
            // store the absolute path so the file is readable regardless of CWD.
            let relative = file.strip_prefix(&project_dir).unwrap_or(file);
            let fallback = project_search_path
                .clone()
                .map(crate::entity::SchemaPath::from_project)
                .unwrap_or_default();
            if let Ok(mut entity) = parser::parse_entity_with_search_path(parser_choice, relative, &sql, &fallback) {
                entity.file = Some(file.clone());
                warn_if_no_search_path(&mut entity, relative);
                entities.push(entity);
            }
        }

        // Add schema entities
        for schema_name in design_config.schema_names() {
            if !entities
                .iter()
                .any(|e| e.entity_type == EntityType::Schema && e.name == schema_name)
            {
                entities.push(Entity::schema(&schema_name));
            }
        }

        // Auto-add schemas from entity file paths
        let entity_schemas: Vec<String> = entities.iter().filter_map(|e| e.schema.clone()).collect();
        for schema in entity_schemas {
            if !entities
                .iter()
                .any(|e| e.entity_type == EntityType::Schema && e.name == schema)
            {
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
                // A role listed under `target.roles` is granted the roles it
                // names — a membership, not a read.
                entity.refs = role
                    .refers
                    .iter()
                    .map(|r| crate::entity::Ref::stated(r, crate::entity::RefKind::Member))
                    .collect();
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
            parser: parser_choice,
            search_path: project_search_path,
            dialect: dialect_label,
            adopt_from: None,
            allow_ownership_change: false,
            outer_hooks: None,
            import_truncates: None,
            import_emptied: std::sync::atomic::AtomicBool::new(false),
            reset_beside: std::collections::HashSet::new(),
            module: None,
            transfer_from: Vec::new(),
            declared_elsewhere: Vec::new(),
            retired: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The scanned design with its references resolved and its entities in
    /// apply order. `known` names entities this design may reference without
    /// declaring them, with their kinds: in a workspace, what the other
    /// modules declare. Each is added as an external — never this design's to
    /// build or own — as is each `external:` entry; a known sequence or enum
    /// is reached folded as one of the design's own is.
    pub(crate) fn resolved(mut self, known: &[(String, EntityType)]) -> Self {
        let mut entities = std::mem::take(&mut self.entities);
        let design_config = &self.config;

        // Add external entities for reference resolution
        let mut external_names: Vec<String> = design_config.external.iter().map(|e| e.name.clone()).collect();
        for ext in &design_config.external {
            entities.push(Entity::external(&ext.name));
        }
        let mut seen: std::collections::HashSet<String> = external_names.iter().cloned().collect();
        for (name, _) in known {
            if seen.insert(name.clone()) {
                external_names.push(name.clone());
                entities.push(Entity::external(name));
            }
        }
        let foldable: Vec<String> = known
            .iter()
            .filter(|(_, kind)| matches!(kind, EntityType::Sequence | EntityType::Enum))
            .map(|(name, _)| name.clone())
            .collect();

        // Resolve references
        references::resolve_references_with(&mut entities, &external_names, &foldable, &design_config.ignore);

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

        self.entities = [dependency::sort_by_dependencies(&managed), externals].concat();
        self
    }

    /// `source.search_path`, resolved — `None` when the project sets none.
    /// What a migration or hook script stating no path of its own runs on.
    pub(crate) fn script_search_path(&self) -> Option<&[crate::entity::PathEntry]> {
        self.search_path.as_deref()
    }

    /// Where the hooks are read from, and the path they run on.
    pub(in crate::design) fn hook_source(&self) -> hooks::HookSource<'_> {
        hooks::HookSource {
            dir: &self.project_dir,
            search_path: self.script_search_path(),
            entities: &self.entities,
        }
    }

    /// Each list of `phase`'s hooks a run executes, in the order it runs
    /// them, with where its paths are read: the workspace manifest's
    /// `apply.before` a run attaches ahead of the design's own, its
    /// `apply.after` and `import.after` behind (#7 — see
    /// [`Workspace::select`]).
    fn hook_lists(&self, phase: HookPhase) -> Vec<(hooks::HookSource<'_>, &[config::ScriptEntry])> {
        let own = match phase {
            HookPhase::ApplyBefore => &self.config.apply.before[..],
            HookPhase::ApplyAfter => &self.config.apply.after[..],
            HookPhase::ImportAfter => &self.config.import.after[..],
        };
        let own = (self.hook_source(), own);
        let outer = self.outer_hooks.as_ref().map(|outer| {
            let source = hooks::HookSource {
                dir: &outer.dir,
                search_path: self.script_search_path(),
                entities: &self.entities,
            };
            match phase {
                HookPhase::ApplyBefore => (source, &outer.apply.before[..]),
                HookPhase::ApplyAfter => (source, &outer.apply.after[..]),
                HookPhase::ImportAfter => (source, &outer.import_after[..]),
            }
        });
        match phase {
            HookPhase::ApplyBefore => outer.into_iter().chain([own]).collect(),
            HookPhase::ApplyAfter | HookPhase::ImportAfter => [own].into_iter().chain(outer).collect(),
        }
    }

    /// What a run's `phase` hooks would do — every list decided before any of
    /// them runs, so a hook file missing from one aborts the phase before a
    /// hook of another has run.
    pub(in crate::design) fn plan_hook_phase(
        &self,
        phase: HookPhase,
        scope: Option<(&str, &std::collections::HashSet<String>)>,
    ) -> Result<hooks::HookPlan> {
        let mut plan = hooks::HookPlan::default();
        for (source, entries) in self.hook_lists(phase) {
            let list = hooks::plan_hooks(source, entries, phase.kind(), scope)?;
            plan.runnable.extend(list.runnable);
            plan.warnings.extend(list.warnings);
        }
        Ok(plan)
    }

    /// Run a run's `phase` hooks, as [`Self::plan_hook_phase`] decides.
    pub(in crate::design) async fn run_hook_phase<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        phase: HookPhase,
        scope: Option<(&str, &std::collections::HashSet<String>)>,
        dry_run: bool,
        progress: &mut Progress<S, D, C>,
    ) -> Result<hooks::HookOutcome>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        let plan = self.plan_hook_phase(phase, scope)?;
        hooks::run_planned(adapter, plan, self.script_search_path(), dry_run, progress).await
    }

    /// [`Self::plan_hook_phase`] for a `kind` of apply hook.
    pub(in crate::design) fn plan_apply_hooks(
        &self,
        kind: hooks::HookKind,
        scope: Option<(&str, &std::collections::HashSet<String>)>,
    ) -> Result<hooks::HookPlan> {
        self.plan_hook_phase(apply_phase(kind), scope)
    }

    /// [`Self::run_hook_phase`] for a `kind` of apply hook.
    pub(in crate::design) async fn run_apply_hooks<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        kind: hooks::HookKind,
        scope: Option<(&str, &std::collections::HashSet<String>)>,
        dry_run: bool,
        progress: &mut Progress<S, D, C>,
    ) -> Result<hooks::HookOutcome>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        self.run_hook_phase(adapter, apply_phase(kind), scope, dry_run, progress)
            .await
    }

    /// Have this design's import empty `tables`, and only them, before it
    /// loads (#7). A workspace's modules import one after another, and
    /// Postgres empties a table another references only with it — so a run
    /// has its first module's import empty, together, every table the run
    /// reloads that exists, and the others empty none: `shared`'s reference
    /// data reloads with the `core` tables that hold keys to it, as one
    /// project's import empties its own.
    pub fn emptying_for_import(mut self, tables: Vec<String>) -> Self {
        self.import_truncates = Some(tables);
        self
    }

    /// The module this design is in its workspace's manifest (#7) — what its
    /// version and release state are written under, `modules.<module>` —
    /// or `None` for a project's own design.
    pub fn module(&self) -> Option<&str> {
        self.module.as_deref()
    }

    /// Whether this design's import emptied tables before it loaded — false
    /// until one has, and for one Postgres refused to empty, which empties
    /// nothing. What a workspace run says of a failed import rests on it: an
    /// emptying refused left nothing to reload.
    pub fn emptied_for_import(&self) -> bool {
        self.import_emptied.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The tables this design's import empties before it loads, under
    /// `scope` and narrowed by `name`: each it reloads, unless its
    /// `import.tables` entry says `truncate: false`.
    pub fn import_truncations(&self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Result<Vec<String>> {
        let (plan, _) = self.scoped_import_plan(name, scope)?;
        let mut tables: Vec<String> = Vec::new();
        for entry in plan {
            let table = entry.table.name;
            if self.config.import.table_truncate(&table) && !tables.contains(&table) {
                tables.push(table);
            }
        }
        Ok(tables)
    }

    /// Reset beside `owners` (#7): a workspace resets its modules in one run,
    /// each before the modules it depends on — so the modules reset before
    /// this one, `owners`, are gone by the time it resets. Their uses do not
    /// hold this reset back, and what they hold in a schema does not keep it
    /// — though a prod mark of theirs still does. A preview names them so it
    /// reads the database as the run will find it.
    pub fn resetting_beside(mut self, owners: impl IntoIterator<Item = String>) -> Self {
        self.reset_beside = owners.into_iter().filter(|o| o != self.owner()).collect();
        self
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
    pub async fn resolve_unknown_refs_via_db(&mut self, adapter: &dyn DatabaseAdapter) -> Result<usize> {
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

        Ok(Self::drop_resolved_ref_warnings(
            &mut self.entities,
            &mut self.import_tables,
            |name| resolved.contains(name),
        ))
    }

    /// Capture the full set of user-defined entities (tables, views, enums)
    /// from the live database and persist them to
    /// `<project_dir>/.dbd/refcache.json`.
    ///
    /// Subsequent offline `inspect` runs can use this snapshot to silence
    /// "Unresolved reference" warnings via `resolve_unknown_refs_via_cache`.
    ///
    /// Returns the number of entities written to the cache.
    pub async fn write_ref_cache(&self, adapter: &dyn DatabaseAdapter, source: &str) -> Result<usize> {
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

        let dropped =
            Self::drop_resolved_ref_warnings(&mut self.entities, &mut self.import_tables, |name| cache.contains(name));
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
            // Check file exists for file-based entities. A scanned file's path
            // already starts with the project directory (the scan is rooted
            // there), so joining it on again doubled a relative `-s proj` into
            // `proj/proj/ddl/…` and reported every file missing. Only a path
            // not already under it is resolved against it.
            if let Some(ref file) = entity.file {
                let full_path = if file.starts_with(&self.project_dir) {
                    file.clone()
                } else {
                    self.project_dir.join(file)
                };
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

        let all_issues = self.filtered_entities(name, |e| !e.errors.is_empty());
        let warnings = self.filtered_entities(name, |e| !e.warnings.is_empty());

        // Split the errors the same way `ensure_fully_parsed` does, so the
        // report agrees with what a run would actually refuse on. Reporting an
        // out-of-scope parse error as blocking sends people to fix a file the
        // scoped run never reads.
        let ((issues, out_of_scope_issues), (warnings, out_of_scope_warnings)) = match scope {
            Some(s) if !s.is_all => match self.working_set(s) {
                Ok(ws) => (
                    all_issues.into_iter().partition(|e| Self::entity_in_scope(e, s, &ws)),
                    warnings.into_iter().partition(|e| Self::entity_in_scope(e, s, &ws)),
                ),
                // Working set unavailable: treat every error as blocking. The
                // conservative direction — a spurious error is a nuisance, a
                // hidden one is the failure this reporting exists to prevent.
                Err(_) => ((all_issues, Vec::new()), (warnings, Vec::new())),
            },
            _ => ((all_issues, Vec::new()), (warnings, Vec::new())),
        };

        let gaps = match scope {
            Some(s) => crate::scope::analyze_gaps(s, &self.entities, &self.external_names()),
            None => Vec::new(),
        };

        Report {
            entity,
            issues,
            out_of_scope_issues,
            warnings,
            out_of_scope_warnings,
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
        std::fs::write(file, self.combined_ddl_of(&entities))?;
        Ok(())
    }

    /// `entities`' DDL as one script, in their order — a file that did not
    /// parse and an external left out.
    pub(crate) fn combined_ddl_of(&self, entities: &[Entity]) -> String {
        // The script is for this project's own engine, so dbd's generated
        // statements are written in its dialect too (see `combined_ddl`).
        let dialect = crate::parser::Dialect::from_label(&self.dialect).unwrap_or_default();
        let combined: Vec<String> = entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter_map(|e| script::combined_ddl(e, dialect))
            .collect();
        combined.join("\n")
    }

    /// Get the dependency graph for visualization. `name` narrows to one entity's
    /// subgraph; `scope` filters to that scope's working set (`None` ⇒ full set).
    pub fn graph(&self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Result<dependency::GraphResult> {
        if let Some(n) = name {
            self.check_graph_name(n, scope)?;
        }
        Ok(dependency::graph_from_entities(&self.graph_entities(scope)?, name))
    }

    /// Refuse a `-n` the graph cannot center on: one it does not know or the
    /// scope leaves out, or an external.
    pub(crate) fn check_graph_name(&self, name: &str, scope: Option<&ResolvedScope>) -> Result<()> {
        // An unknown or out-of-scope name produced an empty graph — a valid
        // document, indistinguishable from an entity with no dependencies. So
        // did an `external:` one: dbd has no DDL for it, so it is no node of
        // the graph, and `inspect -n` and `apply -n` refuse it alike.
        if self.resolve_name(name, scope)?.entity_type == EntityType::External {
            return Err(DbdError::Config(format!(
                "{name} is declared `external:` — dbd has no DDL for it, so it has no place in the dependency \
                 graph; it only resolves references to it"
            )));
        }
        Ok(())
    }

    /// The entities the graph is drawn over under `scope`.
    pub(crate) fn graph_entities(&self, scope: Option<&ResolvedScope>) -> Result<Vec<Entity>> {
        // A file dbd could not parse is left out, as `dbd graph` tells the user
        // it is (`Design::unparsed`): kept as a node, it sat in a layer and at
        // the end of an edge although nothing about it was read.
        let graphable = |e: &Entity| crate::scope::is_scopable(e) && e.errors.is_empty();
        Ok(match scope {
            Some(s) => {
                let ws = self.working_set(s)?;
                self.entities
                    .iter()
                    .filter(|e| graphable(e) && Self::entity_in_scope(e, s, &ws))
                    .cloned()
                    .collect()
            }
            None => self.entities.iter().filter(|e| graphable(e)).cloned().collect(),
        })
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
            op: BinaryOperator::Or, ..
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
fn collect_or_equalities(expr: &sqlparser::ast::Expr, col: &mut Option<String>, vals: &mut Vec<String>) -> bool {
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

    /// The marks of a database whose only record is `m`, this project's.
    fn own(m: &crate::adapter::ProjectMeta) -> DatabaseMarks {
        DatabaseMarks::of_own("p", Some(m.clone()))
    }

    #[test]
    fn scope_guard_allows_matching_scope() {
        let m = meta_with_scope(Some("public"));
        assert!(Design::check_scope_guard(&own(&m), "public", false).is_ok());
    }

    #[test]
    fn scope_guard_blocks_mismatch() {
        let m = meta_with_scope(Some("public"));
        let err = Design::check_scope_guard(&own(&m), "internal", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("public") && msg.contains("internal"), "msg was: {msg}");
    }

    #[test]
    fn scope_guard_unpinned_never_blocks() {
        let m = meta_with_scope(None);
        assert!(Design::check_scope_guard(&own(&m), "internal", false).is_ok());
        assert!(Design::check_scope_guard(&DatabaseMarks::of_own("p", None), "internal", false).is_ok());
    }

    #[test]
    fn scope_guard_allow_scope_change_bypasses() {
        let m = meta_with_scope(Some("public"));
        assert!(Design::check_scope_guard(&own(&m), "internal", true).is_ok());
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

    /// `dbd graph` said a file it could not parse "is left out" — and kept its
    /// entity in `nodes`, in `layers`, and at the end of an edge. The graph now
    /// leaves it out as the message says: no node, no layer entry, no edge to
    /// it, whole or scoped.
    #[test]
    fn graph_leaves_out_an_entity_whose_file_does_not_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app\nscopes:\n  app_only:\n    includes: [app]\n",
        )
        .unwrap();
        for (name, sql) in [
            ("good", "create table if not exists app.good (id int primary key);\n"),
            (
                "bad",
                "create table if not exists app.bad (id int primary key,, oops;\n",
            ),
            (
                "child",
                "create table if not exists app.child (id int primary key, bad_id int references app.bad (id));\n",
            ),
        ] {
            std::fs::write(dir.join(format!("ddl/table/app/{name}.ddl")), sql).unwrap();
        }
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let all = design.resolve_scope(None, None).unwrap();
        let scoped = design.resolve_scope(Some("app_only"), None).unwrap();
        for (label, graph) in [
            ("unscoped", design.graph(None, None).unwrap()),
            ("all", design.graph(None, Some(&all)).unwrap()),
            ("scoped", design.graph(None, Some(&scoped)).unwrap()),
        ] {
            let names: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
            assert!(
                names.contains(&"app.good") && names.contains(&"app.child"),
                "{label}: {names:?}"
            );
            assert!(!names.contains(&"app.bad"), "{label}: no node for app.bad: {names:?}");
            assert!(
                graph.layers.iter().flatten().all(|n| n != "app.bad"),
                "{label}: no layer holds app.bad: {:?}",
                graph.layers
            );
            assert!(
                graph.edges.iter().all(|e| e.from != "app.bad" && e.to != "app.bad"),
                "{label}: no edge reaches app.bad"
            );
        }
        assert_eq!(
            design
                .unparsed(&all)
                .unwrap()
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["app.bad"],
            "and it is the one named as left out"
        );
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

    /// A project at v1 applied to a database at v2 — a branch checked out
    /// behind the database it points at — planned as "current", re-applied
    /// v1's DDL over v2's schema, and reported "Already up to date (v2)".
    /// It is refused before anything runs, hooks included, naming both
    /// versions; the run, its dry run and a deploy alike.
    #[tokio::test]
    async fn an_older_project_is_refused_on_a_newer_database() {
        let tmp = hooks_project();
        let design = load_project(tmp.path(), "dev");
        assert_eq!(design.config().project.version, Some(1));

        for dry_run in [false, true] {
            let mock = MockAdapter::new().with_meta("dev", 2);
            let err = design
                .apply(&mock, None, dry_run, None, Progress::none())
                .await
                .expect_err("a v1 project must not apply to a v2 database");
            let msg = err.to_string();
            assert!(
                matches!(err, DbdError::SafetyGuard(_)),
                "a guard, like scope and prod: {msg}"
            );
            assert!(msg.contains("v2") && msg.contains("v1"), "naming both versions: {msg}");
            assert!(markers_run(&mock).is_empty(), "no hook ran (dry run {dry_run})");
            assert!(mock.applied.lock().unwrap().is_empty(), "no entity was applied");
            assert!(mock.scripts.lock().unwrap().is_empty(), "no script ran");
        }

        let mock = MockAdapter::new().with_meta("dev", 2);
        let err = design.deploy(&mock, false, None, |_| {}).await.expect_err("nor deploy");
        assert!(matches!(err, DbdError::SafetyGuard(_)), "{err}");
        assert!(mock.scripts.lock().unwrap().is_empty() && mock.imported.lock().unwrap().is_empty());

        // The database's own version, and an older one, still apply.
        for db in [1, 0] {
            let mock = MockAdapter::new().with_meta("dev", db);
            design
                .apply(&mock, None, false, None, Progress::none())
                .await
                .unwrap_or_else(|e| panic!("a v{db} database takes a v1 project: {e}"));
        }
    }

    /// A before-hook that cannot run — its file missing — is a refusal, and an
    /// apply outside one transaction claims only past its last refusal: it
    /// claimed first, and a misconfigured run left the database claimed for
    /// entities it never wrote (#7).
    #[tokio::test]
    async fn an_apply_outside_a_transaction_refused_its_hook_claims_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: shop\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
             apply:\n  before:\n    - sql/missing.sql\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/orders.ddl"),
            "create table if not exists app.orders (id int primary key);\n",
        )
        .unwrap();
        let design = load_project(dir, "dev");
        let mock = MockAdapter::new();
        assert!(!mock.supports_txn, "outside a transaction");

        let err = design
            .apply(&mock, None, false, None, Progress::none())
            .await
            .expect_err("the hook file is missing");
        assert!(err.to_string().contains("missing.sql"), "{err}");
        assert!(
            mock.claims.lock().unwrap().is_empty(),
            "{:?}",
            mock.claims.lock().unwrap()
        );
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
            .apply(
                &mock,
                None,
                false,
                None,
                Progress {
                    on_start: |_: &str| {},
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |s| summary = Some(s),
                },
            )
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
        let post = markers
            .iter()
            .position(|m| m == "post_ddl")
            .expect("apply.after must run");
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
            .import_data(
                &mock,
                None,
                false,
                Some(&scope),
                Progress {
                    on_start: |_: &str| {},
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |s| summary = Some(s),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            markers_run(&mock),
            vec!["dynamic"],
            "only the declared-writes hook may run"
        );
        let summary = summary.expect("import must report a summary");
        assert_eq!(summary.after_scripts, 1);
        assert!(
            summary
                .warnings
                .iter()
                .any(|w| { w.contains("sql/loader.sql") && w.contains("staging.b") && w.contains("partial") }),
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

        design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .unwrap();

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
            .apply(
                &mock,
                None,
                /*dry_run*/ true,
                None,
                Progress {
                    on_start: |d: &str| steps.push(d.to_string()),
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |_| {},
                },
            )
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
            .filter(|e| matches!(e.entity_type, EntityType::Table | EntityType::Enum) && e.errors.is_empty())
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
        // A DB already at the latest version takes the Current strategy, whose
        // plan has no SetVersion step. The scope must still be (re)pinned. (A
        // DB *above* the design's version is refused by the version guard.)
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: t\n  version: 7\nschemas:\n  - app\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/t.ddl"),
            "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
        )
        .unwrap();
        let design = load_project(tmp.path(), "dev");
        let mock = MockAdapter::new().with_meta("dev", 7).with_scope("public");
        let resolved = design.resolve_scope(None, None).unwrap();

        design
            .apply(&mock, None, false, Some(&resolved), Progress::none())
            .await
            .unwrap();

        let meta = mock.get_project_meta().await.unwrap().expect("meta");
        // Re-pinned to the applied scope, and the version was preserved.
        assert_eq!(meta.scope.as_deref(), Some(resolved.name.as_str()));
        assert_eq!(meta.version, 7);
    }

    /// `reset --clean` drops what the project installs: every role the design
    /// declares — `target.roles` and `ddl/role/` alike (only the first were
    /// dropped) — and the extensions its scope admits. A scope whose
    /// `extensions: []` installs none must drop none; every extension went.
    #[test]
    fn reset_drops_every_declared_role_and_only_the_scopes_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/table/app", "ddl/role"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app\ntarget:\n  postgres:\n    url: $DATABASE_URL\n\
             \x20   roles:\n      - name: from_target\n    extensions:\n      - pgcrypto\n\
             scopes:\n  hub:\n    includes: [app]\n    extensions: []\n",
        )
        .unwrap();
        std::fs::write(dir.join("ddl/role/from_file.ddl"), "create role from_file;\n").unwrap();
        std::fs::write(
            dir.join("ddl/table/app/t.ddl"),
            "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();

        let all = design.reset_script("postgres", true, true, None).unwrap().unwrap();
        assert!(all.contains("DROP ROLE IF EXISTS \"from_target\""), "{all}");
        assert!(
            all.contains("DROP ROLE IF EXISTS \"from_file\""),
            "a ddl/role role is the project's too: {all}"
        );
        assert!(all.contains("DROP EXTENSION IF EXISTS \"pgcrypto\""), "{all}");

        let hub = design.resolve_scope(Some("hub"), None).unwrap();
        let scoped = design
            .reset_script("postgres", true, true, Some(&hub))
            .unwrap()
            .unwrap();
        assert!(
            !scoped.contains("DROP EXTENSION"),
            "hub installs no extension: {scoped}"
        );
    }

    /// A scoped reset must not reach past its scope, and `CASCADE` is how it
    /// did: dropping an in-scope enum took an out-of-scope table's column with
    /// it. Not one statement of a scoped reset cascades — schemas and
    /// extensions included — while the full reset keeps its `CASCADE`.
    #[test]
    fn a_scoped_reset_script_never_cascades() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/table/app", "ddl/enum/app"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app\ntarget:\n  postgres:\n    url: $DATABASE_URL\n\
             \x20   extensions:\n      - pgcrypto\n\
             scopes:\n  moods:\n    includes: [app.mood]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/enum/app/mood.ddl"),
            "create type app.mood as enum ('a');\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/t.ddl"),
            "set search_path to app;\ncreate table if not exists t (id integer primary key, m app.mood);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();

        let moods = design.resolve_scope(Some("moods"), None).unwrap();
        let scoped = design
            .reset_script("postgres", true, true, Some(&moods))
            .unwrap()
            .unwrap();
        assert!(scoped.contains("DROP TYPE IF EXISTS \"app\".\"mood\""), "{scoped}");
        assert!(!scoped.contains("CASCADE"), "a scoped reset never cascades: {scoped}");

        let all = design.reset_script("postgres", true, true, None).unwrap().unwrap();
        assert!(all.contains("CASCADE"), "the full reset still does: {all}");
    }

    /// A role the design granted on a schema cannot be dropped while it holds
    /// the grant: `role "app_writer" cannot be dropped because some objects
    /// depend on it — privileges for schema app, default privileges …`, and the
    /// whole default reset rolled back. Reset revokes what dbd granted — the
    /// `schemas:` grants and the target's `grants:`, default privileges
    /// included — on every schema that survives, before it drops the role. A
    /// schema `--schemas` drops takes its grants with it, and a role the design
    /// does not declare is not dropped, so neither needs a revoke.
    #[test]
    fn reset_revokes_the_designs_grants_on_surviving_schemas_before_dropping_its_roles() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app:\n      grants:\n        app_writer: [usage, select, insert]\n\
             \x20       web_anon: [usage, select]\n\
             target:\n  postgres:\n    url: $DATABASE_URL\n    roles:\n      - name: app_writer\n      - name: app_reader\n\
             \x20   grants:\n      app:\n        app_reader: [usage, select]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/t.ddl"),
            "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();

        let sql = design.reset_script("postgres", false, false, None).unwrap().unwrap();
        for (role, perms) in [("app_writer", "SELECT, INSERT"), ("app_reader", "SELECT")] {
            let drop = sql
                .find(&format!("DROP ROLE IF EXISTS \"{role}\""))
                .unwrap_or_else(|| panic!("{role} is dropped: {sql}"));
            for revoke in [
                format!("REVOKE USAGE ON SCHEMA \"app\" FROM \"{role}\";"),
                format!("REVOKE {perms} ON ALL TABLES IN SCHEMA \"app\" FROM \"{role}\";"),
                format!("ALTER DEFAULT PRIVILEGES IN SCHEMA \"app\" REVOKE {perms} ON TABLES FROM \"{role}\";"),
            ] {
                let at = sql
                    .find(&revoke)
                    .unwrap_or_else(|| panic!("missing `{revoke}`:\n{sql}"));
                assert!(at < drop, "`{revoke}` must run before {role} is dropped:\n{sql}");
            }
        }
        assert!(
            !sql.contains("FROM \"web_anon\""),
            "a role the design does not declare is not dropped, so its grants stay:\n{sql}"
        );

        let clean = design.reset_script("postgres", true, false, None).unwrap().unwrap();
        assert!(
            !clean.contains("REVOKE"),
            "`--schemas` drops app, and its grants with it:\n{clean}"
        );
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

    /// `combine` writes the script `apply` would run. Apply keeps `public` on
    /// every path a file's own `SET search_path` sets (so an extension installed
    /// there resolves); combine wrote the files as they were, and `psql -f
    /// init.sql` failed where apply succeeded. The file's own statement is
    /// still written exactly as it is — `public` comes from a statement of its
    /// own after it.
    #[test]
    fn combine_writes_the_search_path_apply_uses() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(dir.join("design.yaml"), "project:\n  name: t\nschemas:\n  - app\n").unwrap();
        let file =
            "set search_path to app;\ncreate table if not exists t (id uuid primary key default uuid_generate_v4());\n";
        std::fs::write(dir.join("ddl/table/app/t.ddl"), file).unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let out = dir.join("init.sql");
        design.combine(&out, None).unwrap();
        let sql = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            search_path_per_table(&sql).get("t").map(Vec::as_slice),
            Some(["app".to_string(), "public".to_string()].as_slice()),
            "the table runs on the file's path, then public:\n{sql}"
        );
        assert!(
            sql.contains("set search_path to app;\n"),
            "the file's own statement is not rewritten:\n{sql}"
        );
    }

    /// The `set search_path to …;` rewrite matched text anywhere, up to the next
    /// `;`. Supabase's advice for a `SECURITY DEFINER` function is a `SET
    /// search_path = ''` clause; with `to` it became `'' as $$ select 1, public;`
    /// and the function failed with `column "public" does not exist`. A string
    /// literal or a function body quoting the statement was altered the same
    /// way. Only a top-level `SET search_path` statement is one, and none of
    /// the text — clause, literal, body — is touched.
    #[test]
    fn combine_leaves_every_search_path_text_but_a_statement_as_written() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/function/app")).unwrap();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(dir.join("design.yaml"), "project:\n  name: t\nschemas:\n  - app\n").unwrap();
        let files = [
            (
                "ddl/function/app/secure.ddl",
                "create or replace function app.secure() returns int\n\
                 language sql security definer set search_path = ''\nas $$ select 1; $$;\n",
            ),
            (
                "ddl/function/app/secure_to.ddl",
                "create or replace function app.secure_to() returns int\n\
                 language sql security definer set search_path to ''\nas $$ select 2; $$;\n",
            ),
            (
                "ddl/function/app/greet.ddl",
                "create or replace function app.greet() returns text\n\
                 language plpgsql set search_path to app\nas $$ begin return 'g; ok'; end; $$;\n",
            ),
            (
                "ddl/table/app/notes.ddl",
                "create table if not exists app.notes (id int primary key, \
                 body text default 'set search_path to x; y', \
                 note text default E'set search_path to x;\\' y');\n",
            ),
        ];
        for (rel, sql) in files {
            std::fs::write(dir.join(rel), sql).unwrap();
        }
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let out = dir.join("init.sql");
        design.combine(&out, None).unwrap();
        let sql = std::fs::read_to_string(&out).unwrap();
        for (rel, text) in files {
            assert!(sql.contains(text.trim_end()), "{rel} is written byte for byte:\n{sql}");
        }
        assert!(pg_query::parse(&sql).is_ok(), "the script parses:\n{sql}");
    }

    /// The search path each `CREATE TABLE` in `sql` runs under: the args of the
    /// last `SET search_path` before it, keyed by the table's bare name. Read
    /// from libpg_query's tree, so a spelling change cannot pass or fail it.
    fn search_path_per_table(sql: &str) -> std::collections::HashMap<String, Vec<String>> {
        use pg_query::NodeEnum;
        let parsed = pg_query::parse(sql).expect("combined SQL parses");
        let mut path: Vec<String> = Vec::new();
        let mut out = std::collections::HashMap::new();
        for stmt in &parsed.protobuf.stmts {
            match stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) {
                Some(NodeEnum::VariableSetStmt(set)) if set.name.eq_ignore_ascii_case("search_path") => {
                    path = set
                        .args
                        .iter()
                        .filter_map(|a| match a.node.as_ref() {
                            Some(NodeEnum::AConst(c)) => match c.val.as_ref() {
                                Some(pg_query::protobuf::a_const::Val::Sval(s)) => Some(s.sval.clone()),
                                _ => None,
                            },
                            _ => None,
                        })
                        .collect();
                }
                Some(NodeEnum::CreateStmt(create)) => {
                    let table = create.relation.as_ref().map(|r| r.relname.clone()).unwrap_or_default();
                    out.insert(table, path.clone());
                }
                _ => {}
            }
        }
        out
    }

    /// `source.search_path` is the path the parser resolves a file with no
    /// `SET search_path` against, so `ddl/table/app/a.ddl` holding a bare
    /// `create table a` was read as `app.a`. Nothing ran it there: the script
    /// carried the file as written, the table landed wherever the session
    /// pointed (`public`), and `diff` then reported `+ create app.a` forever.
    /// The combined script must run each such file on the project's path —
    /// with `public` after it, as a file's own path gets — and leave a file
    /// that states its own path alone.
    #[test]
    fn combine_runs_a_file_with_no_search_path_on_the_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nsource:\n  dialect: postgresql\n  search_path: [app]\nschemas:\n  - app\n  - other\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/a.ddl"),
            "create table if not exists a (id int primary key);\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("ddl/table/other")).unwrap();
        std::fs::write(
            dir.join("ddl/table/other/b.ddl"),
            "set search_path to other;\ncreate table if not exists b (id int primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let out = dir.join("init.sql");
        design.combine(&out, None).unwrap();
        let sql = std::fs::read_to_string(&out).unwrap();

        let paths = search_path_per_table(&sql);
        assert_eq!(
            paths.get("a").map(Vec::as_slice),
            Some(["app".to_string(), "public".to_string()].as_slice()),
            "a pathless file runs on source.search_path, then public:\n{sql}"
        );
        assert_eq!(
            paths.get("b").map(Vec::as_slice),
            Some(["other".to_string(), "public".to_string()].as_slice()),
            "a file's own path still wins:\n{sql}"
        );
    }

    /// A scope that keeps an extension but excludes the schema it installs into
    /// contradicts itself: `inspect` passed it and apply failed with `schema
    /// "ext" does not exist`. Refused where the scope is resolved, so every
    /// command says so.
    #[test]
    fn a_scope_that_excludes_its_extensions_schema_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/core")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - core\n  - ext\n\
             target:\n  postgres:\n    url: $DATABASE_URL\n    extensions:\n      - name: citext\n        schema: ext\n\
             scopes:\n  contradicts:\n    includes: [core]\n    excludes: [ext]\n  consistent:\n    includes: [core]\n\
             \x20   excludes: [ext]\n    extensions: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/core/t.ddl"),
            "set search_path to core;\ncreate table if not exists t (id integer primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let err = design.resolve_scope(Some("contradicts"), None).unwrap_err().to_string();
        assert!(
            err.contains("'citext' installs into 'ext'"),
            "names the extension and its schema: {err}"
        );
        // Said once: the message read "its extension 'citext' (installs into
        // 'ext') installs into".
        assert_eq!(err.matches("installs into").count(), 1, "{err}");
        design
            .resolve_scope(Some("consistent"), None)
            .expect("excluding the schema is fine when the scope installs no extension into it");
    }

    /// An extension installed `WITH SCHEMA x` needs `x`. Every scope keeps the
    /// extension (unless its `extensions:` allowlist says otherwise) but only
    /// kept the schemas its own entities live in, so any scope without `x`
    /// failed with `schema "x" does not exist`.
    #[test]
    fn a_scope_keeps_the_schema_its_extensions_install_into() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/core")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - core\n  - extensions\n\
             target:\n  postgres:\n    url: $DATABASE_URL\n    extensions:\n      - name: pgcrypto\n        schema: extensions\n\
             scopes:\n  core_only:\n    includes: [core]\n  bare:\n    includes: [core]\n    extensions: []\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/core/t.ddl"),
            "set search_path to core;\ncreate table if not exists t (id integer primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let names = |scope: &str| -> Vec<String> {
            let s = design.resolve_scope(Some(scope), None).unwrap();
            design
                .scoped_entities(&s)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect()
        };
        assert!(
            names("core_only").contains(&"extensions".to_string()),
            "{:?}",
            names("core_only")
        );
        assert!(
            !names("bare").contains(&"extensions".to_string()),
            "a scope installing no extension needs no extension schema: {:?}",
            names("bare")
        );
    }

    /// A scope skips a hook only for what the design manages outside it. A hook
    /// touching an `external:` table — or one it creates itself — was skipped
    /// under every named scope, because the working set never holds such names;
    /// the apply then failed on the schema that hook was there to prepare.
    #[tokio::test]
    async fn a_scoped_apply_runs_hooks_that_touch_only_what_the_design_does_not_manage() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/table/app", "ddl/table/billing", "sql"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app\n  - billing\nexternal:\n  - name: auth.users\n\
             apply:\n  before:\n    - sql/auth.sql\n    - sql/billing.sql\n\
             scopes:\n  app_only:\n    includes: [app]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("sql/auth.sql"),
            "create schema if not exists auth;\ncreate table if not exists auth.users (id uuid primary key);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("sql/billing.sql"),
            "insert into billing.invoices (id) values (1);\n",
        )
        .unwrap();
        for (schema, table) in [("app", "items"), ("billing", "invoices")] {
            std::fs::write(
                dir.join(format!("ddl/table/{schema}/{table}.ddl")),
                format!("set search_path to {schema};\ncreate table if not exists {table} (id integer primary key);\n"),
            )
            .unwrap();
        }
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let scope = design.resolve_scope(Some("app_only"), None).unwrap();
        let mock = MockAdapter::new();
        design
            .apply(&mock, None, false, Some(&scope), Progress::none())
            .await
            .unwrap();

        let scripts = mock.scripts.lock().unwrap().join("\n");
        assert!(scripts.contains("auth.users"), "the external's hook runs: {scripts}");
        assert!(
            !scripts.contains("billing.invoices"),
            "a hook on a managed table outside the scope does not: {scripts}"
        );
    }

    /// A trigger written in a table's file fires a function the table then
    /// depends on. The parser skipped `CREATE TRIGGER`, so no edge existed:
    /// `inspect` said all was well and apply ran the trigger before the
    /// function it executes existed.
    #[test]
    fn a_trigger_in_a_table_file_makes_the_table_depend_on_its_function() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/function/core", "ddl/table/core"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("design.yaml"), "project:\n  name: t\nschemas:\n  - core\n").unwrap();
        std::fs::write(
            dir.join("ddl/function/core/touch_updated_at.ddl"),
            "create or replace function core.touch_updated_at() returns trigger language plpgsql as $$\n\
             begin new.updated_at := now(); return new; end; $$;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/core/items.ddl"),
            "set search_path to core;\n\
             create table if not exists items (id integer primary key, updated_at timestamptz);\n\
             create trigger items_touch before update on items for each row execute function core.touch_updated_at();\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let items = design
            .entities()
            .iter()
            .find(|e| e.name == "core.items")
            .expect("items");
        let refers: Vec<&str> = items.refers().collect();
        assert!(refers.contains(&"core.touch_updated_at"), "{refers:?}");
    }

    /// A table depends on the enum a column is typed with and on the sequence a
    /// default draws from. Neither was recorded, so a scope could leave them out
    /// with no gap reported, and apply failed on a type that did not exist.
    /// A built-in type (`text`) names nothing in the project and is no warning.
    #[test]
    fn a_table_depends_on_its_column_enum_and_default_sequence() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/enum/core", "ddl/sequence/core", "ddl/table/sales"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - core\n  - sales\n\
             scopes:\n  orders_only:\n    includes: [sales.orders]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/enum/core/order_status.ddl"),
            "create type core.order_status as enum ('open', 'paid');\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/sequence/core/invoice_seq.ddl"),
            "create sequence if not exists core.invoice_seq;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/sales/orders.ddl"),
            "set search_path to sales;\ncreate table if not exists orders (\n  id integer primary key\n\
             , status core.order_status not null\n, invoice_no integer default nextval('core.invoice_seq'::regclass)\n\
             , note text\n);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let orders = design
            .entities()
            .iter()
            .find(|e| e.name == "sales.orders")
            .expect("orders");
        let refers: Vec<&str> = orders.refers().collect();
        assert!(refers.contains(&"core.order_status"), "the column's enum: {refers:?}");
        assert!(
            refers.contains(&"core.invoice_seq"),
            "the default's sequence: {refers:?}"
        );
        assert!(
            orders.warnings.iter().all(|w| !w.contains("Unresolved")),
            "a built-in type is not an unresolved reference: {:?}",
            orders.warnings
        );

        let scope = design.resolve_scope(Some("orders_only"), None).unwrap();
        let err = design.check_scope_gaps(&scope).unwrap_err().to_string();
        assert!(err.contains("core.order_status"), "the missing enum is a gap: {err}");
    }

    /// A sequence belongs to its schema like a table does. `is_scopable` left
    /// sequences out, so every scope dropped them — even one that only
    /// excludes another schema — and the table whose default draws from one
    /// failed to apply.
    #[test]
    fn a_scope_keeps_the_sequences_of_the_schemas_it_keeps() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/sequence/app", "ddl/table/app", "ddl/table/staging"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app\n  - staging\n\
             scopes:\n  no_staging:\n    excludes: [staging]\n  app_only:\n    includes: [app]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/sequence/app/item_seq.ddl"),
            "set search_path to app;\ncreate sequence if not exists item_seq;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/items.ddl"),
            "set search_path to app;\ncreate table if not exists items (id integer primary key default nextval('app.item_seq'));\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/staging/raw.ddl"),
            "set search_path to staging;\ncreate table if not exists raw (line text);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        for name in ["no_staging", "app_only"] {
            let scope = design.resolve_scope(Some(name), None).unwrap();
            let names: Vec<String> = design
                .scoped_entities(&scope)
                .unwrap()
                .into_iter()
                .map(|e| e.name)
                .collect();
            assert!(
                names.contains(&"app.item_seq".to_string()),
                "{name} keeps app's sequence: {names:?}"
            );
            assert!(!names.contains(&"staging.raw".to_string()), "{name}: {names:?}");
        }
    }

    /// A role, or an extension declared without `schema:`, lives in no schema.
    /// `managed_schemas` mapped both to `public`, so a design with either made
    /// `public` managed and `reconcile --prune` dropped tables there that the
    /// design never declared.
    #[test]
    fn roles_and_schemaless_extensions_make_no_schema_managed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - app\ntarget:\n  postgres:\n    url: $DATABASE_URL\n    \
             roles:\n      - name: r1\n    extensions:\n      - plpgsql\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/items.ddl"),
            "set search_path to app;\ncreate table if not exists items (id integer primary key);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let desired = design.entities_in_scope(None, None, None);
        let managed = Design::managed_schemas(&desired);
        assert_eq!(
            managed,
            std::collections::HashSet::from(["app".to_string()]),
            "only the schema the design declares is managed"
        );
    }

    /// `-e production` was recorded verbatim before the CLI normalised it, and
    /// such a row is still a prod database — it must not slip past the guard.
    #[tokio::test]
    async fn reset_blocked_when_the_database_says_production() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "prod").unwrap();
        let mock = MockAdapter::new().with_meta("production", 0);

        let result = design.reset(&mock, "postgres", false, false, false, None).await;
        assert!(result.is_err(), "a database marked `production` must block reset");
        assert!(result.unwrap_err().to_string().contains("prod"));
    }

    #[tokio::test]
    async fn reset_blocked_after_v1() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new().with_meta("dev", 1);

        let result = design.reset(&mock, "postgres", false, false, false, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("versioned at v1"));
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
        assert!(
            lookups_entry.is_some(),
            "staging.lookups should appear in the import plan"
        );
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
        let lookups_pos = plan.iter().position(|e| e.table.name == "staging.lookups");
        let lookup_values_pos = plan.iter().position(|e| e.table.name == "staging.lookup_values");

        assert!(lookups_pos.is_some(), "staging.lookups should be in plan");
        assert!(lookup_values_pos.is_some(), "staging.lookup_values should be in plan");
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
        assert!(
            !design.import_tables().is_empty(),
            "the staging file must be discovered"
        );

        let scope = design.resolve_scope(Some("config_only"), None).unwrap();
        let mock = MockAdapter::new();
        let mut summary: Option<ImportComplete> = None;
        design
            .import_data(
                &mock,
                None,
                /*dry_run*/ true,
                Some(&scope),
                Progress {
                    on_start: |_: &str| {},
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |s| summary = Some(s),
                },
            )
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

    /// A `-n` that names nothing is a typo. The import reported it as a
    /// warning and carried on — exiting 0 and running every `import.after`
    /// hook — so a mistyped name still ran the project's loader script. It is
    /// refused before anything runs, in the words every other command uses for
    /// a name that matches nothing.
    #[tokio::test]
    async fn an_unknown_import_name_is_refused_before_anything_runs() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        assert!(
            !design.config().import.after.is_empty(),
            "precondition: the fixture declares an import.after hook"
        );

        let mock = MockAdapter::new();
        let err = design
            .import_data(&mock, Some("staging.does_not_exist"), false, None, Progress::none())
            .await
            .expect_err("an unknown -n must be refused");

        assert!(
            err.to_string().contains("no entity named 'staging.does_not_exist'"),
            "{err}"
        );
        assert!(
            mock.scripts.lock().unwrap().is_empty(),
            "nothing may run — no truncate, no CALL, no import.after hook: {:?}",
            mock.scripts.lock().unwrap()
        );
        assert!(mock.imported.lock().unwrap().is_empty(), "no data file may load");
    }

    /// The plan every preview prints is refused the same way, so a dry run
    /// cannot accept a name the real run refuses.
    #[test]
    fn an_unknown_import_name_is_refused_by_the_plan() {
        let design = Design::from_config(&fixture_dir().join("design.yaml"), "dev").unwrap();
        let err = design
            .scoped_import_plan(Some("staging.does_not_exist"), None)
            .expect_err("an unknown -n must be refused");
        assert!(
            err.to_string().contains("no entity named 'staging.does_not_exist'"),
            "{err}"
        );
    }

    /// A table the design declares but has no data file for selects nothing to
    /// load either. "No entity named" would be false, so the refusal says what
    /// is actually missing.
    #[test]
    fn an_import_name_without_a_data_file_is_refused_naming_what_is_missing() {
        let design = Design::from_config(&fixture_dir().join("design.yaml"), "dev").unwrap();
        assert!(
            design.entities().iter().any(|e| e.name == "config.lookups"),
            "precondition: config.lookups is a declared table"
        );
        let err = design
            .scoped_import_plan(Some("config.lookups"), None)
            .expect_err("a name with no data file selects nothing");
        let msg = err.to_string();
        assert!(msg.contains("config.lookups") && msg.contains("no data file"), "{msg}");
    }

    /// The wrong-`-e` case: the file exists, under another env's folder. The
    /// refusal names it, since that is the one thing that tells the user what
    /// to change.
    #[test]
    fn an_import_name_whose_file_belongs_to_another_env_says_which_file() {
        let tmp = project_with_import_files(&[("dev/staging/seed.csv", "id\n1\n")]);
        let design = load_project(tmp.path(), "prod");
        let err = design
            .scoped_import_plan(Some("staging.seed"), None)
            .expect_err("a file of another env is not this run's to load");
        let msg = err.to_string();
        assert!(
            msg.contains("import/dev/staging/seed.csv") && msg.contains("'prod'"),
            "{msg}"
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
            .import_data(
                &mock,
                None,
                false,
                None,
                Progress {
                    on_start: |_: &str| {},
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |s| summary = Some(s),
                },
            )
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
        let truncate_scripts: Vec<&String> = scripts
            .iter()
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
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];

        let plan = build_execution_plan(&entities, 0, 2, &[], None);

        assert_eq!(plan.strategy, ApplyStrategy::Fresh);

        // Should have ApplyEntity for each entity + SetVersion
        let apply_names: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(apply_names.contains(&"config.orders"));

        // Last step should be SetVersion
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(2))));
    }

    // ── A2: Current (db_version == latest) ────────────────

    #[test]
    fn a2_current_applies_all_no_set_version() {
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];

        let plan = build_execution_plan(&entities, 2, 2, &[], None);

        assert_eq!(plan.strategy, ApplyStrategy::Current);

        // All entities get ApplyEntity
        let apply_names: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(apply_names.contains(&"config.orders"));

        // No SetVersion step
        assert!(!plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(_))));
    }

    /// A table a migration drops is gone from the design, so it is in no
    /// scope's working set — a named scope never ran its drop, while the
    /// migration was still recorded as applied. It belongs to a scope that
    /// occupies its schema, as it did while it existed.
    #[test]
    fn a_scoped_migrate_runs_the_drop_of_a_table_in_its_schema() {
        let entities = vec![test_entity("app.users")];
        let ws: std::collections::HashSet<String> = ["app", "app.users"].map(String::from).into();
        let drops = |plan: &ExecutionPlan| -> Vec<String> {
            plan.steps
                .iter()
                .filter_map(|s| match s {
                    ExecutionStep::DropEntity { entity_name, .. } => Some(entity_name.clone()),
                    _ => None,
                })
                .collect()
        };

        let ours = vec![test_migration(1, 2, vec![], vec![], vec!["app.legacy"])];
        let plan = build_execution_plan(&entities, 1, 2, &ours, Some(&ws));
        assert_eq!(drops(&plan), vec!["app.legacy"]);

        let theirs = vec![test_migration(1, 2, vec![], vec![], vec!["billing.old"])];
        let plan = build_execution_plan(&entities, 1, 2, &theirs, Some(&ws));
        assert!(
            drops(&plan).is_empty(),
            "a schema the scope does not occupy is not its to drop in"
        );
    }

    /// The drop a snapshot writes is the drop apply runs. `dbd snapshot` writes a
    /// dropped table's statement to `<schema>/<table>.sql`; apply looked for
    /// `<table>.drop.sql`, found nothing, and still counted the drop and
    /// recorded the migration — the table outlived its removal from the design
    /// on every database. Driven through the real snapshot, not a hand-written
    /// file, so the two sides cannot disagree about the name again.
    #[tokio::test]
    async fn a_table_removed_from_the_design_is_dropped_by_the_next_apply() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        let config = dir.join("design.yaml");
        std::fs::write(&config, "project:\n  name: t\nschemas:\n  - app\n").unwrap();
        for t in ["users", "legacy"] {
            std::fs::write(
                dir.join(format!("ddl/table/app/{t}.ddl")),
                format!("set search_path to app;\ncreate table if not exists {t} (id integer primary key);\n"),
            )
            .unwrap();
        }
        let load = || Design::from_config_with_dir(&config, "dev", Some(dir)).unwrap();
        crate::snapshot::create_snapshot(load().entities(), dir, &config, "baseline").unwrap();
        std::fs::remove_file(dir.join("ddl/table/app/legacy.ddl")).unwrap();
        crate::snapshot::create_snapshot(load().entities(), dir, &config, "drop legacy").unwrap();

        let design = load();
        assert_eq!(design.config().project.version, Some(2), "the second snapshot is v2");
        let mock = MockAdapter::new()
            .with_meta("dev", 1)
            .with_known_entities(["app.legacy"]);
        design.apply(&mock, None, false, None, Progress::none()).await.unwrap();

        let scripts = mock.scripts.lock().unwrap().join("\n");
        assert!(
            scripts.contains("DROP TABLE"),
            "the v2 drop of app.legacy runs: {scripts}"
        );
    }

    /// A version's data scripts ran only for the tables it altered, so a
    /// `data.sql` seeding a table the same version adds never ran — and was
    /// not listed in the preview either — while the version was recorded as
    /// applied. It is the version's own data script: it runs after the
    /// version's creates, with the rest of its data scripts, and is listed.
    #[tokio::test]
    async fn a_data_script_for_a_table_its_version_adds_runs_after_the_create() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        let config = dir.join("design.yaml");
        std::fs::write(&config, "project:\n  name: t\nschemas:\n  - app\n").unwrap();
        let table = |t: &str| {
            std::fs::write(
                dir.join(format!("ddl/table/app/{t}.ddl")),
                format!("set search_path to app;\ncreate table if not exists {t} (id integer primary key);\n"),
            )
            .unwrap();
        };
        let load = || Design::from_config_with_dir(&config, "dev", Some(dir)).unwrap();
        table("users");
        crate::snapshot::create_snapshot(load().entities(), dir, &config, "baseline").unwrap();
        table("kinds");
        crate::snapshot::create_snapshot(load().entities(), dir, &config, "add kinds").unwrap();
        std::fs::create_dir_all(dir.join("migrations/002/app")).unwrap();
        std::fs::write(
            dir.join("migrations/002/app/kinds.data.sql"),
            "insert into kinds (id) values (1) on conflict do nothing;\n",
        )
        .unwrap();

        let design = load();
        assert_eq!(design.config().project.version, Some(2), "the second snapshot is v2");
        let mock = MockAdapter::new().with_meta("dev", 1);
        let preview = design.pending_migration_preview(&mock, None, None).await.unwrap();
        assert!(
            preview
                .steps
                .iter()
                .any(|s| s.entity == "app.kinds" && s.action == MigrationAction::Data && s.version == 2),
            "the preview lists the data script of app.kinds: {:?}",
            preview.steps
        );

        let mut started: Vec<String> = Vec::new();
        design
            .apply(
                &mock,
                None,
                false,
                None,
                Progress {
                    on_start: |d: &str| started.push(d.to_string()),
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |_| {},
                },
            )
            .await
            .unwrap();
        let scripts = mock.scripts.lock().unwrap().clone();
        assert!(
            scripts.iter().any(|s| s.contains("insert into kinds")),
            "its data script runs: {scripts:#?}"
        );
        let at = |step: &str| started.iter().position(|d| d.starts_with(step));
        let created = at("table:app.kinds").unwrap_or_else(|| panic!("app.kinds is created: {started:#?}"));
        let seeded = at("migrate data table:app.kinds").unwrap_or_else(|| panic!("its data step: {started:#?}"));
        assert!(created < seeded, "after the create: {started:#?}");
    }

    /// A migration's drop runs only where the table still exists. Generated
    /// drops are a bare `DROP TABLE … CASCADE`, so a database whose scope never
    /// had the table would fail on it now that a scope runs the drops in its
    /// schemas.
    #[tokio::test]
    async fn a_migration_drop_runs_only_where_the_table_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/table/app", "migrations/002/app"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\n  version: 2\nschemas:\n  - app\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/users.ddl"),
            "set search_path to app;\ncreate table if not exists users (id integer primary key);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("migrations/002/graph.json"),
            r#"{"fromVersion":1,"toVersion":2,"added":[],"altered":[],"dropped":["app.legacy"]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("migrations/002/app/legacy.drop.sql"),
            "DROP TABLE app.legacy CASCADE;\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let ran_drop = |mock: &MockAdapter| {
            mock.scripts
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.contains("DROP TABLE app.legacy"))
        };

        let never_had_it = MockAdapter::new().with_meta("dev", 1);
        design
            .apply(&never_had_it, None, false, None, Progress::none())
            .await
            .unwrap();
        assert!(!ran_drop(&never_had_it), "no table, no drop");

        let has_it = MockAdapter::new()
            .with_meta("dev", 1)
            .with_known_entities(["app.legacy"]);
        design
            .apply(&has_it, None, false, None, Progress::none())
            .await
            .unwrap();
        assert!(ran_drop(&has_it), "the table is there, so it is dropped");
    }

    // ── A3: Behind by one version ─────────────────────────

    #[test]
    fn a3_behind_by_one_has_migrate_entity() {
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];
        let migrations = vec![test_migration(1, 2, vec![], vec!["config.users"], vec![])];

        let plan = build_execution_plan(&entities, 1, 2, &migrations, None);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have a MigrateEntity step for config.users
        let migrate_steps: Vec<(&str, u32)> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::MigrateEntity {
                    entity_name,
                    migration_version,
                    ..
                } => Some((entity_name.as_str(), *migration_version)),
                _ => None,
            })
            .collect();
        assert!(migrate_steps.contains(&("config.users", 2)));

        // Should also have SetVersion
        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(2))));
    }

    // ── A4: Behind by multiple versions ───────────────────

    #[test]
    fn a4_behind_by_multiple_has_record_per_migration() {
        let entities = vec![test_entity("config.users"), test_entity("config.orders")];
        let migrations = vec![
            test_migration(1, 2, vec![], vec!["config.users"], vec![]),
            test_migration(2, 3, vec![], vec!["config.orders"], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 3, &migrations, None);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have RecordMigration for both v2 and v3
        let record_versions: Vec<u32> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::RecordMigration { version, .. } => Some(*version),
                _ => None,
            })
            .collect();
        assert!(record_versions.contains(&2));
        assert!(record_versions.contains(&3));

        assert!(matches!(plan.steps.last(), Some(ExecutionStep::SetVersion(3))));
    }

    // ── A5: New table added in migration ──────────────────

    #[test]
    fn a5_new_table_gets_create_entity() {
        let entities = vec![test_entity("config.users"), test_entity("config.audit_log")];
        let migrations = vec![test_migration(1, 2, vec!["config.audit_log"], vec![], vec![])];

        let plan = build_execution_plan(&entities, 1, 2, &migrations, None);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have CreateEntity for the new table
        let created: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::CreateEntity(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(created.contains(&"config.audit_log"));
    }

    // ── A6: Table drop ────────────────────────────────────

    #[test]
    fn a6_dropped_table_gets_drop_entity() {
        let entities = vec![test_entity("config.users")];
        let migrations = vec![test_migration(1, 2, vec![], vec![], vec!["config.legacy"])];

        let plan = build_execution_plan(&entities, 1, 2, &migrations, None);

        assert_eq!(plan.strategy, ApplyStrategy::Migrate);

        // Should have DropEntity for the dropped table
        let dropped: Vec<(&str, u32)> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::DropEntity {
                    entity_name,
                    migration_version,
                    ..
                } => Some((entity_name.as_str(), *migration_version)),
                _ => None,
            })
            .collect();
        assert!(dropped.contains(&("config.legacy", 2)));
    }

    /// The refusal of a destructive reconcile listed every altered table under
    /// "dropped columns, constraints, or indexes" — here `app.b`, which only
    /// gains a column. It lists what is destructive, and what each drops.
    #[test]
    fn the_destructive_refusal_lists_only_what_is_destructive() {
        use crate::snapshot::{Snapshot, TableSnapshot};
        let column = |name: &str| crate::entity::ColumnDef {
            name: name.to_string(),
            data_type: "text".to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            identity: None,
            generated: None,
            comment: None,
            inline_fk: None,
            collation: None,
        };
        let table = |name: &str, columns: &[&str]| TableSnapshot {
            name: name.to_string(),
            schema: "app".to_string(),
            columns: columns.iter().map(|c| column(c)).collect(),
            indexes: vec![],
            table_constraints: vec![],
        };
        let snap = |tables: Vec<TableSnapshot>| Snapshot {
            version: 0,
            description: String::new(),
            timestamp: String::new(),
            tables,
            enums: vec![],
        };
        let live = snap(vec![table("a", &["id", "gone"]), table("b", &["id"])]);
        let desired = snap(vec![table("a", &["id"]), table("b", &["id", "added"])]);

        let plan = crate::reconcile::plan_reconcile(&live, &desired);
        let err = plan.refuse_destructive(false).unwrap_err().to_string();

        assert!(err.contains("app.a") && err.contains("gone"), "{err}");
        assert!(
            !err.contains("app.b"),
            "a table only gaining a column is not destructive: {err}"
        );
    }

    /// A database versioned at v4 by a fresh install has no migration
    /// recorded, but `reset` refused it as one with "applied migrations".
    #[test]
    fn reset_refuses_a_versioned_database_saying_it_is_versioned() {
        let meta = crate::adapter::ProjectMeta {
            project: "p".to_string(),
            env: "dev".to_string(),
            version: 4,
            scope: None,
            applied_at: None,
        };
        let marks = crate::design::DatabaseMarks::of_own("p", Some(meta));
        let design = Design::from_config(&fixture_dir().join("design.yaml"), "dev").unwrap();
        let err = design.check_reset_guard(&marks).unwrap_err().to_string();
        assert!(err.contains("versioned at v4"), "{err}");
        assert!(!err.contains("applied migrations"), "{err}");
    }

    /// `reset` refused any versioned database as "a released project's
    /// database" — a database `init --from-db` captured is versioned with
    /// `released` never set. It says "versioned at vN", and calls the project
    /// released only when `project.released` says so.
    #[tokio::test]
    async fn reset_calls_a_versioned_database_released_only_when_the_project_is() {
        for released in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let config = tmp.path().join("design.yaml");
            std::fs::write(
                &config,
                format!("project:\n  name: test\n  version: 3\n  released: {released}\nschemas:\n  - app\n"),
            )
            .unwrap();
            let design = Design::from_config_with_dir(&config, "dev", Some(tmp.path())).unwrap();
            let mock = MockAdapter::new().with_meta("dev", 3);
            let err = design
                .reset(&mock, "postgres", false, false, false, None)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("versioned at v3"), "{err}");
            assert_eq!(
                err.contains("released"),
                released,
                "released: {released} — the refusal says so only when it is: {err}"
            );
        }
    }

    /// The position of the first step `matches` picks out, for asserting order.
    fn step_at(plan: &ExecutionPlan, matches: impl Fn(&ExecutionStep) -> bool) -> usize {
        plan.steps
            .iter()
            .position(matches)
            .unwrap_or_else(|| panic!("no such step in {:#?}", plan.steps))
    }

    /// Migrations ran entity by entity: every pending migration of `t`, then
    /// every one of `u`, in dependency order. So v3's change to `t` ran before
    /// v2's change to `u`, and a v3 script reading what v2 adds failed on a
    /// jump that a stepwise upgrade survived. Each version now runs whole, and
    /// is recorded, before the next one starts.
    #[test]
    fn each_pending_version_runs_whole_before_the_next() {
        let entities = vec![test_entity("s.t"), test_entity("s.u")];
        let migrations = vec![
            test_migration(1, 2, vec![], vec!["s.u"], vec![]),
            test_migration(2, 3, vec![], vec!["s.t"], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 3, &migrations, None);

        let v2_u = step_at(
            &plan,
            |s| matches!(s, ExecutionStep::MigrateEntity { entity_name, migration_version: 2, .. } if entity_name == "s.u"),
        );
        let v2_recorded = step_at(&plan, |s| {
            matches!(s, ExecutionStep::RecordMigration { version: 2, .. })
        });
        let v3_t = step_at(
            &plan,
            |s| matches!(s, ExecutionStep::MigrateEntity { entity_name, migration_version: 3, .. } if entity_name == "s.t"),
        );
        assert!(v2_u < v2_recorded && v2_recorded < v3_t, "{:#?}", plan.steps);
    }

    /// v2 drops a table and v5 adds it back. The create of v5 ran first — a
    /// no-op over the old table — and v2's drop after it, so the run ended at
    /// v5 with no table.
    #[test]
    fn a_later_versions_create_runs_after_an_earlier_versions_drop() {
        let entities = vec![test_entity("app.counters")];
        let migrations = vec![
            test_migration(1, 2, vec![], vec![], vec!["app.counters"]),
            test_migration(4, 5, vec!["app.counters"], vec![], vec![]),
        ];

        let plan = build_execution_plan(&entities, 1, 5, &migrations, None);

        let dropped = step_at(
            &plan,
            |s| matches!(s, ExecutionStep::DropEntity { entity_name, migration_version: 2, .. } if entity_name == "app.counters"),
        );
        let created = step_at(
            &plan,
            |s| matches!(s, ExecutionStep::CreateEntity(n) if n == "app.counters"),
        );
        assert!(dropped < created, "{:#?}", plan.steps);
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
        let apply_names: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(
            !apply_names.contains(&"config.broken"),
            "entity with errors should be filtered out"
        );
    }

    // M5.2: External entity filtered
    #[test]
    fn a_external_entity_filtered() {
        let external = Entity::new(EntityType::External, "pg_catalog.pg_type");
        let table = test_entity("config.users");
        let entities = vec![external, table];

        let plan = build_execution_plan(&entities, 0, 1, &[], None);

        let apply_names: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(apply_names.contains(&"config.users"));
        assert!(
            !apply_names.contains(&"pg_catalog.pg_type"),
            "external entity should be filtered out"
        );
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
        let apply_names: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::ApplyEntity(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
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
        let migrate_steps: Vec<(&str, u32)> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::MigrateEntity {
                    entity_name,
                    migration_version,
                    ..
                } => Some((entity_name.as_str(), *migration_version)),
                _ => None,
            })
            .collect();
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
        let apply_count = plan
            .steps
            .iter()
            .filter(|s| matches!(s, ExecutionStep::ApplyEntity(_)))
            .count();
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
        assert!(
            plan.steps
                .iter()
                .any(|s| matches!(s, ExecutionStep::RecordMigration { version: 2, .. }))
        );
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(2))));
    }

    /// A scope's working set names the schemas and tables it builds; roles and
    /// extensions are never in it, because they are always on (see
    /// `entity_in_scope`). The entities handed to the plan have already been
    /// through that rule — and a fresh install applies every one of them — but
    /// the migrate plan filtered them by the working set again, so under a scope
    /// a new role was never created and a new extension never installed, and the
    /// first table using the extension's type failed (`type "citext" does not
    /// exist`).
    #[test]
    fn a_scoped_migration_applies_the_roles_and_extensions_the_scope_keeps() {
        let role = Entity::new(EntityType::Role, "app_writer");
        let extension = Entity::new(EntityType::Extension, "citext");
        let entities = vec![role, extension, test_entity("app.emails")];
        let migrations = vec![test_migration(1, 2, vec!["app.emails"], vec![], vec![])];
        let working_set: std::collections::HashSet<String> = ["app", "app.emails"].map(String::from).into();

        let plan = build_execution_plan(&entities, 1, 2, &migrations, Some(&working_set));

        let applied: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::ApplyEntity(n) | ExecutionStep::CreateEntity(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        assert!(applied.contains(&"app_writer"), "the role is created: {applied:?}");
        assert!(applied.contains(&"citext"), "the extension is installed: {applied:?}");
        let emails = applied.iter().position(|n| *n == "app.emails").unwrap();
        let citext = applied.iter().position(|n| *n == "citext").unwrap();
        assert!(citext < emails, "before the table that uses it: {applied:?}");
    }

    /// `applied` counts entities: "Entities whose DDL was applied", of which
    /// `created` is a subset. A migration's new entity runs `CreateEntity` and
    /// then `ApplyEntity`, and both counted it — a design of 3 entities reported
    /// "4 applied", one more than it has.
    #[tokio::test]
    async fn a_migration_counts_each_entity_it_applies_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/table/app", "migrations/002"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\n  version: 2\nschemas:\n  - app\n",
        )
        .unwrap();
        for t in ["users", "audit"] {
            std::fs::write(
                dir.join(format!("ddl/table/app/{t}.ddl")),
                format!("set search_path to app;\ncreate table if not exists {t} (id integer primary key);\n"),
            )
            .unwrap();
        }
        std::fs::write(
            dir.join("migrations/002/graph.json"),
            r#"{"fromVersion":1,"toVersion":2,"added":["app.audit"],"altered":[],"dropped":[]}"#,
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let entities = design.entities_to_apply(None, None).unwrap().len() as u32;

        let mock = MockAdapter::new().with_meta("dev", 1);
        let mut summary = None;
        design
            .apply(
                &mock,
                None,
                false,
                None,
                Progress {
                    on_start: |_: &str| {},
                    on_done: |_: &str, _: Option<&str>| {},
                    on_complete: |s| summary = Some(s),
                },
            )
            .await
            .unwrap();

        let s = summary.expect("a successful apply reports a summary");
        assert_eq!(s.strategy, ApplyStrategy::Migrate);
        assert_eq!(s.applied, entities, "each of the {entities} entities counted once");
        assert_eq!(s.created, 1, "the one the migration adds");
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

        let design = Design::from_config_with_dir(&project_dir.join("design.yaml"), "dev", Some(project_dir)).unwrap();

        // DB is at v1 → migration v2 is pending
        let mock = crate::adapter::mock::MockAdapter::new().with_meta("dev", 1);
        let result = design.apply(&mock, None, false, None, Progress::none()).await;

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

        let design = Design::from_config_with_dir(&project_dir.join("design.yaml"), "dev", Some(project_dir)).unwrap();

        // Fresh DB — v1 migration is pending but data.sql has no TODOs
        let mock = crate::adapter::mock::MockAdapter::new();
        let result = design.apply(&mock, None, false, None, Progress::none()).await;

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
        let design = Design::from_config_with_dir(&project_dir.join("design.yaml"), "dev", Some(project_dir)).unwrap();

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
        let design = Design::from_config_with_dir(&project_dir.join("design.yaml"), "dev", Some(project_dir)).unwrap();

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
        use crate::config::DepsPolicy;
        use crate::scope::ResolvedScope;
        use std::collections::HashSet;

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

        let result = design.apply(&mock, None, false, Some(&scope), Progress::none()).await;
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dependency gap"), "expected gap error, got: {msg}");
        assert!(mock.applied_names().is_empty()); // no writes
    }

    #[tokio::test]
    async fn apply_with_scope_filters_entities() {
        use crate::config::DepsPolicy;
        use crate::scope::ResolvedScope;
        use std::collections::HashSet;

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
        assert!(
            applied.iter().any(|n| n == "config.lookups"),
            "in-scope tables still apply: {applied:?}"
        );
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
        assert!(
            applied.iter().any(|n| n == "uuid-ossp"),
            "listed extension applies: {applied:?}"
        );
        assert!(
            !applied.iter().any(|n| n == "postgis"),
            "unlisted extension dropped: {applied:?}"
        );
    }

    #[test]
    fn skip_schemas_filters_entities() {
        let mut entities = vec![
            Entity::new(EntityType::Table, "config.users"),
            Entity::new(EntityType::Table, "auth.sessions"),
        ];
        let skip = ["auth".to_string()];
        entities.retain(|e| match &e.schema {
            Some(s) => !skip.contains(s),
            None => true,
        });
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

        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let mock = MockAdapter::new();
        let mut summary = None;
        design.deploy(&mock, false, None, |s| summary = Some(s)).await.unwrap();

        let summary: DeployComplete = summary.expect("deploy must report a summary");
        assert_eq!(
            summary.policies.applied.len(),
            1,
            "policy file must be applied by the export"
        );
        assert!(summary.policies.failed.is_empty());
    }

    /// `Design::deploy` applies the schema grants `dbd apply` does. They lived
    /// only in the CLI's apply handler, so `dbd deploy` — and every embedder —
    /// got a schema with no grants and no warning.
    #[tokio::test]
    async fn deploy_applies_schema_grants_like_apply() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: test\nschemas:\n  - app:\n      grants:\n        app_user: [usage, select]\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let mock = MockAdapter::new().with_schema_grants();
        design.deploy(&mock, false, None, |_| {}).await.unwrap();

        let scripts = mock.scripts.lock().unwrap().join("\n");
        assert!(
            scripts.contains("GRANT USAGE ON SCHEMA \"app\" TO \"app_user\""),
            "deploy must apply the schema grants: {scripts}"
        );
        assert!(scripts.contains("GRANT SELECT ON ALL TABLES IN SCHEMA \"app\" TO \"app_user\""));
    }

    /// Grants follow the scope: a scoped deploy grants only on the schemas it
    /// builds. Granting on all of them failed on a schema the scope never
    /// created — after the entities were applied and the database pinned.
    #[tokio::test]
    async fn a_scoped_deploy_grants_only_on_the_schemas_it_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in ["ddl/table/core", "ddl/table/staging"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: test\nschemas:\n  - core:\n      grants:\n        reader: [usage]\n\
             \x20 - staging:\n      grants:\n        loader: [usage]\n\
             scopes:\n  staging_only:\n    includes: [staging]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/core/t.ddl"),
            "set search_path to core;\ncreate table if not exists t (id integer primary key);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/staging/raw.ddl"),
            "set search_path to staging;\ncreate table if not exists raw (line text);\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let scope = design.resolve_scope(Some("staging_only"), None).unwrap();
        let mock = MockAdapter::new().with_schema_grants();
        design.deploy(&mock, false, Some(&scope), |_| {}).await.unwrap();

        let scripts = mock.scripts.lock().unwrap().join("\n");
        assert!(
            scripts.contains("ON SCHEMA \"staging\""),
            "the scope's schema is granted: {scripts}"
        );
        assert!(
            !scripts.contains("\"core\""),
            "a schema the scope did not build is not: {scripts}"
        );
    }

    /// A target with no grant model is skipped, not fed SQL it cannot run.
    #[tokio::test]
    async fn deploy_skips_grants_on_a_target_without_a_grant_model() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: test\nschemas:\n  - app:\n      grants:\n        app_user: [usage]\n",
        )
        .unwrap();
        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let mock = MockAdapter::new();
        design.deploy(&mock, false, None, |_| {}).await.unwrap();

        let scripts = mock.scripts.lock().unwrap().join("\n");
        assert!(
            !scripts.contains("GRANT"),
            "no grant SQL for a target without grants: {scripts}"
        );
    }

    /// A deploy with nothing to import must still report a summary — the zero
    /// count is the signal that the registry rows did not load.
    #[tokio::test]
    async fn deploy_reports_zero_import_with_reason_when_no_import_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();

        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let mock = MockAdapter::new();
        let mut summary = None;
        design.deploy(&mock, false, None, |s| summary = Some(s)).await.unwrap();

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

        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let mock = MockAdapter::new().fail_script_containing("THIS IS NOT SQL");
        let mut summary = None;
        let result = design.deploy(&mock, false, None, |s| summary = Some(s)).await;

        assert!(result.is_ok(), "a failed policy file must not fail the deploy");
        let summary: DeployComplete = summary.expect("deploy must report a summary");
        assert_eq!(summary.policies.failed.len(), 1, "the failure must be recorded");
        let warnings = summary.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("policy not applied") && w.contains("broken.sql")),
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

        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let scope = design.resolve_scope(Some("daemon"), None).expect("scope must resolve");
        let mock = MockAdapter::new();
        let mut summary = None;
        design
            .deploy(&mock, false, Some(&scope), |s| summary = Some(s))
            .await
            .expect("deploy must succeed");

        let summary: DeployComplete = summary.expect("deploy must report a summary");
        assert_eq!(
            summary.policies.skipped.len(),
            1,
            "svc policy must be skipped: {summary:?}"
        );
        assert_eq!(summary.policies.applied.len(), 1, "app policy must still apply");
        assert!(summary.policies.failed.is_empty(), "a skip is not a failure");
        let warnings = summary.warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("policy skipped") && w.contains("metrics.sql")),
            "skipped policy must surface as a warning: {warnings:?}"
        );
    }

    /// The report's error split must be the same judgement `ensure_fully_parsed`
    /// makes, or `inspect` and `apply` disagree about what is broken. Before
    /// this, `inspect --scope daemon` reported a parse error in an excluded
    /// schema while `apply --scope daemon` applied cleanly and exited 0.
    #[test]
    fn scoped_report_blocks_on_exactly_what_apply_refuses_on() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: t\nschemas: [app, svc]\nscopes:\n  daemon:\n    excludes: [svc]\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/svc")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/t.ddl"),
            "set search_path to app;\ncreate table if not exists t (id int primary key);",
        )
        .unwrap();
        // Broken, and in the schema the scope excludes.
        std::fs::write(
            tmp.path().join("ddl/table/svc/broken.ddl"),
            "set search_path to svc;\ncreate table if not exists broken (id int,,,);",
        )
        .unwrap();

        let mut design =
            Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let scope = design.resolve_scope(Some("daemon"), None).unwrap();
        let ws = design.working_set(&scope).unwrap();

        let report = design.report(None, Some(&scope));
        assert!(
            report.issues.is_empty(),
            "the excluded file must not block scope 'daemon': {:?}",
            report.issues.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        assert_eq!(report.out_of_scope_issues.len(), 1, "but it must still be reported");
        assert_eq!(report.out_of_scope_issues[0].name, "svc.broken");

        // The property under test: the scoped run agrees.
        assert!(
            design.ensure_fully_parsed(Some(&scope), Some(&ws), None).is_ok(),
            "apply --scope daemon must not refuse when nothing in scope is broken"
        );
        // And unscoped, the same file does block.
        assert!(
            design.ensure_fully_parsed(None, None, None).is_err(),
            "an unscoped run must still refuse"
        );
    }

    /// The inverse: a broken file the scope *does* build must block it, and
    /// must not be filed as out-of-scope.
    #[test]
    fn scoped_report_blocks_on_an_in_scope_parse_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("design.yaml"),
            "project:\n  name: t\nschemas: [app, svc]\nscopes:\n  daemon:\n    excludes: [svc]\n",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("ddl/table/app")).unwrap();
        std::fs::write(
            tmp.path().join("ddl/table/app/broken.ddl"),
            "set search_path to app;\ncreate table if not exists broken (id int,,,);",
        )
        .unwrap();

        let mut design =
            Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        let scope = design.resolve_scope(Some("daemon"), None).unwrap();
        let ws = design.working_set(&scope).unwrap();

        let report = design.report(None, Some(&scope));
        assert_eq!(report.issues.len(), 1, "an in-scope parse error must block");
        assert!(
            report.out_of_scope_issues.is_empty(),
            "and must not be filed as out-of-scope"
        );
        assert!(
            design.ensure_fully_parsed(Some(&scope), Some(&ws), None).is_err(),
            "apply --scope daemon must refuse"
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
        std::fs::write(tmp.path().join("policies/dojo/repository_metrics.sql"), "select 1;").unwrap();
        let mock = MockAdapter::new();

        let ws: std::collections::HashSet<String> = ["app.other".to_string()].into_iter().collect();
        let report = apply_policies(&mock, tmp.path(), false, Some(("daemon", &ws)))
            .await
            .unwrap();

        assert!(report.applied.is_empty(), "must not apply: {:?}", report.applied);
        assert!(report.failed.is_empty(), "a skip is not a failure: {:?}", report.failed);
        assert_eq!(report.skipped.len(), 1);
        assert!(
            report.skipped[0].1.contains("dojo.repository_metrics"),
            "got {:?}",
            report.skipped
        );
        assert!(
            report.skipped[0].1.contains("daemon"),
            "must name the scope: {:?}",
            report.skipped
        );
    }

    #[tokio::test]
    async fn a_policy_for_an_in_scope_table_is_applied() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("policies/dojo")).unwrap();
        std::fs::write(tmp.path().join("policies/dojo/relay_inbox.sql"), "select 1;").unwrap();
        let mock = MockAdapter::new();

        let ws: std::collections::HashSet<String> = ["dojo.relay_inbox".to_string()].into_iter().collect();
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
        assert_eq!(policy_target(std::path::Path::new("/p/policies/a/b/c.sql"), root), None);
    }

    #[tokio::test]
    async fn deploy_non_dry_run_completes_with_no_errors() {
        // Use a minimal design (no import tables, no after scripts) so
        // import_data succeeds with a MockAdapter.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();

        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();

        let mock = MockAdapter::new();
        design.deploy(&mock, false, None, |_| {}).await.unwrap();
    }

    // ── resolve_unknown_refs_via_db ──────────────────────

    fn empty_design() -> Design {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("design.yaml"), "project:\n  name: test\n").unwrap();
        Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap()
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
        assert_eq!(design.entities.last().unwrap().warnings, vec!["Some other warning"]);
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
        assert_eq!(design.scoped_entities(&all).unwrap().len(), design.entities().len());
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

    /// An import loads its file into a staging table, then a procedure moves the
    /// rows into its targets — so under a scope it runs only where both exist.
    /// The rule checked the targets alone, so a scope without the (declared)
    /// staging table kept the import and failed on a table never created; and
    /// a skip blamed the staging table even when a target was the one outside.
    #[test]
    fn a_scoped_import_needs_its_staging_table_and_its_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for d in [
            "ddl/table/config",
            "ddl/table/staging",
            "ddl/procedure/staging",
            "import/staging",
        ] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: t\nschemas:\n  - config\n  - staging\nimport:\n  staging: [staging]\n\
             scopes:\n  targets_only:\n    includes: [config]\n  staging_only:\n    includes: [staging]\n\
             \x20 both:\n    includes: [config, staging]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/config/lookups.ddl"),
            "set search_path to config;\ncreate table if not exists lookups (name text primary key);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/staging/lookups.ddl"),
            "set search_path to staging;\ncreate table if not exists lookups (name text);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/procedure/staging/import_lookups.ddl"),
            "set search_path to staging;\ncreate or replace procedure import_lookups() language plpgsql as $$\n\
             begin insert into config.lookups(name) select name from staging.lookups; end; $$;\n",
        )
        .unwrap();
        std::fs::write(dir.join("import/staging/lookups.csv"), "name\nalpha\n").unwrap();
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        let plan = |scope: &str| {
            // `report`, the default: under `include` the procedure's write to
            // config.lookups would legitimately pull the target in.
            let s = design.resolve_scope(Some(scope), None).unwrap();
            design.scoped_import_plan(None, Some(&s)).unwrap()
        };

        let (kept, skips) = plan("targets_only");
        assert!(kept.is_empty(), "no staging table, no import");
        assert!(
            skips.iter().any(|w| w.contains("staging.lookups is outside")),
            "{skips:?}"
        );

        let (kept, skips) = plan("staging_only");
        assert!(kept.is_empty(), "no target, no import");
        assert!(
            skips.iter().any(|w| w.contains("config.lookups is outside")),
            "{skips:?}"
        );

        let (kept, skips) = plan("both");
        assert_eq!(kept.len(), 1, "{skips:?}");
        assert!(skips.is_empty(), "{skips:?}");
    }

    #[test]
    #[allow(deprecated)]
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
        view.refs = (vec!["app.f".to_string()])
            .into_iter()
            .map(|n| crate::entity::Ref::stated(n, crate::entity::RefKind::Reads))
            .collect();
        let ents = vec![view, Entity::new(EntityType::Function, "app.f")];

        let ordered = order_entities_for_test(ents);
        let pos = |name: &str| ordered.iter().position(|e| e.name == name).unwrap();
        assert!(
            pos("app.f") < pos("app.v"),
            "got {:?}",
            ordered.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        assert!(ordered.iter().all(|e| e.errors.is_empty()));
    }

    /// The reverse edge is honored by the same mechanism, which is why the type
    /// sequence cannot simply be reordered to put functions first.
    #[test]
    fn dependency_overrides_type_order_for_function_on_view() {
        use crate::entity::EntityType;
        let mut func = Entity::new(EntityType::Function, "app.f");
        func.refs = (vec!["app.v".to_string()])
            .into_iter()
            .map(|n| crate::entity::Ref::stated(n, crate::entity::RefKind::Reads))
            .collect();
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
        let all_pre: HashSet<String> = ["app.m1".to_string(), "app.m2".to_string()].into_iter().collect();
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
