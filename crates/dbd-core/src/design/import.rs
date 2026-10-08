use std::collections::{HashMap, HashSet};

use super::*;

/// The staging table → loader decision [`Design::import_loaders`] makes.
#[derive(Default)]
struct Loaders<'a> {
    by_table: HashMap<&'a str, &'a Entity>,
    /// Tables whose loader dbd cannot pick, with the candidates.
    undecided: Vec<(&'a str, Vec<&'a Entity>)>,
}

impl<'a> Loaders<'a> {
    /// Make `loader` the loader of `table`. A procedure loads one table — it
    /// is CALLed once — so a second claim on it leaves `table` undecided.
    fn claim(&mut self, table: &'a str, loader: &'a Entity) {
        if self.loads_any(&loader.name) {
            self.undecided(table, vec![loader]);
        } else {
            self.by_table.insert(table, loader);
        }
    }

    fn undecided(&mut self, table: &'a str, candidates: Vec<&'a Entity>) {
        self.undecided.push((table, candidates));
    }

    fn loads_any(&self, procedure: &str) -> bool {
        self.by_table.values().any(|p| p.name == procedure)
    }
}

/// The names of `entities`, each once, in first-seen order.
fn distinct_names<'a>(entities: impl Iterator<Item = &'a Entity>) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    entities.map(|e| e.name.as_str()).filter(|n| seen.insert(*n)).collect()
}

/// `name` without its schema: `staging.items` → `items`.
fn bare_name(name: &str) -> &str {
    name.split_once('.').map_or(name, |(_, bare)| bare)
}

impl Design {
    /// Staging data files excluded from the import plan because their entity
    /// failed to parse.
    ///
    /// Exposed so callers can report them — a data file that dbd could not read
    /// must never vanish from a run without a word.
    pub fn import_invalid_tables(&self) -> Vec<&Entity> {
        self.import_tables.iter().filter(|t| !t.errors.is_empty()).collect()
    }

    /// What every import path refuses before it touches the database: the
    /// import itself, its `--dry-run`, and `deploy` (which validates its
    /// import phase before applying anything, so a refusal never lands half a
    /// deploy). `name` is the caller's `-n`, if any.
    ///
    /// Public so a caller can refuse before it even connects — the mistake is
    /// in the command line or the project, and an error about the database
    /// would hide it.
    pub fn check_import_plan(&self, name: Option<&str>) -> Result<()> {
        if let Some(n) = name {
            self.check_import_name(n)?;
        }
        self.check_import_staging(name)?;
        self.check_import_loaders(name)
    }

    /// Whether `import.staging` lets an import load into `schema`.
    ///
    /// An empty list declares no restriction: a project that never set it
    /// loads every schema, as it always has. A target with no schema cannot be
    /// vouched for once a list is set — it resolves through the database's
    /// search_path, to whatever schema that names.
    fn import_schema_allowed(&self, schema: Option<&str>) -> bool {
        let staging = &self.config.import.staging;
        staging.is_empty() || schema.is_some_and(|s| staging.iter().any(|allowed| allowed == s))
    }

    /// Refuse an import into a table outside `import.staging`.
    ///
    /// `import.staging` lists the schemas an import may load, and the load
    /// truncates its table first. The list was parsed and never read, so a
    /// data file under `import/app/` emptied a live table and replaced its
    /// rows. Public for the ad-hoc `-n <table> -f <file>` load, which names a
    /// table rather than a data file and is held to the same list.
    pub fn check_import_target(&self, table: &str) -> Result<()> {
        let schema = table.split_once('.').map(|(schema, _)| schema);
        if self.import_schema_allowed(schema) {
            return Ok(());
        }
        let allowed = self.config.import.staging.join(", ");
        Err(DbdError::Config(match schema {
            Some(schema) => format!(
                "{table} is not in a staging schema — import.staging allows only: {allowed}. An import \
                 loads staging tables; list '{schema}' under import.staging if it is one"
            ),
            None => format!(
                "{table} names no schema, so import.staging ({allowed}) cannot vouch for it — \
                 qualify it with its staging schema, e.g. {}.{table}",
                self.config.import.staging[0]
            ),
        }))
    }

    /// Refuse every data file this run would load outside `import.staging`
    /// (see [`Design::check_import_target`]), naming each — all at once, so
    /// fixing one does not just reveal the next.
    ///
    /// Only the files `name` selects are judged: `-n` narrows the run to one
    /// file, and a misplaced file the run does not touch cannot harm it.
    fn check_import_staging(&self, name: Option<&str>) -> Result<()> {
        let outside: Vec<String> = self
            .import_tables
            .iter()
            .filter(|t| name.is_none_or(|n| t.name == n))
            .filter(|t| !self.import_schema_allowed(t.schema.as_deref()))
            .map(|t| {
                let file = t
                    .file
                    .as_deref()
                    .map(|f| f.strip_prefix(&self.project_dir).unwrap_or(f));
                format!(
                    "  {} → {}",
                    file.map(|f| f.display().to_string()).unwrap_or_default(),
                    t.name
                )
            })
            .collect();
        if outside.is_empty() {
            return Ok(());
        }
        Err(DbdError::Config(format!(
            "import refused: import.staging allows only {}, and these data files load other schemas:\n{}\n\
             An import loads (and by default truncates) staging tables only. Move each file under \
             import/<staging schema>/, or list its schema under import.staging if it is a staging schema.",
            self.config.import.staging.join(", "),
            outside.join("\n")
        )))
    }

