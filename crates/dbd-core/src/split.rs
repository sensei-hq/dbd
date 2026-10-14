//! `dbd split --from-scopes` (#7 phase 3, slice 6): a project divided into a
//! workspace by its scopes.
//!
//! Each region of the scopes' Venn diagram — over each scope's entities with
//! their dependencies — is a module: the region every scope holds is
//! `shared`, a region one scope holds is named for it (`default` becoming
//! `core`), one no scope holds `unscoped`. Each module depends on the modules
//! its entities reference, and each scope becomes the modules it holds. The
//! project's files move into the modules' folders: its DDL by entity, its
//! import data and policies by table.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::Design;
use crate::config::{DepsPolicy, ScopeEntry};
use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};

/// What `dbd split --from-scopes` would do: the modules, the scopes they
/// become, and the files that move.
#[derive(Debug, Clone)]
pub struct SplitPlan {
    /// The project's name: each module is `<project>/<module>`.
    pub project: String,
    pub modules: Vec<PlannedModule>,
    pub scopes: Vec<PlannedScope>,
    /// Each file's move, relative to the project folder.
    pub moves: Vec<FileMove>,
    /// The manifest's `import.after` scripts, each moved: `(from, to)`.
    pub script_paths: Vec<(String, String)>,
    /// A cycle the modules' dependencies would make, in order — `--write`
    /// is refused while there is one.
    pub cycle: Option<Vec<String>>,
}

/// One module the split makes.
#[derive(Debug, Clone)]
pub struct PlannedModule {
    pub name: String,
    /// The scopes its region is in, in the order design.yaml lists them.
    pub region: Vec<String>,
    /// The entities it holds, by name, sorted.
    pub entities: Vec<String>,
    /// The modules its entities reference, in the plan's order.
    pub depends_on: Vec<String>,
}

/// One scope, as the split rewrites it.
#[derive(Debug, Clone)]
pub struct PlannedScope {
    pub name: String,
    /// The modules it selects — `None` for an `all` scope, which stays `all`.
    pub modules: Option<Vec<String>>,
    /// What it left out of the manifest's own — a schema the scope did not
    /// deploy — which every module has: left out of each module it selects.
    pub excludes: Vec<String>,
    /// Its `extensions:`, kept.
    pub extensions: Option<Vec<String>>,
}

/// One file the split moves, relative to the project folder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What the split moves: each file, and of them the manifest's
/// `import.after` scripts, `(from, to)`, whose paths it rewrites.
struct Moves {
    files: Vec<FileMove>,
    scripts: Vec<(String, String)>,
}

/// Where the manifest's `import.after` scripts under `import/` go.
const SCRIPTS: &str = "scripts";

