//! A workspace: a manifest and its modules (#7).
//!
//! A `design.yaml` with a `modules:` block partitions one design into modules
//! that evolve separately. Each module is a [`Design`] of its own — owned by
//! `<project>/<module>`, rooted at its folder, with its own version, release
//! state and hooks — so everything keyed on the owner and the project root
//! (claims, uses, the record in `dbd.meta`, snapshots, migrations) works per
//! module unchanged, and the registry keeps modules apart as it keeps projects
//! apart. A `design.yaml` without `modules:` is a workspace of one: the
//! project itself, owned by `project.name`, exactly as before.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use super::Design;
use crate::adapter::OwnedObject;
use crate::config::{DesignConfig, ModuleConfig, ProjectConfig};
use crate::entity::{EntityType, Ref};
use crate::error::{DbdError, Result};

/// The folders a module's design reads and writes, each under its own folder.
const MODULE_FOLDERS: &[&str] = &["ddl", "import", "policies", "snapshots", "migrations"];

/// A manifest and its modules, in `depends_on` order: each after the modules
/// it depends on — of those whose dependencies are in place, the one
/// `modules:` lists first.
pub struct Workspace {
    /// The manifest as written, for a design with `modules:`. A design
    /// without them is its one module's config.
    manifest: Option<DesignConfig>,
    modules: Vec<Module>,
    /// The project folder: the module folders' parent, and where the
    /// manifest's hook paths are read from.
    root: PathBuf,
    /// The modules each manifest scope selects, by scope name — see
    /// [`Workspace::under_scope`].
    scope_modules: IndexMap<String, HashSet<String>>,
    /// The scope [`Workspace::under_scope`] narrowed the workspace to.
    scope: Option<String>,
    /// The modules it left out, for saying which an entity is in.
    left_out: Vec<Module>,
}

/// One module of a [`Workspace`]: a design of its own.
pub struct Module {
    name: Option<String>,
    depends_on: Vec<String>,
    design: Design,
}

impl Module {
    /// Its name under `modules:` — `None` for the project of a design
    /// without them.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The owner its runs claim as: `<project>/<module>`, or `project.name`
    /// for a design without modules.
    pub fn owner(&self) -> &str {
        self.design.owner()
    }

    /// The modules it depends on, as written.
    pub fn depends_on(&self) -> &[String] {
        &self.depends_on
    }

    pub fn design(&self) -> &Design {
        &self.design
    }

    pub fn design_mut(&mut self) -> &mut Design {
        &mut self.design
    }

    pub fn into_design(self) -> Design {
        self.design
    }
}

impl Workspace {
    /// Load the workspace `config_path` describes, its module folders relative
    /// to `project_dir` (the config's own folder when `None`).
    ///
    /// Refuses a `modules:` block that does not describe a partition: an
    /// unknown dependency or a cycle, a module name that cannot name an owner,
    /// a folder that is outside the project, the project's own, inside another
    /// module's, shared or missing, `project.version` or `project.released`
    /// (each module has its own), files at the root no module holds — and then
    /// two modules declaring the same entity (T19), or a module referencing
    /// one it does not depend on, directly or through its dependencies (T20).
    /// All before any database is involved.
    ///
    /// Each module knows what its dependencies declare without `external:`
    /// entries: their entities resolve its references as externals — never
    /// its own to build, claim or own; their definitions stay in their module.
    pub fn load(config_path: &Path, env: &str, project_dir: Option<&Path>) -> Result<Self> {
        let project_dir = project_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_path.parent().unwrap_or(Path::new(".")).to_path_buf());
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let text = std::fs::read_to_string(config_path)
            .map_err(|e| DbdError::Config(format!("Cannot read {}: {}", config_path.display(), e)))?;
        let manifest: DesignConfig = serde_yaml::from_str(&text)?;
        if manifest.modules.is_empty() {
            let design = Design::scan(manifest, env, project_dir.clone())?.resolved(&[]);
            return Ok(Self {
                modules: vec![Module {
                    name: None,
                    depends_on: Vec::new(),
                    design,
                }],
                manifest: None,
                root: project_dir,
                scope_modules: IndexMap::new(),
                scope: None,
                left_out: Vec::new(),
            });
        }

        let folders = check_manifest(&manifest, &project_dir)?;
        let order = depends_on_order(&manifest.modules)?;

        let mut scanned: Vec<(String, Design)> = Vec::new();
        for name in &order {
            let config = module_config(&text, &manifest.project.name, name)?;
            let mut design = Design::scan(config, env, folders[name].clone())?;
            design.module = Some(name.clone());
            scanned.push((name.clone(), design));
        }
        let objects: Vec<Vec<OwnedObject>> = scanned.iter().map(|(_, d)| d.declared_objects()).collect();
        check_overlaps(&scanned, &objects)?;

        let declared = Declared::of(&scanned, &objects);
        let reach = dependency_closure(&manifest.modules);
        let manifest_externals: HashSet<&str> = manifest.external.iter().map(|e| e.name.as_str()).collect();

