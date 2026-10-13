use super::*;

impl Design {
    /// The entities [`Design::apply`] would act on, after every gate it runs
    /// before writing — and without a database.
    ///
    /// This is what a preview must list. `dbd apply --dry-run` used to filter
    /// the entities itself, and the copy drifted from the real run twice: it
    /// listed every target extension whatever the scope's `extensions:`
    /// allowlist said, and it printed "no issues" over a file the real run
    /// refused to read. Sharing the gate is what keeps the two from disagreeing
    /// again.
    ///
    /// Errors exactly where `apply` would: a dependency gap under `report`, a
    /// closure that needs what the scope excludes, or a file that did not parse.
    pub fn entities_to_apply(&self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Result<Vec<&Entity>> {
        Ok(self.gated_apply_set(name, scope)?.1)
    }

    /// The pending migrations [`Self::apply`] would run against `adapter`'s
    /// database: the migration steps of the plan it builds there — from the
    /// version the database records, under the same gates and scope — each
    /// with the statements of the files it runs. Writes nothing.
    ///
    /// `apply --dry-run` listed entities and never these, though the run
    /// executes them first — including a migration that drops a table. Built
    /// from the run's own plan rather than from `migrations/`, so a preview
    /// lists what the run does under a scope and not every file on disk; and a
    /// drop of a table this database does not have is marked as skipped, as
    /// the run skips it.
    pub async fn pending_migration_preview(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        scope: Option<&ResolvedScope>,
    ) -> Result<MigrationPreview> {
        let (working_set, entities) = self.gated_apply_set(name, scope)?;
        let to_version = self.config.project.version.unwrap_or(0);
        // A batch adapter applies without an execution plan, so no migration runs.
        if adapter.prefers_batch_apply() {
            return Ok(MigrationPreview {
                to_version,
                ..Default::default()
            });
        }
        let from_version = adapter.get_db_version().await?;
        let pending = snapshot::pending_migrations(from_version, &self.project_dir);
        refuse_a_named_apply_over_pending_migrations(name, from_version, to_version, &pending)?;
        ensure_no_pending_todos(&pending)?;
        let owned: Vec<Entity> = entities.iter().map(|e| (*e).clone()).collect();
        let plan = build_execution_plan(&owned, from_version, to_version, &pending, working_set.as_ref());

        // The version each step belongs to: the plan records a version after
        // its last step, so walking backwards the next record is its own.
        let mut versions: Vec<u32> = Vec::with_capacity(plan.steps.len());
        let mut recorded = to_version;
        for step in plan.steps.iter().rev() {
            if let ExecutionStep::RecordMigration { version, .. } = step {
                recorded = *version;
            }
            versions.push(recorded);
        }
        versions.reverse();

        let mut steps = Vec::new();
        // The entities whose enum values a version commits first: their
        // `.sql` runs those statements again as no-ops, so the preview lists
        // them once, where they take effect.
        let mut hoisted: std::collections::HashSet<(u32, &str)> = std::collections::HashSet::new();
        for (step, version) in plan.steps.iter().zip(versions) {
            match step {
                ExecutionStep::AddEnumValues {
                    migration_version,
                    scripts,
                } => {
                    for (entity, script) in scripts {
                        hoisted.insert((*migration_version, entity.as_str()));
                        steps.push(MigrationStepPreview {
                            version: *migration_version,
                            entity: entity.clone(),
                            action: MigrationAction::EnumValues,
                            statements: planned_statements_of(script),
                            runs: true,
                        });
                    }
                }
                ExecutionStep::CreateEntity(entity) => {
                    steps.push(MigrationStepPreview {
                        version,
                        entity: entity.clone(),
                        action: MigrationAction::Create,
                        statements: Vec::new(),
                        runs: true,
                    });
                }
                // An altered entity with no `.sql` of its own — a table whose
                // version only maps data — runs nothing at this step.
                ExecutionStep::MigrateEntity {
                    entity_name,
                    migration_sql_path,
                    migration_version,
                } if migration_sql_path.exists() => {
                    let sql = crate::source_text::read_to_string(migration_sql_path)?;
                    let statements = if hoisted.contains(&(*migration_version, entity_name.as_str())) {
                        let rest =
                            planned_statements_where(&sql, |piece| !crate::script::is_enum_value_addition(piece));
                        // Nothing left once its values moved ahead.
                        if rest.is_empty() {
                            continue;
                        }
                        rest
                    } else {
                        planned_statements_of(&sql)
                    };
                    steps.push(MigrationStepPreview {
                        version: *migration_version,
                        entity: entity_name.clone(),
                        action: MigrationAction::Migrate,
                        statements,
                        runs: true,
                    });
                }
                ExecutionStep::MigrateData {
                    entity_name,
                    data_sql_path,
                    migration_version,
                } => {
                    steps.push(MigrationStepPreview {
                        version: *migration_version,
                        entity: entity_name.clone(),
                        action: MigrationAction::Data,
                        statements: planned_statements(data_sql_path)?,
                        runs: true,
                    });
                }
                ExecutionStep::DropEntity {
                    entity_name,
                    drop_sql_path,
                    migration_version,
                } => {
                    let runs = drop_sql_path.exists() && adapter.resolve_entity(entity_name).await?.is_some();
                    steps.push(MigrationStepPreview {
                        version: *migration_version,
                        entity: entity_name.clone(),
                        action: MigrationAction::Drop,
                        statements: planned_statements(drop_sql_path)?,
                        runs,
                    });
                }
                ExecutionStep::MigrateEntity { .. }
                | ExecutionStep::ApplyEntity(_)
                | ExecutionStep::RecordMigration { .. }
                | ExecutionStep::SetVersion(_) => {}
            }
        }
        let recorded = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                ExecutionStep::RecordMigration { version, .. } => Some(*version),
                _ => None,
            })
            .collect();
        Ok(MigrationPreview {
            from_version,
            to_version,
            steps,
            recorded,
        })
    }

    /// The scope's working set and the entities an apply acts on, once every
    /// pre-write gate has passed. Shared by [`Self::apply`] and
    /// [`Self::entities_to_apply`] so a preview cannot pass what the run refuses.
    #[allow(clippy::type_complexity)]
    fn gated_apply_set(
        &self,
        name: Option<&str>,
        scope: Option<&ResolvedScope>,
    ) -> Result<(Option<std::collections::HashSet<String>>, Vec<&Entity>)> {
        // Resolve scope → working set (gap-gated under `report`), then filter to
        // the valid, in-scope, name-matching entities. The gate runs even under
        // `dry_run`: a gappy scope is misconfigured regardless of writes.
        let working_set = self.scope_working_set(scope)?;
        // A `name` that selects nothing applied nothing and reported success.
        if let Some(n) = name {
            self.resolve_name(n, scope)?;
        }
        // Refuse a design with a file dbd could not read, before any write.
        // `entities_in_scope` drops those entities silently, which is how apply
        // used to report success while never creating the object. Like the scope
        // gate above, this runs under `dry_run` too: an incomplete design is
        // incomplete whether or not we are about to write.
        self.ensure_fully_parsed(scope, working_set.as_ref(), name)?;
        let entities = self.entities_in_scope(scope, working_set.as_ref(), name);
        // The name exists, but as something apply never builds: an `external:`
        // declaration, or an import staging table with no DDL of its own.
        if let Some(n) = name
            && entities.is_empty()
        {
            return Err(DbdError::Config(format!(
                "{n} has no DDL in this design for apply to run — it is declared external or is an import staging table"
            )));
        }
        Ok((working_set, entities))
    }

    /// Apply all entities to the database via the adapter.
    ///
    /// Uses `build_execution_plan()` to determine strategy (Fresh / Migrate / Current)
    /// and executes the plan steps in order.
    ///
    /// The three callbacks travel together in one [`Progress`] value — they are
    /// **not** three arguments. Saying otherwise is what this doc comment used
    /// to do, and every downstream example copied the misreading; the snippet
    /// below is a doctest so the next one cannot.
    ///
    /// - `on_start(desc)` — just before each visible step
    /// - `on_done(desc, err)` — after each; `err` is `None` on success
    /// - `on_complete(summary)` — once, after all steps succeed
    ///
    /// Use [`Progress::none`] for a silent run.
    ///
    /// ```no_run
    /// use dbd_core::{Design, design::Progress};
    /// use std::path::Path;
    ///
    /// # async fn example() -> dbd_core::Result<()> {
    /// let design = Design::from_config(Path::new("design.yaml"), "prod")?;
    /// let adapter = dbd_core::connect("postgres://localhost/mydb", "myproject").await?;
    ///
    /// // Silent.
    /// design.apply(&*adapter, None, false, None, Progress::none()).await?;
    ///
    /// // Reporting. `scope` is `None` for the whole design.
    /// let scope = design.resolve_scope(None, None)?;
    /// design
    ///     .apply(
    ///         &*adapter,
    ///         None,
    ///         false,
    ///         Some(&scope),
    ///         Progress {
    ///             on_start: |desc: &str| println!("→ {desc}"),
    ///             on_done: |desc: &str, err: Option<&str>| {
    ///                 if let Some(e) = err {
    ///                     eprintln!("✗ {desc}: {e}")
    ///                 }
    ///             },
    ///             on_complete: |s: dbd_core::ApplyComplete| println!("applied {} entities", s.applied),
    ///         },
    ///     )
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub async fn apply<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        progress: Progress<S, D, C>,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(ApplyComplete),
    {
        // A real run holds the database's dbd write lock, shared, from its
        // first read to its last write: another project's reset waits for it
        // and reads what it built, and it waits for one (#7).
        let run = self.apply_run(adapter, name, dry_run, scope, progress);
        if dry_run {
            return run.await;
        }
        ownership::with_write_lock(adapter, false, run).await
    }

    /// [`Design::apply`]'s run, under the write lock when it writes.
    async fn apply_run<S, D, C>(
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
        let (working_set, valid_entities) = self.gated_apply_set(name, scope)?;
        // Before the first hook or write: a dev run against a database marked
        // prod is refused, not left to re-mark it (see `check_env_guard`). And
        // a project older than the database it points at is refused before
        // anything runs — the `apply.before` hooks included — and under
        // `dry_run` too, since the database is there to ask. Both read the
        // database's marks without healing the bookkeeping, which is itself a
        // write.
        self.check_apply_guards(&self.database_marks(adapter).await?)?;
        // Also before the first hook, and under `dry_run` (see the function).
        let db_version = adapter.get_db_version().await?;
        let latest_version = self.config.project.version.unwrap_or(0);
        let pending = snapshot::pending_migrations(db_version, &self.project_dir);
        refuse_a_named_apply_over_pending_migrations(name, db_version, latest_version, &pending)?;
        // A pending migration whose `data.sql` still holds a `-- TODO:` is
        // refused here, before the first hook — and under `dry_run`, which
        // listed it and said "no issues" while the run refused it. A batch
        // adapter runs no migration, so has none to refuse.
        if !adapter.prefers_batch_apply() {
            ensure_no_pending_todos(&pending)?;
        }
        // The last gate, before the first hook: refuse a design declaring an
        // entity another owner holds (#7). A real run heals the bookkeeping
        // first. In one transaction it claims with its work, below; outside
        // one nothing it writes rolls back — its hooks included — so it claims
        // now, before the first of them: see `Design::claim_first`.
        self.preflight(
            adapter,
            if dry_run {
                ownership::Preflight::Read
            } else {
                ownership::Preflight::Write
            },
        )
        .await?;
        // What a pending migration drops that another owner depends on is
        // refused before anything runs, dry run included (#7) — read from the
        // run's own plan, built here without writing.
        if !adapter.prefers_batch_apply() && !pending.is_empty() {
            let owned: Vec<Entity> = valid_entities.iter().map(|e| (*e).clone()).collect();
            let preview = build_execution_plan(&owned, db_version, latest_version, &pending, working_set.as_ref());
            self.guard_migration_drops(adapter, &preview).await?;
        }

        // A hook is filtered against the same working set the entities were.
        // `None` — unscoped or the all-scope — runs every hook, and skips the
        // derivation cost entirely on that common path.
        let hook_scope = self.hook_scope(scope, working_set.as_ref());
        let hook_scope = hook_scope.as_ref().map(|(name, outside)| (*name, outside));

        let outside_txn = !adapter.supports_transactional_apply() || std::env::var_os("DBD_NO_TX").is_some();
        if !dry_run && outside_txn {
            // The before-hooks' own refusals — a file missing, a path outside
            // the project — come first: a refused run claims nothing.
            hooks::plan_hooks(
                self.hook_source(),
                &self.config.apply.before,
                hooks::HookKind::Before,
                hook_scope,
            )?;
            self.claim(adapter).await?;
        }

        // `apply.before` runs here: after the scope and parse gates, so a bad
        // design still refuses before any hook fires, and before the first
        // entity, so a hook can prepare the ground the DDL lands on.
        let before = hooks::run_hooks(
            adapter,
            self.hook_source(),
            &self.config.apply.before,
            hooks::HookKind::Before,
            hook_scope,
            dry_run,
            &mut progress,
        )
        .await?;

        if dry_run {
            // A dry run executes nothing but must still name every script the
            // real run would — including the after-hooks, which the early
            // return below would otherwise hide, and including a missing file,
            // which is a misconfiguration whether or not we are about to write.
            hooks::run_hooks(
                adapter,
                self.hook_source(),
                &self.config.apply.after,
                hooks::HookKind::After,
                hook_scope,
                dry_run,
                &mut progress,
            )
            .await?;
            return Ok(());
        }

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
            // Batch adapters take this shortcut past the execution plan, but a
            // hook is a hook: leaving `apply.after` out here would recreate, for
            // one backend, exactly the silent no-op this feature removes.
            let after = hooks::run_hooks(
                adapter,
                self.hook_source(),
                &self.config.apply.after,
                hooks::HookKind::After,
                hook_scope,
                dry_run,
                &mut progress,
            )
            .await?;
            (progress.on_complete)(ApplyComplete {
                strategy: ApplyStrategy::Current,
                from_version: 0,
                to_version: 0,
                applied: count,
                migrated: 0,
                created: 0,
                dropped: 0,
                before_scripts: before.ran,
                after_scripts: after.ran,
                warnings: [before.warnings, after.warnings].concat(),
                previously_built: false,
            });
            return Ok(());
        }

        // Build execution plan
        let db_version = adapter.get_db_version().await?;
        let latest_version = self.config.project.version.unwrap_or(0);
        let pending = snapshot::pending_migrations(db_version, &self.project_dir);

        // Filter entities by name if scoped
        let scoped_entities: Vec<Entity> = valid_entities.iter().map(|e| (*e).clone()).collect();
        let plan = build_execution_plan(
            &scoped_entities,
            db_version,
            latest_version,
            &pending,
            working_set.as_ref(),
        );

        // No recorded version does not mean an empty database: one built
        // before the project had a version records none, and neither does one
        // built by hand. Asked before the plan runs, since its SetVersion step
        // writes the very record this looks for.
        let previously_built = plan.strategy == ApplyStrategy::Fresh && {
            let live: std::collections::HashSet<String> = adapter.list_entities().await?.into_iter().collect();
            adapter.get_project_meta().await?.is_some() || valid_entities.iter().any(|e| live.contains(&e.name))
        };

        // Running tallies for the on_complete summary.
        let mut counts = ApplyCounts::default();

        // Build entity lookup for ApplyEntity / CreateEntity steps
        let entity_map: std::collections::HashMap<&str, &Entity> =
            self.entities.iter().map(|e| (e.name.as_str(), e)).collect();

        // Materialized views this run applies (respecting the scope + name
        // filter). Capture which of them ALREADY exist before applying, so that
        // afterwards we stamp the `dbd:hash` sentinel only on the ones this run
        // newly creates. Skip the state query entirely when there are none.
        let applied_matviews: Vec<&Entity> = valid_entities
            .iter()
            .copied()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .collect();
        let pre_existing_states = if applied_matviews.is_empty() {
            std::collections::HashMap::new()
        } else {
            adapter.matview_states().await?
        };
        let pre_existing_matviews: std::collections::HashSet<String> = pre_existing_states.keys().cloned().collect();

        // Wrap the whole plan in one transaction when the backend supports it,
        // so an interrupted upgrade rolls back to the prior schema instead of
        // leaving objects half-applied. `DBD_NO_TX` opts out for plans that
        // contain non-transactional DDL (e.g. CREATE INDEX CONCURRENTLY).
        //
        // The one exception is a version that adds enum values: the run
        // commits after running them, and carries on in a new transaction —
        // see `ExecutionStep::AddEnumValues`.
        let use_txn =
            adapter.supports_transactional_apply() && std::env::var_os("DBD_NO_TX").is_none() && !plan.steps.is_empty();

        if use_txn {
            adapter.begin_batch().await?;
        } else if !outside_txn {
            // A plan with nothing to build opens no transaction: claim here.
            self.claim(adapter).await?;
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
                // A version's new enum values commit before the rest of it
                // uses them (see `ExecutionStep::AddEnumValues`) — claimed
                // first, so nothing commits unclaimed (#7).
                if use_txn && matches!(step, ExecutionStep::AddEnumValues { .. }) {
                    self.claim_built(adapter).await?;
                    adapter.commit_batch().await?;
                    adapter.begin_batch().await?;
                }
            }
            // Built: claim it, in the batch, so the claim commits with the
            // work and rolls back with it (#7). Claimed already outside one.
            if use_txn {
                self.claim_built(adapter).await
            } else {
                self.settle(adapter).await
            }
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
        let adopted = self
            .verified_unstamped_matviews(adapter, &applied_matviews, &pre_existing_states)
            .await?;
        for e in matviews_to_stamp(&applied_matviews, &pre_existing_matviews)
            .into_iter()
            .chain(adopted)
        {
            adapter
                .stamp_matview(
                    &crate::reconcile::qualified_entity_name(e),
                    &crate::reconcile::matview_stamp(&crate::reconcile::matview_hash(e)),
                )
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

        // `apply.after` closes with the phase: every entity is applied, every
        // matview stamped, and — critically — `policies/` has not run yet.
        // Publications and grants attach to objects that must already exist;
        // RLS is independent of them, so it stays last (see `deploy_with_progress`).
        let after = hooks::run_hooks(
            adapter,
            self.hook_source(),
            &self.config.apply.after,
            hooks::HookKind::After,
            hook_scope,
            dry_run,
            &mut progress,
        )
        .await?;

        (progress.on_complete)(ApplyComplete {
            strategy: plan.strategy,
            from_version: db_version,
            to_version: latest_version,
            applied: counts.applied,
            migrated: counts.migrated,
            created: counts.created,
            dropped: counts.dropped,
            before_scripts: before.ran,
            after_scripts: after.ran,
            warnings: [before.warnings, after.warnings].concat(),
            previously_built,
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
                    counts.applied(entity_name);
                }
            }
            ExecutionStep::ApplyEntity(entity_name) => {
                if let Some(entity) = entity_map.get(entity_name.as_str()) {
                    let desc = format!("{}:{entity_name}", entity.entity_type.tag());
                    on_start(&desc);
                    let result = adapter.apply_entity(entity).await;
                    report_step_result(&desc, on_done, result)?;
                    counts.applied(entity_name);
                }
            }
            ExecutionStep::MigrateEntity {
                entity_name,
                migration_sql_path,
                migration_version,
            } => {
                let type_tag = entity_map
                    .get(entity_name.as_str())
                    .map(|e| e.entity_type.tag())
                    .unwrap_or_else(|| "entity".to_string());
                let desc = format!("migrate {type_tag}:{entity_name} → v{migration_version}");
                on_start(&desc);
                let result: Result<()> = async {
                    if migration_sql_path.exists() {
                        // dbd writes migrations as UTF-8, but a hand-edit in
                        // SSMS does not stay that way — see `source_text`.
                        let sql = crate::source_text::read_to_string(migration_sql_path)?;
                        // Its enum values already ran, and committed, at the
                        // head of the version; here they are no-ops.
                        adapter
                            .execute_project_script(
                                &crate::script::idempotent_enum_value_additions(&sql),
                                self.migration_search_path(entity_map.get(entity_name.as_str()).copied()),
                            )
                            .await?;
                    }
                    Ok(())
                }
                .await;
                report_step_result(&desc, on_done, result)?;
                counts.migrated(entity_name);
            }
            ExecutionStep::AddEnumValues {
                migration_version,
                scripts,
            } => {
                for (entity_name, script) in scripts {
                    let type_tag = entity_map
                        .get(entity_name.as_str())
                        .map(|e| e.entity_type.tag())
                        .unwrap_or_else(|| "entity".to_string());
                    let desc = format!("add values {type_tag}:{entity_name} → v{migration_version}");
                    on_start(&desc);
                    let result = adapter
                        .execute_project_script(
                            script,
                            self.migration_search_path(entity_map.get(entity_name.as_str()).copied()),
                        )
                        .await;
                    report_step_result(&desc, on_done, result)?;
                }
            }
            ExecutionStep::MigrateData {
                entity_name,
                data_sql_path,
                migration_version,
            } => {
                let type_tag = entity_map
                    .get(entity_name.as_str())
                    .map(|e| e.entity_type.tag())
                    .unwrap_or_else(|| "entity".to_string());
                let desc = format!("migrate data {type_tag}:{entity_name} → v{migration_version}");
                on_start(&desc);
                let result: Result<()> = async {
                    let sql = crate::source_text::read_to_string(data_sql_path)?;
                    adapter
                        .execute_project_script(
                            &sql,
                            self.migration_search_path(entity_map.get(entity_name.as_str()).copied()),
                        )
                        .await
                }
                .await;
                report_step_result(&desc, on_done, result)?;
                counts.migrated(entity_name);
            }
            ExecutionStep::DropEntity {
                entity_name,
                drop_sql_path,
                migration_version,
            } => {
                let type_tag = entity_map
                    .get(entity_name.as_str())
                    .map(|e| e.entity_type.tag())
                    .unwrap_or_else(|| "entity".to_string());
                let desc = format!("drop {type_tag}:{entity_name} (v{migration_version})");
                on_start(&desc);
                // Only where the table still exists: a generated drop is a bare
                // `DROP TABLE … CASCADE`, and a database whose scope never had
                // the table would fail on it.
                let result: Result<bool> = async {
                    if drop_sql_path.exists() && adapter.resolve_entity(entity_name).await?.is_some() {
                        // Decoded like the migration's `.sql` and `.data.sql`.
                        let sql = crate::source_text::read_to_string(drop_sql_path)?;
                        // Gone from the design, so `source.search_path` alone.
                        adapter
                            .execute_project_script(&sql, self.migration_search_path(None))
                            .await?;
                        return Ok(true);
                    }
                    Ok(false)
                }
                .await;
                // Counted only when it ran: the summary must not claim a drop
                // that left the table where it was.
                let dropped = matches!(result, Ok(true));
                report_step_result(&desc, on_done, result.map(|_| ()))?;
                if dropped {
                    counts.dropped += 1;
                }
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

    /// The path a migration file of `entity` runs on when it states none of
    /// its own: the one `entity`'s CREATE runs on — the path its DDL file sets
    /// before it (the last, when the file sets several), else
    /// `source.search_path` — and, for a table the design no longer has,
    /// `source.search_path`. `None` is the session's default.
    ///
    /// `dbd snapshot` writes a column's type as the DDL file wrote it, so a
    /// generated `ADD COLUMN was mood` needs the file's own path to find
    /// `mood`; the project's alone failed it with `type "mood" does not
    /// exist` (see [`crate::script::runnable_script`]).
    fn migration_search_path<'a>(&'a self, entity: Option<&'a Entity>) -> Option<&'a [crate::entity::PathEntry]> {
        use crate::entity::PathSource;
        match entity {
            Some(e)
                if matches!(e.schema_path.source, PathSource::File | PathSource::Project)
                    && !e.schema_path.entries.is_empty() =>
            {
                Some(&e.schema_path.entries)
            }
            _ => self.script_search_path(),
        }
    }

    /// The pre-existing matviews among `applied` that carry no stamp and whose
    /// deployed definition is the design's — the ones apply may adopt.
    ///
    /// Apply stamps what it creates and leaves an existing matview alone,
    /// because stamping one whose deployed definition may differ would vouch
    /// for it and mask that drift. A matview dbd did not create is the
    /// brownfield case (`init --from-db` read it from this very database): left
    /// unstamped, every `dbd diff` reported it "not stamped by dbd" and the
    /// round trip never converged. Checking the deployed definition first is
    /// what makes stamping it safe. One already stamped keeps its stamp — a
    /// stale one is drift to report, not to overwrite.
    async fn verified_unstamped_matviews<'a>(
        &self,
        adapter: &dyn DatabaseAdapter,
        applied: &[&'a Entity],
        pre_existing: &std::collections::HashMap<String, Option<String>>,
    ) -> Result<Vec<&'a Entity>> {
        let unstamped: Vec<&'a Entity> = applied
            .iter()
            .copied()
            .filter(|e| {
                pre_existing
                    .get(&e.name)
                    .is_some_and(|c| crate::reconcile::parse_dbd_hash(c.as_deref()).is_none())
            })
            .collect();
        if unstamped.is_empty() {
            return Ok(Vec::new());
        }
        let live = adapter.live_matviews().await?;
        Ok(unstamped
            .into_iter()
            .filter(|e| {
                live.iter()
                    .find(|l| l.name == e.name)
                    .is_some_and(|l| crate::reconcile::same_matview_definition(e, l))
            })
            .collect())
    }

    /// The grants a run under `scope` applies: schema → role → privileges.
    ///
    /// Every `schemas:` entry's `grants:`, with the target's `grants:` merged
    /// on top — per schema, a target role adds to or overrides the universal
    /// entry for that role — narrowed to the schemas the scope builds.
    ///
    /// One definition for [`Self::apply_grants`], which runs them, and
    /// `dbd emit`, which has to report the ones it does not translate; two
    /// copies would let the report list grants the database never got. The
    /// merge itself is [`crate::config::DesignConfig::effective_schema_grants`],
    /// which `reset` revokes from too.
    pub(crate) fn declared_grants(
        &self,
        scope: Option<&ResolvedScope>,
    ) -> Result<std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>> {
        let mut schema_grants = self.config.effective_schema_grants();
        if let Some(s) = scope.filter(|s| !s.is_all) {
            let ws = self.working_set(s)?;
            schema_grants.retain(|schema, _| ws.contains(schema));
        }
        Ok(schema_grants)
    }

    /// The `apply.before` and `apply.after` hooks a run under `scope` skips,
    /// one line each saying why — the run's own decision
    /// (`hooks::plan_hooks`), made without running anything.
    ///
    /// The run reports these in [`ApplyComplete::warnings`], which only a run
    /// that succeeds produces: a preview had no word of a skip, and neither
    /// did a run that failed, though a skipped hook is often why it failed. So
    /// a caller says them before the run. A hook file that is missing is an
    /// error here, as it is in the run.
    pub fn hook_skips(&self, scope: Option<&ResolvedScope>) -> Result<Vec<String>> {
        let working_set = self.scope_working_set(scope)?;
        let hook_scope = self.hook_scope(scope, working_set.as_ref());
        let hook_scope = hook_scope.as_ref().map(|(name, outside)| (*name, outside));
        let mut skips = Vec::new();
        for (entries, kind) in [
            (&self.config.apply.before, hooks::HookKind::Before),
            (&self.config.apply.after, hooks::HookKind::After),
        ] {
            skips.extend(hooks::plan_hooks(self.hook_source(), entries, kind, hook_scope)?.warnings);
        }
        Ok(skips)
    }

    /// The `apply.before` and `apply.after` hooks a run under `scope` executes,
    /// as `"apply.before sql/x.sql"`, in the order it runs them — the run's own
    /// decision (`hooks::plan_hooks`), made without running anything.
    ///
    /// For a preview of a run that executes hooks: `reconcile --dry-run`
    /// listed the schema changes and never the hooks its run went on to.
    pub fn apply_hooks_to_run(&self, scope: Option<&ResolvedScope>) -> Result<Vec<String>> {
        let working_set = self.scope_working_set(scope)?;
        let hook_scope = self.hook_scope(scope, working_set.as_ref());
        let hook_scope = hook_scope.as_ref().map(|(name, outside)| (*name, outside));
        let mut run = Vec::new();
        for (entries, phase) in [
            (&self.config.apply.before, "apply.before"),
            (&self.config.apply.after, "apply.after"),
        ] {
            let kind = if phase == "apply.before" {
                hooks::HookKind::Before
            } else {
                hooks::HookKind::After
            };
            run.extend(
                hooks::plan_hooks(self.hook_source(), entries, kind, hook_scope)?
                    .runnable
                    .into_iter()
                    .map(|(script, _)| format!("{phase} {script}")),
            );
        }
        Ok(run)
    }

    /// The grants a run under `scope` applies, one line per schema —
    /// `"app: app_reader (usage, select); app_writer (usage, select, insert)"`
    /// — sorted. Empty when the design declares none. For a preview: the
    /// grants [`Self::apply_grants`] runs, said without running them.
    pub fn grants_to_apply(&self, scope: Option<&ResolvedScope>) -> Result<Vec<String>> {
        let grants = self.declared_grants(scope)?;
        let mut lines: Vec<String> = grants
            .into_iter()
            .map(|(schema, roles)| {
                let mut roles: Vec<String> = roles
                    .into_iter()
                    .map(|(role, privileges)| format!("{role} ({})", privileges.join(", ")))
                    .collect();
                roles.sort();
                format!("{schema}: {}", roles.join("; "))
            })
            .collect();
        lines.sort();
        Ok(lines)
    }

    /// Apply the design's grants: every `schemas:` entry's `grants:`, with the
    /// target's `grants:` merged on top (per schema, a target role adds to or
    /// overrides the universal entry for that role), plus PostgREST's schema
    /// USAGE and config reload on a `supabase` target.
    ///
    /// Run by `dbd apply` and by [`Design::deploy`]. It lived in the CLI's apply
    /// handler alone, so a deploy — and every embedder — got no grants at all.
    ///
    /// Under a scope, only the schemas the scope builds are granted on: the
    /// rest were never created, and a GRANT on one fails the run.
    pub async fn apply_grants(
        &self,
        adapter: &dyn DatabaseAdapter,
        scope: Option<&ResolvedScope>,
    ) -> Result<GrantsOutcome> {
        let schema_grants = self.declared_grants(scope)?;
        if schema_grants.is_empty() {
            return Ok(GrantsOutcome::None);
        }
        // PostgREST USAGE grants ride along only when the user configured
        // some grants, so a no-grants Supabase apply stays a no-op.
        let mut supabase_schemas: Vec<String> = if self.config.default_target() == Some("supabase") {
            self.config.schema_names()
        } else {
            Vec::new()
        };
        if let Some(s) = scope.filter(|s| !s.is_all) {
            let ws = self.working_set(s)?;
            supabase_schemas.retain(|schema| ws.contains(schema));
        }
        // A cross-target design may declare schema grants yet apply to SQLite
        // or Convex, which have no grant model to run them against.
        if !adapter.supports_schema_grants() {
            return Ok(GrantsOutcome::Unsupported);
        }
        match crate::script::build_grants_script(&schema_grants, &supabase_schemas) {
            Some(sql) => {
                adapter.execute_script(&sql).await?;
                Ok(GrantsOutcome::Applied)
            }
            None => Ok(GrantsOutcome::None),
        }
    }

    /// Deploy the full schema: apply DDL, apply grants, import seed data, then
    /// apply RLS policies.
    ///
    /// This is the same pipeline `dbd deploy` runs, and the CLI
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
        self.deploy_with_progress(
            adapter,
            dry_run,
            scope,
            Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete,
            },
        )
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
        // The import phase's refusals (`Design::scoped_import_plan`) — a data
        // file outside `import.staging`, a staging table whose loader dbd
        // cannot pick, a loader the scope leaves out — are known before the
        // first statement. Checked here, not when the import phase reaches
        // them, so a refusal never leaves the deploy half done: schema
        // applied, data not loaded.
        self.scoped_import_plan(None, scope)?;

        let mut apply_summary: Option<ApplyComplete> = None;
        let mut import_summary: Option<ImportComplete> = None;

        self.apply(
            adapter,
            None,
            dry_run,
            scope,
            Progress {
                on_start: &mut progress.on_start,
                on_done: &mut progress.on_done,
                on_complete: |s| {
                    apply_summary = Some(s);
                },
            },
        )
        .await?;

        // Grants attach to schemas and tables the apply just created, and the
        // data loaded next may be read through them — so they go in between.
        let grants = if dry_run {
            GrantsOutcome::None
        } else {
            self.apply_grants(adapter, scope).await?
        };

        // Always run the import phase, even when the plan is empty: it still
        // executes the project's `import.after` scripts, and its summary is what
        // reports "0 table(s) loaded" plus the reason why.
        self.import_data(
            adapter,
            None,
            dry_run,
            scope,
            Progress {
                on_start: &mut progress.on_start,
                on_done: &mut progress.on_done,
                on_complete: |s| {
                    import_summary = Some(s);
                },
            },
        )
        .await?;

        // A policy protects one table; a plane without that table has nothing
        // for the file to do. Filter it the way every other phase is filtered.
        let policy_ws = match scope {
            Some(sc) if !sc.is_all => self.working_set(sc).ok().map(|ws| (sc.name.clone(), ws)),
            _ => None,
        };
        let policies = super::apply_policies(
            adapter,
            &self.project_dir,
            dry_run,
            policy_ws.as_ref().map(|(n, ws)| (n.as_str(), ws)),
        )
        .await?;

        (progress.on_complete)(DeployComplete {
            apply: apply_summary.unwrap_or_default(),
            grants,
            import: import_summary.unwrap_or_default(),
            policies,
        });
        Ok(())
    }
}

