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
use crate::config::{DesignConfig, ModuleConfig, ProjectConfig};
use crate::entity::EntityType;
use crate::error::{DbdError, Result};

/// The folders a module's design reads and writes, each under its own folder.
const MODULE_FOLDERS: &[&str] = &["ddl", "import", "policies", "snapshots", "migrations"];

/// A manifest and its modules, in `depends_on` order: each after the modules
/// it depends on, ties in the order `modules:` lists them.
pub struct Workspace {
    modules: Vec<Module>,
    has_modules: bool,
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
    /// unknown dependency or a cycle, a folder outside the project, missing or
    /// shared, a module name that cannot name an owner, `project.version` or
    /// `project.released` (each module has its own), files at the root no
    /// module holds — and then two modules declaring the same entity (T19), or
    /// a module referencing one it does not depend on, directly or through its
    /// dependencies (T20). All before any database is involved.
    ///
    /// Each module knows what its dependencies declare without `external:`
    /// entries: their entities resolve its references, and are never its own.
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
                has_modules: false,
            });
        }

        let folders = check_manifest(&manifest, &project_dir)?;
        let order = depends_on_order(&manifest.modules)?;

        let mut scanned: Vec<(String, Design)> = Vec::new();
        for name in &order {
            let config = module_config(&text, &manifest.project.name, name)?;
            scanned.push((name.clone(), Design::scan(config, env, folders[name].clone())?));
        }
        check_overlaps(&scanned)?;

        // What each module declares, by the name a reference reaches it by.
        let declared_by: HashMap<String, String> = scanned
            .iter()
            .flat_map(|(name, design)| {
                design
                    .entities
                    .iter()
                    .filter(|e| super::ownership::owned_kind(e.entity_type).is_some())
                    .map(move |e| (e.qualified_key(), name.clone()))
            })
            .collect();
        let reach = dependency_closure(&manifest.modules);

        // Resolved against everything every other module declares, so that a
        // reference past `depends_on` is found as one rather than reported as
        // unresolved — then left knowing only what it may reach.
        let mut modules = Vec::new();
        let mut gaps: Vec<String> = Vec::new();
        for (name, design) in scanned {
            let known: Vec<String> = declared_by
                .iter()
                .filter(|(_, owner)| **owner != name)
                .map(|(key, _)| key.clone())
                .collect();
            let mut design = design.resolved(&known);
            let reaches = |module: &String| *module == name || reach[&name].contains(module);
            for entity in design.entities.iter().filter(|e| e.entity_type != EntityType::External) {
                for r in entity.refs.iter().filter(|r| !r.unresolved) {
                    if let Some(other) = declared_by.get(&r.name).filter(|m| !reaches(m)) {
                        let gap = format!(
                            "✗ dependency gap: {} ({name}) requires {} ({other}) — add {other} to \
                             modules.{name}.depends_on",
                            entity.name, r.name
                        );
                        if !gaps.contains(&gap) {
                            gaps.push(gap);
                        }
                    }
                }
            }
            design
                .entities
                .retain(|e| e.entity_type != EntityType::External || declared_by.get(&e.name).is_none_or(reaches));
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
            has_modules: true,
        })
    }

    /// Whether the manifest declares `modules:` — `false` for a project that
    /// is a workspace of one.
    pub fn has_modules(&self) -> bool {
        self.has_modules
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
        if let Some((other, _)) = folders.iter().find(|(_, f)| **f == relative) {
            return Err(DbdError::Config(format!(
                "modules \"{other}\" and \"{name}\" share the folder {} — each module has a folder of its own",
                shown(&relative)
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

    // A module rooted at the project folder holds the root's folders.
    if !folders.values().any(|f| f.as_os_str().is_empty()) {
        for folder in MODULE_FOLDERS {
            if holds_files(&project_dir.join(folder)) {
                return Err(DbdError::Config(format!(
                    "{folder}/ at the root holds files no module holds — design.yaml declares modules:, so \
                     each module's {folder}/ is in its folder; move them into a module's folder, or root a \
                     module at the project (path: .)"
                )));
            }
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

/// The module's folder relative to the project — empty for the project folder
/// itself — refusing one outside it.
fn module_folder(name: &str, module: &ModuleConfig) -> Result<PathBuf> {
    let Some(path) = module.path.as_deref() else {
        return Ok(PathBuf::from(name));
    };
    if path.trim_end_matches('/') == "." {
        return Ok(PathBuf::new());
    }
    crate::path_safe::safe_relative_path(path).ok_or_else(|| {
        DbdError::Config(format!(
            "module \"{name}\": path {path} is outside the project — a module's folder is a relative path \
             inside it, without .."
        ))
    })
}

fn shown(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.display().to_string()
    }
}

/// Whether `dir` holds a file anywhere beneath it. An empty folder holds
/// nothing a module would lose.
fn holds_files(dir: &Path) -> bool {
    dir.is_dir()
        && walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().is_file())
}

/// The modules in `depends_on` order: each after its dependencies, ties in
/// declared order. Refuses a cycle, naming it.
fn depends_on_order(modules: &IndexMap<String, ModuleConfig>) -> Result<Vec<String>> {
    fn visit(
        name: &str,
        modules: &IndexMap<String, ModuleConfig>,
        path: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if order.iter().any(|o| o == name) {
            return Ok(());
        }
        if let Some(start) = path.iter().position(|p| p == name) {
            let mut cycle = path[start..].to_vec();
            cycle.push(name.to_string());
            return Err(DbdError::Config(format!(
                "modules depend on each other in a cycle: {} — a module's dependencies deploy before it, so \
                 one of them must not depend back",
                cycle.join(" → ")
            )));
        }
        path.push(name.to_string());
        for dep in &modules[name].depends_on {
            visit(dep, modules, path, order)?;
        }
        path.pop();
        order.push(name.to_string());
        Ok(())
    }

    let mut order = Vec::new();
    for name in modules.keys() {
        visit(name, modules, &mut Vec::new(), &mut order)?;
    }
    Ok(order)
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
fn check_overlaps(scanned: &[(String, Design)]) -> Result<()> {
    let mut declared: IndexMap<(String, String, String), (String, Vec<&str>)> = IndexMap::new();
    for (name, design) in scanned {
        // SQLite knows an entity by its name alone, compared without case — as
        // its adapter's `same_object` does.
        let sqlite = design.dialect() == "sqlite";
        for object in design.declared_objects() {
            let shown = format!("{} {}.{}", object.namespace, object.schema, object.name);
            let key = if sqlite {
                (object.namespace, String::new(), object.name.to_ascii_lowercase())
            } else {
                (object.namespace, object.schema, object.name)
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