        // Resolved against everything the other modules declare, so that a
        // reference past `depends_on` is found as one rather than reported as
        // unresolved — then left knowing only what it may reach.
        let mut modules = Vec::new();
        let mut gaps: Vec<String> = Vec::new();
        for (name, design) in scanned {
            // What a reference can name in the module itself. Not its schemas
            // and extensions: every module has those, and a role another
            // module declares is that module's though a schema here shares
            // its name.
            let own: HashSet<String> = design
                .entities
                .iter()
                .filter(|e| !declares_nothing(e.entity_type))
                .map(|e| e.qualified_key())
                .collect();
            let mut design = design.resolved(&declared.known_to(&own));
            let reaches = |module: &String| *module == name || reach[&name].contains(module);
            for entity in design.entities.iter().filter(|e| e.entity_type != EntityType::External) {
                for r in &entity.refs {
                    if !r.unresolved && own.contains(&r.name) {
                        continue;
                    }
                    if let Some(by) = declared.of_ref(r).filter(|by| !by.iter().any(reaches)) {
                        let gap = format!(
                            "✗ dependency gap: {} ({name}) requires {} ({}) — add {} to modules.{name}.depends_on",
                            entity.name,
                            r.name,
                            by.join(" or "),
                            by.join(" or ")
                        );
                        if !gaps.contains(&gap) {
                            gaps.push(gap);
                        }
                    }
                }
            }
            design.entities.retain(|e| {
                e.entity_type != EntityType::External
                    || manifest_externals.contains(e.name.as_str())
                    || declared.of_name(&e.name).is_none_or(|by| by.iter().any(reaches))
            });
            // The project's other owners hand over to this module what it now
            // declares, and what they declare is moving to them (#7, the
            // workspace transfer rule).
            design.transfer_from = std::iter::once(manifest.project.name.clone())
                .chain(
                    order
                        .iter()
                        .filter(|m| **m != name)
                        .map(|m| format!("{}/{m}", manifest.project.name)),
                )
                .collect();
            design.declared_elsewhere = order
                .iter()
                .zip(&objects)
                .filter(|(m, _)| **m != name)
                .flat_map(|(m, objects)| objects.iter().map(|o| (m.clone(), o.clone())))
                .collect();
            modules.push(Module {
                depends_on: manifest.modules[&name].depends_on.clone(),
                name: Some(name),
                design,
            });
        }
        // Each `export:` entry is the module's that declares its entity; one no
        // module declares stays with the first, which warns about it once.
        let declares = |m: &Module, name: &str| {
            m.design
                .entities
                .iter()
                .any(|e| e.name == name && e.entity_type != EntityType::External)
        };
        let owners_of_exports: Vec<Option<usize>> = manifest
            .export
            .iter()
            .map(|entry| modules.iter().position(|m| declares(m, &entry.name())))
            .collect();
        for (i, module) in modules.iter_mut().enumerate() {
            let mut entry = 0;
            module.design.config.export.retain(|_| {
                let owner = owners_of_exports[entry];
                entry += 1;
                owner == Some(i) || (owner.is_none() && i == 0)
            });
        }
        if !gaps.is_empty() {
            return Err(DbdError::Config(format!(
                "{} dependency gap(s) between modules — a module references only its own entities and \
                 those of the modules it depends on:\n  {}",
                gaps.len(),
                gaps.join("\n  ")
            )));
        }

