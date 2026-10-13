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
    write(tmp.path(), "shared/ddl/table/app/rates.ddl", &table("rates"));
    let selected = scoped(tmp.path(), Some("dojo"));
    assert_eq!(tables_under(&selected[0], "dojo"), ["app.currencies", "app.rates"]);
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

/// A bare schema in `includes:` selects the modules with entities in it — not
/// every module that has the schema, as each has the manifest's `schemas:`.
#[test]
fn a_schema_item_selects_the_modules_with_entities_in_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root, "  rep:\n    includes: [reporting]\n");
    let manifest = std::fs::read_to_string(root.join("design.yaml")).unwrap();
    write(
        root,
        "design.yaml",
        &manifest.replace("  - app\n", "  - app\n  - reporting\n"),
    );
    write(
        root,
        "core/ddl/view/reporting/order_counts.ddl",
        "set search_path to reporting;\ncreate or replace view order_counts as select count(*) as n from app.orders;\n",
    );
    assert_eq!(owners(&scoped(root, Some("rep"))), ["shop/shared", "shop/core"]);
}

/// A scope leaving out of one module what another it selects requires is
/// refused when it is chosen — a gap, as in one design — and a run under
/// another scope is not held back by it.
#[test]
fn a_scope_excluding_what_another_module_needs_is_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(
        root,
        "  trim:\n    modules: [core]\n    excludes: [app.currencies]\n  dojo:\n    modules: [dojo]\n",
    );
    write(
        root,
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\ncreate table if not exists orders (\n  id integer primary key\n, \
         currency text references currencies (id)\n);\n",
    );
    let err = match load(root).unwrap().under_scope(Some("trim")) {
        Ok(_) => panic!("app.orders requires what trim leaves out"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("app.orders") && err.contains("app.currencies") && err.contains("trim"),
        "{err}"
    );
    assert_eq!(owners(&scoped(root, Some("dojo"))), ["shop/shared", "shop/dojo"]);
}

/// With no `--scope`, the `default` scope runs the module asked for when it
/// selects it; a module it leaves out runs on its own, with its dependencies —
/// as a run from that module's folder does.
#[test]
fn no_scope_runs_a_module_the_default_leaves_out_on_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    let selected = load(tmp.path())
        .unwrap()
        .under_scope_of(None, Some("dojo"))
        .unwrap()
        .select(Some("dojo"))
        .unwrap();
    assert_eq!(owners(&selected), ["shop/shared", "shop/dojo"]);
    let selected = load(tmp.path())
        .unwrap()
        .under_scope_of(None, Some("core"))
        .unwrap()
        .select(Some("core"))
        .unwrap();
    assert_eq!(owners(&selected), ["shop/shared", "shop/core"]);

    // Named, a scope still holds the module to it.
    assert!(
        load(tmp.path())
            .unwrap()
            .under_scope_of(Some("default"), Some("dojo"))
            .unwrap()
            .select(Some("dojo"))
            .is_err()
    );
}

/// `-n` on an entity of a module the scope leaves out says so.
#[test]
fn a_named_entity_outside_the_scope_names_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    let err = match load(tmp.path())
        .unwrap()
        .under_scope(None)
        .unwrap()
        .select_declaring(None, "app.belts")
    {
        Ok(_) => panic!("app.belts is dojo's, which default leaves out"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("\"dojo\"") && err.contains("default"), "{err}");
}

/// Run inside a module folder that is a link to the module's files, the
/// workspace is found along the path the run was started in.
#[cfg(unix)]
#[test]
fn a_linked_module_folder_finds_its_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", "project:\n  name: p\n\nmodules:\n  core: {}\n");
    std::fs::create_dir_all(elsewhere.path().join("core/ddl")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("core"), root.join("core")).unwrap();

    let found = Workspace::locate(&root.join("core"), Path::new("design.yaml"));
    assert_eq!(found, Some((root.to_path_buf(), Some("core".to_string()))));
}

/// A scope naming no module and including nothing selects every module —
/// less what it excludes.
#[test]
fn a_scope_with_only_excludes_selects_every_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  default:\n    excludes: [app.notes]\n");
    let selected = scoped(tmp.path(), None);
    assert_eq!(owners(&selected), ["shop/shared", "shop/core", "shop/dojo"]);
    assert_eq!(tables_under(&selected[1], "default"), ["app.orders"]);
}

/// Entities a scope includes run on their module's dependencies, whole.
#[test]
fn includes_alone_bring_their_modules_dependencies_whole() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  pick:\n    includes: [app.orders]\n");
    let selected = scoped(tmp.path(), Some("pick"));
    assert_eq!(owners(&selected), ["shop/shared", "shop/core"]);
    assert_eq!(tables_under(&selected[0], "pick"), ["app.currencies"]);
    assert_eq!(tables_under(&selected[1], "pick"), ["app.orders"]);
}

/// A scope's `deps:` is each module's: `include` takes in what an included
/// entity needs of its module.
#[test]
fn a_scopes_deps_policy_is_each_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    shop(root, "  pick:\n    includes: [app.notes]\n    deps: include\n");
    write(
        root,
        "core/ddl/table/app/notes.ddl",
        "set search_path to app;\ncreate table if not exists notes (\n  id integer primary key\n, \
         order_id integer references orders (id)\n);\n",
    );
    let selected = scoped(root, Some("pick"));
    assert_eq!(tables_under(&selected[1], "pick"), ["app.notes", "app.orders"]);
}

/// `export` and `reset` take a module within the scope too.
#[test]
fn export_and_reset_refuse_a_module_outside_the_scope() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), SCOPES);
    let narrowed = || load(tmp.path()).unwrap().under_scope(Some("dojo")).unwrap();
    assert!(narrowed().select_alone(Some("core")).is_err());
    assert!(narrowed().selected_to_reset(Some("core")).is_err());
}

/// A scope written `every: all` is every module, each resolving it as `all`.
#[test]
fn a_bare_all_scope_selects_and_resolves_in_every_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path(), "  every: all\n");
    let selected = scoped(tmp.path(), Some("every"));
    assert_eq!(owners(&selected), ["shop/shared", "shop/core", "shop/dojo"]);
    for module in &selected {
        let resolved = module.design().resolve_scope(Some("every"), None).expect("resolve");
        assert!(resolved.is_all, "{}", module.owner());
    }
}