/// The plan for dividing `design` — a project, not a workspace — by its
/// scopes. `renames` renames the modules the plan names, `(module, name)`.
/// Refuses, before anything moves, what cannot divide: a project with
/// snapshots or migrations, one without scopes, a file that maps to no
/// module or to two, a module name that cannot name an owner, a module
/// folder that already holds files.
pub fn plan_from_scopes(design: &Design, renames: &[(String, String)]) -> Result<SplitPlan> {
    let root = design.project_dir();
    for folder in ["snapshots", "migrations"] {
        if holds_files(&root.join(folder)) {
            return Err(DbdError::Config(format!(
                "{folder}/ holds this project's history, and a migration is SQL over several tables, which \
                 does not divide by module — dbd split divides a project before its first release; split it \
                 by hand, or before releasing"
            )));
        }
    }
    let scope_names: Vec<String> = design
        .config()
        .scopes
        .iter()
        .filter(|(_, entry)| matches!(entry, ScopeEntry::Spec(_)))
        .map(|(name, _)| name.clone())
        .collect();
    if scope_names.is_empty() {
        return Err(DbdError::Config(
            "design.yaml has no scopes: to divide the project by — dbd split --from-scopes makes a module of \
             each region of its scopes"
                .to_string(),
        ));
    }

    // A file that does not parse has no references: what it needs — and so
    // its module — is unknown.
    let unparsed: Vec<String> = design
        .entities()
        .iter()
        .filter(|e| moves_with_a_module(e) && !e.errors.is_empty())
        .map(|e| format!("{} ({})", e.name, e.errors.join("; ")))
        .collect();
    if !unparsed.is_empty() {
        return Err(DbdError::Config(format!(
            "what these files reference — and so the module each is in — is unknown, as they do not parse; fix \
             them first:\n  {}",
            unparsed.join("\n  ")
        )));
    }
    // A schema only a file's path names is in only the modules whose files
    // name it, not the manifest's every module has.
    let declared: HashSet<String> = design.config().schemas.iter().map(|s| s.name()).collect();
    let mut path_schemas: Vec<String> = design
        .entities()
        .iter()
        .filter(|e| e.entity_type == EntityType::Schema && e.file.is_none() && !declared.contains(&e.name))
        .map(|e| e.name.clone())
        .collect();
    path_schemas.sort();
    if !path_schemas.is_empty() {
        return Err(DbdError::Config(format!(
            "schema {} is named only by files' paths — add it to schemas: in design.yaml first: every module has \
             the manifest's schemas, and a schema only some files name would be in only their modules",
            path_schemas.join(", ")
        )));
    }

    // Each scope's entities with their dependencies — and a scope that leaves
    // out what they need refused: as modules, it would deploy that too.
    let mut sets: Vec<HashSet<String>> = Vec::new();
    for name in &scope_names {
        let resolved = design.resolve_scope(Some(name), Some(DepsPolicy::Include))?;
        let set = design
            .working_set(&resolved)
            .map_err(|e| DbdError::Config(format!("scope {name}: {e}")))?;
        let own = design.working_set(&design.resolve_scope(Some(name), None)?)?;
        let scopable = |n: &&String| {
            design
                .entities()
                .iter()
                .any(|e| &e.name == *n && crate::scope::is_scopable(e))
        };
        let mut left_out: Vec<&String> = set.difference(&own).filter(scopable).collect();
        if !left_out.is_empty() {
            left_out.sort();
            return Err(DbdError::Config(format!(
                "scope {name} leaves out {}, which what it deploys needs — as modules it would deploy them; \
                 include them in the scope, or give it deps: include, first",
                left_out.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )));
        }
        sets.push(set);
    }
    // An earlier split's leftovers: a folder holding what a module's does.
    if let Some(leftover) = leftovers(root)?.first() {
        return Err(DbdError::Config(format!(
            "{leftover} holds files as a module's folder does — an earlier split's, left half-done? Move them \
             back, or out of it, first"
        )));
    }

    // Each entity's region, by the scopes holding it; what every scope
    // deploys — a role — in all of them.
    let all: Vec<usize> = (0..scope_names.len()).collect();
    let mut regions: Vec<Vec<usize>> = Vec::new();
    let mut entity_region: HashMap<String, usize> = HashMap::new();
    let mut members: Vec<Vec<&Entity>> = Vec::new();
    for e in design.entities().iter().filter(|e| moves_with_a_module(e)) {
        let region: Vec<usize> = if crate::scope::is_scopable(e) {
            all.iter().copied().filter(|&i| sets[i].contains(&e.name)).collect()
        } else {
            all.clone()
        };
        let index = match regions.iter().position(|r| *r == region) {
            Some(i) => i,
            None => {
                regions.push(region);
                members.push(Vec::new());
                regions.len() - 1
            }
        };
        members[index].push(e);
        entity_region.insert(e.name.clone(), index);
    }

    // The plan's order: shared, each scope's own, those of several, unscoped.
    let mut order: Vec<usize> = (0..regions.len()).collect();
    let rank = |r: &Vec<usize>| -> (usize, Vec<usize>) {
        let class = if r.len() == all.len() && all.len() > 1 {
            0
        } else if r.len() == 1 {
            1
        } else if r.is_empty() {
            3
        } else {
            2
        };
        (class, r.clone())
    };
    order.sort_by_key(|&i| rank(&regions[i]));
    if order.is_empty() {
        return Err(DbdError::Config(
            "the project declares no entity a module would hold — no module to make".to_string(),
        ));
    }

    let mut names: Vec<String> = order.iter().map(|&i| region_name(&regions[i], &scope_names)).collect();
    for (from, to) in renames {
        let Some(at) = names.iter().position(|n| n == from) else {
            return Err(DbdError::Config(format!(
                "--rename {from}={to}: the plan has no module {from} — its modules are {}",
                names.join(", ")
            )));
        };
        names[at] = to.clone();
    }
    for (i, name) in names.iter().enumerate() {
        if !crate::design::is_module_name(name) {
            return Err(DbdError::Config(format!(
                "module \"{name}\" cannot name an owner (<project>/<module>) or a folder — give it another name \
                 with --rename {name}=<name>"
            )));
        }
        if names[..i].contains(name) {
            return Err(DbdError::Config(format!(
                "two regions would both be module \"{name}\" — give one another name with --rename"
            )));
        }
        if let Some(other) = names[..i].iter().find(|n| n.eq_ignore_ascii_case(name)) {
            return Err(DbdError::Config(format!(
                "modules \"{other}\" and \"{name}\" differ only in case, and on a case-insensitive filesystem \
                 their folders are one — give one another name with --rename"
            )));
        }
        // A module name is `[A-Za-z0-9_][A-Za-z0-9_-]*` (checked above): a folder of the project's.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        if holds_files(&root.join(name)) {
            return Err(DbdError::Config(format!(
                "{name}/ already holds files — module {name}'s folder would be it; move them, or give the \
                 module another name with --rename {name}=<name>"
            )));
        }
    }
    // Region index → its module's position in the plan.
    let position: HashMap<usize, usize> = order.iter().enumerate().map(|(at, &i)| (i, at)).collect();
    let module_of = |entity: &str| entity_region.get(entity).map(|i| position[i]);

    // What each file creates — a type beside its table included — is its
    // entity's module's, as the workspace's gap check reads it (T20).
    let mut created: HashMap<String, usize> = HashMap::new();
    for (e, keys) in design.entity_keys() {
        if let Some(at) = module_of(&e.name) {
            for k in keys {
                created.insert(format!("{}.{}", k.schema, k.name), at);
            }
        }
    }
    let target_of = |name: &str| {
        module_of(name)
            .or_else(|| created.get(name).copied())
            .or_else(|| created.get(&name.to_ascii_lowercase()).copied())
    };
    // What each module's entities reference.
    let mut depends: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); order.len()];
    for (region, entities) in members.iter().enumerate() {
        let at = position[&region];
        for e in entities {
            for r in &e.refs {
                if let Some(target) = target_of(&r.name)
                    && target != at
                {
                    depends[at].insert(target);
                }
            }
        }
    }
    let cycle = find_cycle(&depends).map(|c| c.into_iter().map(|i| names[i].clone()).collect());

    let modules: Vec<PlannedModule> = order
        .iter()
        .enumerate()
        .map(|(at, &i)| {
            let mut entities: Vec<String> = members[i].iter().map(|e| e.name.clone()).collect();
            entities.sort();
            entities.dedup();
            PlannedModule {
                name: names[at].clone(),
                region: regions[i].iter().map(|&s| scope_names[s].clone()).collect(),
                entities,
                depends_on: depends[at].iter().map(|&d| names[d].clone()).collect(),
            }
        })
        .collect();

    let scopes = rewritten_scopes(design, &scope_names, &sets, &modules, &depends);
    let moves = file_moves(design, &|entity| module_of(entity).map(|at| names[at].clone()))?;

    Ok(SplitPlan {
        project: design.config().project.name.clone(),
        modules,
        scopes,
        moves: moves.files,
        script_paths: moves.scripts,
        cycle,
    })
}

