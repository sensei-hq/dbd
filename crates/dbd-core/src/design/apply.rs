use super::*;

impl Design {
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
        mut progress: Progress<S, D, C>,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(ApplyComplete),
    {
        // Resolve scope → working set (gap-gated under `report`), then filter to
        // the valid, in-scope, name-matching entities. The gate runs even under
        // `dry_run`: a gappy scope is misconfigured regardless of writes.
        let working_set = self.scope_working_set(scope)?;
        // Refuse a design with a file dbd could not read, before any write.
        // `entities_in_scope` drops those entities silently, which is how apply
        // used to report success while never creating the object. Like the scope
        // gate above, this runs under `dry_run` too: an incomplete design is
        // incomplete whether or not we are about to write.
        self.ensure_fully_parsed(scope, working_set.as_ref(), name)?;
        let valid_entities = self.entities_in_scope(scope, working_set.as_ref(), name);

        if dry_run {
            return Ok(());
        }

        // Heal-first: ensure bookkeeping storage exists (and is at the current
        // layout, healing any legacy layout in place) before anything else reads
        // or writes it — every ownership operation calls this up front.
        adapter.heal_bookkeeping().await?;

        // Batch adapters (e.g. Convex) short-circuit — no execution plan needed
        if adapter.prefers_batch_apply() {
            let count = valid_entities.len() as u32;
            let owned: Vec<Entity> = valid_entities.into_iter().cloned().collect();
            adapter.apply_entities(&owned).await?;
            // Batch adapters skip the execution plan (and its SetVersion step), so
            // pin the applied scope here. Preserve the recorded version.
            let db_version = adapter.get_db_version().await?;
            adapter
                .set_project_meta(&self.env, db_version, scope.map(|s| s.name.as_str()))
                .await?;
            (progress.on_complete)(ApplyComplete {
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

        // Running tallies for the on_complete summary.
        let mut counts = ApplyCounts::default();

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
        // through rollback below rather than returning early. Each step's logic
        // lives in `execute_plan_step`, keeping this a thin driver loop.
        let exec: Result<()> = async {
            for step in &plan.steps {
                self.execute_plan_step(
                    adapter,
                    step,
                    &entity_map,
                    scope,
                    &mut counts,
                    &mut progress.on_start,
                    &mut progress.on_done,
                )
                .await?;
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

        // The Current strategy (DB already at latest) emits no SetVersion step, so
        // the scope would never be pinned/re-pinned on an up-to-date apply. Stamp it
        // here — post-commit, alongside the matview sentinels — so every successful
        // `apply`/`deploy` pins the applied scope (needed for `--allow-scope-change`
        // re-pinning and for pinning pre-existing/legacy databases). Preserve the
        // recorded version (`db_version`) to avoid downgrading it. Fresh/Migrate
        // already pinned via their SetVersion step, so skip to avoid a double write.
        if !plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(_))) {
            adapter
                .set_project_meta(&self.env, db_version, scope.map(|s| s.name.as_str()))
                .await?;
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
        adapter.sync_refresh_jobs(&self.all_matview_jobs()).await?;

        (progress.on_complete)(ApplyComplete {
            strategy: plan.strategy,
            from_version: db_version,
            to_version: latest_version,
            applied: counts.applied,
            migrated: counts.migrated,
            created: counts.created,
            dropped: counts.dropped,
        });
        Ok(())
    }

    /// Execute one step of an apply execution plan: run its DDL/meta write via
    /// `adapter`, report progress through `on_start`/`on_done`, and update
    /// `counts`. Factored out of [`Design::apply`] so its plan loop stays a thin
    /// driver.
    #[allow(clippy::too_many_arguments)]
    async fn execute_plan_step<S, D>(
        &self,
        adapter: &dyn DatabaseAdapter,
        step: &ExecutionStep,
        entity_map: &std::collections::HashMap<&str, &Entity>,
        scope: Option<&ResolvedScope>,
        counts: &mut ApplyCounts,
        on_start: &mut S,
        on_done: &mut D,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        match step {
            ExecutionStep::CreateEntity(entity_name) => {
                if let Some(entity) = entity_map.get(entity_name.as_str()) {
                    let desc = format!("{}:{entity_name}", entity.entity_type.tag());
                    on_start(&desc);
                    let result = adapter.apply_entity(entity).await;
                    report_step_result(&desc, on_done, result)?;
                    counts.created += 1;
                    counts.applied += 1;
                }
            }
            ExecutionStep::ApplyEntity(entity_name) => {
                if let Some(entity) = entity_map.get(entity_name.as_str()) {
                    let desc = format!("{}:{entity_name}", entity.entity_type.tag());
                    on_start(&desc);
                    let result = adapter.apply_entity(entity).await;
                    report_step_result(&desc, on_done, result)?;
                    counts.applied += 1;
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
                report_step_result(&desc, on_done, result)?;
                counts.migrated += 1;
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
                report_step_result(&desc, on_done, result)?;
                counts.dropped += 1;
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
        Ok(())
    }

    /// Deploy the full schema: apply DDL, import seed data, then apply RLS
    /// policies.
    ///
    /// This is the same three-phase pipeline `dbd deploy` runs, and the CLI
    /// delegates to it — one implementation, so the library and the command can
    /// not drift apart. dbd handles fresh / migrate / current strategy
    /// automatically, so this is safe to call on every bootstrap (idempotent
    /// when the schema is already current).
    ///
    /// Policies come from `policies/` and are applied unconditionally, unlike
    /// [`Design::apply`] where they are opt-in: "deploy" means bringing the
    /// database fully up from source. A policy file that fails does NOT fail the
    /// deploy — it lands in [`DeployComplete::policies`]`.failed` and the caller
    /// reports it as a warning.
    ///
    /// `progress.on_complete(summary)` is called once after all phases with the
    /// combined counts and every non-fatal diagnostic
    /// ([`DeployComplete::warnings`]).
    pub async fn deploy<C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        on_complete: C,
    ) -> Result<()>
    where
        C: FnMut(DeployComplete),
    {
        self.deploy_with_progress(adapter, dry_run, scope, Progress {
            on_start: |_: &str| {},
            on_done: |_: &str, _: Option<&str>| {},
            on_complete,
        })
        .await
    }

    /// [`Design::deploy`] with per-step progress callbacks, for callers that
    /// render a spinner or log each step (this is what `dbd deploy` uses).
    ///
    /// Identical pipeline and reporting — only the progress plumbing differs.
    pub async fn deploy_with_progress<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut progress: Progress<S, D, C>,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(DeployComplete),
    {
        let mut apply_summary: Option<ApplyComplete> = None;
        let mut import_summary: Option<ImportComplete> = None;

        self.apply(adapter, None, dry_run, scope, Progress {
            on_start: &mut progress.on_start,
            on_done: &mut progress.on_done,
            on_complete: |s| {
                apply_summary = Some(s);
            },
        })
        .await?;

        // Always run the import phase, even when the plan is empty: it still
        // executes the project's `import.after` scripts, and its summary is what
        // reports "0 table(s) loaded" plus the reason why.
        self.import_data(adapter, None, dry_run, scope, Progress {
            on_start: &mut progress.on_start,
            on_done: &mut progress.on_done,
            on_complete: |s| {
                import_summary = Some(s);
            },
        })
        .await?;

        let policies = super::apply_policies(adapter, &self.project_dir, dry_run).await?;

        (progress.on_complete)(DeployComplete {
            apply: apply_summary.unwrap_or_default(),
            import: import_summary.unwrap_or_default(),
            policies,
        });
        Ok(())
    }
}
