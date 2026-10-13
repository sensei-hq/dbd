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
            let design = Design::scan(manifest, env, project_dir)?.resolved(&[]);
            return Ok(Self {
                modules: vec![Module {
                    name: None,
                    depends_on: Vec::new(),
                    design,
                }],
                manifest: None,
            });
        }

        let folders = check_manifest(&manifest, &project_dir)?;
        let order = depends_on_order(&manifest.modules)?;

        let mut scanned: Vec<(String, Design)> = Vec::new();
        for name in &order {
            let config = module_config(&text, &manifest.project.name, name)?;
            scanned.push((name.clone(), Design::scan(config, env, folders[name].clone())?));
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
            let own: HashSet<String> = design.entities.iter().map(|e| e.qualified_key()).collect();
            let known: Vec<(String, EntityType)> = declared
                .by_name
                .iter()
                .filter(|(key, _)| !own.contains(*key))
                .map(|(key, (kind, _))| (key.clone(), *kind))
                .collect();
            let mut design = design.resolved(&known);
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
                    || declared
                        .by_name
                        .get(&e.name)
                        .is_none_or(|(_, by)| by.iter().any(reaches))
            });
            modules.push(Module {
                depends_on: manifest.modules[&name].depends_on.clone(),
                name: Some(name),
                design,
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

        Ok(Self {
            modules,
            manifest: Some(manifest),
        })
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
}

/// What the modules declare, and which module declares each — every module's
/// `known` and the index the T20 check reads.
struct Declared {
    /// Each entity a module's file declares, by the name a reference reaches
    /// it by, whatever its kind: a role is nobody's to own, but one module's
    /// file declares it all the same. In load order, so what each module knows
    /// of the others is the same on every load.
    by_name: IndexMap<String, (EntityType, Vec<String>)>,
    /// Each object a module's files create, as the registry keys it
    /// (`schema.name`): what a file creates beside the entity its path names —
    /// a type a table's file creates — is its module's too, as T19 knows.
    created: HashMap<String, Vec<String>>,
}

impl Declared {
    fn of(scanned: &[(String, Design)], objects: &[Vec<OwnedObject>]) -> Self {
        let mut by_name: IndexMap<String, (EntityType, Vec<String>)> = IndexMap::new();
        let mut created: HashMap<String, Vec<String>> = HashMap::new();
        for ((name, design), objects) in scanned.iter().zip(objects) {
            let files = design
                .entities
                .iter()
                .filter(|e| e.file.is_some() && e.entity_type != EntityType::External);
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

    /// The modules declaring what `r` reaches: the entity it resolved to, else
    /// an object a module's file creates by that name — as written, or folded
    /// as Postgres folds an unquoted one.
    fn of_ref(&self, r: &Ref) -> Option<&Vec<String>> {
        (!r.unresolved)
            .then(|| self.by_name.get(&r.name).map(|(_, by)| by))
            .flatten()
            .or_else(|| self.created.get(&r.name))
            .or_else(|| self.created.get(&r.name.to_ascii_lowercase()))
    }
}

/// The module `name`'s design config: the manifest's, with the module's own
/// project identity, version, release state and hooks. Scopes select over
/// the whole workspace, not within a module, so a module has none.
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
                 digits, '_' and '-'"
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

    for folder in MODULE_FOLDERS {
        if holds_files(&project_dir.join(folder)) {
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

/// `[A-Za-z0-9_][A-Za-z0-9_-]*`: what can follow `<project>/` in an owner and
/// name a folder, unambiguously.
fn is_module_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

/// Whether `dir` holds a file anywhere beneath it. An empty folder holds
/// nothing a module would lose, and neither does a dotfile — the `.gitkeep`
/// `dbd init` keeps an empty folder in git with — nor anything under a dot
/// folder.
fn holds_files(dir: &Path) -> bool {
    let hidden = |e: &walkdir::DirEntry| e.depth() > 0 && e.file_name().to_string_lossy().starts_with('.');
    dir.is_dir()
        && walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_entry(|e| !hidden(e))
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