/// Whether `e` is an entity a module holds: one of the project's files
/// declares it. Schemas and extensions are the manifest's, every module's.
fn moves_with_a_module(e: &Entity) -> bool {
    e.file.is_some() && !matches!(e.entity_type, EntityType::External | EntityType::Import)
}

/// A region's module name: `shared` for the one every scope (of two or more)
/// holds, a scope's own name for the one only it holds — `default` as
/// `core` — the names of several joined with `-`, `unscoped` for none.
fn region_name(region: &[usize], scopes: &[String]) -> String {
    let name = |i: &usize| {
        if scopes[*i] == "default" {
            "core".to_string()
        } else {
            scopes[*i].clone()
        }
    };
    match region {
        [] => "unscoped".to_string(),
        r if r.len() == scopes.len() && scopes.len() > 1 => "shared".to_string(),
        r => r.iter().map(name).collect::<Vec<_>>().join("-"),
    }
}

/// A cycle among `depends`, in order, if there is one.
fn find_cycle(depends: &[BTreeSet<usize>]) -> Option<Vec<usize>> {
    fn visit(
        at: usize,
        depends: &[BTreeSet<usize>],
        path: &mut Vec<usize>,
        done: &mut HashSet<usize>,
    ) -> Option<Vec<usize>> {
        if let Some(start) = path.iter().position(|&p| p == at) {
            let mut cycle = path[start..].to_vec();
            cycle.push(at);
            return Some(cycle);
        }
        if !done.insert(at) {
            return None;
        }
        path.push(at);
        for &next in &depends[at] {
            if let Some(cycle) = visit(next, depends, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }
    let mut done = HashSet::new();
    (0..depends.len()).find_map(|at| visit(at, depends, &mut Vec::new(), &mut done))
}

/// Each scope as the modules it holds — less those another of them depends
/// on, which it takes along — keeping its `extensions:`; `all` as it was.
fn rewritten_scopes(
    design: &Design,
    scope_names: &[String],
    sets: &[HashSet<String>],
    modules: &[PlannedModule],
    depends: &[BTreeSet<usize>],
) -> Vec<PlannedScope> {
    let reach = |from: usize| -> HashSet<usize> {
        let mut seen = HashSet::new();
        let mut stack: Vec<usize> = depends[from].iter().copied().collect();
        while let Some(at) = stack.pop() {
            if seen.insert(at) {
                stack.extend(depends[at].iter().copied());
            }
        }
        seen
    };
    design
        .config()
        .scopes
        .iter()
        .map(|(name, entry)| match entry {
            ScopeEntry::All(_) => PlannedScope {
                name: name.clone(),
                modules: None,
                excludes: Vec::new(),
                extensions: None,
            },
            ScopeEntry::Spec(spec) => {
                let holding: Vec<usize> = (0..modules.len())
                    .filter(|&m| modules[m].region.contains(name))
                    .collect();
                let taken_along: HashSet<usize> = holding.iter().flat_map(|&m| reach(m)).collect();
                let selected = holding
                    .into_iter()
                    .filter(|m| !taken_along.contains(m))
                    .map(|m| modules[m].name.clone())
                    .collect();
                // The manifest's own — its schemas — are every module's: what
                // the scope did not deploy of them, it leaves out.
                let set = scope_names.iter().position(|s| s == name).map(|i| &sets[i]);
                let mut excludes: Vec<String> = design
                    .entities()
                    .iter()
                    .filter(|e| e.file.is_none() && e.entity_type == EntityType::Schema)
                    .filter(|e| set.is_some_and(|set| !set.contains(&e.name)))
                    .map(|e| e.name.clone())
                    .collect();
                excludes.sort();
                PlannedScope {
                    name: name.clone(),
                    modules: Some(selected),
                    excludes,
                    extensions: spec.extensions.clone(),
                }
            }
        })
        .collect()
}

/// The moves of every file of `ddl/`, `import/` and `policies/`, each to the
/// folder of the module owning its entity or table — the manifest's
/// `import.after` scripts to `scripts/` — refusing, named, a file that maps
/// to no module or to two, two files planned onto one path, and a path
/// already taken.
fn file_moves(design: &Design, module_of: &dyn Fn(&str) -> Option<String>) -> Result<Moves> {
    let root = design.project_dir();
    // A scanned file's path starts with the project folder already.
    let relative = |path: &Path| -> PathBuf {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.to_path_buf();
        }
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        absolute.strip_prefix(root).map(Path::to_path_buf).unwrap_or(absolute)
    };
    let dotted = |file: &Path| file.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
    let mut refusals: Vec<String> = Vec::new();
    let mut moves: Vec<FileMove> = Vec::new();

    // DDL, by the entities each file declares — every one of them, a
    // dotfile the scanner reads included.
    let mut by_file: HashMap<PathBuf, BTreeSet<String>> = HashMap::new();
    for e in design.entities().iter().filter(|e| moves_with_a_module(e)) {
        let file = relative(e.file.as_ref().expect("a file"));
        if let Some(module) = module_of(&e.name) {
            by_file.entry(file).or_default().insert(module);
        }
    }
    let walked = files_under(root, "ddl")?;
    for file in &walked {
        match by_file.get(file).map(|m| m.iter().collect::<Vec<_>>()).as_deref() {
            Some([module]) => moves.push(FileMove {
                to: Path::new(&module).join(file),
                from: file.clone(),
            }),
            Some(several) => refusals.push(format!(
                "{} declares entities of modules {} — divide it into a file per entity first",
                file.display(),
                several.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(" and ")
            )),
            // A dotfile no entity is declared in — `.gitkeep` — stays.
            None if dotted(file) => {}
            None => refusals.push(format!(
                "{} declares no entity of the project — move it out of ddl/ first",
                file.display()
            )),
        }
    }
    let mut unwalked: Vec<&PathBuf> = by_file.keys().filter(|f| !walked.contains(f)).collect();
    unwalked.sort();
    for file in unwalked {
        refusals.push(format!(
            "{} declares an entity of the project and is not under ddl/ — move it there first",
            file.display()
        ));
    }

    // Import data, by the table each file loads; the manifest's scripts out
    // of import/.
    let scripts: HashSet<String> = design
        .config()
        .import
        .after
        .iter()
        .map(|s| s.script().trim_start_matches("./").to_string())
        .collect();
    let mut script_paths = Vec::new();
    for file in files_under(root, "import")? {
        let shown = file.to_string_lossy().to_string();
        let is_data = file
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| crate::scanner::IMPORT_EXTENSIONS.contains(&x));
        if !is_data {
            if scripts.contains(&shown) {
                let to = Path::new(SCRIPTS).join(file.strip_prefix("import").unwrap_or(&file));
                script_paths.push((shown, to.to_string_lossy().to_string()));
                moves.push(FileMove { from: file, to });
            } else if !dotted(&file) {
                refusals.push(format!(
                    "{shown} is no data file and no import.after script — move it out of import/ first"
                ));
            }
            continue;
        }
        let folders = file.parent().map(|p| p.components().count()).unwrap_or(0);
        let table = Entity::from_import_file(&file).name;
        match module_of(&table) {
            Some(module) if folders <= 3 => moves.push(FileMove {
                to: Path::new(&module).join(&file),
                from: file,
            }),
            _ => refusals.push(format!(
                "{shown} loads {table}, which no module would hold — move or delete it first"
            )),
        }
    }

    // Policies, by their table.
    for file in files_under(root, "policies")? {
        let shown = file.to_string_lossy().to_string();
        let is_policy = file
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x == "sql" || x == "ddl");
        if !is_policy && dotted(&file) {
            continue;
        }
        match crate::design::policy_target(&root.join(&file), root).and_then(|table| module_of(&table)) {
            Some(module) => moves.push(FileMove {
                to: Path::new(&module).join(&file),
                from: file,
            }),
            None => refusals.push(format!(
                "{shown} is the policy of no table a module would hold — move or delete it first"
            )),
        }
    }

    // No two files onto one path, nor onto one taken.
    let mut onto: HashMap<&Path, Vec<&Path>> = HashMap::new();
    for m in &moves {
        onto.entry(m.to.as_path()).or_default().push(m.from.as_path());
    }
    let mut collisions: Vec<String> = onto
        .iter()
        .filter(|(_, from)| from.len() > 1)
        .map(|(to, from)| {
            let mut from: Vec<String> = from.iter().map(|f| f.display().to_string()).collect();
            from.sort();
            format!(
                "{} would all move to {} — rename one first",
                from.join(" and "),
                to.display()
            )
        })
        .collect();
    collisions.sort();
    refusals.extend(collisions);
    for m in &moves {
        // Every move is of a file found under the project's own folders, into a
        // module's folder of it, or `scripts/`.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        if std::fs::symlink_metadata(root.join(&m.to)).is_ok() {
            refusals.push(format!(
                "{} is where {} would move, and is taken",
                m.to.display(),
                m.from.display()
            ));
        }
    }

    if !refusals.is_empty() {
        return Err(DbdError::Config(format!(
            "the project does not divide by its scopes as it is:\n  {}",
            refusals.join("\n  ")
        )));
    }
    moves.sort();
    Ok(Moves {
        files: moves,
        scripts: script_paths,
    })
}

