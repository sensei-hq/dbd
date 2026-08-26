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

    /// Diagnostics for everything the `import/` scan and plan construction left
    /// out, before any scope filtering. Empty when nothing was excluded.
    ///
    /// `name` is the caller's `--name` filter, if any, so an entity name that
    /// matches no staging file is reported as the typo it is rather than as a
    /// clean empty import.
    pub fn import_warnings(&self, name: Option<&str>) -> Vec<String> {
        let mut warnings = Vec::new();

        if let Some(n) = name
            && !self.import_tables.iter().any(|t| t.name == n)
        {
            warnings.push(format!("no staging data file matches --name {n} — nothing to import"));
        }

        if self.import_scan_skips.dir_missing {
            warnings.push(
                "no import/ directory — nothing to import (create import/<schema>/<table>.csv to seed data)"
                    .to_string(),
            );
        }

        for env in self.import_scan_skips.skipped_envs() {
            let count =
                self.import_scan_skips.skipped_by_env.iter().filter(|(e, _)| e == env).count();
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

        warnings
    }

    /// The `import.after` hooks a run under `scope` would execute, and one
    /// warning per hook it would skip.
    ///
    /// Public so `dbd import --dry-run` previews exactly what the real run does
    /// without opening a connection — the same reason
    /// [`import_entry_in_scope`](crate::design::import_entry_in_scope) is
    /// public. Both share [`plan_hooks`](super::hooks::plan_hooks), so the
    /// preview cannot drift from the run.
    pub fn import_after_preview(
        &self,
        scope: Option<&ResolvedScope>,
    ) -> Result<(Vec<String>, Vec<String>)> {
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
        Ok((plan.runnable.into_iter().map(|(script, _)| script).collect(), plan.warnings))
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
    /// Staging tables that failed to parse are excluded here; they are reported
    /// separately via [`Design::import_invalid_tables`].
    pub fn import_plan(&self, name: Option<&str>) -> Vec<ImportPlanEntry> {
        let tables: Vec<&Entity> = self
            .import_tables
            .iter()
            .filter(|t| t.errors.is_empty())
            .filter(|t| name.is_none_or(|n| t.name == n))
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

        let plan = self.import_plan(name);
        let plan: Vec<ImportPlanEntry> = match &narrowed {
            Some((scope_name, ws)) => {
                let (kept, dropped): (Vec<_>, Vec<_>) =
                    plan.into_iter().partition(|e| import_entry_in_scope(e, ws, false));
                for entry in &dropped {
                    warnings.push(format!(
                        "staging table {} not imported — outside scope '{scope_name}'",
                        entry.table.name
                    ));
                }
                kept
            }
            None => plan,
        };

        // Ensure internal dbd procedures are present before any JSONL import runs.
        // Uses CREATE OR REPLACE so it self-heals and stays current with dbd's version.
        if !dry_run {
            let has_jsonl = plan.iter().any(|e| {
                e.table.format.as_deref().is_some_and(|f| f == "json" || f == "jsonl")
            });
            if has_jsonl {
                adapter.ensure_import_procedure().await?;
            }
        }

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
                    let qualified = entry.table.name.replace('.', "\".\"");
                    adapter
                        .execute_script(&format!("TRUNCATE \"{qualified}\""))
                        .await?;
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
            let result =
                if dry_run { Ok(()) } else { adapter.import_data(table, null_value, false).await };
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
                let result = if dry_run { Ok(()) } else { adapter.execute_script(&format!("CALL {proc_name}();")).await };
                report_step_result(&desc, on_done, result)?;
                count += 1;
            }
        }
        Ok(count)
    }

}
