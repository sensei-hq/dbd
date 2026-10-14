//! What a command writing an artifact of the design reads (#7 phase 3,
//! slice 5): the schema model, the graph, DBML and the combined and emitted
//! scripts read the modules a run selects as one design.

use std::collections::HashSet;

use super::{Design, Module, Workspace};
use crate::config::{DepsPolicy, DesignConfig};
use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};
use crate::history::HistoryEntry;
use crate::scope::ResolvedScope;

/// The designs `dbml`, `diagram`, `graph`, `combine` and `emit` read, as one:
/// a project's own, or the modules a run selects, in `depends_on` order, each
/// through its scope.
///
/// Each entity is read once, from the module declaring it: in its dependents
/// a dependency's entity is an external, standing in for it, and the
/// manifest's schemas are every module's.
pub struct Reading {
    name: String,
    note: Option<String>,
    parts: Vec<(Module, ResolvedScope)>,
}

impl Workspace {
    /// What a command writing an artifact reads: the modules `scope` and
    /// `module` select, as a run that builds or reads selects them, each
    /// resolving `scope` (with `deps`) as its own — without a run's gap
    /// check: a table a scope leaves out of a dependency is drawn as a stub,
    /// as in one design.
    pub fn reading(self, module: Option<&str>, scope: Option<&str>, deps: Option<DepsPolicy>) -> Result<Reading> {
        let project = &self.config().project;
        let (name, note) = (project.name.clone(), project.note.clone());
        let parts = self
            .under_scope_of(scope, module)?
            .select(module)?
            .into_iter()
            .map(|m| {
                let resolved = m.design().resolve_scope(scope, deps)?;
                Ok((m, resolved))
            })
            .collect::<Result<Vec<_>>>()?;
        if parts.is_empty() {
            return Err(DbdError::Config("the run selects no module to read".to_string()));
        }
        Ok(Reading { name, note, parts })
    }
}

impl Reading {
    /// The project's name — the manifest's, for a workspace.
    pub fn project_name(&self) -> &str {
        &self.name
    }

    /// The project's note — the manifest's, for a workspace.
    pub fn project_note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// `source.dialect`, which every module shares.
    pub fn dialect(&self) -> &str {
        &self.config().source.dialect
    }

    /// What the modules share of the manifest — `external:`, `dbml:` — or the
    /// project's config.
    pub(crate) fn config(&self) -> &DesignConfig {
        self.parts[0].0.design().config()
    }

    /// The modules read, in `depends_on` order — a project's one, unnamed.
    pub fn modules(&self) -> impl Iterator<Item = &Module> {
        self.parts.iter().map(|(m, _)| m)
    }

    /// Each module read, with the scope it is read through.
    pub(crate) fn parts(&self) -> impl Iterator<Item = (&Design, &ResolvedScope)> {
        self.parts.iter().map(|(m, s)| (m.design(), s))
    }

    /// The scope the run reads through, by name — each module resolves it as
    /// its own.
    pub fn scope(&self) -> &ResolvedScope {
        &self.parts[0].1
    }

    /// The module declaring `name`, in a workspace.
    pub fn module_of(&self, name: &str) -> Option<&str> {
        self.parts
            .iter()
            .find(|(m, _)| declares(m.design(), name))
            .and_then(|(m, _)| m.name())
    }

    /// Each module's entities `pick` takes, each once: not an external another
    /// module read declares — the stand-in for a dependency's entity — and not
    /// an entity a module before it already gave, the manifest's schemas
    /// among them.
    fn sections(
        &self,
        pick: impl Fn(&Design, &ResolvedScope) -> Result<Vec<Entity>>,
    ) -> Result<Vec<(&Module, Vec<Entity>)>> {
        let mut given: HashSet<(EntityType, String)> = HashSet::new();
        let mut sections = Vec::new();
        for (i, (module, scope)) in self.parts.iter().enumerate() {
            let elsewhere = |name: &str| {
                self.parts
                    .iter()
                    .enumerate()
                    .any(|(j, (m, _))| j != i && declares(m.design(), name))
            };
            let kept: Vec<Entity> = pick(module.design(), scope)?
                .into_iter()
                .filter(|e| !(e.entity_type == EntityType::External && elsewhere(&e.name)))
                .filter(|e| !given.contains(&(e.entity_type, e.name.clone())))
                .collect();
            given.extend(kept.iter().map(|e| (e.entity_type, e.name.clone())));
            sections.push((module, kept));
        }
        Ok(sections)
    }

