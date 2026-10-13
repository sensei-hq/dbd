//! Loading a workspace (#7 phase 3, slice 1).
//!
//! A `design.yaml` with a `modules:` block is a workspace: each module is a
//! design of its own, owned by `<project>/<module>` and rooted at its folder,
//! loaded in `depends_on` order. One without `modules:` is a workspace of one
//! module — the project itself, as before. A workspace whose modules overlap
//! (T19) or reach past their `depends_on` (T20) does not load.

use dbd_core::design::Workspace;
use dbd_core::{Design, EntityType};
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn table(name: &str, columns: &str) -> String {
    format!("set search_path to app;\n\ncreate table if not exists {name} (\n  {columns}\n);\n")
}

/// `shop`: `core` (its orders reference `shared`'s currencies), declared before
/// the `shared` module it depends on.
fn shop(root: &Path) {
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\n\
         modules:\n  core:\n    depends_on: [shared]\n  shared:\n    note: Reference data\n",
    );
    write(root, "shared/ddl/table/app/currencies.ddl", &table("currencies", "code text primary key"));
    write(
        root,
        "core/ddl/table/app/orders.ddl",
        &table("orders", "id integer primary key\n, currency text references currencies (code)"),
    );
}

fn load(root: &Path) -> dbd_core::Result<Workspace> {
    Workspace::load(&root.join("design.yaml"), "dev", Some(root))
}

fn load_err(root: &Path) -> String {
    match load(root) {
        Ok(_) => panic!("the workspace must not load"),
        Err(e) => e.to_string(),
    }
}

fn kind_of(design: &Design, name: &str) -> Option<EntityType> {
    design.entities().iter().find(|e| e.name == name).map(|e| e.entity_type)
}

#[test]
fn a_design_without_modules_is_a_workspace_of_one_owned_by_its_project() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", "project:\n  name: shop\n\nschemas:\n  - app\n");
    write(root, "ddl/table/app/orders.ddl", &table("orders", "id integer primary key"));

    let workspace = load(root).expect("load");
    assert!(!workspace.has_modules());
    let [only] = workspace.modules() else {
        panic!("one module, the project itself")
    };
    assert_eq!(only.name(), None);
    assert_eq!(only.owner(), "shop");
    assert_eq!(only.depends_on(), &[] as &[String]);
    assert_eq!(only.design().project_dir(), root);
    assert_eq!(kind_of(only.design(), "app.orders"), Some(EntityType::Table));
}

#[test]
fn each_module_is_a_design_of_its_own_rooted_at_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);

    let workspace = load(root).expect("load");
    assert!(workspace.has_modules());
    let owners: Vec<&str> = workspace.modules().iter().map(|m| m.owner()).collect();
    assert_eq!(owners, ["shop/shared", "shop/core"], "a dependency loads before its dependents");

    let shared = &workspace.modules()[0];
    let core = &workspace.modules()[1];
    assert_eq!(shared.name(), Some("shared"));
    assert_eq!(core.name(), Some("core"));
    assert_eq!(core.depends_on(), ["shared".to_string()]);
    assert_eq!(shared.design().project_dir(), root.join("shared"));
    assert_eq!(core.design().project_dir(), root.join("core"));
    assert_eq!(core.design().config().project.name, "shop/core");

    assert_eq!(kind_of(shared.design(), "app.currencies"), Some(EntityType::Table));
    assert_eq!(kind_of(shared.design(), "app.orders"), None, "a module knows nothing of its dependents");
    assert_eq!(kind_of(core.design(), "app.orders"), Some(EntityType::Table));
    assert_eq!(
        kind_of(core.design(), "app.currencies"),
        Some(EntityType::External),
        "a dependency's entity is known to the module, and never its own"
    );
}

/// The point of `depends_on`: a module references what its dependencies
/// declare as it would its own, with no `external:` entry.
#[test]
fn a_dependencys_entities_resolve_without_external_declarations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);

    let workspace = load(root).expect("load");
    let core = workspace.modules().iter().find(|m| m.name() == Some("core")).unwrap();
    let orders = core
        .design()
        .entities()
        .iter()
        .find(|e| e.name == "app.orders")
        .unwrap();
    assert!(orders.warnings.is_empty(), "{:?}", orders.warnings);
    assert!(
        orders.refs.iter().any(|r| r.name == "app.currencies" && !r.unresolved),
        "{:?}",
        orders.refs
    );
}

#[test]
fn modules_load_in_depends_on_order_ties_in_declared_order() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: p\n\nmodules:\n  c:\n    depends_on: [a]\n  b:\n    depends_on: [a]\n  a: {}\n  d: {}\n",
    );
    for m in ["a", "b", "c", "d"] {
        std::fs::create_dir_all(root.join(m)).unwrap();
    }

    let workspace = load(root).expect("load");
    let names: Vec<&str> = workspace.modules().iter().filter_map(|m| m.name()).collect();
    assert_eq!(names, ["a", "c", "b", "d"]);
}