/// Refuse `apply -n <name>` while the run would migrate.
///
/// A named apply builds one entity, but a migrating plan records every pending
/// version and stamps the latest. So `apply -n app.people` ran v8's change to
/// `app.people` — and v8's drops, of tables it was not named for — skipped the
/// rest of v8, and recorded v8 as applied; the next full apply said "Already up
/// to date" over a missing column. Running every pending migration instead
/// would do far more than the user asked for, so the run refuses and says how
/// to proceed. With nothing pending, a named apply applies its entity as ever.
///
/// Checked before any write — the `apply.before` hooks included — and by the
/// preview, so a dry run refuses where the run does.
fn refuse_a_named_apply_over_pending_migrations(
    name: Option<&str>,
    db_version: u32,
    latest_version: u32,
    pending: &[PendingMigration],
) -> Result<()> {
    let Some(name) = name else { return Ok(()) };
    if !super::plan::migrates(db_version, latest_version, pending) {
        return Ok(());
    }
    let versions: Vec<String> = pending.iter().map(|m| format!("v{}", m.to_version)).collect();
    let (noun, verb, them) = if versions.len() == 1 {
        ("migration", "is", "it")
    } else {
        ("migrations", "are", "them")
    };
    Err(DbdError::Config(format!(
        "apply -n {name} refused: the database is at v{db_version} and {noun} {} {verb} pending. A named \
         apply builds one entity and cannot run {them}, and would record {them} as applied. Run `dbd apply` \
         first to apply {them}, then `dbd apply -n {name}`.",
        versions.join(", ")
    )))
}