        let scope_modules = project_scopes(&manifest, &mut modules)?;
        Ok(Self {
            modules,
            manifest: Some(manifest),
            root: project_dir,
            scope_modules,
            scope: None,
            left_out: Vec::new(),
        })
    }

    /// Where a run started in `dir` finds its workspace (#7): the nearest folder
    /// above `dir` holding `config_name` and declaring `modules:` — as Cargo finds
    /// its workspace — with the module whose folder holds `dir`, if one does.
    /// `None` when the nearest such file above declares no modules, or there is
    /// none: a design without modules is not looked for above where it is run.
    pub fn locate(dir: &Path, config_name: &Path) -> Option<(PathBuf, Option<String>)> {
        // Along the path the run was started in — a module folder may be a
        // link to its files elsewhere — and, failing that, the real one: a
        // link from outside the workspace to a module folder.
        let real = dir.canonicalize().ok();
        let logical = logical_path(dir).or_else(|| real.clone())?;
        let walk = |start: &Path| -> Option<Option<(PathBuf, Option<String>)>> {
            for root in start.ancestors().skip(1) {
                let config_path = root.join(config_name);
                if !config_path.is_file() {
                    continue;
                }
                let Ok(manifest) = crate::config::read(&config_path) else {
                    // Its own load says what is wrong with it.
                    return Some(Some((root.to_path_buf(), None)));
                };
                if manifest.modules.is_empty() {
                    return Some(None);
                }
                let folders: Vec<(&String, PathBuf)> = manifest
                    .modules
                    .iter()
                    .filter_map(|(name, module)| Some((name, root.join(module_folder(name, module).ok()?))))
                    .collect();
                // The folder the run starts in names its module first; the
                // real folder a link in it points at, only failing that.
                let module = folders
                    .iter()
                    .find(|(_, folder)| logical.starts_with(folder))
                    .or_else(|| {
                        folders.iter().find(|(_, folder)| {
                            matches!((&real, folder.canonicalize().ok()), (Some(d), Some(f)) if d.starts_with(&f))
                        })
                    })
                    .map(|(name, _)| (*name).clone());
                return Some(Some((root.to_path_buf(), module)));
            }
            None
        };
        match walk(&logical) {
            Some(found) => found,
            None => real.as_deref().and_then(walk).flatten(),
        }
    }

    /// The modules `config_path` declares, by name, each with its folder under
    /// `project_dir` — read without loading any module's design, for the
    /// commands that go over the files of a workspace (`format`, `doctor`,
    /// `snapshot --list`). Empty for a design without `modules:`. Refuses a
    /// folder outside the project, or one that is not there.
    pub fn folders(config_path: &Path, project_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
        let manifest = crate::config::read(config_path)?;
        manifest
            .modules
            .iter()
            .map(|(name, module)| {
                let dir = project_dir.join(module_folder(name, module)?);
                if !dir.is_dir() {
                    return Err(DbdError::Config(format!(
                        "module \"{name}\": its folder {} does not exist",
                        dir.display()
                    )));
                }
                Ok((name.clone(), dir))
            })
            .collect()
    }

    /// Whether the manifest declares `modules:` — `false` for a project that
    /// is a workspace of one.
    pub fn has_modules(&self) -> bool {
        self.manifest.is_some()
    }

    /// The manifest: `design.yaml` as written — its `modules:`, `scopes:` and
    /// the `apply:` hooks that run around the whole workspace.
    pub fn config(&self) -> &DesignConfig {
        self.manifest
            .as_ref()
            .unwrap_or_else(|| self.modules[0].design.config())
    }

    /// Its modules, in `depends_on` order.
    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    pub fn modules_mut(&mut self) -> &mut [Module] {
        &mut self.modules
    }

    pub fn into_modules(self) -> Vec<Module> {
        self.modules
    }

    /// The workspace a run under `scope` sees (#7): the modules the scope
    /// selects — those it names with every module they depend on, whole, and
    /// those it includes entities of — each resolving the scope as its own,
    /// under the same name. `None` is the `default` scope if the manifest has
    /// one, else every module; `all` is every module. A design without
    /// `modules:` resolves its scopes itself, and is returned as it is.
    pub fn under_scope(mut self, scope: Option<&str>) -> Result<Self> {
        let Some(manifest) = &self.manifest else {
            return Ok(self);
        };
        let name = match scope {
            Some(name) => name,
            None if manifest.scopes.contains_key("default") => "default",
            None => return Ok(self),
        };
        if name == "all" {
            return Ok(self);
        }
        let Some(selected) = self.scope_modules.get(name) else {
            let available: Vec<&str> = std::iter::once("all")
                .chain(self.scope_modules.keys().map(String::as_str))
                .collect();
            return Err(DbdError::Config(format!(
                "unknown scope '{name}'. Available: {}",
                available.join(", ")
            )));
        };
        let selected = selected.clone();
        let (kept, left_out): (Vec<Module>, Vec<Module>) = std::mem::take(&mut self.modules)
            .into_iter()
            .partition(|m| m.name.as_ref().is_some_and(|n| selected.contains(n)));
        self.modules = kept;
        self.left_out = left_out;
        self.scope = Some(name.to_string());
        Ok(self)
    }

    /// [`Self::under_scope`] for a run of `module` — named, or the folder it
    /// is run in: with no `scope`, the `default` scope when it selects the
    /// module; a module the default leaves out runs on its own, as asked for,
    /// each module resolving its own `default`, else `all`.
    pub fn under_scope_of(self, scope: Option<&str>, module: Option<&str>) -> Result<Self> {
        let default_leaves_out = |m: &str| {
            self.scope_modules
                .get("default")
                .is_some_and(|selected| !selected.contains(m))
        };
        match (scope, module) {
            (None, Some(m)) if default_leaves_out(m) => self.under_scope(Some("all")),
            _ => self.under_scope(scope),
        }
    }

    /// Refuse a module `name` the scope this workspace runs under leaves out.
    fn in_scope(&self, name: &str) -> Result<()> {
        match &self.scope {
            Some(scope) if !self.modules.iter().any(|m| m.name.as_deref() == Some(name)) => {
                Err(DbdError::Config(format!(
                    "module \"{name}\" is not in scope {scope}, which runs {}",
                    self.module_names().join(", ")
                )))
            }
            _ => Ok(()),
        }
    }

    fn module_names(&self) -> Vec<&str> {
        self.modules.iter().filter_map(|m| m.name.as_deref()).collect()
    }

    /// The modules a run that builds or reads goes over — `apply`, `deploy`,
    /// `reconcile`, an import, a diff — in `depends_on` order: every module,
    /// or the module `module` and every module it depends on, directly or
    /// through another. The manifest's own hooks run once around them: its
    /// `apply.before` ahead of the first module's, its `apply.after` and
    /// `import.after` behind the last's. A design without `modules:` is its
    /// one module, with no module to name.
    pub fn select(self, module: Option<&str>) -> Result<Vec<Module>> {
        let Some(manifest) = &self.manifest else {
            return match module {
                Some(name) => Err(no_modules_to_name(name)),
                None => Ok(self.modules),
            };
        };
        let wanted = selection(manifest, module)?;
        if let Some(name) = module {
            self.in_scope(name)?;
        }
        self.with_hooks_around(|m| wanted.contains(m.name.as_deref().unwrap_or_default()))
    }

    /// [`Self::select`] narrowed to the module declaring the entity `name` —
    /// what `-n` runs — with the manifest's hooks around it, as around any
    /// run. Refuses a name no module declares, and one `module` does not
    /// select, naming its module.
    pub fn select_declaring(self, module: Option<&str>, name: &str) -> Result<Vec<Module>> {
        let Some(manifest) = &self.manifest else {
            return self.select(module);
        };
        let wanted = selection(manifest, module)?;
        if let Some(name) = module {
            self.in_scope(name)?;
        }
        let declaring: Vec<&str> = self
            .modules
            .iter()
            .filter(|m| {
                m.design
                    .resolve_name(name, None)
                    .is_ok_and(|e| e.entity_type != EntityType::External)
            })
            .filter_map(|m| m.name.as_deref())
            .collect();
        if declaring.is_empty() {
            let outside = self.left_out.iter().find(|m| {
                m.design
                    .resolve_name(name, None)
                    .is_ok_and(|e| e.entity_type != EntityType::External)
            });
            return Err(DbdError::Config(match (outside, &self.scope) {
                (Some(m), Some(scope)) => format!(
                    "{name} is module \"{}\"'s, which scope {scope} does not select",
                    m.name.as_deref().unwrap_or_default()
                ),
                _ => format!("no module declares {name}"),
            }));
        }
        if !declaring.iter().any(|d| wanted.contains(*d)) {
            return Err(DbdError::Config(format!(
                "{name} is module \"{}\"'s, and --module {} does not select it",
                declaring.join("\" or \""),
                module.unwrap_or_default()
            )));
        }
        let declaring: HashSet<String> = declaring.into_iter().map(str::to_string).collect();
        self.with_hooks_around(|m| {
            m.name
                .as_ref()
                .is_some_and(|n| declaring.contains(n) && wanted.contains(n))
        })
    }

    /// [`Self::select`], leaving out the modules `except` names — and the
    /// manifest's hooks around those it keeps, not on one it left out. A run
    /// that may not touch some modules — `reconcile`, a released one — goes
    /// over the rest. Returns the modules kept and the names of those left
    /// out. A design without `modules:` keeps its one design.
    pub fn select_except(
        self,
        module: Option<&str>,
        except: impl Fn(&Module) -> bool,
    ) -> Result<(Vec<Module>, Vec<String>)> {
        let Some(manifest) = &self.manifest else {
            return Ok((self.select(module)?, Vec::new()));
        };
        let wanted = selection(manifest, module)?;
        if let Some(name) = module {
            self.in_scope(name)?;
        }
        let in_run = |m: &Module| m.name.as_ref().is_some_and(|n| wanted.contains(n));
        let left_out: Vec<String> = self
            .modules
            .iter()
            .filter(|m| in_run(m) && except(m))
            .filter_map(|m| m.name.clone())
            .collect();
        let kept = self.with_hooks_around(|m| in_run(m) && !except(m))?;
        Ok((kept, left_out))
    }

    /// The modules a run that only reads one module goes over — `export`: the
    /// module `module` alone, or every module in `depends_on` order. Its
    /// dependencies are not read with it, and no hooks run.
    pub fn select_alone(self, module: Option<&str>) -> Result<Vec<Module>> {
        let Some(manifest) = &self.manifest else {
            return match module {
                Some(name) => Err(no_modules_to_name(name)),
                None => Ok(self.modules),
            };
        };
        match module {
            None => Ok(self.modules),
            Some(name) => {
                known_module(manifest, name)?;
                self.in_scope(name)?;
                Ok(self
                    .modules
                    .into_iter()
                    .filter(|m| m.name.as_deref() == Some(name))
                    .collect())
            }
        }
    }

    /// The modules a reset goes over: every module in reverse `depends_on`
    /// order, each dropped before the modules it depends on — or exactly the
    /// module `module`. Not its dependencies: they are other owners, and what
    /// it used of theirs the reset no longer needs. A reset runs no hooks.
    pub fn selected_to_reset(self, module: Option<&str>) -> Result<Vec<Module>> {
        let all = module.is_none();
        let selected = self.select_alone(module)?;
        Ok(if all {
            selected.into_iter().rev().collect()
        } else {
            selected
        })
    }

    /// The modules `keep` keeps, in load order, with the manifest's hooks on
    /// the first and the last.
    fn with_hooks_around(self, keep: impl Fn(&Module) -> bool) -> Result<Vec<Module>> {
        let mut selected: Vec<Module> = self.modules.into_iter().filter(|m| keep(m)).collect();
        let Some(mut manifest) = self.manifest else {
            return Ok(selected);
        };
        let crate::config::ApplyConfig { before, after } = std::mem::take(&mut manifest.apply);
        let import_after = std::mem::take(&mut manifest.import.after);
        let outer = || super::OuterHooks {
            dir: self.root.clone(),
            apply: crate::config::ApplyConfig::default(),
            import_after: Vec::new(),
        };
        if let Some(first) = selected.first_mut()
            && !before.is_empty()
        {
            first.design.outer_hooks.get_or_insert_with(outer).apply.before = before;
        }
        if let Some(last) = selected.last_mut()
            && !(after.is_empty() && import_after.is_empty())
        {
            let hooks = last.design.outer_hooks.get_or_insert_with(outer);
            hooks.apply.after = after;
            hooks.import_after = import_after;
        }
        Ok(selected)
    }
}