#[test]
fn a_module_path_defaults_to_its_name_and_may_be_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: p\n\nmodules:\n  shared: {}\n  core:\n    path: db/core\n",
    );
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::create_dir_all(root.join("db/core")).unwrap();

    let workspace = load(root).expect("load");
    let dirs: Vec<&Path> = workspace.modules().iter().map(|m| m.design().project_dir()).collect();
    assert_eq!(dirs, [root.join("shared"), root.join("db/core")]);
}

/// A module at `.` holds the root's `ddl/` — the folders a module holds are
/// its own wherever they are.
#[test]
fn a_module_may_be_rooted_at_the_project_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: p\n\nschemas:\n  - app\n\nmodules:\n  core:\n    path: .\n  extra: {}\n",
    );
    write(root, "ddl/table/app/orders.ddl", &table("orders", "id integer primary key"));
    std::fs::create_dir_all(root.join("extra")).unwrap();

    let workspace = load(root).expect("load");
    let core = &workspace.modules()[0];
    assert_eq!(core.design().project_dir(), root);
    assert_eq!(kind_of(core.design(), "app.orders"), Some(EntityType::Table));
}

/// Each module releases on its own: its version, its release state and its
/// apply hooks are in its block. The manifest's own `apply:` runs around the
/// whole workspace, not inside any module.
#[test]
fn a_modules_version_release_and_hooks_are_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: p\n\napply:\n  before: [root.sql]\n\n\
         modules:\n  shared: {}\n  core:\n    note: The app\n    version: 3\n    released: true\n    \
         apply:\n      before: [hooks/seed.sql]\n",
    );
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::create_dir_all(root.join("core")).unwrap();

    let workspace = load(root).expect("load");
    let [shared, core] = workspace.modules() else { panic!("two modules") };

    let core = core.design().config();
    assert_eq!(core.project.version, Some(3));
    assert!(core.project.released);
    assert_eq!(core.project.note.as_deref(), Some("The app"));
    let hooks: Vec<&str> = core.apply.before.iter().map(|s| s.script()).collect();
    assert_eq!(hooks, ["hooks/seed.sql"]);

    let shared = shared.design().config();
    assert_eq!(shared.project.version, None);
    assert!(!shared.project.released);
    assert!(shared.apply.before.is_empty(), "the manifest's hooks are not a module's");
}

#[test]
fn design_from_config_refuses_a_design_with_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);

    let err = match Design::from_config_with_dir(&root.join("design.yaml"), "dev", Some(root)) {
        Ok(_) => panic!("a workspace is not one design"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("modules:") && err.contains("Workspace::load"), "{err}");
}

// ── The modules: block, validated ───────────────────────────────────────────

/// `design.yaml` with `modules` as its block, and a folder for each module.
fn workspace_with(root: &Path, modules: &str, folders: &[&str]) {
    write(root, "design.yaml", &format!("project:\n  name: p\n\nmodules:\n{modules}"));
    for folder in folders {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
}

#[test]
fn an_unknown_dependency_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    workspace_with(tmp.path(), "  core:\n    depends_on: [nope]\n", &["core"]);
    let err = load_err(tmp.path());
    assert!(err.contains("\"core\"") && err.contains("\"nope\""), "{err}");
}

#[test]
fn a_dependency_cycle_is_refused_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    workspace_with(
        tmp.path(),
        "  free: {}\n  a:\n    depends_on: [b]\n  b:\n    depends_on: [a]\n",
        &["free", "a", "b"],
    );
    let err = load_err(tmp.path());
    assert!(err.contains("cycle") && err.contains("a → b → a"), "{err}");
}

#[test]
fn a_module_depending_on_itself_is_a_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    workspace_with(tmp.path(), "  a:\n    depends_on: [a]\n", &["a"]);
    let err = load_err(tmp.path());
    assert!(err.contains("cycle") && err.contains("a → a"), "{err}");
}

#[test]
fn a_module_path_outside_the_project_is_refused() {
    for path in ["../elsewhere", "/tmp", "core/../../x"] {
        let tmp = tempfile::tempdir().unwrap();
        workspace_with(tmp.path(), &format!("  core:\n    path: {path}\n"), &[]);
        let err = load_err(tmp.path());
        assert!(err.contains("\"core\"") && err.contains("outside"), "{path}: {err}");
    }
}

#[test]
fn a_module_without_its_folder_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    workspace_with(tmp.path(), "  core: {}\n", &[]);
    let err = load_err(tmp.path());
    assert!(err.contains("\"core\"") && err.contains("does not exist"), "{err}");
}

#[test]
fn two_modules_sharing_a_folder_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    workspace_with(
        tmp.path(),
        "  a:\n    path: db\n  b:\n    path: ./db\n",
        &["db"],
    );
    let err = load_err(tmp.path());
    assert!(err.contains("\"a\"") && err.contains("\"b\"") && err.contains("folder"), "{err}");
}

