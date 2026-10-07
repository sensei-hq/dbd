use super::*;

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
        self.check_import_staging(name)
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

        // A function is never run as a loader (see `import_plan`). When one reads
        // a staging table that no procedure loads, it is most likely a loader
        // written as a function — say so, or the target stays empty in silence.
        for entry in self.import_plan(name).iter().filter(|e| e.procedure.is_none()) {
            for function in self.routines_reading(EntityType::Function, &entry.table.name) {
                warnings.push(format!(
                    "{} has no loader procedure — function {} reads it but is not run: loaders are \
                     CALLed, so write it as a procedure, or run it from import.after",
                    entry.table.name, function.name
                ));
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
    /// Procedure matching is based on reads/writes analysis, not naming convention:
    /// - A procedure that *reads from* a staging table is its import procedure
    /// - Procedures are ordered so that if proc A writes to table X, and proc B
    ///   reads from table X (via FK), A runs before B
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
    /// Staging tables that failed to parse are excluded here; they are reported
    /// separately via [`Design::import_invalid_tables`].
    pub fn import_plan(&self, name: Option<&str>) -> Vec<ImportPlanEntry> {
        let tables: Vec<&Entity> = self
            .import_tables
            .iter()
            .filter(|t| t.errors.is_empty())
            .filter(|t| name.is_none_or(|n| t.name == n))
            .collect();

        // Build entries: match each staging table to the procedure that reads from it
        let mut entries: Vec<ImportPlanEntry> = tables
            .iter()
            .map(|table| {
                let matched_proc = self.routines_reading(EntityType::Procedure, &table.name).next();

                ImportPlanEntry {
                    table: (*table).clone(),
                    procedure: matched_proc.map(|p| p.name.clone()),
                    writes: matched_proc
                        .map(|p| p.writes().map(|r| r.name.clone()).collect())
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
        let hook_scope = narrowed.as_ref().map(|(name, ws)| (*name, ws));
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
}
