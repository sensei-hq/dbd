//! Selecting a workspace's modules for a run (#7 phase 3, slice 2).
//!
//! A run goes over the modules in `depends_on` order — all of them, or one
//! and the modules it depends on — and a reset over them in reverse, or over
//! exactly the one named. The manifest's own `apply:` hooks run once around
//! the selection: before the first module's, after the last's.

use dbd_core::design::{Module, Workspace};
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `shop`: `shared`, then `core` and `dojo`, each depending on `shared`; the
/// manifest's hooks at the root, `shared` and `core` with hooks of their own.
fn shop(root: &Path) {
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\n\
         apply:\n  before: [hooks/ws-before.sql]\n  after: [hooks/ws-after.sql]\n\n\
         modules:\n  shared:\n    apply:\n      before: [hooks/shared-before.sql]\n  \
         core:\n    depends_on: [shared]\n    apply:\n      after: [hooks/core-after.sql]\n  \
         dojo:\n    depends_on: [shared]\n",
    );
    for hook in [
        "hooks/ws-before.sql",
        "hooks/ws-after.sql",
        "shared/hooks/shared-before.sql",
        "core/hooks/core-after.sql",
    ] {
        write(root, hook, "select 1;\n");
    }
    for (module, table) in [("shared", "currencies"), ("core", "orders"), ("dojo", "belts")] {
        write(
            root,
            &format!("{module}/ddl/table/app/{table}.ddl"),
            &format!("set search_path to app;\ncreate table if not exists {table} (id integer primary key);\n"),
        );
    }
}

fn load(root: &Path) -> Workspace {
    Workspace::load(&root.join("design.yaml"), "dev", Some(root)).expect("load")
}

fn owners(modules: &[Module]) -> Vec<&str> {
    modules.iter().map(|m| m.owner()).collect()
}

fn hooks(module: &Module) -> Vec<String> {
    module.design().apply_hooks_to_run(None).expect("hooks")
}

#[test]
fn a_run_selects_every_module_in_depends_on_order() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let selected = load(tmp.path()).select(None).expect("select");
    assert_eq!(owners(&selected), ["shop/shared", "shop/core", "shop/dojo"]);
}

#[test]
fn a_module_selects_itself_and_the_modules_it_depends_on() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let selected = load(tmp.path()).select(Some("dojo")).expect("select");
    assert_eq!(owners(&selected), ["shop/shared", "shop/dojo"]);
    let selected = load(tmp.path()).select(Some("shared")).expect("select");
    assert_eq!(owners(&selected), ["shop/shared"]);
}

#[test]
fn an_unknown_module_is_refused_naming_the_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let err = match load(tmp.path()).select(Some("nope")) {
        Ok(_) => panic!("there is no module nope"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("\"nope\"") && err.contains("shared, core, dojo"), "{err}");
}

#[test]
fn a_design_without_modules_selects_itself_and_has_no_module_to_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", "project:\n  name: shop\n");
    let selected = load(root).select(None).expect("select");
    assert_eq!(owners(&selected), ["shop"]);
    let selected = load(root).selected_to_reset(None).expect("select");
    assert_eq!(owners(&selected), ["shop"]);

    for err in [
        load(root).select(Some("core")).err(),
        load(root).selected_to_reset(Some("core")).err(),
    ] {
        let err = err.expect("no module to name").to_string();
        assert!(err.contains("modules:"), "{err}");
    }
}

/// A reset drops each module before what it depends on — a module's
/// dependents are other owners, which the uses guard protects — and resets
/// exactly the module named, never its dependencies.
#[test]
fn a_reset_selects_every_module_in_reverse_or_exactly_the_one_named() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let selected = load(tmp.path()).selected_to_reset(None).expect("select");
    assert_eq!(owners(&selected), ["shop/dojo", "shop/core", "shop/shared"]);
    let selected = load(tmp.path()).selected_to_reset(Some("core")).expect("select");
    assert_eq!(owners(&selected), ["shop/core"]);

    let err = match load(tmp.path()).selected_to_reset(Some("nope")) {
        Ok(_) => panic!("there is no module nope"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("\"nope\""), "{err}");
}

