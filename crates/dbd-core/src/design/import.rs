use super::*;

impl Design {
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
        mut progress: Progress<S, D, C>,
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
        let after_scripts = self
            .import_run_after_scripts(adapter, dry_run, &mut progress.on_start, &mut progress.on_done)
            .await?;

        (progress.on_complete)(ImportComplete { tables, procedures, after_scripts });
        Ok(())
    }

    /// Import phase: truncate staging tables (when configured) then load each
    /// entry's data file. Returns the number of tables loaded.
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
        if self.config.import.options.truncate && !dry_run {
            for entry in plan {
                let qualified = entry.table.name.replace('.', "\".\"");
                adapter
                    .execute_script(&format!("TRUNCATE \"{qualified}\""))
                    .await?;
            }
        }

        let mut count = 0;
        for entry in plan {
            let fmt = entry.table.format.as_deref().unwrap_or("csv");
            let desc = format!("import {} ({})", entry.table.name, fmt);
            on_start(&desc);
            let result = if dry_run { Ok(()) } else { adapter.import_data(&entry.table, false).await };
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

    /// Import phase: run the project-global `import.after` scripts. These are
    /// intentionally NOT scope-filtered — they are post-import hooks, not tied to
    /// individual entries; scoped callers ensure their after-scripts are safe.
    /// Returns the number of scripts run.
    async fn import_run_after_scripts<S, D>(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
        on_start: &mut S,
        on_done: &mut D,
    ) -> Result<u32>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        let mut count = 0;
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
            report_step_result(&desc, on_done, result)?;
            count += 1;
        }
        Ok(count)
    }
}