/// Refuse a run whose scope leaves out of one of its modules what another
/// requires (#7) — a gap, as in one design, but one no design's own gap
/// analysis sees: the module requiring it holds it as an external. Checked
/// over `modules`, the ones the run goes over, each resolving `scope` and
/// `deps` as the run's commands do — a module the `default` scope leaves out
/// resolving its own — and naming the module a refusal is for.
pub fn check_scope_gaps(
    modules: &[Module],
    scope: Option<&str>,
    deps: Option<crate::config::DepsPolicy>,
) -> Result<()> {
    if modules.iter().all(|m| m.name.is_none()) {
        return Ok(());
    }
    let label = |m: &Module| m.name.clone().unwrap_or_default();
    let mut in_scope: HashMap<String, (String, HashSet<String>)> = HashMap::new();
    for module in modules {
        let name = label(module);
        let refused = |e: DbdError| DbdError::Config(format!("module \"{name}\": {e}"));
        let resolved = module.design.resolve_scope(scope, deps).map_err(refused)?;
        let working_set = module.design.working_set(&resolved).map_err(refused)?;
        in_scope.insert(name, (resolved.name, working_set));
    }
    let declared_by = |entity: &str| {
        modules.iter().find_map(|m| {
            m.design
                .entities
                .iter()
                .any(|e| e.name == entity && e.entity_type != EntityType::External)
                .then(|| label(m))
        })
    };
    let mut gaps: Vec<String> = Vec::new();
    for module in modules {
        let name = label(module);
        let externals: HashSet<&str> = module
            .design
            .entities
            .iter()
            .filter(|e| e.entity_type == EntityType::External)
            .map(|e| e.name.as_str())
            .collect();
        let (_, own) = &in_scope[&name];
        for entity in module.design.entities.iter().filter(|e| own.contains(&e.name)) {
            for r in entity
                .refs
                .iter()
                .filter(|r| !r.unresolved && externals.contains(r.name.as_str()))
            {
                let Some(owner) = declared_by(&r.name) else { continue };
                let (owner_scope, theirs) = &in_scope[&owner];
                if !theirs.contains(&r.name) {
                    let gap = format!(
                        "{} (module \"{name}\") requires {}, which scope '{owner_scope}' leaves out of module \"{owner}\"",
                        entity.name, r.name
                    );
                    if !gaps.contains(&gap) {
                        gaps.push(gap);
                    }
                }
            }
        }
    }
    if gaps.is_empty() {
        return Ok(());
    }
    Err(DbdError::Config(format!(
        "{} dependency gap(s) between modules — stop excluding what is required, or exclude what requires it \
         too:\n  {}",
        gaps.len(),
        gaps.join("\n  ")
    )))
}