/// Every file under `root/folder`, relative to `root`, sorted — a dotfile
/// included, as the scanners read one — and a folder it cannot read refused:
/// walked past, its files stayed at the root.
fn files_under(root: &Path, folder: &str) -> Result<Vec<PathBuf>> {
    let dir = root.join(folder);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&dir) {
        let entry = entry.map_err(|e| DbdError::Config(format!("{folder}/ cannot be read whole: {e}")))?;
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(root)
        {
            files.push(rel.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

/// The folders at the project's root, other than its own, holding a `ddl/`,
/// `import/` or `policies/` with files in — as only a module's does.
fn leftovers(root: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(found);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let own = ["ddl", "import", "policies", "snapshots", "migrations"].contains(&name.as_str());
        if own || name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        for folder in ["ddl", "import", "policies"] {
            if holds_files(&entry.path().join(folder)) {
                found.push(format!("{name}/{folder}/"));
                break;
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Whether `dir` holds a file, dotfiles aside.
fn holds_files(dir: &Path) -> bool {
    dir.is_dir()
        && walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_entry(|e| !(e.depth() > 0 && e.file_name().to_string_lossy().starts_with('.')))
            .filter_map(|e| e.ok())
            .any(|e| e.file_type().is_file())
}

/// Carry `plan` out, whole or not at all: check first, in a scratch copy of
/// what the split would make, that the workspace loads and deploys under
/// each scope what the project does; write the new manifest aside; move the
/// files — with `git mv` in a git work tree — each recorded, and on any
/// failure put each back; then put the new manifest in place. Refused while
/// the plan's dependencies cycle.
pub fn write_split(plan: &SplitPlan, config_path: &Path, project_dir: &Path) -> Result<()> {
    if let Some(cycle) = &plan.cycle {
        return Err(DbdError::Config(format!(
            "the modules' dependencies would cycle — {} — move entities so they run one way first",
            cycle.join(" → ")
        )));
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let text = std::fs::read_to_string(config_path)
        .map_err(|e| DbdError::Config(format!("Cannot read {}: {e}", config_path.display())))?;
    let written = rewrite_manifest(&text, plan)?;
    check_in_scratch(plan, config_path, project_dir, &written)?;

    // The new manifest, aside: a split that cannot write it moves nothing.
    let aside = config_path.with_file_name(format!(
        "{}.dbd-split",
        config_path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
    ));
    write_new(&aside, &written)?;

    let git = in_git_work_tree(project_dir);
    let mut done: Vec<&FileMove> = Vec::new();
    for m in &plan.moves {
        if let Err(e) = move_one(project_dir, m, git) {
            let undo = put_back(project_dir, &done, git);
            let _ = std::fs::remove_file(&aside);
            return Err(DbdError::Config(format!(
                "{e} — {}",
                undo_said(&undo, done.len(), "design.yaml is as it was")
            )));
        }
        done.push(m);
    }
    if let Err(e) = std::fs::rename(&aside, config_path) {
        let undo = put_back(project_dir, &done, git);
        let _ = std::fs::remove_file(&aside);
        return Err(DbdError::Config(format!(
            "design.yaml could not be replaced: {e} — {}",
            undo_said(&undo, done.len(), "design.yaml is as it was")
        )));
    }
    if let Err(e) = crate::design::Workspace::load(config_path, "dev", Some(project_dir)) {
        let restored = write_new(&aside, &text).and_then(|()| {
            std::fs::rename(&aside, config_path).map_err(|e| DbdError::Config(format!("restore design.yaml: {e}")))
        });
        let undo = put_back(project_dir, &done, git);
        let manifest = match restored {
            Ok(()) => "design.yaml is as it was".to_string(),
            Err(e) => format!("design.yaml could not be put back ({e})"),
        };
        return Err(DbdError::Config(format!(
            "the workspace does not load: {e} — {}",
            undo_said(&undo, done.len(), &manifest)
        )));
    }
    Ok(())
}

/// `text` written to `path`, which must not exist yet.
fn write_new(path: &Path, text: &str) -> Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| DbdError::Config(format!("write the new design.yaml beside it, {}: {e}", path.display())))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            let _ = std::fs::remove_file(path);
            DbdError::Config(format!("write the new design.yaml beside it, {}: {e}", path.display()))
        })
}

/// Move one file — `git mv` for one git tracks, refused rather than moved
/// behind git's back when git will not — onto a path that must be free.
fn move_one(project_dir: &Path, m: &FileMove, git: bool) -> Result<()> {
    // Every move is of a file found under the project's own folders, into a
    // module's folder of it, or `scripts/`.
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let to = project_dir.join(&m.to);
    if std::fs::symlink_metadata(&to).is_ok() {
        return Err(DbdError::Config(format!(
            "move {} to {}: it is taken",
            m.from.display(),
            m.to.display()
        )));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| DbdError::Config(format!("move {}: create {}: {e}", m.from.display(), parent.display())))?;
    }
    if git && tracked(project_dir, &m.from) {
        return git_mv(project_dir, &m.from, &m.to)
            .map_err(|e| DbdError::Config(format!("git mv {} {}: {e}", m.from.display(), m.to.display())));
    }
    std::fs::rename(project_dir.join(&m.from), &to)
        .map_err(|e| DbdError::Config(format!("move {} to {}: {e}", m.from.display(), m.to.display())))
}

/// Each of `done` put back, newest first — what could not be, said.
fn put_back(project_dir: &Path, done: &[&FileMove], git: bool) -> Vec<String> {
    let mut failed = Vec::new();
    for m in done.iter().rev() {
        let back = FileMove {
            from: m.to.clone(),
            to: m.from.clone(),
        };
        if let Err(e) = move_one(project_dir, &back, git) {
            failed.push(e.to_string());
        }
    }
    failed
}

/// What a failed split says of what it put back.
fn undo_said(failed: &[String], moved: usize, manifest: &str) -> String {
    if failed.is_empty() {
        format!("every move before it ({moved}) was put back, and {manifest}")
    } else {
        format!(
            "of the {moved} moves before it, these could not be put back, and {manifest}:\n  {}",
            failed.join("\n  ")
        )
    }
}

/// What the split would make, made in a scratch folder — its manifest, each
/// file moved (import data as an empty file: only its name is read) — then
/// loaded: refused unless the workspace loads and deploys under each scope,
/// and with none given, what the project does.
fn check_in_scratch(plan: &SplitPlan, config_path: &Path, project_dir: &Path, written: &str) -> Result<()> {
    let scratch = std::env::temp_dir().join(format!(
        "dbd-split-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let outcome = (|| -> Result<()> {
        let fail = |what: &str, e: std::io::Error| DbdError::Config(format!("the split's check: {what}: {e}"));
        std::fs::create_dir_all(&scratch).map_err(|e| fail("create a scratch folder", e))?;
        let manifest = scratch.join(config_path.file_name().unwrap_or_default());
        std::fs::write(&manifest, written).map_err(|e| fail("write its design.yaml", e))?;
        for m in &plan.moves {
            let to = scratch.join(&m.to);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| fail("create a folder", e))?;
            }
            let data = m.from.starts_with("import")
                && m.from
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| crate::scanner::IMPORT_EXTENSIONS.contains(&x));
            if data {
                std::fs::write(&to, "").map_err(|e| fail("write an import file", e))?;
            } else {
                std::fs::copy(project_dir.join(&m.from), &to).map_err(|e| fail("copy a file", e))?;
            }
        }
        let before = crate::Design::from_config_with_dir(config_path, "dev", Some(project_dir))?;
        let scopes: Vec<Option<&str>> = std::iter::once(None)
            .chain(plan.scopes.iter().map(|s| Some(s.name.as_str())))
            .collect();
        let mut differences = Vec::new();
        for scope in scopes {
            let resolved = before.resolve_scope(scope, None)?;
            let was: BTreeSet<String> = before
                .scoped_entities(&resolved)?
                .into_iter()
                .filter(|e| e.entity_type != EntityType::External)
                .map(|e| e.name)
                .collect();
            let reading = crate::design::Workspace::load(&manifest, "dev", Some(&scratch))
                .map_err(|e| DbdError::Config(format!("the workspace would not load: {e}")))?
                .reading(None, scope, None)?;
            let now: BTreeSet<String> = reading
                .entities()?
                .into_iter()
                .filter(|e| e.entity_type != EntityType::External)
                .map(|e| e.name)
                .collect();
            if was != now {
                let named = scope.map_or("with no --scope".to_string(), |s| format!("scope {s}"));
                let gone: Vec<&String> = was.difference(&now).collect();
                let new: Vec<&String> = now.difference(&was).collect();
                differences.push(format!(
                    "{named} would deploy{}{}",
                    if new.is_empty() {
                        String::new()
                    } else {
                        format!(" {} it does not now", list(&new))
                    },
                    if gone.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "{} not {}, which it does now",
                            if new.is_empty() { "" } else { ", and" },
                            list(&gone)
                        )
                    }
                ));
            }
        }
        if differences.is_empty() {
            Ok(())
        } else {
            Err(DbdError::Config(format!(
                "as a workspace, the project would not deploy what it does:\n  {}",
                differences.join("\n  ")
            )))
        }
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    outcome
}