    /// A `-n` must select a data file this run loads.
    ///
    /// It was a warning, and the run went on — exiting 0 and running every
    /// `import.after` hook — so a typo still ran the project's loader script.
    /// Selecting nothing is never what the name was for, so each way it can
    /// fail is refused, saying which it is.
    fn check_import_name(&self, name: &str) -> Result<()> {
        if self.import_tables.iter().any(|t| t.name == name) {
            return Ok(());
        }
        // The wrong-`-e` case: the file exists, under another env's folder.
        // Naming it is what tells the user which flag to change.
        let elsewhere: Vec<String> = self
            .import_scan_skips
            .skipped_by_env
            .iter()
            .filter(|(_, rel)| Entity::from_import_file(&Path::new("import").join(rel)).name == name)
            .map(|(_, rel)| format!("import/{}", rel.display()))
            .collect();
        if !elsewhere.is_empty() {
            return Err(DbdError::Config(format!(
                "no data file for {name} in this run: {} belongs to another env, and this run's env is '{}'",
                elsewhere.join(", "),
                self.env
            )));
        }
        // The words every command uses for a name that matches nothing.
        self.resolve_name(name, None)?;
        Err(DbdError::Config(format!(
            "{name} has no data file under import/ — nothing to import \
             (a table's data file is import/<schema>/<table>.csv, .tsv, .json or .jsonl)"
        )))
    }

    /// Diagnostics for everything the `import/` scan and plan construction left
    /// out, before any scope filtering. Empty when nothing was excluded.
    ///
    /// `name` is the caller's `--name` filter, if any. A name that selects no
    /// data file is not reported here: it is refused by
    /// [`Design::check_import_plan`].
    pub fn import_warnings(&self, name: Option<&str>) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.import_scan_skips.dir_missing {
            warnings.push(
                "no import/ directory — nothing to import (create import/<schema>/<table>.csv to seed data)"
                    .to_string(),
            );
        }

        for env in self.import_scan_skips.skipped_envs() {
            let count = self
                .import_scan_skips
                .skipped_by_env
                .iter()
                .filter(|(e, _)| e == env)
                .count();
            warnings.push(format!(
                "{count} data file(s) under import/{env}/ skipped — this run's env is '{}'",
                self.env
            ));
        }

        for table in self.import_invalid_tables() {
            let file = table.file.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
            warnings.push(format!(
                "staging table {} not imported — data file failed to parse ({file}): {}",
                table.name,
                table.errors.join("; ")
            ));
        }

        // Judged per table, not per data file: a table may have two (a shared
        // one and an env's own), and saying everything twice says nothing more.
        let loaders = self.import_loaders();
        for table in distinct_names(self.loadable_tables(name)) {
            match loaders.by_table.get(table) {
                // A function is never run as a loader (see `import_plan`). When
                // one reads a staging table that no procedure loads, it is most
                // likely a loader written as a function — say so, or the target
                // stays empty in silence.
                None => {
                    for function in self.routines_reading(EntityType::Function, table) {
                        warnings.push(format!(
                            "{table} has no loader procedure — function {} reads it but is not run: \
                             loaders are CALLed, so write it as a procedure, or run it from import.after",
                            function.name
                        ));
                    }
                }
                // Each staging table has one loader. Another procedure reading
                // it is not run — said, because every reader after the first
                // used to be dropped without a word, and one of them may have
                // been a load.
                Some(loader) => {
                    for idle in self
                        .routines_reading(EntityType::Procedure, table)
                        .filter(|p| !loaders.loads_any(&p.name))
                    {
                        warnings.push(format!(
                            "{} reads {table} but is not called — {} is its loader; call {} from there, \
                             or run it from import.after",
                            idle.name, loader.name, idle.name
                        ));
                    }
                }
            }
        }