/// Each manifest scope, projected onto each module it selects (#7): set in
/// the module's config under the scope's name, so the module resolves it as
/// its own. A module the scope names — or one a named module depends on, or
/// one depending on which a module it includes entities of — runs whole, less
/// what the scope excludes of it; a module the scope only includes entities
/// of runs those. A scope naming no module and including nothing selects
/// every module. Returns the modules each scope selects; refuses a scope
/// naming a module `modules:` does not declare, or an item no module has.
fn project_scopes(manifest: &DesignConfig, modules: &mut [Module]) -> Result<IndexMap<String, HashSet<String>>> {
    use crate::config::{ScopeEntry, ScopeSpec};
    let closure = dependency_closure(&manifest.modules);
    let mut selected_by: IndexMap<String, HashSet<String>> = IndexMap::new();
    for (scope, entry) in &manifest.scopes {
        let spec = match entry {
            ScopeEntry::All(keyword) => {
                for module in modules.iter_mut() {
                    module
                        .design
                        .config
                        .scopes
                        .insert(scope.clone(), ScopeEntry::All(keyword.clone()));
                }
                selected_by.insert(scope.clone(), modules.iter().filter_map(|m| m.name.clone()).collect());
                continue;
            }
            ScopeEntry::Spec(spec) => spec,
        };
        // The modules an item names something of: for an include, an entity
        // of the module's own — every module has the manifest's schemas, so
        // a schema alone selects none; for an exclude, anything, its schema
        // included.
        let matched = |item: &str, entities_only: bool| -> Result<IndexMap<String, ()>> {
            let mut hits = IndexMap::new();
            let mut known = false;
            for module in modules.iter() {
                let own: Vec<crate::entity::Entity> = module
                    .design
                    .entities
                    .iter()
                    .filter(|e| e.entity_type != EntityType::External)
                    .cloned()
                    .collect();
                let names = crate::scope::match_item(item, &own);
                known |= names.is_ok();
                let hit = names.is_ok_and(|names| {
                    names
                        .iter()
                        .any(|n| !entities_only || own.iter().any(|e| &e.name == n && crate::scope::is_scopable(e)))
                });
                if hit && let Some(name) = &module.name {
                    hits.insert(name.clone(), ());
                }
            }
            // An item some module knows — a declared schema with nothing in
            // it, say — selects nothing; one no module knows is a mistake.
            if !known {
                return Err(DbdError::Config(format!(
                    "scope '{scope}': item '{item}' matches nothing any module declares"
                )));
            }
            Ok(hits)
        };

        let mut whole: HashSet<String> = HashSet::new();
        for name in &spec.modules {
            if !manifest.modules.contains_key(name) {
                return Err(DbdError::Config(format!(
                    "scope '{scope}' selects module \"{name}\", which modules: does not declare"
                )));
            }
            whole.insert(name.clone());
            whole.extend(closure[name].iter().cloned());
        }
        let mut includes: IndexMap<String, Vec<String>> = IndexMap::new();
        for item in &spec.includes {
            for module in matched(item, true)?.into_keys() {
                includes.entry(module).or_default().push(item.clone());
            }
        }
        let mut excludes: IndexMap<String, Vec<String>> = IndexMap::new();
        for item in &spec.excludes {
            for module in matched(item, false)?.into_keys() {
                excludes.entry(module).or_default().push(item.clone());
            }
        }
        if spec.modules.is_empty() && spec.includes.is_empty() {
            whole.extend(manifest.modules.keys().cloned());
        }
        // A module whose entities the scope includes runs on the modules it
        // depends on, whole, as `--module` runs it.
        for module in includes.keys() {
            whole.extend(closure[module].iter().cloned());
        }

        let mut selected = HashSet::new();
        for module in modules.iter_mut() {
            let Some(name) = module.name.clone() else { continue };
            let includes = if whole.contains(&name) {
                Vec::new()
            } else if let Some(items) = includes.get(&name) {
                items.clone()
            } else {
                continue;
            };
            module.design.config.scopes.insert(
                scope.clone(),
                ScopeEntry::Spec(ScopeSpec {
                    modules: Vec::new(),
                    includes,
                    excludes: excludes.get(&name).cloned().unwrap_or_default(),
                    deps: spec.deps,
                    extensions: spec.extensions.clone(),
                }),
            );
            selected.insert(name);
        }
        selected_by.insert(scope.clone(), selected);
    }
    Ok(selected_by)
}