/// The statements of a migration file, one line each, for a preview — empty
/// when there is no such file, as the run then runs nothing for it.
///
/// Split and read by libpg_query, the grammar the run's server uses: the text
/// is the parse tree deparsed (so comments and layout fall away), and whether
/// a statement destroys something comes from its node, not from its spelling.
/// A file the grammar cannot split is shown whole and unmarked — the run sends
/// it as one script either way.
fn planned_statements(path: &std::path::Path) -> Result<Vec<PlannedStatement>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(planned_statements_of(&crate::source_text::read_to_string(path)?))
}

/// [`planned_statements`] of script text.
fn planned_statements_of(sql: &str) -> Vec<PlannedStatement> {
    planned_statements_where(sql, |_| true)
}

/// [`planned_statements_of`], keeping the statements `keep` accepts. A script
/// the grammar cannot split is shown whole.
fn planned_statements_where(sql: &str, keep: impl Fn(&str) -> bool) -> Vec<PlannedStatement> {
    match pg_query::split_with_parser(sql) {
        Ok(pieces) => pieces
            .into_iter()
            .filter(|p| keep(p))
            .filter_map(planned_statement)
            .collect(),
        Err(_) => planned_statement(sql).into_iter().collect(),
    }
}

/// One statement as [`planned_statements`] shows it, or `None` for a piece
/// that holds no statement (a trailing comment).
fn planned_statement(piece: &str) -> Option<PlannedStatement> {
    use pg_query::NodeEnum;
    let parsed = pg_query::parse(piece).ok();
    if parsed.as_ref().is_some_and(|p| p.protobuf.stmts.is_empty()) {
        return None;
    }
    let drops = parsed.as_ref().is_some_and(|p| {
        p.protobuf
            .stmts
            .iter()
            .filter_map(|s| s.stmt.as_ref().and_then(|n| n.node.as_ref()))
            .any(|n| match n {
                NodeEnum::DropStmt(_) | NodeEnum::TruncateStmt(_) => true,
                NodeEnum::AlterTableStmt(a) => a.cmds.iter().any(|c| {
                    matches!(
                        c.node.as_ref(),
                        Some(NodeEnum::AlterTableCmd(cmd))
                            if cmd.subtype == pg_query::protobuf::AlterTableType::AtDropColumn as i32
                                || cmd.subtype == pg_query::protobuf::AlterTableType::AtDropConstraint as i32
                    )
                }),
                _ => false,
            })
    });
    let text = parsed
        .and_then(|p| p.deparse().ok())
        .unwrap_or_else(|| piece.to_string());
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    // Long enough to name the statement and its target; a DO block or a long
    // ALTER is cut rather than wrapped across the preview.
    const WIDTH: usize = 120;
    let text = match line.char_indices().nth(WIDTH) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    };
    Some(PlannedStatement { text, drops })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::mock::MockAdapter;

    /// A migration's `.sql` and `.data.sql` were read decoded — SSMS may have
    /// re-saved them as UTF-16 — but its `.drop.sql` was read with
    /// `read_to_string`, so the same edit failed the drop step with an encoding
    /// error.
    #[tokio::test]
    async fn a_utf16_drop_script_runs() {
        let design = Design::from_config(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/design.yaml"),
            "dev",
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let drop_sql = tmp.path().join("config.legacy.drop.sql");
        std::fs::write(
            &drop_sql,
            crate::source_text::utf16le("DROP TABLE IF EXISTS config.legacy;\n"),
        )
        .unwrap();

        // The table exists: a migration's drop runs only where it does.
        let mock = MockAdapter::new().with_known_entities(["config.legacy"]);
        let step = ExecutionStep::DropEntity {
            entity_name: "config.legacy".to_string(),
            drop_sql_path: drop_sql,
            migration_version: 2,
        };
        design
            .execute_plan_step(
                &mock,
                &step,
                &std::collections::HashMap::new(),
                None,
                &mut ApplyCounts::default(),
                &mut |_: &str| {},
                &mut |_: &str, _: Option<&str>| {},
            )
            .await
            .expect("a UTF-16 drop script must run");

        let scripts = mock.scripts.lock().unwrap().clone();
        assert_eq!(scripts, vec!["DROP TABLE IF EXISTS config.legacy;\n".to_string()]);
    }
}