        warnings
    }

    /// The `import.after` hooks a run under `scope` would execute, and one
    /// warning per hook it would skip.
    ///
    /// Public so `dbd import --dry-run` previews exactly what the real run does
    /// without opening a connection — the same reason
    /// [`import_entry_in_scope`] is
    /// public. Both share `plan_hooks`, so the
    /// preview cannot drift from the run.
    pub fn import_after_preview(&self, scope: Option<&ResolvedScope>) -> Result<(Vec<String>, Vec<String>)> {
        let narrowed = match scope {
            Some(s) if !s.is_all => Some((s.name.as_str(), self.working_set(s)?)),
            _ => None,
        };
        let plan = hooks::plan_hooks(
            &self.project_dir,
            &self.config.import.after,
            hooks::HookKind::After,
            narrowed.as_ref().map(|(name, ws)| (*name, ws)),
        )?;
        Ok((
            plan.runnable.into_iter().map(|(script, _)| script).collect(),
            plan.warnings,
        ))
    }

    /// The import plan under `scope`, plus why each left-out entry was left out.
    ///
    /// An entry loads its file into a staging table and a procedure moves the
    /// rows into its targets, so it runs only where both can happen: every
    /// table the procedure writes is in the working set, and so is the staging
    /// table — when the design declares it (one it does not declare is not the
    /// scope's to judge). A proc-less entry needs its own table. Checking the
    /// targets alone kept an import whose staging table the scope never built.
    ///
    /// The one rule for `import`, `deploy`, and both commands' `--dry-run`.
    /// `None` or the all-scope keeps the whole plan. Refuses what
    /// [`Design::check_import_plan`] refuses, so no preview can show a plan
    /// the real run would not carry out.
    pub fn scoped_import_plan(
        &self,
        name: Option<&str>,
        scope: Option<&ResolvedScope>,
    ) -> Result<(Vec<ImportPlanEntry>, Vec<String>)> {
        self.check_import_plan(name)?;
        let plan = self.import_plan(name);
        let Some(scope) = scope.filter(|s| !s.is_all) else {
            return Ok((plan, Vec::new()));
        };
        let ws = self.working_set(scope)?;
        let declared: std::collections::HashSet<&str> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Table)
            .map(|e| e.name.as_str())
            .collect();
        let mut kept = Vec::new();
        let mut skips = Vec::new();
        for entry in plan {
            let staging = &entry.table.name;
            let staging_needed = entry.writes.is_empty() || declared.contains(staging.as_str());
            let missing = if staging_needed && !ws.contains(staging) {
                Some(staging.clone())
            } else {
                entry.writes.iter().find(|w| !ws.contains(*w)).cloned()
            };
            match missing {
                Some(m) => skips.push(format!(
                    "{staging} not imported — {m} is outside scope '{}'",
                    scope.name
                )),
                None => kept.push(entry),
            }
        }
        Ok((kept, skips))
    }

    /// Build the import plan: staging tables paired with procedures, ordered by dependencies.
    ///
    /// Each staging table's loader is picked by [`Design::import_loaders`]:
    /// among the procedures that *read* it, the one named `import_<table>`,
    /// else the only one that reads no other staging table, else the only one
    /// no other table claimed. Procedures are then ordered so that if proc A
    /// writes to table X, and proc B's target references X (via FK), A runs
    /// before B.
    ///
    /// Example: import_lookups reads staging.lookups, writes config.lookups
    ///          import_lookup_values reads staging.lookup_values, writes config.lookup_values
    ///          config.lookup_values has FK to config.lookups
    ///          → import_lookups must run before import_lookup_values
    ///
    /// Only procedures are matched. The loader step is `CALL`, which Postgres
    /// refuses for a function, and a function that reads a staging table is far
    /// more often a helper or a trigger function than a loader — matching one
    /// failed the whole import. Such a function is reported instead (see
    /// [`Design::import_warnings`]).
    ///
    /// A table whose loader dbd cannot pick has no procedure here; every import
    /// path refuses it through [`Design::check_import_plan`]. Staging tables
    /// that failed to parse are excluded here; they are reported separately
    /// via [`Design::import_invalid_tables`].
    pub fn import_plan(&self, name: Option<&str>) -> Vec<ImportPlanEntry> {
        let loaders = self.import_loaders();
        let mut entries: Vec<ImportPlanEntry> = self
            .loadable_tables(name)
            .map(|table| {
                let loader = loaders.by_table.get(table.name.as_str());
                ImportPlanEntry {
                    table: table.clone(),
                    procedure: loader.map(|p| p.name.clone()),
                    writes: loader
                        .map(|p| p.writes().map(|r| r.name.clone()).collect())
                        .unwrap_or_default(),
                }
            })
            .collect();

        // Sort by write dependencies:
        // If entry A writes to a table that entry B's target table references (via FK),
        // A must come before B.
        self.sort_import_plan(&mut entries);

        // A table with two data files is two entries and one load: both files
        // load, then its loader runs once over the rows of both. Every entry
        // keeps the loader's `writes`, so a scope judges the two alike.
        let mut called: HashSet<String> = HashSet::new();
        for entry in &mut entries {
            if let Some(procedure) = &entry.procedure
                && !called.insert(procedure.clone())
            {
                entry.procedure = None;
            }
        }

        entries
    }

    /// The data files an import loads — every one dbd could read, or the ones
    /// `name` selects.
    fn loadable_tables<'a>(&'a self, name: Option<&'a str>) -> impl Iterator<Item = &'a Entity> + 'a {
        self.import_tables
            .iter()
            .filter(|t| t.errors.is_empty())
            .filter(move |t| name.is_none_or(|n| t.name == n))
    }

    /// Which procedure loads each staging table.
    ///
    /// Matching took the first procedure that read a staging table. A loader
    /// that joins a second staging table — cities looking up their country —
    /// was then matched to both: it ran twice, the second table's own loader
    /// never ran, and the import reported every call as a success. Now, among
    /// the procedures reading a table, in order:
    ///
    /// 1. the one named `import_<table>` — the name every doc example uses,
    ///    and the way to settle anything the rules below cannot;
    /// 2. the one that reads no other staging table;
    /// 3. the one no other table claimed by 1 or 2 — and when that leaves
    ///    several, the one writing a table of the same name.
    ///
    /// A procedure loads at most one table. One left as the only reader of
    /// several (it joins them, and nothing else loads them) is called once,
    /// for the table it writes a namesake of, else the first. Two or more
    /// candidates still equal at the deciding rule is ambiguous: the table gets
    /// no loader here and [`Design::check_import_plan`] refuses it, rather than
    /// guess and drop the other's load in silence.
    ///
    /// Decided over every data file at once, so a `-n` run calls the loader a
    /// full run would.
    fn import_loaders(&self) -> Loaders<'_> {
        // Per table, not per data file: a table's second file (a shared one
        // and an env's own) is the same load, not a second table claiming
        // its loader.
        let tables: Vec<&str> = distinct_names(self.loadable_tables(None));
        // "Another staging table" covers every data file's table and every
        // table declared in a staging schema, loaded this run or not.
        let staging: HashSet<&str> = tables
            .iter()
            .copied()
            .chain(
                self.entities
                    .iter()
                    .filter(|e| e.entity_type == EntityType::Table)
                    .filter(|e| {
                        let schema = e.name.split_once('.').map(|(s, _)| s);
                        schema.is_some_and(|s| self.config.import.staging.iter().any(|x| x == s))
                    })
                    .map(|e| e.name.as_str()),
            )
            .collect();
        let reads_only = |p: &Entity, table: &str| {
            p.reads()
                .filter(|r| staging.contains(r.name.as_str()))
                .all(|r| r.name == table)
        };
        let writes_namesake = |p: &Entity, table: &str| p.writes().any(|w| bare_name(&w.name) == bare_name(table));

        let mut loaders = Loaders::default();
        let mut pending: Vec<(&str, Vec<&Entity>)> = Vec::new();
        for &table in &tables {
            let candidates: Vec<&Entity> = self.routines_reading(EntityType::Procedure, table).collect();
            let conventional = format!("import_{}", bare_name(table));
            let named: Vec<&Entity> = candidates
                .iter()
                .copied()
                .filter(|p| bare_name(&p.name) == conventional)
                .collect();
            let exclusive: Vec<&Entity> = candidates.iter().copied().filter(|p| reads_only(p, table)).collect();
            match (named.as_slice(), exclusive.as_slice()) {
                ([loader], _) | ([], [loader]) => loaders.claim(table, loader),
                ([_, _, ..], _) => loaders.undecided(table, named),
                ([], [_, _, ..]) => loaders.undecided(table, exclusive),
                ([], []) if candidates.is_empty() => {}
                ([], []) => pending.push((table, candidates)),
            }
        }

        // Rule 3, once rules 1 and 2 have claimed what they can. A table left
        // with no unclaimed reader is data another table's loader joins, not
        // a load of its own.
        let mut picks: Vec<(&str, &Entity)> = Vec::new();
        for (table, candidates) in pending {
            let unclaimed: Vec<&Entity> = candidates.into_iter().filter(|p| !loaders.loads_any(&p.name)).collect();
            match unclaimed.as_slice() {
                [] => {}
                [loader] => picks.push((table, *loader)),
                _ => {
                    let namesakes: Vec<&Entity> = unclaimed
                        .iter()
                        .copied()
                        .filter(|p| writes_namesake(p, table))
                        .collect();
                    match namesakes.as_slice() {
                        [loader] => picks.push((table, *loader)),
                        _ => loaders.undecided(table, unclaimed),
                    }
                }
            }
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for (_, loader) in &picks {
            if !seen.insert(loader.name.as_str()) {
                continue;
            }
            let its: Vec<&str> = picks
                .iter()
                .filter(|(_, p)| p.name == loader.name)
                .map(|(t, _)| *t)
                .collect();
            let table = its
                .iter()
                .copied()
                .find(|t| writes_namesake(loader, t))
                .unwrap_or(its[0]);
            loaders.claim(table, loader);
        }
        loaders
    }

    /// Refuse every staging table this run loads whose loader dbd cannot pick
    /// (see [`Design::import_loaders`]), naming the candidates and the way out.
    fn check_import_loaders(&self, name: Option<&str>) -> Result<()> {
        let loaders = self.import_loaders();
        let mut refusals: Vec<String> = loaders
            .undecided
            .iter()
            .filter(|(table, _)| name.is_none_or(|n| n == *table))
            .map(|(table, candidates)| {
                let mut names: Vec<&str> = candidates.iter().map(|p| p.name.as_str()).collect();
                names.sort_unstable();
                let why = match names.as_slice() {
                    [one] => format!("{one} claims it but already loads another staging table"),
                    _ => format!("{} have an equal claim", names.join(", ")),
                };
                format!(
                    "cannot tell which procedure loads {table}: {why}. An import calls one loader per \
                     staging table — name it import_{}, and call the others from it or from import.after",
                    bare_name(table)
                )
            })
            .collect();
        if refusals.is_empty() {
            return Ok(());
        }
        refusals.sort();
        Err(DbdError::Config(refusals.join("\n")))
    }

    /// The `kind` routines (procedures or functions) whose body reads `table`.
    fn routines_reading<'a>(&'a self, kind: EntityType, table: &'a str) -> impl Iterator<Item = &'a Entity> + 'a {
        self.entities
            .iter()
            .filter(move |e| e.entity_type == kind && e.reads().any(|r| r.name == table))
    }

    /// Sort import entries so that procedures writing to tables referenced by other
    /// procedures' targets come first.
    fn sort_import_plan(&self, entries: &mut Vec<ImportPlanEntry>) {
        // Build dependency: entry depends on another if its writes target has a FK
        // to a table written by another entry.
        // For now, use the DDL entity's refers to check FK deps between write targets.
        let entity_refs: std::collections::HashMap<String, Vec<String>> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Table)
            .map(|e| (e.name.clone(), e.refers().map(str::to_string).collect::<Vec<_>>()))
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
                        !entries
                            .iter()
                            .enumerate()
                            .any(|(j, other)| !placed[j] && j != i && other.writes.contains(dep))
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
        mut progress: Progress<S, D, C>,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(ImportComplete),
    {
        let mut warnings = self.import_warnings(name);

        // Resolve the scope once. The plan filter and the after-script hooks
        // must agree on what "in scope" means — the two disagreeing is exactly
        // how a loader came to run against half-loaded data.
        let narrowed = match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                Some((s.name.as_str(), self.working_set(s)?))
            }
            _ => None,
        };

        let (plan, skips) = self.scoped_import_plan(name, scope)?;
        warnings.extend(skips);

        // Run the import phases in order, tallying each for the summary.
        let tables = self
            .import_load_staging(adapter, &plan, dry_run, &mut progress.on_start, &mut progress.on_done)
            .await?;
        let procedures = self
            .import_call_procedures(adapter, &plan, dry_run, &mut progress.on_start, &mut progress.on_done)
            .await?;
        let outside = narrowed.as_ref().map(|(name, ws)| (*name, self.managed_outside(ws)));
        let hook_scope = outside.as_ref().map(|(name, out)| (*name, out));
        let after = hooks::run_hooks(
            adapter,
            &self.project_dir,
            &self.config.import.after,
            hooks::HookKind::After,
            hook_scope,
            dry_run,
            &mut progress,
        )
        .await?;
        warnings.extend(after.warnings);

        (progress.on_complete)(ImportComplete {
            tables,
            procedures,
            after_scripts: after.ran,
            warnings,
        });
        Ok(())
    }

    /// Import phase: truncate staging tables (per-table override wins over the
    /// global setting) then load each entry's data file. Returns the number of
    /// tables loaded.
    async fn import_load_staging<S, D>(
        &self,
        adapter: &dyn DatabaseAdapter,
        plan: &[ImportPlanEntry],
        dry_run: bool,
        on_start: &mut S,
        on_done: &mut D,
    ) -> Result<u32>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        if !dry_run {
            for entry in plan {
                if self.config.import.table_truncate(&entry.table.name) {
                    adapter.truncate_table(&entry.table.name).await?;
                }
            }
        }

        let mut count = 0;
        for entry in plan {
            // A per-table `format` override forces that parser regardless of the
            // file extension; clone the entity only when an override applies.
            let overridden;
            let table = match self.config.import.table_format(&entry.table.name) {
                Some(fmt) if entry.table.format.as_deref() != Some(fmt) => {
                    let mut t = entry.table.clone();
                    t.format = Some(fmt.to_string());
                    overridden = t;
                    &overridden
                }
                _ => &entry.table,
            };
            let fmt = table.format.as_deref().unwrap_or("csv");
            let desc = format!("import {} ({})", table.name, fmt);
            on_start(&desc);
            let null_value = self.config.import.table_null_value(&entry.table.name);
            let result = if dry_run {
                Ok(())
            } else {
                adapter.import_data(table, null_value, false).await
            };
            report_step_result(&desc, on_done, result)?;
            count += 1;
        }
        Ok(count)
    }

    /// Import phase: call each entry's import procedure. Returns the number
    /// called.
    async fn import_call_procedures<S, D>(
        &self,
        adapter: &dyn DatabaseAdapter,
        plan: &[ImportPlanEntry],
        dry_run: bool,
        on_start: &mut S,
        on_done: &mut D,
    ) -> Result<u32>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        let mut count = 0;
        for entry in plan {
            if let Some(ref proc_name) = entry.procedure {
                let desc = format!("call {proc_name}()");
                on_start(&desc);
                let result = if dry_run {
                    Ok(())
                } else {
                    adapter.execute_script(&format!("CALL {proc_name}();")).await
                };
                report_step_result(&desc, on_done, result)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::mock::MockAdapter;

    /// A project with one staging table and its data file, a target table, and
    /// whichever routines `routines` lays out (`(type folder, name, ddl)`).
    fn project_with(routines: &[(&str, &str, &str)]) -> (tempfile::TempDir, Design) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let write = |rel: &str, body: &str| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        write("design.yaml", "project:\n  name: t\nschemas:\n  - stage\n  - app\n");
        write(
            "ddl/table/stage/items.ddl",
            "set search_path to stage;\ncreate table if not exists items (id integer, name text);\n",
        );
        write(
            "ddl/table/app/items.ddl",
            "set search_path to app;\ncreate table if not exists items (id integer primary key, name text);\n",
        );
        write("import/stage/items.csv", "id,name\n1,a\n");
        for (folder, name, ddl) in routines {
            write(&format!("ddl/{folder}/stage/{name}.ddl"), ddl);
        }
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        (tmp, design)
    }

    const HELPER_FUNCTION: (&str, &str, &str) = (
        "function",
        "item_count",
        "set search_path to stage;\n\
         create or replace function item_count() returns bigint language sql\n\
         as $$ select count(*) from stage.items $$;\n",
    );

    const LOADER_PROCEDURE: (&str, &str, &str) = (
        "procedure",
        "import_items",
        "set search_path to stage;\n\
         create or replace procedure import_items() language sql\n\
         as $$ insert into app.items (id, name) select id, name from stage.items $$;\n",
    );

    /// The import runs each matched loader with `CALL`, which Postgres refuses
    /// for a function ("is not a procedure"). Matching a function that merely
    /// reads a staging table — a helper, a trigger function — failed the whole
    /// import. Loaders are procedures, as the import docs have always said.
    #[test]
    fn a_function_that_reads_a_staging_table_is_not_matched_as_its_loader() {
        let (_tmp, design) = project_with(&[HELPER_FUNCTION]);
        let function = design
            .entities()
            .iter()
            .find(|e| e.name == "stage.item_count")
            .expect("the helper function loads");
        assert!(
            function.reads().any(|r| r.name == "stage.items"),
            "precondition: the function reads the staging table"
        );

        let plan = design.import_plan(None);
        let entry = plan.iter().find(|e| e.table.name == "stage.items").expect("planned");
        assert_eq!(entry.procedure, None, "a function must not be run as a loader");
        assert!(entry.writes.is_empty(), "{:?}", entry.writes);
    }

    /// Skipping a function is not allowed to be silent: someone who wrote their
    /// loader as a function would otherwise see the staging table load and the
    /// target stay empty, with nothing saying why.
    #[test]
    fn a_function_that_reads_an_unloaded_staging_table_is_reported() {
        let (_tmp, design) = project_with(&[HELPER_FUNCTION]);
        let warnings = design.import_warnings(None);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("stage.items") && w.contains("stage.item_count") && w.contains("procedure")),
            "the unrun function must be named, with what to do about it: {warnings:?}"
        );
    }

    /// A project whose `import.staging` is `staging` (or absent when `None`),
    /// with a staging table and a live `app.order_notes`, and `files` written
    /// under `import/`.
    fn project_with_staging(staging: Option<&str>, files: &[(&str, &str)]) -> (tempfile::TempDir, Design) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let write = |rel: &str, body: &str| {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };
        let import = staging
            .map(|s| format!("import:\n  staging: [{s}]\n"))
            .unwrap_or_default();
        write(
            "design.yaml",
            &format!("project:\n  name: t\nschemas:\n  - staging\n  - app\n{import}"),
        );
        write(
            "ddl/table/staging/items.ddl",
            "set search_path to staging;\ncreate table if not exists items (id integer, name text);\n",
        );
        write(
            "ddl/table/app/order_notes.ddl",
            "set search_path to app;\ncreate table if not exists order_notes (id integer primary key, note text);\n",
        );
        for (rel, body) in files {
            write(&format!("import/{rel}"), body);
        }
        let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
        (tmp, design)
    }

    const STAGED_ITEMS: (&str, &str) = ("staging/items.csv", "id,name\n1,a\n");
    const LIVE_NOTES: (&str, &str) = ("app/order_notes.csv", "id,note\n1,replaced\n");

    /// `import.staging` lists the schemas an import may load, and the docs say
    /// the import fails for any other — but the list was parsed and never
    /// read. A data file under `import/app/` was loaded like any staging file:
    /// `app.order_notes` was TRUNCATEd and its rows replaced. It is refused,
    /// naming the file, before anything — the staging file's truncate
    /// included — touches the database.
    #[tokio::test]
    async fn a_data_file_outside_the_staging_schemas_is_refused_before_any_truncate() {
        let (_tmp, design) = project_with_staging(Some("staging"), &[STAGED_ITEMS, LIVE_NOTES]);

        let mock = MockAdapter::new();
        let err = design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .expect_err("a file loading app.order_notes must be refused");

        let msg = err.to_string();
        assert!(
            msg.contains("import/app/order_notes.csv") && msg.contains("app.order_notes"),
            "the refusal names the file and the table it would load: {msg}"
        );
        assert!(msg.contains("import.staging"), "and the setting that refuses it: {msg}");
        assert!(
            mock.scripts.lock().unwrap().is_empty(),
            "no TRUNCATE (of either table), no CALL, no hook: {:?}",
            mock.scripts.lock().unwrap()
        );
        assert!(mock.imported.lock().unwrap().is_empty(), "nothing loads");
    }

    /// Every preview prints this plan, so a dry run refuses what the run
    /// refuses rather than listing a load that will never happen.
    #[test]
    fn the_import_plan_refuses_a_data_file_outside_the_staging_schemas() {
        let (_tmp, design) = project_with_staging(Some("staging"), &[STAGED_ITEMS, LIVE_NOTES]);
        let err = design
            .scoped_import_plan(None, None)
            .expect_err("the plan must refuse it too");
        assert!(err.to_string().contains("import/app/order_notes.csv"), "{err}");
    }

    /// `deploy` applies the schema, then imports. Refusing only at the import
    /// would leave the schema applied and the deploy failed half way; the
    /// import's refusals are checked before the first statement instead.
    #[tokio::test]
    async fn deploy_refuses_a_data_file_outside_the_staging_schemas_before_applying() {
        let (_tmp, design) = project_with_staging(Some("staging"), &[STAGED_ITEMS, LIVE_NOTES]);
        let mock = MockAdapter::new();
        let err = design
            .deploy(&mock, false, None, |_| {})
            .await
            .expect_err("deploy must refuse the misplaced file");
        assert!(err.to_string().contains("import/app/order_notes.csv"), "{err}");
        assert!(
            mock.applied.lock().unwrap().is_empty(),
            "nothing applied: {:?}",
            mock.applied.lock().unwrap()
        );
        assert!(mock.scripts.lock().unwrap().is_empty(), "nothing run");
    }

    /// `-n` narrows the run to one file, and only what the run loads is
    /// judged: a misplaced file elsewhere does not block loading a staging one.
    #[tokio::test]
    async fn a_named_staging_file_loads_though_another_file_is_misplaced() {
        let (_tmp, design) = project_with_staging(Some("staging"), &[STAGED_ITEMS, LIVE_NOTES]);
        let mock = MockAdapter::new();
        design
            .import_data(&mock, Some("staging.items"), false, None, Progress::none())
            .await
            .expect("the named staging file is allowed");
        assert_eq!(*mock.imported.lock().unwrap(), vec!["staging.items".to_string()]);
    }

    /// A project that never declared `import.staging` has nothing to enforce,
    /// and loads every schema as it always has.
    #[tokio::test]
    async fn without_import_staging_every_schema_loads_as_before() {
        let (_tmp, design) = project_with_staging(None, &[STAGED_ITEMS, LIVE_NOTES]);
        let mock = MockAdapter::new();
        design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .expect("no staging list, no restriction");
        let mut imported = mock.imported.lock().unwrap().clone();
        imported.sort();
        assert_eq!(
            imported,
            vec!["app.order_notes".to_string(), "staging.items".to_string()]
        );
    }

    /// With a real loader procedure present, a helper function reading the same
    /// table neither displaces it nor draws a warning.
    #[test]
    fn a_procedure_is_the_loader_even_when_a_function_also_reads_the_table() {
        let (_tmp, design) = project_with(&[HELPER_FUNCTION, LOADER_PROCEDURE]);
        let plan = design.import_plan(None);
        let entry = plan.iter().find(|e| e.table.name == "stage.items").expect("planned");
        assert_eq!(entry.procedure.as_deref(), Some("stage.import_items"));
        assert_eq!(entry.writes, vec!["app.items".to_string()]);
        assert!(
            !design.import_warnings(None).iter().any(|w| w.contains("item_count")),
            "{:?}",
            design.import_warnings(None)
        );
    }

    // ── Which procedure loads which staging table ─────────────────────────────

    /// A project from `(path, body)` pairs, `design.yaml` included.
    fn project_from(files: &[(&str, &str)]) -> (tempfile::TempDir, Design) {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();
        (tmp, design)
    }

    /// The audit's project: countries and cities staged, each with a live
    /// target (`app.cities` references `app.countries`), and a city loader that
    /// joins the staged countries to find each city's country.
    const CITIES_PROJECT: &[(&str, &str)] = &[
        (
            "design.yaml",
            "project:\n  name: t\nschemas:\n  - staging\n  - app\nimport:\n  staging: [staging]\n",
        ),
        (
            "ddl/table/staging/countries.ddl",
            "set search_path to staging;\ncreate table if not exists countries (code text, name text);\n",
        ),
        (
            "ddl/table/staging/cities.ddl",
            "set search_path to staging;\ncreate table if not exists cities (name text, country text);\n",
        ),
        (
            "ddl/table/staging/languages.ddl",
            "set search_path to staging;\ncreate table if not exists languages (code text);\n",
        ),
        (
            "ddl/table/app/countries.ddl",
            "set search_path to app;\ncreate table if not exists countries (code text primary key, name text);\n",
        ),
        (
            "ddl/table/app/cities.ddl",
            "set search_path to app;\ncreate table if not exists cities (\n  name text\n, country_code text references app.countries (code)\n);\n",
        ),
        (
            "ddl/procedure/staging/load_countries.ddl",
            "set search_path to staging;\n\
             create or replace procedure load_countries() language sql\n\
             as $$ insert into app.countries (code, name) select code, name from staging.countries $$;\n",
        ),
        (
            "ddl/procedure/staging/load_cities.ddl",
            "set search_path to staging;\n\
             create or replace procedure load_cities() language sql\n\
             as $$ insert into app.cities (name, country_code)\n\
                   select c.name, k.code from staging.cities c\n\
                   join staging.countries s on s.name = c.country\n\
                   join app.countries k on k.code = s.code $$;\n",
        ),
        ("import/staging/countries.csv", "code,name\nDE,Germany\n"),
        ("import/staging/cities.csv", "name,country\nBerlin,Germany\n"),
        ("import/staging/languages.csv", "code\nde\n"),
    ];

    fn loader_of<'a>(plan: &'a [ImportPlanEntry], table: &str) -> Option<&'a str> {
        plan.iter()
            .find(|e| e.table.name == table)
            .unwrap_or_else(|| panic!("{table} is planned"))
            .procedure
            .as_deref()
    }

    /// `load_cities` reads `staging.countries` too — it joins it to find each
    /// city's country — and loader matching took the first procedure reading a
    /// staging table. Both staged tables got `load_cities`, so it ran twice,
    /// `load_countries` never ran, and the import reported "2 procedure(s)
    /// called" over two empty targets. A procedure that reads only one staging
    /// table is that table's loader; a joining loader belongs to the table no
    /// one else loads.
    #[test]
    fn a_loader_that_joins_another_staging_table_is_not_that_tables_loader() {
        let (_tmp, design) = project_from(CITIES_PROJECT);
        let plan = design.import_plan(None);
        assert_eq!(loader_of(&plan, "staging.countries"), Some("staging.load_countries"));
        assert_eq!(loader_of(&plan, "staging.cities"), Some("staging.load_cities"));
        assert_eq!(loader_of(&plan, "staging.languages"), None);
    }

    /// The run calls each loader once, countries before the cities that
    /// reference them.
    #[tokio::test]
    async fn the_import_calls_each_loader_once_in_dependency_order() {
        let (_tmp, design) = project_from(CITIES_PROJECT);
        let mock = MockAdapter::new();
        design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .unwrap();
        let calls: Vec<String> = mock
            .scripts
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("CALL"))
            .cloned()
            .collect();
        assert_eq!(
            calls,
            vec![
                "CALL staging.load_countries();".to_string(),
                "CALL staging.load_cities();".to_string()
            ]
        );
    }

    /// `-n` picks the loader a full run would: the one decision, made over the
    /// whole import set, then narrowed.
    #[test]
    fn a_named_import_calls_the_loader_a_full_run_would() {
        let (_tmp, design) = project_from(CITIES_PROJECT);
        let plan = design.import_plan(Some("staging.countries"));
        assert_eq!(loader_of(&plan, "staging.countries"), Some("staging.load_countries"));
    }

    /// A single loader joining two staged tables that nothing else loads is
    /// one load, so it is called once — it was matched to both tables and
    /// called twice. It belongs to the table it writes (`app.cities`).
    #[tokio::test]
    async fn a_loader_reading_two_staging_tables_is_called_once() {
        let files: Vec<(&str, &str)> = CITIES_PROJECT
            .iter()
            .filter(|(rel, _)| !rel.ends_with("load_countries.ddl"))
            .copied()
            .collect();
        let (_tmp, design) = project_from(&files);
        let plan = design.import_plan(None);
        assert_eq!(loader_of(&plan, "staging.cities"), Some("staging.load_cities"));
        assert_eq!(loader_of(&plan, "staging.countries"), None);

        let mock = MockAdapter::new();
        design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .unwrap();
        let calls = mock
            .scripts
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("CALL"))
            .count();
        assert_eq!(calls, 1, "{:?}", mock.scripts.lock().unwrap());
    }

    /// A project of one staged table and the procedures given, each reading it.
    fn items_with_procedures(names: &[&str]) -> (tempfile::TempDir, Design) {
        let mut files: Vec<(String, String)> = vec![
            (
                "design.yaml".into(),
                "project:\n  name: t\nschemas:\n  - staging\n  - app\nimport:\n  staging: [staging]\n".into(),
            ),
            (
                "ddl/table/staging/items.ddl".into(),
                "set search_path to staging;\ncreate table if not exists items (id integer);\n".into(),
            ),
            ("import/staging/items.csv".into(), "id\n1\n".into()),
        ];
        for name in names {
            files.push((
                format!("ddl/table/app/{name}_out.ddl"),
                format!("set search_path to app;\ncreate table if not exists {name}_out (id integer);\n"),
            ));
            files.push((
                format!("ddl/procedure/staging/{name}.ddl"),
                format!(
                    "set search_path to staging;\n\
                     create or replace procedure {name}() language sql\n\
                     as $$ insert into app.{name}_out (id) select id from staging.items $$;\n"
                ),
            ));
        }
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        project_from(&borrowed)
    }

    /// Every doc example names a loader `import_<table>`. When more than one
    /// procedure reads a staging table, that name says which one loads it.
    #[test]
    fn a_procedure_named_import_table_is_that_tables_loader() {
        let (_tmp, design) = items_with_procedures(&["audit_items", "import_items"]);
        let plan = design.import_plan(None);
        assert_eq!(loader_of(&plan, "staging.items"), Some("staging.import_items"));
    }

    /// The reader that lost is not run, and that is said: before, every reader
    /// after the first was dropped without a word.
    #[test]
    fn a_procedure_reading_a_staging_table_it_does_not_load_is_reported() {
        let (_tmp, design) = items_with_procedures(&["audit_items", "import_items"]);
        let warnings = design.import_warnings(None);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("staging.audit_items") && w.contains("staging.import_items")),
            "the idle reader and the loader chosen over it are named: {warnings:?}"
        );
    }

    /// Two procedures with an equal claim — each reads only this table, and
    /// neither is named for it — leave nothing to decide by. Calling the first
    /// was a guess that silently dropped the other's load; the import is
    /// refused, naming both and how to settle it, before anything runs.
    #[tokio::test]
    async fn two_equal_candidate_loaders_are_refused_naming_both() {
        let (_tmp, design) = items_with_procedures(&["sync_items", "copy_items"]);
        let err = design
            .scoped_import_plan(None, None)
            .expect_err("an ambiguous loader must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("staging.items")
                && msg.contains("staging.sync_items")
                && msg.contains("staging.copy_items")
                && msg.contains("import_items"),
            "the table, both candidates and the way out: {msg}"
        );

        let mock = MockAdapter::new();
        design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .expect_err("the import refuses it too");
        assert!(
            mock.scripts.lock().unwrap().is_empty() && mock.imported.lock().unwrap().is_empty(),
            "before anything runs"
        );
    }

    // ── import.after under a scope: the preview and the run agree ────────────

    /// A staging table, a live `core.items`, an `import.after` hook updating
    /// `core.items`, and two scopes: `core_only` keeps the hook's table,
    /// `no_core` leaves it out.
    const HOOKED_PROJECT: &[(&str, &str)] = &[
        (
            "design.yaml",
            "project:\n  name: t\nschemas:\n  - staging\n  - core\nimport:\n  staging: [staging]\n  after:\n    - import/after.sql\n\
             scopes:\n  core_only:\n    includes: [core]\n  no_core:\n    excludes: [core]\n",
        ),
        (
            "ddl/table/staging/items.ddl",
            "set search_path to staging;\ncreate table if not exists items (id integer);\n",
        ),
        (
            "ddl/table/core/items.ddl",
            "set search_path to core;\ncreate table if not exists items (id integer primary key);\n",
        ),
        ("import/staging/items.csv", "id\n1\n"),
        ("import/after.sql", "update core.items set id = id;\n"),
    ];

    /// Whether the real import under `scope` runs the `import.after` hook.
    async fn the_run_executes_the_hook(design: &Design, scope: &str) -> bool {
        let resolved = design.resolve_scope(Some(scope), None).unwrap();
        let mock = MockAdapter::new();
        design
            .import_data(&mock, None, false, Some(&resolved), Progress::none())
            .await
            .unwrap();
        mock.scripts
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("update core.items"))
    }

    /// The hook touches `core.items`, which `core_only` builds: the run
    /// executes it. The preview said it was skipped — it judged the hook
    /// against the working set as though that were what the scope leaves out,
    /// the reverse of the run's rule.
    #[tokio::test]
    async fn the_import_preview_runs_a_hook_the_scoped_run_runs() {
        let (_tmp, design) = project_from(HOOKED_PROJECT);
        assert!(
            the_run_executes_the_hook(&design, "core_only").await,
            "precondition: the run executes it"
        );

        let resolved = design.resolve_scope(Some("core_only"), None).unwrap();
        let (runnable, skips) = design.import_after_preview(Some(&resolved)).unwrap();
        assert_eq!(runnable, vec!["import/after.sql".to_string()], "skips: {skips:?}");
        assert!(skips.is_empty(), "{skips:?}");
    }

    /// The other direction: `no_core` leaves `core.items` out, so the run
    /// skips the hook — and the preview said it would run it.
    #[tokio::test]
    async fn the_import_preview_skips_a_hook_the_scoped_run_skips() {
        let (_tmp, design) = project_from(HOOKED_PROJECT);
        assert!(
            !the_run_executes_the_hook(&design, "no_core").await,
            "precondition: the run skips it"
        );

        let resolved = design.resolve_scope(Some("no_core"), None).unwrap();
        let (runnable, skips) = design.import_after_preview(Some(&resolved)).unwrap();
        assert!(runnable.is_empty(), "{runnable:?}");
        assert!(
            skips.len() == 1 && skips[0].contains("import/after.sql") && skips[0].contains("core.items"),
            "the skip names the hook and the table the scope leaves out: {skips:?}"
        );
    }

    /// Shared seed data in `import/staging/` and dev-only rows in
    /// `import/dev/staging/` are two data files for one table. Both load, and
    /// then the table's loader runs — once, over both. It ran once per file,
    /// so a loader that inserts moved the shared rows twice; and the second
    /// file must not read as a second table claiming the same loader.
    #[tokio::test]
    async fn a_table_with_two_data_files_calls_its_loader_once() {
        let (_tmp, design) = project_from(&[
            (
                "design.yaml",
                "project:\n  name: t\nschemas:\n  - staging\n  - app\nimport:\n  staging: [staging]\n",
            ),
            (
                "ddl/table/staging/items.ddl",
                "set search_path to staging;\ncreate table if not exists items (id integer);\n",
            ),
            (
                "ddl/table/app/items.ddl",
                "set search_path to app;\ncreate table if not exists items (id integer);\n",
            ),
            (
                "ddl/procedure/staging/import_items.ddl",
                "set search_path to staging;\n\
                 create or replace procedure import_items() language sql\n\
                 as $$ insert into app.items (id) select id from staging.items $$;\n",
            ),
            ("import/staging/items.csv", "id\n1\n"),
            ("import/dev/staging/items.csv", "id\n2\n"),
        ]);

        let (plan, _) = design.scoped_import_plan(None, None).expect("not refused");
        assert_eq!(plan.len(), 2, "both files load");
        assert_eq!(
            plan.iter().filter(|e| e.procedure.is_some()).count(),
            1,
            "the plan (and so the dry run) lists the call once: {:?}",
            plan.iter().map(|e| (&e.table.name, &e.procedure)).collect::<Vec<_>>()
        );

        let mock = MockAdapter::new();
        design
            .import_data(&mock, None, false, None, Progress::none())
            .await
            .unwrap();
        let calls: Vec<String> = mock
            .scripts
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("CALL"))
            .cloned()
            .collect();
        assert_eq!(calls, vec!["CALL staging.import_items();".to_string()]);
        assert_eq!(mock.imported.lock().unwrap().len(), 2, "both files load first");
    }
}