/// The modules `module` selects: itself and every module it depends on,
/// directly or through another — or every module.
fn selection(manifest: &DesignConfig, module: Option<&str>) -> Result<HashSet<String>> {
    Ok(match module {
        None => manifest.modules.keys().cloned().collect(),
        Some(name) => {
            known_module(manifest, name)?;
            let mut wanted: HashSet<String> = dependency_closure(&manifest.modules).remove(name).unwrap_or_default();
            wanted.insert(name.to_string());
            wanted
        }
    })
}

/// Refuse a module `name` the manifest does not declare, naming those it does.
fn known_module(manifest: &DesignConfig, name: &str) -> Result<()> {
    if manifest.modules.contains_key(name) {
        return Ok(());
    }
    let declared: Vec<&str> = manifest.modules.keys().map(String::as_str).collect();
    Err(DbdError::Config(format!(
        "no module \"{name}\" — design.yaml's modules: are {}",
        declared.join(", ")
    )))
}

fn no_modules_to_name(name: &str) -> DbdError {
    DbdError::Config(format!(
        "no module \"{name}\" to select — design.yaml declares no modules:"
    ))
}

/// What the modules declare, and which module declares each — every module's
/// `known` and the index the T20 check reads.
struct Declared {
    /// Each entity a module's file declares, by the name a reference reaches
    /// it by, whatever its kind but a schema or an extension (see
    /// [`declares_nothing`]): a role is nobody's to own, but one module's file
    /// declares it all the same. In load order, so what each module knows of
    /// the others is the same on every load.
    by_name: IndexMap<String, (EntityType, Vec<String>)>,
    /// Each object a module's files create, as the registry keys it
    /// (`schema.name`, as Postgres names it): what a file creates beside the
    /// entity its path names — a type a table's file creates — is its
    /// module's too, as T19 knows. In load order.
    created: IndexMap<String, Vec<String>>,
}

/// Every module has the schemas and extensions it uses — the manifest's, and
/// those its entities live in — so one module's file declaring one is nothing
/// another module depends on it for.
fn declares_nothing(kind: EntityType) -> bool {
    matches!(kind, EntityType::Schema | EntityType::Extension | EntityType::External)
}

impl Declared {
    fn of(scanned: &[(String, Design)], objects: &[Vec<OwnedObject>]) -> Self {
        let mut by_name: IndexMap<String, (EntityType, Vec<String>)> = IndexMap::new();
        let mut created: IndexMap<String, Vec<String>> = IndexMap::new();
        for ((name, design), objects) in scanned.iter().zip(objects) {
            let files = design
                .entities
                .iter()
                .filter(|e| e.file.is_some() && !declares_nothing(e.entity_type));
            for entity in files {
                let (_, by) = by_name
                    .entry(entity.qualified_key())
                    .or_insert_with(|| (entity.entity_type, Vec::new()));
                if !by.contains(name) {
                    by.push(name.clone());
                }
            }
            for object in objects {
                let by = created.entry(format!("{}.{}", object.schema, object.name)).or_default();
                if !by.contains(name) {
                    by.push(name.clone());
                }
            }
        }
        Self { by_name, created }
    }

    /// What a module whose own names are `own` resolves its references
    /// against besides: every entity another module's file declares, and every
    /// object one creates beside it — by the name Postgres gives it, so a bare
    /// name on a file's search path reaches it. An object that is a declared
    /// entity by another spelling (`OrderStatus.ddl` creates `orderstatus`) is
    /// reached through the entity, as a folded name reaches one.
    fn known_to(&self, own: &HashSet<String>) -> Vec<(String, EntityType)> {
        let folded = |key: &String| key.to_ascii_lowercase();
        let entities: HashSet<String> = self.by_name.keys().chain(own).map(folded).collect();
        let named = self
            .by_name
            .iter()
            .filter(|(key, _)| !own.contains(*key))
            .map(|(key, (kind, _))| (key.clone(), *kind));
        let created = self
            .created
            .keys()
            .filter(|key| !entities.contains(&folded(key)))
            .map(|key| (key.clone(), EntityType::External));
        named.chain(created).collect()
    }

    /// The modules declaring the entity or creating the object called `name`.
    fn of_name(&self, name: &str) -> Option<&Vec<String>> {
        self.by_name
            .get(name)
            .map(|(_, by)| by)
            .or_else(|| self.created.get(name))
    }

    /// The modules declaring what `r` reaches: the entity or object it
    /// resolved to, else an object a module's file creates by the name it
    /// states. Never by a name folded here: the parser folds an unquoted name
    /// already, so a ref spelled `app.Status` is a quoted `"Status"` — not the
    /// `app.status` another module may create.
    fn of_ref(&self, r: &Ref) -> Option<&Vec<String>> {
        if r.unresolved {
            self.created.get(&r.name)
        } else {
            self.of_name(&r.name)
        }
    }
}

