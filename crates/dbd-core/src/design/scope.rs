use super::*;

impl Design {
    /// External entity names from config (for ref resolution / gap analysis).
    pub(in crate::design) fn external_names(&self) -> Vec<String> {
        self.config.external.iter().map(|e| e.name.clone()).collect()
    }

    /// Resolve a scope by name. `None` ⇒ `default` scope if defined, else `all`.
    /// `deps_override` (CLI `--deps`) wins over the scope's own `deps`.
    pub fn resolve_scope(&self, name: Option<&str>, deps_override: Option<DepsPolicy>) -> Result<ResolvedScope> {
        crate::scope::resolve(
            &self.config.scopes,
            name,
            deps_override,
            &self.entities,
            &self.external_names(),
        )
    }

    /// The set of entity names an operation should act on under this scope.
    /// `include` policy expands to the dependency closure.
    pub fn working_set(&self, scope: &ResolvedScope) -> Result<std::collections::HashSet<String>> {
        match scope.deps {
            DepsPolicy::Include => crate::scope::closure(scope, &self.entities, &self.external_names()),
            DepsPolicy::Report => Ok(scope.entities.clone()),
        }
    }

    /// Whether an entity is kept under a resolved scope's working set.
    /// Roles/externals are always-on infrastructure. Extensions are too by
    /// default, unless the scope sets an `extensions` allowlist — then only the
    /// named extensions apply (an empty list drops them all), letting a scope
    /// target a database that lacks an extension (e.g. an embedded PG without
    /// pgvector).
    pub(in crate::design) fn entity_in_scope(
        entity: &Entity,
        scope: &ResolvedScope,
        working_set: &std::collections::HashSet<String>,
    ) -> bool {
        if scope.is_all {
            return true;
        }
        match entity.entity_type {
            EntityType::Extension => match &scope.extensions {
                Some(allow) => allow.contains(&entity.name),
                None => true,
            },
            EntityType::Role | EntityType::External => true,
            _ => working_set.contains(&entity.name),
        }
    }

    /// The loaded entities filtered to a resolved scope's working set
    /// (closure under `include`, the plain set under `report`). The all-scope
    /// returns every entity. Read-only and gap-neutral — for entity-selecting
    /// commands like `dbml` that document/emit a subset without the write-path
    /// gap gate.
    pub fn scoped_entities(&self, scope: &ResolvedScope) -> Result<Vec<Entity>> {
        if scope.is_all {
            return Ok(self.entities.clone());
        }
        let ws = self.working_set(scope)?;
        Ok(self
            .entities
            .iter()
            .filter(|e| Self::entity_in_scope(e, scope, &ws))
            .cloned()
            .collect())
    }

    /// Resolve a scope to its working set, running the `report`-policy gap gate
    /// first (aborts before any write). `None`/all-scope ⇒ `Ok(None)` (no
    /// filtering). Shared by `apply`, `reconcile`, and `diff_live`.
    pub(in crate::design) fn scope_working_set(
        &self,
        scope: Option<&ResolvedScope>,
    ) -> Result<Option<std::collections::HashSet<String>>> {
        match scope {
            Some(s) if !s.is_all => {
                self.check_scope_gaps(s)?;
                Ok(Some(self.working_set(s)?))
            }
            _ => Ok(None),
        }
    }

