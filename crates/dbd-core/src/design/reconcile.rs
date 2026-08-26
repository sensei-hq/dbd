use super::*;

impl Design {
    /// Read-only materialized-view detection for `reconcile`: for each design
    /// matview, decide create/skip/restamp/warn against the live `dbd:hash`
    /// sentinels, recording creates into `plan.matview_creates`, restamps into
    /// `plan.matview_restamps`, and drift/unstamped notes into `plan.warnings`.
    /// Returns the `(entity, want-hash)` pairs the write pass should CREATE and
    /// the ones it should RESTAMP, so no second `matview_states()` fetch is
    /// needed.
    async fn detect_reconcile_matviews<'a>(
        &self,
        adapter: &dyn DatabaseAdapter,
        desired_entities: &[&'a Entity],
        plan: &mut crate::reconcile::ReconcilePlan,
    ) -> Result<(Vec<(&'a Entity, String)>, Vec<(&'a Entity, String)>)> {
        use crate::reconcile::{MatviewAction, decide_matview_action, matview_hash, parse_dbd_hash};
        let matviews: Vec<&'a Entity> = desired_entities
            .iter()
            .copied()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .collect();
        let mut mv_to_create: Vec<(&'a Entity, String)> = Vec::new();
        let mut mv_to_restamp: Vec<(&'a Entity, String)> = Vec::new();
        if matviews.is_empty() {
            return Ok((mv_to_create, mv_to_restamp));
        }
        let states = adapter.matview_states().await?;
        for &e in &matviews {
            let want = matview_hash(e);
            // `matview_states` returns the raw comment (adapter I/O boundary is
            // public API and must not leak the internal `Sentinel` type); parse
            // it into a sentinel here, where the interpretation belongs.
            let sentinel = states.get(&e.name).map(|c| parse_dbd_hash(c.as_deref()));
            match decide_matview_action(&want, sentinel.clone()) {
                MatviewAction::Skip => {}
                MatviewAction::Create => {
                    plan.matview_creates.push(e.name.clone());
                    mv_to_create.push((e, want));
                }
                MatviewAction::Restamp => {
                    // A v1 sentinel hashes a different body contract, so it is
                    // never comparable to `want` — upgrade it silently rather
                    // than routing through `plan.warnings` like real drift.
                    plan.matview_restamps.push(e.name.clone());
                    mv_to_restamp.push((e, want));
                }
                MatviewAction::Warn => {
                    // Detected drift, but dbd never auto-recreates a matview.
                    // Surface it through the plan's warnings (the same channel
                    // `dbd reconcile` prints for risky-change advisories).
                    let has_sentinel = matches!(sentinel, Some(Some(_)));
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
        Ok((mv_to_create, mv_to_restamp))
    }

    /// Execute reconcile's write passes against the live database, in order:
    /// A prerequisites + newly-added tables/enums, B ALTERs (with a `search_path`
    /// prelude), C code objects (CREATE OR REPLACE), C-mv matview creates,
    /// C-mv-restamp matview sentinel upgrades, and D prune (only when `prune`).
    /// Returns the tallied summary. Factored out of [`Design::reconcile`] so its
    /// body reads as setup → detect → write.
    #[allow(clippy::too_many_arguments)]
    async fn execute_reconcile_writes<S, D>(
        &self,
        adapter: &dyn DatabaseAdapter,
        plan: &crate::reconcile::ReconcilePlan,
        desired_entities: &[&Entity],
        managed_schemas: &std::collections::HashSet<String>,
        mv_to_create: &[(&Entity, String)],
        mv_to_restamp: &[(&Entity, String)],
        prune: bool,
        on_start: &mut S,
        on_done: &mut D,
    ) -> Result<crate::reconcile::ReconcileComplete>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
    {
        use crate::reconcile::{
            ReconcileComplete, matview_create_sql, matview_hash_comment_sql, qualified_entity_name,
        };
        use std::collections::{HashMap, HashSet};

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
        for e in desired_entities {
            let do_apply = match e.entity_type {
                EntityType::Schema | EntityType::Extension | EntityType::Role | EntityType::Sequence => true,
                EntityType::Enum | EntityType::Table => added.contains(qualified_entity_name(e).as_str()),
                _ => false,
            };
            if !do_apply {
                continue;
            }
            let is_created = matches!(e.entity_type, EntityType::Enum | EntityType::Table);
            let desc = format!("{}:{}", e.entity_type.tag(), e.name);
            on_start(&desc);
            let result = adapter.apply_entity(e).await;
            report_step_result(&desc, on_done, result)?;
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
        let search_path = search_path_prelude(managed_schemas);

        // Pass B — ALTER existing tables/enums, in dependency order.
        for e in desired_entities {
            let n = qualified_entity_name(e);
            if let Some(sql) = alter_sql.get(n.as_str()) {
                let desc = format!("alter {}:{n}", e.entity_type.tag());
                on_start(&desc);
                let script = format!("{search_path}{sql}");
                let result = adapter.execute_script(&script).await;
                report_step_result(&desc, on_done, result)?;
                summary.altered += 1;
            }
        }

        // Pass C — code objects (CREATE OR REPLACE), after tables/columns exist.
        for e in desired_entities {
            if !matches!(
                e.entity_type,
                EntityType::View | EntityType::Function | EntityType::Procedure
            ) {
                continue;
            }
            let desc = format!("{}:{}", e.entity_type.tag(), e.name);
            on_start(&desc);
            let result = adapter.apply_entity(e).await;
            report_step_result(&desc, on_done, result)?;
            summary.reapplied += 1;
        }

        // Pass C-mv (WRITES) — CREATE the matviews detection found absent. Reuses
        // the pre-computed `mv_to_create` (no second `matview_states()` fetch).
        // Each CREATE carries the same `search_path` prelude the ALTERs use.
        for (e, want) in mv_to_create {
            let desc = format!("{}:{}", e.entity_type.tag(), e.name);
            on_start(&desc);
            let result = adapter
                .execute_script(&format!("{search_path}{}", matview_create_sql(e, want)))
                .await;
            report_step_result(&desc, on_done, result)?;
            summary.reapplied += 1;
        }

        // Pass C-mv-restamp (WRITES) — upgrade the sentinel on matviews whose
        // live comment is a v1 (unversioned) stamp. This never runs under
        // `--dry-run`: `Design::reconcile` returns before this method is even
        // called in that case. A v1 hash covers a superseded body contract, so
        // it is not comparable to `want` — this is a metadata-only rewrite of
        // the same value's current form, never a CREATE or a DROP.
        for (e, want) in mv_to_restamp {
            let desc = format!("{}:{} (restamp v1→v2)", e.entity_type.tag(), e.name);
            on_start(&desc);
            let sql = matview_hash_comment_sql(&qualified_entity_name(e), want);
            let result = adapter.execute_script(&format!("{search_path}{sql}")).await;
            report_step_result(&desc, on_done, result)?;
            summary.restamped += 1;
        }

        // Pass D — prune orphaned tables (in managed schemas, gone from the
        // design), only when explicitly requested. Otherwise they are reported
        // via the returned plan and left untouched.
        if prune {
            for stmt in &plan.dropped {
                let desc = format!("prune table:{}", stmt.entity_name);
                on_start(&desc);
                let result = adapter.execute_script(&stmt.sql).await;
                report_step_result(&desc, on_done, result)?;
                summary.dropped += 1;
            }
        }

        Ok(summary)
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
        mut progress: Progress<S, D, C>,
    ) -> Result<crate::reconcile::ReconcilePlan>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(crate::reconcile::ReconcileComplete),
    {
        use crate::reconcile::{
            plan_check_convergence, plan_comment_convergence, plan_fk_convergence, plan_index_convergence,
            plan_reconcile, raw_snapshot_from_entities, snapshot_from_entities,
        };

        // Batch adapters (e.g. Convex) have no live SQL schema to diff.
        if adapter.prefers_batch_apply() {
            return Err(DbdError::Config(
                "reconcile is not supported for this target (no live SQL schema to diff)".to_string(),
            ));
        }

        // Desired entities: valid, non-external, in scope — in dependency order.
        let working_set = self.scope_working_set(scope)?;
        // A file dbd could not read means the desired schema has a hole in it.
        // Refuse before planning, `dry_run` included: a plan computed from a
        // partial design reports drift that isn't there and misses drift that is.
        self.ensure_fully_parsed(scope, working_set.as_ref(), None)?;
        let desired_entities = self.entities_in_scope(scope, working_set.as_ref(), None);

        // Desired snapshot (tables + enums, schema-normalized).
        let desired_owned: Vec<Entity> = desired_entities.iter().map(|e| (*e).clone()).collect();
        let desired = snapshot_from_entities(&desired_owned);

        // Schemas the design manages. Reconcile only diffs within these, so it
        // never considers (or prunes) tables in schemas the project doesn't own.
        let managed_schemas = Self::managed_schemas(&desired_entities);

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
        let live_raw = restrict_snapshot_to_schemas(raw_snapshot_from_entities(&live_entities), &managed_schemas);
        plan_fk_convergence(&mut plan, &live_raw, &desired_raw);

        // Secondary indexes (issue #12): canonicalize also strips indexes, so —
        // like FKs — converge them from the RAW snapshots. Adds a declared index
        // the live DB lacks (idempotent CREATE), and drops (destructive) one the
        // design removed. PK/UNIQUE-backing indexes are excluded on both sides.
        plan_index_convergence(&mut plan, &live_raw, &desired_raw);

        // CHECK constraints: canonicalize strips these too, and nothing used to
        // put them back — so `dbd diff` reported CHECK drift that reconcile could
        // never act on, even with --allow-destructive. Converge them from the RAW
        // snapshots, matching by canonical expression since Postgres auto-names
        // every CHECK.
        plan_check_convergence(&mut plan, &live_raw, &desired_raw);

        // Column comments: canonicalize clears these as well, so reconcile could
        // never act on the comment drift `dbd diff` kept reporting. Metadata only,
        // so never destructive.
        plan_comment_convergence(&mut plan, &live_raw, &desired_raw);

        // Materialized-view DETECTION (read-only) — done BEFORE the dry_run return
        // so `--dry-run` previews matview creates, restamps, AND drift warnings.
        // Postgres has no `CREATE OR REPLACE MATERIALIZED VIEW`, and dbd
        // deliberately never auto-drops one (a DROP … CASCADE would repopulate it
        // and drop its dependents — unacceptable in a dev-loop reconcile). So:
        // CREATE an absent matview (stamping a `dbd:hash` sentinel), SKIP one
        // whose stored hash matches the design, RESTAMP one whose sentinel is a
        // superseded `v1` stamp (its hash isn't comparable, so this is never a
        // drift signal), and for one that actually drifted (or carries no
        // sentinel) WARN and leave it untouched. Only the CREATE/RESTAMP *writes*
        // run after the return; here we merely record the decisions into the plan
        // and local `mv_to_create`/`mv_to_restamp` lists the write pass reuses (no
        // second states fetch).
        let (mv_to_create, mv_to_restamp) = self
            .detect_reconcile_matviews(adapter, &desired_entities, &mut plan)
            .await?;

        if dry_run {
            return Ok(plan);
        }

        // Heal-first: ensure bookkeeping storage exists (and is at the current
        // layout, healing any legacy layout in place) before any meta read/write
        // below — every ownership operation calls this up front. Runs after the
        // dry-run return above so `--dry-run` stays read-only.
        adapter.heal_bookkeeping().await?;

        ensure_reconcile_not_destructive(&plan, allow_destructive)?;

        // Write passes A–D (see `execute_reconcile_writes`).
        let summary = self
            .execute_reconcile_writes(
                adapter,
                &plan,
                &desired_entities,
                &managed_schemas,
                &mv_to_create,
                &mv_to_restamp,
                prune,
                &mut progress.on_start,
                &mut progress.on_done,
            )
            .await?;

        // Sync pg_cron refresh jobs, mirroring the apply path. This MUST cover the
        // whole design, not the scoped `desired_entities`: `sync_refresh_jobs`
        // unschedules every `dbd:refresh:%` job absent from the set it is given, so
        // a scoped reconcile fed only its subset would unschedule out-of-scope
        // matviews' jobs. The adapter guards on pg_cron presence, so it is a safe
        // no-op on databases (and non-Postgres targets) without the extension.
        adapter.sync_refresh_jobs(&self.all_matview_jobs()).await?;

        // Stamp the project version so `migrate --status` / `apply` stay consistent.
        let version = self.config.project.version.unwrap_or(1);
        adapter
            .set_project_meta(&self.env, version, scope.map(|s| s.name.as_str()))
            .await?;

        (progress.on_complete)(summary);
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
        use crate::reconcile::raw_snapshot_from_entities;

        // Batch adapters (e.g. Convex) have no live SQL schema to diff.
        if adapter.prefers_batch_apply() {
            return Err(DbdError::Config(
                "diff is not supported for this target (no live SQL schema to diff)".to_string(),
            ));
        }

        // Desired entities: valid, non-external, in scope — in dependency order.
        let working_set = self.scope_working_set(scope)?;
        // Read-only, but the same refusal applies: "in sync with the design" is a
        // false statement when part of the design was never read.
        self.ensure_fully_parsed(scope, working_set.as_ref(), None)?;
        let desired_entities = self.entities_in_scope(scope, working_set.as_ref(), None);

        // Desired snapshot (tables + enums, schema-normalized). Raw (un-canonicalized)
        // so FK/CHECK/indexes/comments survive for `SchemaDiff` to compare.
        let desired_owned: Vec<Entity> = desired_entities.iter().map(|e| (*e).clone()).collect();
        let desired = raw_snapshot_from_entities(&desired_owned);

        // Schemas the design manages. The diff is scoped to these, so it never
        // reports drift for tables in schemas the project doesn't own.
        let managed_schemas = Self::managed_schemas(&desired_entities);

        // Live snapshot, restricted to managed schemas. Raw so introspected
        // FK/CHECK/indexes/comments reach `SchemaDiff::normalize_for_diff`.
        let live_entities = adapter.introspect().await?;
        let live_full = raw_snapshot_from_entities(&live_entities);
        let live = restrict_snapshot_to_schemas(live_full, &managed_schemas);

        let mut diff = crate::SchemaDiff::compute(live, desired);

        // Materialized-view drift (read-only) — matviews aren't in the tables/
        // enums snapshots above, so `compute` never reports them; categorize them
        // separately (Missing / Drifted / Unstamped / Orphan).
        diff.matview_drift = self
            .matview_drift(adapter, &desired_entities, &live_entities, &managed_schemas)
            .await?;

        Ok(diff)
    }

    /// Categorize materialized-view drift for [`Design::diff_live`] (read-only).
    /// Compares each design matview's hash to the live `dbd:hash` sentinel —
    /// Missing (absent) / Drifted (sentinel mismatch) / Unstamped (no sentinel) —
    /// and adds Orphan for a live matview in a managed schema that's gone from the
    /// design. Only fetches `matview_states()` when the design has a matview
    /// (orphans come from `live_entities`). Returned sorted by name.
    async fn matview_drift(
        &self,
        adapter: &dyn DatabaseAdapter,
        desired_entities: &[&Entity],
        live_entities: &[Entity],
        managed_schemas: &std::collections::HashSet<String>,
    ) -> Result<Vec<crate::schema_diff::MatviewDrift>> {
        use crate::reconcile::{DEFAULT_SCHEMA, Sentinel, matview_hash, parse_dbd_hash};
        use crate::schema_diff::{MatviewDrift, MatviewDriftKind};
        use std::collections::HashSet;

        let design_matviews: Vec<&Entity> = desired_entities
            .iter()
            .copied()
            .filter(|e| e.entity_type == EntityType::MaterializedView)
            .collect();
        let design_mv_names: HashSet<&str> = design_matviews.iter().map(|e| e.name.as_str()).collect();

        let mut drift: Vec<MatviewDrift> = Vec::new();

        if !design_matviews.is_empty() {
            let states = adapter.matview_states().await?;
            for e in &design_matviews {
                let want = matview_hash(e);
                // See `detect_reconcile_matviews`: the adapter returns the raw
                // comment, parsed into a sentinel here at the crate-internal
                // interpretation boundary.
                let sentinel = states.get(&e.name).map(|c| parse_dbd_hash(c.as_deref()));
                let kind = match sentinel {
                    None => MatviewDriftKind::Missing,
                    Some(Some(Sentinel::V2(h))) if h == want => continue, // in sync — emit nothing
                    Some(Some(Sentinel::V2(_))) => MatviewDriftKind::Drifted,
                    // A v1 stamp predates the current hash contract, so it isn't
                    // comparable to `want` — like a matview with no sentinel at
                    // all, this diff can't verify the live definition from it.
                    // `dbd reconcile` upgrades it to v2 silently on its next run.
                    Some(Some(Sentinel::V1(_))) | Some(None) => MatviewDriftKind::Unstamped,
                };
                drift.push(MatviewDrift {
                    name: e.name.clone(),
                    kind,
                });
            }
        }

        // Orphans: live matviews in a managed schema, absent from the design. No
        // hash needed, so this reuses the already-fetched `live_entities`.
        for e in live_entities {
            if e.entity_type != EntityType::MaterializedView {
                continue;
            }
            let schema = {
                let s = e.schema.clone().unwrap_or_default();
                if s.is_empty() { DEFAULT_SCHEMA.to_string() } else { s }
            };
            if managed_schemas.contains(&schema) && !design_mv_names.contains(e.name.as_str()) {
                drift.push(MatviewDrift {
                    name: e.name.clone(),
                    kind: MatviewDriftKind::Orphan,
                });
            }
        }

        drift.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(drift)
    }
}