/// The module `name`'s design config: the manifest's, with the module's own
/// project identity, version, release state and hooks. Scopes select over
/// the whole workspace, not within a module, so a module has none; the
/// manifest's hooks are the workspace's, and its `export:` entries are each
/// the module's that declares the entity (see `Workspace::load`).
fn module_config(text: &str, project: &str, name: &str) -> Result<DesignConfig> {
    let mut config: DesignConfig = serde_yaml::from_str(text)?;
    let module = config
        .modules
        .shift_remove(name)
        .ok_or_else(|| DbdError::Config(format!("module \"{name}\" is not in modules:")))?;
    config.project = ProjectConfig {
        name: format!("{project}/{name}"),
        note: module.note,
        version: module.version,
        released: module.released,
    };
    config.apply = module.apply;
    // The manifest's `import.after` is the workspace's, as its `apply:` is:
    // it runs once, after the last module's import (see `Workspace::select`).
    config.import.after.clear();
    config.modules.clear();
    config.scopes.clear();
    Ok(config)
}

/// Each module's folder, once the `modules:` block is found to describe a
/// partition of the project — see [`Workspace::load`].
fn check_manifest(manifest: &DesignConfig, project_dir: &Path) -> Result<IndexMap<String, PathBuf>> {
    let modules = &manifest.modules;
    for name in modules.keys() {
        if !is_module_name(name) {
            return Err(DbdError::Config(format!(
                "module name \"{name}\" cannot name an owner (<project>/<module>) or a folder — use letters, \
                 digits, '_' and '-', in a name YAML reads as a string (not a number, `true`, `no` or `null`)"
            )));
        }
    }
    for (field, set) in [
        ("version", manifest.project.version.is_some()),
        ("released", manifest.project.released),
    ] {
        if set {
            return Err(DbdError::Config(format!(
                "project.{field} does not apply to a design with modules: — each module has its own; set \
                 modules.<name>.{field}"
            )));
        }
    }
    for (name, module) in modules {
        if let Some(missing) = module.depends_on.iter().find(|d| !modules.contains_key(*d)) {
            return Err(DbdError::Config(format!(
                "module \"{name}\" depends on \"{missing}\", which modules: does not declare"
            )));
        }
    }

    let mut folders: IndexMap<String, PathBuf> = IndexMap::new();
    for (name, module) in modules {
        let relative = module_folder(name, module)?;
        for (other, folder) in &folders {
            let said = if *folder == relative {
                format!(
                    "modules \"{other}\" and \"{name}\" share the folder {}",
                    relative.display()
                )
            } else if relative.starts_with(folder) || folder.starts_with(&relative) {
                let (inner, outer) = if relative.starts_with(folder) {
                    ((name, &relative), (other, folder))
                } else {
                    ((other, folder), (name, &relative))
                };
                format!(
                    "module \"{}\"'s folder {} is inside module \"{}\"'s folder {}",
                    inner.0,
                    inner.1.display(),
                    outer.0,
                    outer.1.display()
                )
            } else {
                continue;
            };
            return Err(DbdError::Config(format!(
                "{said} — each module has a folder of its own, which it takes with it if it moves to a \
                 repository of its own"
            )));
        }
        let dir = project_dir.join(&relative);
        if !dir.is_dir() {
            return Err(DbdError::Config(format!(
                "module \"{name}\": its folder {} does not exist",
                dir.display()
            )));
        }
        folders.insert(name.clone(), relative);
    }

    let module_dirs: Vec<PathBuf> = folders.values().map(|f| project_dir.join(f)).collect();
    for folder in MODULE_FOLDERS {
        if holds_files(&project_dir.join(folder), &module_dirs) {
            return Err(DbdError::Config(format!(
                "{folder}/ at the root holds files no module holds — design.yaml declares modules:, so each \
                 module's {folder}/ is in its folder; move them into the folder of the module they belong to"
            )));
        }
    }

    Ok(folders
        .into_iter()
        .map(|(name, relative)| (name, project_dir.join(relative)))
        .collect())
}

/// `[A-Za-z0-9_][A-Za-z0-9_-]*`, and read by YAML as a string: what can
/// follow `<project>/` in an owner, name a folder, and key its block,
/// unambiguously.
fn is_module_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        // Written as a key, YAML reads `2024`, `true` or `null` as other than
        // a string — and the module's block as another key's.
        && matches!(serde_yaml::from_str::<serde_yaml::Value>(name), Ok(serde_yaml::Value::String(_)))
        && !yaml_1_1_reads_otherwise(name)
}

/// YAML 1.1 readers — PyYAML among them — also read `NO`, `on`, `y`,
/// `1_000` and `0b101` as other than a string.
fn yaml_1_1_reads_otherwise(name: &str) -> bool {
    const KEYWORDS: [&str; 8] = ["y", "yes", "n", "no", "on", "off", "true", "false"];
    let keyword = KEYWORDS.contains(&name.to_ascii_lowercase().as_str());
    let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c == '_' || c.is_digit(radix));
    let number = digits(name, 10)
        || name.strip_prefix("0b").is_some_and(|s| digits(s, 2))
        || name.strip_prefix("0x").is_some_and(|s| digits(s, 16));
    keyword || number
}

/// The module's folder relative to the project, refusing one outside it and
/// the project's own.
fn module_folder(name: &str, module: &ModuleConfig) -> Result<PathBuf> {
    let Some(path) = module.path.as_deref() else {
        return Ok(PathBuf::from(name));
    };
    if path.trim_end_matches('/') == "." {
        return Err(DbdError::Config(format!(
            "module \"{name}\": path . is the project's own folder, which holds design.yaml and every other \
             module — give the module a folder of its own"
        )));
    }
    crate::path_safe::safe_relative_path(path).ok_or_else(|| {
        DbdError::Config(format!(
            "module \"{name}\": path {path} is outside the project — a module's folder is a relative path \
             inside it, without .."
        ))
    })
}