    /// Valid, non-external entities kept under `scope`'s working set, optionally
    /// narrowed to a single entity `name`. Shared by `apply` (which passes a
    /// `name`) and `reconcile`/`diff_live` (which pass `None`).
    pub(in crate::design) fn entities_in_scope(
        &self,
        scope: Option<&ResolvedScope>,
        working_set: Option<&std::collections::HashSet<String>>,
        name: Option<&str>,
    ) -> Vec<&Entity> {
        self.entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .filter(|e| match (working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect()
    }

    /// Entities that would be in scope but were dropped by
    /// [`Self::entities_in_scope`]'s error filter — its exact inverse, and it
    /// must stay in step with it.
    pub(in crate::design) fn unparseable_in_scope(
        &self,
        scope: Option<&ResolvedScope>,
        working_set: Option<&std::collections::HashSet<String>>,
        name: Option<&str>,
    ) -> Vec<&Entity> {
        self.entities
            .iter()
            .filter(|e| !e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .filter(|e| match (working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect()
    }

    /// Refuse to act on a design dbd could not fully read.
    ///
    /// [`Self::entities_in_scope`] silently drops an entity that carries a parse
    /// error, and without this guard that filter is invisible: `dbd apply`
    /// reported "N entities applied" and exited 0 while never creating the
    /// object, and `dbd reconcile` reported no drift for it. A design is only a
    /// source of truth if every file in it was understood, so callers check this
    /// *before* any write — a partial apply is what makes the failure expensive.
    ///
    /// This fires only on SQL Postgres itself rejects. Valid SQL that merely
    /// outruns sqlparser is recovered via libpg_query inside
    /// `parser::parse_with_sqlparser`, reached from the scan path through
    /// `parser::parse_entity_with` → `SqlparserDdl`, and never reaches here.
    pub(in crate::design) fn ensure_fully_parsed(
        &self,
        scope: Option<&ResolvedScope>,
        working_set: Option<&std::collections::HashSet<String>>,
        name: Option<&str>,
    ) -> Result<()> {
        let bad = self.unparseable_in_scope(scope, working_set, name);
        if bad.is_empty() {
            return Ok(());
        }

        let mut msg = format!(
            "{} file(s) could not be parsed, so the design is incomplete and cannot be applied:\n",
            bad.len()
        );
        for e in &bad {
            let where_ = e
                .file
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_else(|| e.name.clone());
            msg.push_str(&format!("\n  {where_}\n"));
            for err in &e.errors {
                msg.push_str(&format!("    {err}\n"));
            }
        }
        msg.push_str("\nFix the file(s), or run `dbd inspect` for the full report.");
        Err(DbdError::Config(msg))
    }

    /// The schemas a desired-entity set occupies: a bare `Schema` entity → its
    /// name; anything else → its `schema` (or the default schema). Shared by
    /// `reconcile` and `diff_live` to bound the live diff to managed schemas.
    pub(in crate::design) fn managed_schemas(desired: &[&Entity]) -> std::collections::HashSet<String> {
        desired
            .iter()
            .map(|e| match e.entity_type {
                EntityType::Schema => e.name.clone(),
                _ => {
                    let s = e.schema.clone().unwrap_or_default();
                    if s.is_empty() {
                        crate::reconcile::DEFAULT_SCHEMA.to_string()
                    } else {
                        s
                    }
                }
            })
            .collect()
    }

    /// Under `report` policy, error if the scope has dependency gaps (an in-scope
    /// entity that references a managed entity outside the scope). No-op for the
    /// all-scope, `include` policy, or a gap-free scope. Shared by `apply` and
    /// `import_data` so both refuse the same way before any write.
    pub fn check_scope_gaps(&self, scope: &ResolvedScope) -> Result<()> {
        if scope.is_all || scope.deps != DepsPolicy::Report {
            return Ok(());
        }
        let gaps = crate::scope::analyze_gaps(scope, &self.entities, &self.external_names());
        if gaps.is_empty() {
            return Ok(());
        }
        let detail: String = gaps
            .iter()
            .map(|g| format!("  {} requires {} ({})", g.required_by, g.missing, g.chain.join(" → ")))
            .collect::<Vec<_>>()
            .join("\n");
        Err(DbdError::Config(format!(
            "scope '{}' has {} dependency gap(s) — add them or use --deps include:\n{detail}",
            scope.name,
            gaps.len()
        )))
    }

    /// Scope guard: refuse to operate under a scope different from the one this
    /// database was pinned to. `meta` is the stored project meta (`None` on a
    /// fresh database), `requested` is the resolved scope name for this run, and
    /// `allow_scope_change` bypasses the guard (the next successful write re-pins
    /// the DB). A database with no recorded scope (`meta.scope == None`) is
    /// unpinned and never blocks — the current run pins it. Mirrors the prod
    /// guard in [`Design::reset`]; invoked from the CLI write handlers.
    pub fn check_scope_guard(
        meta: Option<&crate::adapter::ProjectMeta>,
        requested: &str,
        allow_scope_change: bool,
    ) -> Result<()> {
        if allow_scope_change {
            return Ok(());
        }
        if let Some(pinned) = meta.and_then(|m| m.scope.as_deref())
            && pinned != requested
        {
            return Err(DbdError::SafetyGuard(format!(
                "scope guard: this database is pinned to scope '{pinned}', but you requested '{requested}'.\n\
                 Applying a different scope would build a divergent schema.\n\
                 → re-run with --scope {pinned}, or pass --allow-scope-change to re-point this database to '{requested}'."
            )));
        }
        Ok(())
    }
}
