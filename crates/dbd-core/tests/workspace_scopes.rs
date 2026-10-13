//! Scopes over a workspace's modules (#7 phase 3, slice 3).
//!
//! In a workspace a scope selects modules — `modules: [dojo]` is dojo and
//! the modules it depends on, whole — and can still select entities:
//! `includes:` takes entities of a module the scope does not select whole,
//! `excludes:` leaves entities of a selected one out. Each selected module
//! resolves the scope as its own, under the same name, so a run, its scope
//! pin and its gaps work per module unchanged.

use dbd_core::design::{Module, Workspace};
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn table(name: &str) -> String {
    format!("set search_path to app;\ncreate table if not exists {name} (id integer primary key);\n")
}

/// `shop`: `shared` (currencies), `core` (orders, notes) and `dojo` (belts),
/// `core` and `dojo` each depending on `shared`; `scopes` as given.
fn shop(root: &Path, scopes: &str) {
    write(
        root,
        "design.yaml",
        &format!(
            "project:\n  name: shop\n\nschemas:\n  - app\n\nmodules:\n  shared: {{}}\n  \
             core:\n    depends_on: [shared]\n  dojo:\n    depends_on: [shared]\n\nscopes:\n{scopes}"
        ),
    );
    write(root, "shared/ddl/table/app/currencies.ddl", &table("currencies"));
    write(root, "core/ddl/table/app/orders.ddl", &table("orders"));
    write(root, "core/ddl/table/app/notes.ddl", &table("notes"));
    write(root, "dojo/ddl/table/app/belts.ddl", &table("belts"));
}

fn load(root: &Path) -> dbd_core::Result<Workspace> {
    Workspace::load(&root.join("design.yaml"), "dev", Some(root))
}

fn owners(modules: &[Module]) -> Vec<&str> {
    modules.iter().map(|m| m.owner()).collect()
}

fn scoped(root: &Path, scope: Option<&str>) -> Vec<Module> {
    load(root)
        .expect("load")
        .under_scope(scope)
        .expect("scope")
        .select(None)
        .expect("select")
}

/// The tables `module` builds under `scope`, sorted.
fn tables_under(module: &Module, scope: &str) -> Vec<String> {
    let design = module.design();
    let resolved = design.resolve_scope(Some(scope), None).expect("resolve");
    assert_eq!(resolved.name, scope);
    let mut tables: Vec<String> = design
        .scoped_entities(&resolved)
        .expect("entities")
        .into_iter()
        .filter(|e| e.entity_type == dbd_core::EntityType::Table)
        .map(|e| e.name)
        .collect();
    tables.sort();
    tables
}

const SCOPES: &str = "  dojo:\n    modules: [dojo]\n  default:\n    modules: [core]\n";

#[test]
fn a_scope_selects_its_modules_and_their_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    assert_eq!(owners(&scoped(tmp.path(), Some("dojo"))), ["shop/shared", "shop/dojo"]);
    assert_eq!(
        owners(&scoped(tmp.path(), Some("all"))),
        ["shop/shared", "shop/core", "shop/dojo"]
    );
}

/// No `--scope` is the `default` scope, as in a design without modules.
#[test]
fn no_scope_is_the_default_scope() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    assert_eq!(owners(&scoped(tmp.path(), None)), ["shop/shared", "shop/core"]);

    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  dojo:\n    modules: [dojo]\n");
    assert_eq!(
        owners(&scoped(tmp.path(), None)),
        ["shop/shared", "shop/core", "shop/dojo"],
        "with no default scope, every module"
    );
}

/// Each module resolves the workspace's scope as its own: a module the scope
/// selects, a dependency included, builds all of itself.
#[test]
fn each_selected_module_resolves_the_scope_whole() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    let selected = scoped(tmp.path(), Some("dojo"));
    assert_eq!(tables_under(&selected[0], "dojo"), ["app.currencies"]);
    assert_eq!(tables_under(&selected[1], "dojo"), ["app.belts"]);
}

