use super::*;

impl Design {
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
    /// intact since roles are not scope-selectable. The bookkeeping of what it
    /// drops goes too: the project's recorded migrations, the drift stamp of
    /// every materialized view the reset removed, and — on a reset of all the
    /// database was built with — the database's record of the project (env,
    /// version, scope pin).
    ///
    /// Returns what it dropped, read from the database before and after rather
    /// than from the script: every drop is `IF EXISTS`, so the script alone
    /// cannot tell a reset that emptied the schema from one that found nothing.
    pub async fn reset(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<ResetComplete> {
        // The database's dbd write lock, exclusive, from its first read to its
        // last drop: what it reads of other projects stays true until it has
        // dropped — no other project's run writes between (#7).
        let run = self.reset_run(adapter, target, force, drop_schemas, drop_extensions, scope);
        super::ownership::with_write_lock(adapter, true, run).await
    }

    /// [`Design::reset`]'s run, under the write lock.
    async fn reset_run(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<ResetComplete> {
        // The guards first, as `apply` runs them — they read the marks without
        // healing, and a renamed project is best told it was renamed. Then
        // heal and claim the design's entities, refusing a design declaring
        // another owner's, before anything is dropped: a reset's drops commit,
        // so it claims first — see `Design::claim_first` (#7).
        let marks = self.database_marks(adapter).await?;
        if !force {
            self.check_reset_guard(&marks)?;
            self.check_reset_prod_elsewhere(adapter).await?;
        }
        self.claim_first(adapter).await?;

        let (keep, roles_kept) = self
            .reset_keeps(adapter, target, drop_schemas, drop_extensions, scope)
            .await?;
        let plan = self.reset_plan(target, drop_schemas, drop_extensions, scope, &keep)?;
        let mut summary = ResetComplete {
            roles_kept,
            kept: keep.notes,
            ..Default::default()
        };
        if let Some(sql) = &plan.sql {
            let existed = adapter.existing_objects(&plan.objects).await?;
            // The matviews that exist now and not after the drop are the ones
            // this reset dropped — by name, with their schema, or by CASCADE
            // from what they read. Their drift stamps go with them.
            let before = adapter.matview_states().await?;
            adapter.execute_script(sql).await?;
            let after = adapter.matview_states().await?;
            let mut dropped: Vec<String> = before.into_keys().filter(|mv| !after.contains_key(mv)).collect();
            dropped.sort();
            adapter.forget_matview_stamps(&dropped).await?;

            let remaining = adapter.existing_objects(&plan.objects).await?;
            summary.dropped = plan
                .objects
                .into_iter()
                .filter(|o| existed.contains(o) && !remaining.contains(o))
                .collect();
        }
        adapter.clear_project_migrations().await?;
        // A reset of everything the database was built with leaves nothing of
        // the project, so its record of the database goes too, and the next
        // apply records it afresh. That is the way to rebuild, with `-e dev`, a
        // database a run without `-e` marked prod: the env guard refuses a dev
        // run against the mark, and the mark survived the reset. "Everything"
        // is the all-scope, or the scope the database is pinned to — a design
        // with a `default:` scope builds and resets under it, and a bare
        // `reset --force` there kept the mark. A narrower scope leaves the rest
        // of the project — and its version and pin — in place.
        let pinned = marks.own.as_ref().and_then(|m| m.scope.as_deref());
        if scope.is_none_or(|s| s.is_all || pinned == Some(s.name.as_str())) {
            adapter.forget_project_meta().await?;
        }

        Ok(summary)
    }

    /// The guard [`Design::reset`] checks unless forced: a database marked prod,
    /// or one a project has versioned, is not reset. `marks` are what
    /// the database records (one dbd has never written to passes): this
    /// project's record or, after a rename, the one its old `project.name` left
    /// (see [`DatabaseMarks`]) — `--force` overrides either alike.
    ///
    /// Public so a preview refuses exactly where the real run does: `reset
    /// --dry-run` printed the whole plan for a prod database, and exited 0,
    /// while the real run refused it.
    ///
    /// A method, not an associated function, because the refusal calls this
    /// project released only when `project.released` says so: a database
    /// `init --from-db` captured is versioned with `released` never set, and
    /// was refused as "a released project's database".
    pub fn check_reset_guard(&self, marks: &DatabaseMarks) -> Result<()> {
        // Compared through the alias table: rows written before the CLI
        // normalised `-e` can say `production`.
        let is_prod = |m: &&crate::adapter::ProjectMeta| {
            crate::config::normalize_env(Some(&m.env)).is_ok_and(|env| env == "prod")
        };
        // (the row, why — as this project's mark, as another project's). A
        // recorded version is what is known: a fresh install of a released
        // project records its version with no migration ever run, and this
        // said "has applied migrations" of it.
        let (row, own, other) = match (marks.rows().find(is_prod), marks.rows().find(|m| m.version >= 1)) {
            (Some(row), _) => (
                row,
                "database is marked as prod".to_string(),
                "marked this database prod".to_string(),
            ),
            (None, Some(row)) => (
                row,
                format!(
                    "the database is versioned at v{}{}",
                    row.version,
                    if self.config.project.released {
                        " (a released project's database)"
                    } else {
                        ""
                    }
                ),
                format!("versioned this database at v{}", row.version),
            ),
            (None, None) => return Ok(()),
        };
        Err(DbdError::SafetyGuard(match marks.recorded_by(row) {
            None => format!("reset is blocked — {own}. Use --force to override."),
            Some(project) => format!(
                "reset is blocked — project '{project}' {other}.\n{}Use --force to override.",
                marks.rename_note()
            ),
        }))
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
        self.reset_script_keeping(target, drop_schemas, drop_extensions, scope, &[])
    }

    /// [`Self::reset_script`], keeping the roles named in `keep` — those
    /// [`Self::roles_kept_by_reset`] found another database holding — as the
    /// real run does. A preview with a database to ask uses this, so it does
    /// not list a `DROP ROLE` the run will not perform.
    pub fn reset_script_keeping(
        &self,
        target: &str,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
        keep: &[String],
    ) -> Result<Option<String>> {
        let keep = super::ownership::ResetKeep {
            held_elsewhere: keep.to_vec(),
            ..Default::default()
        };
        Ok(self
            .reset_plan(target, drop_schemas, drop_extensions, scope, &keep)?
            .sql)
    }

    /// What a reset of the database `adapter` reaches would run, and leave —
    /// asked of that database as the real run asks it, refusing where it
    /// refuses: the script, keeping the roles another database holds and what
    /// other projects have or use there (#7), and a summary with nothing
    /// dropped and those kept. For `reset --dry-run` with a database to ask.
    pub async fn reset_preview(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<(Option<String>, ResetComplete)> {
        if !force {
            self.check_reset_prod_elsewhere(adapter).await?;
        }
        let (keep, roles_kept) = self
            .reset_keeps(adapter, target, drop_schemas, drop_extensions, scope)
            .await?;
        let sql = self
            .reset_plan(target, drop_schemas, drop_extensions, scope, &keep)?
            .sql;
        Ok((
            sql,
            ResetComplete {
                roles_kept,
                kept: keep.notes,
                ..Default::default()
            },
        ))
    }

    /// Everything a reset keeps: the roles another database on the server
    /// holds, and what other projects in this one have or use — refusing when
    /// another project depends on what it drops.
    async fn reset_keeps(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<(super::ownership::ResetKeep, Vec<KeptRole>)> {
        let roles_kept = self.roles_kept_by_reset(adapter, target, scope).await?;
        let drops = self.reset_drops(target, drop_schemas, drop_extensions, scope)?;
        let mut keep = self.reset_ownership(adapter, &drops).await?;
        keep.held_elsewhere = roles_kept.iter().map(|k| k.role.clone()).collect();
        Ok((keep, roles_kept))
    }

    /// What a reset would drop by name, before anything is kept.
    fn reset_drops(
        &self,
        target: &str,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<ResetDrops> {
        // SQLite's reset drops the project's tables and views, without
        // `CASCADE`, and has no schemas, extensions or roles to drop.
        if target == "sqlite" {
            return Ok(ResetDrops {
                entities: self
                    .reset_entities(scope)?
                    .iter()
                    .filter(|e| matches!(e.entity_type, EntityType::Table | EntityType::View))
                    .map(|e| e.name.clone())
                    .collect(),
                schemas: Vec::new(),
                extensions: Vec::new(),
                roles: Vec::new(),
            });
        }
        let entities = self.reset_entities(scope)?.iter().map(|e| e.name.clone()).collect();
        let schemas = if drop_schemas {
            self.reset_target_schemas(scope)?
                .into_iter()
                .filter(|s| !script::schema_is_protected(s, target))
                .collect()
        } else {
            Vec::new()
        };
        let extensions = if drop_extensions {
            self.reset_extensions(scope)
        } else {
            Vec::new()
        };
        // Roles are dropped on a `postgres` target only (see `reset_plan`).
        let roles = if target == "postgres" {
            self.reset_roles(scope).into_iter().map(|r| r.name).collect()
        } else {
            Vec::new()
        };
        Ok(ResetDrops {
            entities,
            schemas,
            extensions,
            roles,
        })
    }

    /// The data-model entities a reset drops one by one — the scope's, or all
    /// of them — in apply order.
    fn reset_entities(&self, scope: Option<&ResolvedScope>) -> Result<Vec<&Entity>> {
        let is_data_model = |e: &Entity| crate::entity::TYPES_WITH_SCHEMA.contains(&e.entity_type);
        Ok(match scope {
            Some(s) if !s.is_all => {
                let ws = self.working_set(s)?;
                self.entities
                    .iter()
                    .filter(|e| is_data_model(e) && Self::entity_in_scope(e, s, &ws))
                    .collect()
            }
            _ => self.entities.iter().filter(|e| is_data_model(e)).collect(),
        })
    }

    /// The roles a reset drops. Roles aren't scope-selectable, so only a full
    /// reset drops them; a subset scope leaves shared roles intact. Every role
    /// the design declares counts — `ddl/role/` files as well as
    /// `target.roles`, which used to be the only ones dropped.
    fn reset_roles(&self, scope: Option<&ResolvedScope>) -> Vec<crate::config::RoleEntry> {
        if matches!(scope, Some(s) if !s.is_all) {
            return Vec::new();
        }
        self.entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Role)
            .map(|e| crate::config::RoleEntry {
                name: e.name.clone(),
                refers: Vec::new(),
            })
            .collect()
    }

    /// The active target's extensions (by bare name) a reset with
    /// `--extensions` drops — only those the scope installs: a scope's
    /// `extensions:` allowlist bounds what reset drops as it bounds what apply
    /// creates.
    pub(crate) fn reset_extensions(&self, scope: Option<&ResolvedScope>) -> Vec<String> {
        let admitted = |name: &str| match scope {
            Some(s) if !s.is_all => s.extensions.as_ref().is_none_or(|allow| allow.contains(name)),
            _ => true,
        };
        self.config
            .target
            .values()
            .next()
            .map(|t| {
                t.extensions
                    .iter()
                    .map(|e| e.name().to_string())
                    .filter(|n| admitted(n))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The design's roles a reset of the database `adapter` reaches must keep,
    /// because they hold something in another database on the same server.
    ///
    /// Roles are server-wide: two databases built from one design share them.
    /// A reset of one dropped them out from under the other — or failed trying,
    /// with an error that called dbd's own grants there ones "dbd did not
    /// grant". Only a full reset on a postgres target drops roles, so only one
    /// asks; the answer comes from the server's catalogs, read-only.
    pub async fn roles_kept_by_reset(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        scope: Option<&ResolvedScope>,
    ) -> Result<Vec<KeptRole>> {
        if target != "postgres" || matches!(scope, Some(s) if !s.is_all) {
            return Ok(Vec::new());
        }
        let roles: Vec<String> = self
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::Role)
            .map(|e| e.name.clone())
            .collect();
        let held = adapter.roles_held_elsewhere(&roles).await?;
        let mut kept: Vec<KeptRole> = held
            .into_iter()
            .map(|(role, databases)| KeptRole { role, databases })
            .collect();
        kept.sort_by(|a, b| a.role.cmp(&b.role));
        Ok(kept)
    }

    /// [`Self::reset_script`]'s SQL, and every object it drops — by kind and
    /// name, in the order it drops them — so a reset can report which it found
    /// and dropped.
    fn reset_plan(
        &self,
        target: &str,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
        keep: &super::ownership::ResetKeep,
    ) -> Result<ResetPlan> {
        let is_subset = matches!(scope, Some(s) if !s.is_all);
        // A role kept for another project here keeps its grants too — that
        // project relies on them — so the script leaves it alone. One kept for
        // another database has this database's grants revoked (see
        // `script::build_reset_script_keeping`).
        let roles: Vec<crate::config::RoleEntry> = self
            .reset_roles(scope)
            .into_iter()
            .filter(|r| !keep.roles.contains(&r.name))
            .collect();

        // Data-model entities to drop individually (scope-filtered), in the
        // builder's reverse dependency order.
        let entities = self.reset_entities(scope)?;

        let mut objects: Vec<(EntityType, String)> =
            entities.iter().rev().map(|e| (e.entity_type, e.name.clone())).collect();

        // SQLite has the tables and views alone, and its own dialect for them.
        if target == "sqlite" {
            objects.retain(|(kind, _)| matches!(kind, EntityType::Table | EntityType::View));
            return Ok(ResetPlan {
                sql: script::build_sqlite_reset_script(&entities),
                objects,
            });
        }

        // Schemas the `--schemas` path may drop — all managed schemas, or just
        // those the scope occupies — and the extensions `--extensions` may,
        // less what other projects have or use (#7).
        let schemas: Vec<String> = self
            .reset_target_schemas(scope)?
            .into_iter()
            .filter(|s| !keep.schemas.contains(s))
            .collect();
        let extensions: Vec<String> = self
            .reset_extensions(scope)
            .into_iter()
            .filter(|e| !keep.extensions.contains(e))
            .collect();
        let keep = &keep.held_elsewhere;

        if drop_extensions {
            objects.extend(extensions.iter().map(|e| (EntityType::Extension, e.clone())));
        }
        if drop_schemas {
            objects.extend(
                schemas
                    .iter()
                    .filter(|s| !script::schema_is_protected(s, target))
                    .map(|s| (EntityType::Schema, s.clone())),
            );
        }

        // A subset scope drops its own objects and nothing else: no CASCADE.
        if is_subset {
            return Ok(ResetPlan {
                sql: script::build_scoped_reset_script(
                    &entities,
                    &extensions,
                    target,
                    drop_schemas,
                    drop_extensions,
                    &schemas,
                ),
                objects,
            });
        }

        // What apply granted on the schemas, so the roles it granted to can be
        // dropped: Postgres refuses to drop a role that still holds a grant.
        let grants = self.config.effective_schema_grants();

        // Roles are dropped on a postgres target only (see `build_reset_script`).
        if target == "postgres" {
            objects.extend(
                roles
                    .iter()
                    .rev()
                    .filter(|r| !keep.contains(&r.name))
                    .map(|r| (EntityType::Role, r.name.clone())),
            );
        }
        let sql = script::build_reset_script_keeping(
            &entities,
            &roles,
            &extensions,
            target,
            drop_schemas,
            drop_extensions,
            &schemas,
            &grants,
            keep,
        )
        .map_err(DbdError::SafetyGuard)?;
        Ok(ResetPlan { sql, objects })
    }
}

/// What a reset would drop by name before anything is kept — the data-model
/// entities, and on request the schemas, extensions and roles.
pub(crate) struct ResetDrops {
    pub(crate) entities: Vec<String>,
    pub(crate) schemas: Vec<String>,
    pub(crate) extensions: Vec<String>,
    pub(crate) roles: Vec<String>,
}

/// What [`Design::reset_plan`] would run, and what it would drop.
struct ResetPlan {
    sql: Option<String>,
    objects: Vec<(EntityType, String)>,
}

/// What a [`Design::reset`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResetComplete {
    /// Every one of the project's objects the reset dropped — `(kind, name)`,
    /// in the order it dropped them. Only what existed before and is gone
    /// after: an object the database never had is not counted.
    pub dropped: Vec<(EntityType, String)>,
    /// The design's roles the reset left in place because another database on
    /// the server holds something of theirs.
    pub roles_kept: Vec<KeptRole>,
    /// What the reset left for other projects in this database, one line each
    /// — `schema app kept: it holds table app.invoices ("billing")` (#7).
    pub kept: Vec<String>,
}

/// A role [`Design::reset`] kept, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptRole {
    pub role: String,
    /// The other databases on the server where it holds privileges or objects
    /// (or which it owns).
    pub databases: Vec<String>,
}

impl ResetComplete {
    /// How many objects of `kind` the reset dropped.
    pub fn count(&self, kind: EntityType) -> usize {
        self.dropped.iter().filter(|(k, _)| *k == kind).count()
    }
}
