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
    /// intact since roles are not scope-selectable.
    pub async fn reset(
        &self,
        adapter: &dyn DatabaseAdapter,
        target: &str,
        force: bool,
        drop_schemas: bool,
        drop_extensions: bool,
        scope: Option<&ResolvedScope>,
    ) -> Result<()> {
        if !force
            && let Some(meta) = adapter.get_project_meta().await? {
                if meta.env == "prod" {
                    return Err(DbdError::SafetyGuard(
                        "reset is blocked — database is marked as prod. Use --force to override."
                            .to_string(),
                    ));
                }
                if meta.version >= 1 {
                    return Err(DbdError::SafetyGuard(
                        "reset is blocked — database has applied migrations. Use --force to override."
                            .to_string(),
                    ));
                }
            }

        if let Some(sql) = self.reset_script(target, drop_schemas, drop_extensions, scope)? {
            adapter.execute_script(&sql).await?;
        }
        adapter.clear_project_migrations().await?;

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
        // subset scope leaves shared roles intact.
        let is_subset = matches!(scope, Some(s) if !s.is_all);
        let roles: &[_] = if is_subset {
            &[]
        } else {
            self.config
                .target
                .values()
                .next()
                .map(|t| &t.roles[..])
                .unwrap_or(&[])
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

        // Schemas the `--schemas` path may drop — all managed schemas, or just
        // those the scope occupies.
        let schemas = self.reset_target_schemas(scope)?;

        // The active target's extensions (by bare name) for the `--extensions` path.
        let extensions: Vec<String> = self
            .config
            .target
            .values()
            .next()
            .map(|t| t.extensions.iter().map(|e| e.name().to_string()).collect())
            .unwrap_or_default();

        script::build_reset_script(
            &entities,
            roles,
            &extensions,
            target,
            drop_schemas,
            drop_extensions,
            &schemas,
        )
        .map_err(DbdError::SafetyGuard)
    }
}
