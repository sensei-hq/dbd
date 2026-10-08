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
    /// drops goes too: the project's recorded migrations, and the drift stamp of
    /// every materialized view the reset removed.
    pub async fn reset(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<()> {
        // Heal-first: ensure bookkeeping storage exists (and is at the current
        // layout, healing any legacy layout in place) before the meta read below
        // — every ownership operation calls this up front.
        adapter.heal_bookkeeping().await?;

        if !force {
            Self::check_reset_guard(adapter.get_project_meta().await?.as_ref())?;
        }

        if let Some(sql) = self.reset_script(target, drop_schemas, drop_extensions, scope)? {
            // The matviews that exist now and not after the drop are the ones
            // this reset dropped — by name, with their schema, or by CASCADE
            // from what they read. Their drift stamps go with them.
            let before = adapter.matview_states().await?;
            adapter.execute_script(&sql).await?;
            let after = adapter.matview_states().await?;
            let mut dropped: Vec<String> = before.into_keys().filter(|mv| !after.contains_key(mv)).collect();
            dropped.sort();
            adapter.forget_matview_stamps(&dropped).await?;
        }
        adapter.clear_project_migrations().await?;

        Ok(())
    }

    /// The guard [`Design::reset`] checks unless forced: a database marked prod,
    /// or one with applied migrations, is not reset. `meta` is the database's
    /// recorded meta (`None` for one dbd has never written to, which passes).
    ///
    /// Public so a preview refuses exactly where the real run does: `reset
    /// --dry-run` printed the whole plan for a prod database, and exited 0,
    /// while the real run refused it.
    pub fn check_reset_guard(meta: Option<&crate::adapter::ProjectMeta>) -> Result<()> {
        let Some(meta) = meta else { return Ok(()) };
        // Compared through the alias table: rows written before the CLI
        // normalised `-e` can say `production`.
        if crate::config::normalize_env(Some(&meta.env)).is_ok_and(|env| env == "prod") {
            return Err(DbdError::SafetyGuard(
                "reset is blocked — database is marked as prod. Use --force to override.".to_string(),
            ));
        }
        if meta.version >= 1 {
            return Err(DbdError::SafetyGuard(
                "reset is blocked — database has applied migrations. Use --force to override.".to_string(),
            ));
        }
        Ok(())
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
        // Roles aren't scope-selectable, so only a full reset drops them; a
        // subset scope leaves shared roles intact. Every role the design
        // declares counts — `ddl/role/` files as well as `target.roles`, which
        // used to be the only ones dropped.
        let is_subset = matches!(scope, Some(s) if !s.is_all);
        let roles: Vec<crate::config::RoleEntry> = if is_subset {
            Vec::new()
        } else {
            self.entities
                .iter()
                .filter(|e| e.entity_type == EntityType::Role)
                .map(|e| crate::config::RoleEntry {
                    name: e.name.clone(),
                    refers: Vec::new(),
                })
                .collect()
        };

        // Data-model entities to drop individually (scope-filtered), in the
        // builder's reverse dependency order.
        let is_data_model = |e: &Entity| crate::entity::TYPES_WITH_SCHEMA.contains(&e.entity_type);
        let entities: Vec<&Entity> = match scope {
            Some(s) if !s.is_all => {
                let ws = self.working_set(s)?;
                self.entities
                    .iter()
                    .filter(|e| is_data_model(e) && Self::entity_in_scope(e, s, &ws))
                    .collect()
            }
            _ => self.entities.iter().filter(|e| is_data_model(e)).collect(),
        };

        // SQLite has the tables and views alone, and its own dialect for them.
        if target == "sqlite" {
            return Ok(script::build_sqlite_reset_script(&entities));
        }

        // Schemas the `--schemas` path may drop — all managed schemas, or just
        // those the scope occupies.
        let schemas = self.reset_target_schemas(scope)?;

        // The active target's extensions (by bare name) for the `--extensions`
        // path — only those the scope installs: a scope's `extensions:`
        // allowlist bounds what reset drops as it bounds what apply creates.
        let admitted = |name: &str| match scope {
            Some(s) if !s.is_all => s.extensions.as_ref().is_none_or(|allow| allow.contains(name)),
            _ => true,
        };
        let extensions: Vec<String> = self
            .config
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
            .unwrap_or_default();

        // A subset scope drops its own objects and nothing else: no CASCADE.
        if is_subset {
            return Ok(script::build_scoped_reset_script(
                &entities,
                &extensions,
                target,
                drop_schemas,
                drop_extensions,
                &schemas,
            ));
        }

        // What apply granted on the schemas, so the roles it granted to can be
        // dropped: Postgres refuses to drop a role that still holds a grant.
        let grants = self.config.effective_schema_grants();

        script::build_reset_script(
            &entities,
            &roles,
            &extensions,
            target,
            drop_schemas,
            drop_extensions,
            &schemas,
            &grants,
        )
        .map_err(DbdError::SafetyGuard)
    }
}