/// The manifest's hooks run once around the run: its `apply.before` before the
/// first module's own, its `apply.after` after the last module's — read from
/// the project folder, not a module's.
#[test]
fn the_manifests_hooks_run_around_the_selected_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let selected = load(tmp.path()).select(None).expect("select");
    let [shared, core, dojo] = &selected[..] else {
        panic!("three modules")
    };
    assert_eq!(
        hooks(shared),
        [
            "apply.before hooks/ws-before.sql",
            "apply.before hooks/shared-before.sql"
        ]
    );
    assert_eq!(hooks(core), ["apply.after hooks/core-after.sql"]);
    assert_eq!(hooks(dojo), ["apply.after hooks/ws-after.sql"]);

    let selected = load(tmp.path()).select(Some("core")).expect("select");
    let [shared, core] = &selected[..] else {
        panic!("two modules")
    };
    assert_eq!(
        hooks(shared),
        [
            "apply.before hooks/ws-before.sql",
            "apply.before hooks/shared-before.sql"
        ]
    );
    assert_eq!(
        hooks(core),
        ["apply.after hooks/core-after.sql", "apply.after hooks/ws-after.sql"]
    );
}

/// A design without modules runs its own hooks once, as it always did.
#[test]
fn a_design_without_modules_runs_its_hooks_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n\napply:\n  before: [hooks/b.sql]\n",
    );
    write(root, "hooks/b.sql", "select 1;\n");
    let selected = load(root).select(None).expect("select");
    assert_eq!(hooks(&selected[0]), ["apply.before hooks/b.sql"]);
}

/// `-n`: the module declaring the entity, alone — with the manifest's hooks
/// around it, as around any run.
#[test]
fn a_named_entity_selects_its_module_with_the_manifests_hooks_around_it() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let selected = load(tmp.path()).select_declaring(None, "app.orders").expect("select");
    let [core] = &selected[..] else { panic!("one module") };
    assert_eq!(core.owner(), "shop/core");
    assert_eq!(
        hooks(core),
        [
            "apply.before hooks/ws-before.sql",
            "apply.after hooks/core-after.sql",
            "apply.after hooks/ws-after.sql"
        ]
    );

    let err = match load(tmp.path()).select_declaring(None, "app.nothing") {
        Ok(_) => panic!("no module declares it"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("app.nothing"), "{err}");
}

/// A module `--module` leaves out is named, not taken for missing.
#[test]
fn a_named_entity_outside_the_selected_module_names_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let err = match load(tmp.path()).select_declaring(Some("shared"), "app.orders") {
        Ok(_) => panic!("app.orders is core's"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("app.orders") && err.contains("\"core\"") && err.contains("shared"),
        "{err}"
    );
}

/// A module's dependencies are selected through their own: `top` takes `mid`,
/// and `mid` takes `base`.
#[test]
fn a_dependency_of_a_dependency_is_selected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: p\n\nmodules:\n  top:\n    depends_on: [mid]\n  mid:\n    depends_on: [base]\n  base: {}\n",
    );
    for m in ["top", "mid", "base"] {
        std::fs::create_dir_all(root.join(m)).unwrap();
    }
    let selected = load(root).select(Some("top")).expect("select");
    assert_eq!(owners(&selected), ["p/base", "p/mid", "p/top"]);
}

/// The manifest's `import.after` is the workspace's, as its `apply:` is: it
/// runs once, after the last module's import, read from the project folder —
/// not once per module, from each module's.
#[test]
fn the_manifests_import_after_runs_once_after_the_last_module() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    let manifest = std::fs::read_to_string(root.join("design.yaml")).unwrap();
    write(
        root,
        "design.yaml",
        &format!("{manifest}\nimport:\n  after: [sql/loader.sql]\n"),
    );
    write(root, "sql/loader.sql", "select 1;\n");

    let selected = load(root).select(None).expect("select");
    let after: Vec<Vec<String>> = selected
        .iter()
        .map(|m| m.design().import_after_preview(None).expect("preview").0)
        .collect();
    assert_eq!(after, [vec![], vec![], vec!["sql/loader.sql".to_string()]]);
    for module in &selected {
        assert!(module.design().config().import.after.is_empty(), "{}", module.owner());
    }
}

/// Each `export:` entry is the module's that declares its entity; one no
/// module declares stays with the first, which warns about it once.
#[test]
fn each_export_entry_is_the_modules_that_declares_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root);
    let manifest = std::fs::read_to_string(root.join("design.yaml")).unwrap();
    write(
        root,
        "design.yaml",
        &format!("{manifest}\nexport:\n  - app.orders\n  - app.currencies\n  - app.ghost\n"),
    );

    let workspace = load(root);
    let exports: Vec<Vec<String>> = workspace
        .modules()
        .iter()
        .map(|m| m.design().config().export.iter().map(|e| e.name()).collect())
        .collect();
    assert_eq!(
        exports,
        [
            vec!["app.currencies".to_string(), "app.ghost".to_string()],
            vec!["app.orders".to_string()],
            vec![],
        ]
    );
}
