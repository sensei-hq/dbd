//! Each module's own history (#7 phase 3, slice 4): its version and release
//! state live in its `modules.<name>` block, and its snapshots and migrations
//! in its folder.

use dbd_core::config;
use dbd_core::design::Workspace;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const MANIFEST: &str = "project:\n  name: shop  # the shop\n\nschemas:\n  - app\n\n\
modules:\n  # reference data first\n  shared: {}\n  core:\n    depends_on: [shared]  # orders\n\n\
scopes:\n  default:\n    modules: [core]\n";

fn manifest(root: &Path) -> config::DesignConfig {
    config::read(&root.join("design.yaml")).expect("read")
}

#[test]
fn a_modules_version_is_written_in_its_block_keeping_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", MANIFEST);

    config::update_version_of(&root.join("design.yaml"), Some("core"), 2).expect("core");
    config::update_version_of(&root.join("design.yaml"), Some("shared"), 1).expect("shared");
    let read = manifest(root);
    assert_eq!(read.modules["core"].version, Some(2));
    assert_eq!(read.modules["core"].depends_on, ["shared"]);
    assert_eq!(read.modules["shared"].version, Some(1));
    assert_eq!(read.project.version, None, "the project's is not the modules'");
    assert_eq!(read.scopes.len(), 1);

    let text = std::fs::read_to_string(root.join("design.yaml")).unwrap();
    for kept in ["# the shop", "# reference data first", "# orders"] {
        assert!(text.contains(kept), "{kept}: {text}");
    }

    config::update_version_of(&root.join("design.yaml"), Some("core"), 3).expect("core again");
    assert_eq!(manifest(root).modules["core"].version, Some(3));
}

#[test]
fn a_modules_release_is_written_in_its_block() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", MANIFEST);
    config::set_released_of(&root.join("design.yaml"), Some("core"), true).expect("core");
    let read = manifest(root);
    assert!(read.modules["core"].released);
    assert!(!read.modules["shared"].released);
    assert!(!read.project.released);
}

/// A module block written in flow style — `core: { depends_on: [shared] }` —
/// is written all the same.
#[test]
fn a_flow_style_module_block_is_written_too() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n\nmodules:\n  shared: {}\n  core: { depends_on: [shared] }\n",
    );
    config::update_version_of(&root.join("design.yaml"), Some("core"), 4).expect("core");
    let read = manifest(root);
    assert_eq!(read.modules["core"].version, Some(4));
    assert_eq!(read.modules["core"].depends_on, ["shared"]);
}

/// A module's snapshot is in its folder, and its version in its block — and
/// the workspace loads after it.
#[test]
fn a_modules_snapshot_is_in_its_folder_and_versions_its_block() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "design.yaml", MANIFEST);
    write(
        root,
        "shared/ddl/table/app/currencies.ddl",
        "set search_path to app;\ncreate table if not exists currencies (code text primary key);\n",
    );
    write(
        root,
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\ncreate table if not exists orders (id integer primary key);\n",
    );
    let workspace = Workspace::load(&root.join("design.yaml"), "dev", Some(root)).expect("load");
    let core = workspace.modules().iter().find(|m| m.name() == Some("core")).unwrap();
    assert_eq!(core.design().module(), Some("core"));
    dbd_core::snapshot::create_snapshot_of(
        core.design().entities(),
        core.design().project_dir(),
        &root.join("design.yaml"),
        core.design().module(),
        "first",
    )
    .expect("snapshot");

    assert!(root.join("core/snapshots/001.json").exists());
    assert!(!root.join("snapshots").exists());
    assert_eq!(manifest(root).modules["core"].version, Some(1));
    Workspace::load(&root.join("design.yaml"), "dev", Some(root)).expect("the workspace still loads");
}