    /// Each module's entities in view, module by module — see [`Self::entities`].
    pub(crate) fn sections_in_view(&self) -> Result<Vec<(&Module, Vec<Entity>)>> {
        self.sections(|design, scope| design.scoped_entities(scope))
    }

    /// The entities in view, each once, in `depends_on` order: what each
    /// module's scope keeps of it.
    pub fn entities(&self) -> Result<Vec<Entity>> {
        Ok(self.sections_in_view()?.into_iter().flat_map(|(_, e)| e).collect())
    }

    /// Every entity the modules read declare, each once, before their scopes —
    /// what a stub for an entity the scope leaves out is described from.
    pub fn declared(&self) -> Vec<Entity> {
        self.sections(|design, _| Ok(design.entities().to_vec()))
            .map(|sections| sections.into_iter().flat_map(|(_, e)| e).collect())
            .unwrap_or_default()
    }

    /// How many of the entities read are in view, of how many — each
    /// module's, together.
    pub fn counts(&self) -> Result<(usize, usize)> {
        let mut counts = (0, 0);
        for (design, scope) in self.parts() {
            let (kept, total) = design.scope_counts(scope)?;
            counts = (counts.0 + kept, counts.1 + total);
        }
        Ok(counts)
    }

    /// The files in view that did not parse, module by module.
    pub fn unparsed(&self) -> Result<Vec<&Entity>> {
        let mut unparsed = Vec::new();
        for (design, scope) in self.parts() {
            unparsed.extend(design.unparsed(scope)?);
        }
        Ok(unparsed)
    }

    /// The dependency graph over the modules read; `name` narrows it to that
    /// entity's subgraph, across the modules — refused for one no module read
    /// declares, as one design refuses it.
    pub fn graph(&self, name: Option<&str>) -> Result<crate::dependency::GraphResult> {
        if let Some(n) = name {
            let (module, scope) = self
                .parts
                .iter()
                .find(|(m, _)| declares(m.design(), n))
                .unwrap_or(&self.parts[0]);
            module.design().check_graph_name(n, Some(scope))?;
        }
        let entities: Vec<Entity> = self
            .sections(|design, scope| design.graph_entities(Some(scope)))?
            .into_iter()
            .flat_map(|(_, e)| e)
            .collect();
        Ok(crate::dependency::graph_from_entities(&entities, name))
    }

    /// The entities in view as one script, in `depends_on` order — a
    /// module's statements after a comment naming it.
    pub fn combined(&self) -> Result<String> {
        let sections: Vec<String> = self
            .sections_in_view()?
            .into_iter()
            .map(|(module, entities)| {
                let ddl = module.design().combined_ddl_of(&entities);
                match module.name() {
                    Some(name) => format!("-- module {name} ({})\n{ddl}", module.owner()),
                    None => ddl,
                }
            })
            .collect();
        Ok(sections.join("\n\n"))
    }

    /// What each snapshot changed, oldest first — a workspace's, each module's
    /// in turn, in `depends_on` order, each entry naming its module — seen
    /// through each module's scope. A scope is a set of entities, and the
    /// history names tables that no longer exist, so each is asked of the
    /// scope's definition rather than of today's model; see
    /// [`crate::scope::admits`].
    pub fn history(&self) -> Result<Vec<HistoryEntry>> {
        let mut history = Vec::new();
        for (module, scope) in &self.parts {
            let design = module.design();
            let dir = design.project_dir();
            let entries = if scope.is_all {
                crate::history::load(dir)
            } else {
                let working = design.working_set(scope)?;
                let existing: HashSet<String> = design.entities().iter().map(|e| e.name.clone()).collect();
                let scopes = &design.config().scopes;
                crate::history::load_scoped(dir, |schema, name| {
                    crate::scope::admits(scopes, scope, &working, &existing, &format!("{schema}.{name}"))
                })
            };
            let mut entries = match (entries, module.name()) {
                (Ok(entries), _) => entries,
                (Err(e), None) => return Err(e),
                (Err(e), Some(name)) => return Err(DbdError::Config(format!("module {name}: {e}"))),
            };
            for entry in &mut entries {
                entry.module = module.name().map(str::to_string);
            }
            history.extend(entries);
        }
        Ok(history)
    }
}

/// Whether `design` declares `name` — not merely knows it as an external.
fn declares(design: &Design, name: &str) -> bool {
    design
        .entities()
        .iter()
        .any(|e| e.name == name && e.entity_type != EntityType::External)
}