/// Whether `dir` holds a file anywhere beneath it that is no module's. An
/// empty folder holds nothing a module would lose, and neither does a dotfile —
/// the `.gitkeep` `dbd init` keeps an empty folder in git with — nor anything
/// under a dot folder; and what is in a module's folder (`module_dirs`) is
/// that module's, a module named `import` included.
fn holds_files(dir: &Path, module_dirs: &[PathBuf]) -> bool {
    let hidden = |e: &walkdir::DirEntry| e.depth() > 0 && e.file_name().to_string_lossy().starts_with('.');
    let a_module = |e: &walkdir::DirEntry| module_dirs.iter().any(|m| e.path() == m);
    dir.is_dir()
        && walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_entry(|e| !hidden(e) && !a_module(e))
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().is_file())
}

/// The modules in `depends_on` order: each after its dependencies — of those
/// whose dependencies are in place, the one `modules:` lists first. Refuses a
/// cycle, naming it.
fn depends_on_order(modules: &IndexMap<String, ModuleConfig>) -> Result<Vec<String>> {
    let mut order: Vec<String> = Vec::new();
    while order.len() < modules.len() {
        let placed = |m: &String| order.contains(m);
        let ready = modules
            .iter()
            .find(|(name, module)| !placed(name) && module.depends_on.iter().all(placed));
        match ready {
            Some((name, _)) => order.push(name.clone()),
            None => {
                let first = modules.keys().find(|m| !placed(m)).expect("a module is left to place");
                return Err(DbdError::Config(format!(
                    "modules depend on each other in a cycle: {} — a module's dependencies deploy before it, \
                     so one of them must not depend back",
                    cycle_from(first, modules, &order).join(" → ")
                )));
            }
        }
    }
    Ok(order)
}

/// A cycle among the modules not yet `placed`, followed from `start`: each
/// unplaced module waits on an unplaced dependency, so following those must
/// come back round.
fn cycle_from(start: &str, modules: &IndexMap<String, ModuleConfig>, placed: &[String]) -> Vec<String> {
    let mut path: Vec<String> = vec![start.to_string()];
    loop {
        let last = path.last().expect("the path starts with a module");
        let next = modules[last.as_str()]
            .depends_on
            .iter()
            .find(|d| !placed.contains(d))
            .expect("an unplaced module waits on an unplaced dependency");
        if let Some(at) = path.iter().position(|m| m == next) {
            let mut cycle = path.split_off(at);
            cycle.push(next.clone());
            return cycle;
        }
        path.push(next.clone());
    }
}

/// Every module each module reaches through `depends_on`, transitively.
/// Called once the block is known to have no cycle.
fn dependency_closure(modules: &IndexMap<String, ModuleConfig>) -> HashMap<String, HashSet<String>> {
    modules
        .keys()
        .map(|name| {
            let mut reached: HashSet<String> = HashSet::new();
            let mut stack: Vec<&String> = modules[name].depends_on.iter().collect();
            while let Some(dep) = stack.pop() {
                if reached.insert(dep.clone()) {
                    stack.extend(modules[dep].depends_on.iter());
                }
            }
            (name.clone(), reached)
        })
        .collect()
}

/// Refuse two modules declaring the same entity, as the registry would know
/// it (T19): a module is a partition, and the second module's claim would be
/// refused in the database after the first had deployed.
fn check_overlaps(scanned: &[(String, Design)], objects: &[Vec<OwnedObject>]) -> Result<()> {
    let mut declared: IndexMap<(String, String, String), (String, Vec<&str>)> = IndexMap::new();
    for ((name, design), objects) in scanned.iter().zip(objects) {
        // SQLite knows an entity by its name alone, compared without case — as
        // its adapter's `same_object` does.
        let sqlite = crate::parser::Dialect::from_label(design.dialect()) == Some(crate::parser::Dialect::Sqlite);
        for object in objects {
            let shown = format!("{} {}.{}", object.namespace, object.schema, object.name);
            let key = if sqlite {
                (
                    object.namespace.clone(),
                    String::new(),
                    object.name.to_ascii_lowercase(),
                )
            } else {
                (object.namespace.clone(), object.schema.clone(), object.name.clone())
            };
            let (_, by) = declared.entry(key).or_insert_with(|| (shown, Vec::new()));
            if !by.contains(&name.as_str()) {
                by.push(name);
            }
        }
    }
    let overlaps: Vec<String> = declared
        .values()
        .filter(|(_, by)| by.len() > 1)
        .map(|(shown, by)| {
            let by: Vec<String> = by.iter().map(|m| format!("\"{m}\"")).collect();
            format!("{shown}: modules {}", by.join(" and "))
        })
        .collect();
    if overlaps.is_empty() {
        return Ok(());
    }
    Err(DbdError::Config(format!(
        "{} declared by more than one module — an entity belongs to one module: keep it in one, and list that \
         module in the depends_on of the others that use it:\n  {}",
        if overlaps.len() == 1 {
            "1 entity is".to_string()
        } else {
            format!("{} entities are", overlaps.len())
        },
        overlaps.join("\n  ")
    )))
}

/// `dir` made absolute along the path a shell started there names it —
/// `$PWD`, when it is the folder dbd runs in — with `.` and `..` resolved by
/// name. `None` when there is no such folder.
fn logical_path(dir: &Path) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let base = std::env::var_os("PWD")
        .map(PathBuf::from)
        .filter(|pwd| pwd.canonicalize().ok() == cwd.canonicalize().ok())
        .unwrap_or(cwd);
    let mut path = PathBuf::new();
    for component in base.join(dir).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                path.pop();
            }
            other => path.push(other),
        }
    }
    path.is_dir().then_some(path)
}