/// A module's name is half its owner's, `<project>/<module>`, and its default
/// folder: a separator in it would make both ambiguous.
#[test]
fn a_module_name_that_cannot_name_an_owner_is_refused() {
    for name in ["\"a/b\"", "\"..\"", "\"with space\""] {
        let tmp = tempfile::tempdir().unwrap();
        workspace_with(tmp.path(), &format!("  {name}: {{}}\n"), &[]);
        let err = load_err(tmp.path());
        assert!(err.contains("module name"), "{name}: {err}");
    }
}

/// A misspelt `depends_on` must not quietly leave a module without its
/// dependencies.
#[test]
fn an_unknown_key_in_a_module_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    workspace_with(tmp.path(), "  core:\n    dependson: [shared]\n  shared: {}\n", &["core", "shared"]);
    let err = load_err(tmp.path());
    assert!(err.contains("dependson"), "{err}");
}

/// In a workspace each module has its own version and release state; the
/// project's would be read by nothing.
#[test]
fn a_project_version_or_release_in_a_workspace_is_refused() {
    for key in ["version: 2", "released: true"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "design.yaml", &format!("project:\n  name: p\n  {key}\n\nmodules:\n  core: {{}}\n"));
        std::fs::create_dir_all(root.join("core")).unwrap();
        let err = load_err(root);
        let field = key.split(':').next().unwrap();
        assert!(
            err.contains(&format!("project.{field}")) && err.contains(&format!("modules.<name>.{field}")),
            "{key}: {err}"
        );
    }
}

/// Files left at the root after a project gains `modules:` belong to no module,
/// and would silently never deploy. (An empty folder holds nothing to lose.)
#[test]
fn a_root_folder_no_module_holds_is_refused() {
    for folder in ["ddl/table/app", "import", "policies", "snapshots", "migrations"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        workspace_with(root, "  core: {}\n", &["core"]);
        write(root, &format!("{folder}/file.sql"), "select 1;\n");
        let err = load_err(root);
        let top = folder.split('/').next().unwrap();
        assert!(err.contains(&format!("{top}/")) && err.contains("no module"), "{folder}: {err}");
    }
}

#[test]
fn an_empty_root_folder_is_no_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with(root, "  core: {}\n", &["core", "ddl/table", "import"]);
    load(root).expect("an empty folder holds nothing a module would lose");
}

// ── T19: modules never overlap ──────────────────────────────────────────────

#[test]
fn two_modules_declaring_the_same_entity_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    write(root, "core/ddl/table/app/currencies.ddl", &table("currencies", "code text primary key"));

    let err = load_err(root);
    assert!(
        err.contains("relation app.currencies") && err.contains("\"core\"") && err.contains("\"shared\""),
        "{err}"
    );
}

/// The same entity as the database sees it: a file creating `app.currencies`
/// under another file name overlaps all the same.
#[test]
fn an_entity_one_module_creates_from_a_file_of_another_name_overlaps_too() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    write(
        root,
        "core/ddl/table/app/rates.ddl",
        "set search_path to app;\n\ncreate table if not exists rates (id integer primary key);\n\
         create table if not exists currencies (code text primary key);\n",
    );

    let err = load_err(root);
    assert!(err.contains("relation app.currencies"), "{err}");
}

// ── T20: references follow depends_on ───────────────────────────────────────

#[test]
fn a_reference_into_a_module_that_is_not_a_dependency_is_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    // `core` loses its dependency on `shared`, and still references it.
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\nmodules:\n  core: {}\n  shared: {}\n",
    );

    let err = load_err(root);
    assert!(err.contains("dependency gap"), "{err}");
    assert!(err.contains("app.orders") && err.contains("app.currencies"), "{err}");
    assert!(err.contains("modules.core.depends_on") && err.contains("shared"), "{err}");
}

/// What a dependency depends on is deployed before the module too.
#[test]
fn a_reference_into_a_transitive_dependency_is_not_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: p\n\nschemas:\n  - app\n\n\
         modules:\n  base: {}\n  mid:\n    depends_on: [base]\n  top:\n    depends_on: [mid]\n",
    );
    write(root, "base/ddl/table/app/currencies.ddl", &table("currencies", "code text primary key"));
    std::fs::create_dir_all(root.join("mid")).unwrap();
    write(
        root,
        "top/ddl/table/app/orders.ddl",
        &table("orders", "id integer primary key\n, currency text references currencies (code)"),
    );

    load(root).expect("a dependency of a dependency is reachable");
}

/// A reference no module declares is what it always was: an unresolved
/// reference, warned about.
#[test]
fn a_reference_no_module_declares_stays_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    write(
        root,
        "core/ddl/table/app/orders.ddl",
        &table("orders", "id integer primary key\n, ghost_id integer references ghosts (id)"),
    );

    let workspace = load(root).expect("load");
    let core = &workspace.modules()[1];
    let orders = core
        .design()
        .entities()
        .iter()
        .find(|e| e.name == "app.orders")
        .unwrap();
    assert!(
        orders.warnings.iter().any(|w| w.contains("Unresolved reference: app.ghosts")),
        "{:?}",
        orders.warnings
    );
}