/// `includes:` takes entities of a module the scope does not select whole —
/// with that module's dependencies, whole, as `--module` takes them.
#[test]
fn a_scope_includes_entities_of_a_module_it_does_not_select_whole() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  pick:\n    modules: [dojo]\n    includes: [app.orders]\n");
    let selected = scoped(tmp.path(), Some("pick"));
    assert_eq!(owners(&selected), ["shop/shared", "shop/core", "shop/dojo"]);
    assert_eq!(tables_under(&selected[0], "pick"), ["app.currencies"]);
    assert_eq!(tables_under(&selected[1], "pick"), ["app.orders"]);
    assert_eq!(tables_under(&selected[2], "pick"), ["app.belts"]);
}

/// `excludes:` leaves entities of a selected module out.
#[test]
fn a_scope_excludes_entities_of_the_modules_it_selects() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  trim:\n    modules: [core]\n    excludes: [app.notes]\n");
    let selected = scoped(tmp.path(), Some("trim"));
    assert_eq!(owners(&selected), ["shop/shared", "shop/core"]);
    assert_eq!(tables_under(&selected[1], "trim"), ["app.orders"]);
}

/// A scope's `extensions:` allowlist is each module's.
#[test]
fn a_scopes_extension_allowlist_is_each_modules() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  lean:\n    modules: [dojo]\n    extensions: []\n");
    for module in scoped(tmp.path(), Some("lean")) {
        let resolved = module.design().resolve_scope(Some("lean"), None).expect("resolve");
        assert_eq!(resolved.extensions, Some(Default::default()), "{}", module.owner());
    }
}

/// `--module` within a scope: one of the modules it selects.
#[test]
fn a_module_outside_the_scope_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    let err = match load(tmp.path())
        .unwrap()
        .under_scope(Some("dojo"))
        .unwrap()
        .select(Some("core"))
    {
        Ok(_) => panic!("core is not in scope dojo"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("\"core\"") && err.contains("dojo"), "{err}");
    let selected = load(tmp.path())
        .unwrap()
        .under_scope(Some("dojo"))
        .unwrap()
        .select(Some("shared"))
        .unwrap();
    assert_eq!(owners(&selected), ["shop/shared"]);
}

#[test]
fn an_unknown_scope_is_refused_naming_the_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    let err = match load(tmp.path()).unwrap().under_scope(Some("nope")) {
        Ok(_) => panic!("there is no scope nope"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("nope") && err.contains("dojo") && err.contains("default"),
        "{err}"
    );
}

// ── Refused as the workspace loads ──────────────────────────────────────────

fn load_err(root: &Path) -> String {
    match load(root) {
        Ok(_) => panic!("the workspace must not load"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_scope_naming_an_unknown_module_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  odd:\n    modules: [nope]\n");
    let err = load_err(tmp.path());
    assert!(err.contains("odd") && err.contains("\"nope\""), "{err}");
}

#[test]
fn a_scope_item_no_module_declares_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  odd:\n    includes: [app.ghost]\n");
    let err = load_err(tmp.path());
    assert!(err.contains("odd") && err.contains("app.ghost"), "{err}");
}

/// `modules:` selects the modules of a workspace; a design without them has
/// none to select.
#[test]
fn a_scope_selecting_modules_in_a_design_without_modules_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: plain\n\nschemas:\n  - app\n\nscopes:\n  odd:\n    modules: [core]\n",
    );
    write(root, "ddl/table/app/orders.ddl", &table("orders"));
    let err = load_err(root);
    assert!(err.contains("odd") && err.contains("modules:"), "{err}");
    let err = match dbd_core::Design::from_config_with_dir(&root.join("design.yaml"), "dev", Some(root)) {
        Ok(_) => panic!("refused"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("odd") && err.contains("modules:"), "{err}");
}