fn list(names: &[&String]) -> String {
    names.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")
}

/// Whether `dir` is inside a git work tree.
fn in_git_work_tree(dir: &Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(dir)
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// Whether git tracks `file` in `dir`'s work tree.
fn tracked(dir: &Path, file: &Path) -> bool {
    std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(file)
        .current_dir(dir)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `git mv from to` in `dir` — git's own words, when it will not.
fn git_mv(dir: &Path, from: &Path, to: &Path) -> std::result::Result<(), String> {
    let out = std::process::Command::new("git")
        .args(["mv", "--"])
        .arg(from)
        .arg(to)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The manifest `text` with `modules:` and the plan's `scopes:` where its
/// `scopes:` block was, and its `import.after` scripts' new paths — the rest
/// of it as written, comments and line endings included. Checked: it must
/// read as the manifest did, but for what the split changes, and as a
/// design.yaml.
pub fn rewrite_manifest(text: &str, plan: &SplitPlan) -> Result<String> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut edited = text.to_string();
    for (from, to) in &plan.script_paths {
        edited = edited.replace(from.as_str(), to.as_str());
    }
    let lines: Vec<&str> = edited.lines().collect();
    let top_level = |line: &str| line.chars().next().is_some_and(|c| !c.is_whitespace() && c != '#');
    let start = lines
        .iter()
        .position(|l| {
            l.strip_prefix("scopes:")
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '#', '{']))
        })
        .ok_or_else(|| DbdError::Config("design.yaml has no top-level scopes: block".to_string()))?;
    let end = (start + 1..lines.len())
        .find(|&i| top_level(lines[i]))
        .unwrap_or(lines.len());
    // Comments and blank lines just before the next key are its own.
    let mut block_end = end;
    while block_end > start + 1
        && (lines[block_end - 1].trim().is_empty() || lines[block_end - 1].trim_start().starts_with('#'))
    {
        block_end -= 1;
    }

    let blocks = format!("{}\n\n{}", render_modules(plan), render_scopes(plan));
    let mut out: Vec<String> = lines[..start].iter().map(|l| l.to_string()).collect();
    out.extend(blocks.lines().map(str::to_string));
    if block_end < lines.len() && !lines[block_end].trim().is_empty() {
        out.push(String::new());
    }
    out.extend(lines[block_end..].iter().map(|l| l.to_string()));
    let mut written = out.join(eol);
    if text.ends_with('\n') && !written.ends_with('\n') {
        written.push_str(eol);
    }

    // It reads as the manifest did, but for what the split changes.
    let mut expected: serde_yaml::Value = serde_yaml::from_str(text)?;
    let got: serde_yaml::Value = serde_yaml::from_str(&written)?;
    if let Some(map) = expected.as_mapping_mut() {
        let wanted: serde_yaml::Value = serde_yaml::from_str(&blocks)?;
        for key in ["modules", "scopes"] {
            map.remove(key);
            if let Some(v) = wanted.get(key) {
                map.insert(serde_yaml::Value::String(key.to_string()), v.clone());
            }
        }
        if let Some(after) = map
            .get_mut("import")
            .and_then(|i| i.get_mut("after"))
            .and_then(|a| a.as_sequence_mut())
        {
            let moved = |s: &mut String| {
                let (prefix, path) = match s.strip_prefix("./") {
                    Some(path) => ("./", path.to_string()),
                    None => ("", s.clone()),
                };
                if let Some((_, to)) = plan.script_paths.iter().find(|(from, _)| *from == path) {
                    *s = format!("{prefix}{to}");
                }
            };
            for entry in after.iter_mut() {
                match entry {
                    serde_yaml::Value::String(s) => moved(s),
                    serde_yaml::Value::Mapping(m) => {
                        if let Some(serde_yaml::Value::String(s)) = m.get_mut("script") {
                            moved(s);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let as_json = |v: &serde_yaml::Value| serde_json::to_value(v).ok();
    if as_json(&got) != as_json(&expected) {
        return Err(DbdError::Config(
            "design.yaml could not be edited in place — its scopes: block, or a path the split rewrites, is \
             written in a way the split does not edit; write modules: and scopes: by hand, as the plan shows"
                .to_string(),
        ));
    }
    serde_yaml::from_str::<crate::config::DesignConfig>(&written)
        .map_err(|e| DbdError::Config(format!("the design.yaml the split would write does not read: {e}")))?;
    Ok(written)
}

/// The plan's `modules:` block.
fn render_modules(plan: &SplitPlan) -> String {
    let mut out = String::from("modules:");
    for m in &plan.modules {
        if m.depends_on.is_empty() {
            out.push_str(&format!("\n  {}: {{}}", m.name));
        } else {
            out.push_str(&format!(
                "\n  {}:\n    depends_on: [{}]",
                m.name,
                yaml_list(&m.depends_on)
            ));
        }
    }
    out
}

/// The plan's `scopes:` block.
fn render_scopes(plan: &SplitPlan) -> String {
    let mut out = String::from("scopes:");
    for s in &plan.scopes {
        match &s.modules {
            None => out.push_str(&format!("\n  {}: all", yaml_scalar(&s.name))),
            Some(modules) => {
                out.push_str(&format!(
                    "\n  {}:\n    modules: [{}]",
                    yaml_scalar(&s.name),
                    yaml_list(modules)
                ));
                if !s.excludes.is_empty() {
                    out.push_str(&format!("\n    excludes: [{}]", yaml_list(&s.excludes)));
                }
                if let Some(extensions) = &s.extensions {
                    out.push_str(&format!("\n    extensions: [{}]", yaml_list(extensions)));
                }
            }
        }
    }
    out
}

fn yaml_list(items: &[String]) -> String {
    items.iter().map(|i| yaml_scalar(i)).collect::<Vec<_>>().join(", ")
}

/// A name as a YAML scalar — quoted unless YAML reads it as itself, in a
/// flow list as in a key: `2024`, `true` and `a: b` are quoted.
fn yaml_scalar(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.contains([':', '#', ',', '[', ']', '{', '}', '"', '\''])
        && matches!(serde_yaml::from_str::<serde_yaml::Value>(name), Ok(serde_yaml::Value::String(s)) if s == name);
    if plain { name.to_string() } else { format!("{name:?}") }
}
