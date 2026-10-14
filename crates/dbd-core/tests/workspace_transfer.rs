//! The workspace transfer rule without a database (#7 phase 3, slice 6):
//! what an adapter that keeps no registry, or keeps one the old way, does
//! with a module that takes over from its project's other owners.

use dbd_core::adapter::mock::MockAdapter;

fn write(root: &std::path::Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// An adapter written before the transfer rule implements `claim_objects`
/// alone; a module of a two-module workspace takes over from two owners — the
/// project as it was and the other module — and still runs on it.
#[tokio::test]
async fn an_adapter_written_before_runs_a_two_module_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\nmodules:\n  shared: {}\n  core:\n    depends_on: [shared]\n",
    );
    write(
        root,
        "shared/ddl/table/app/currencies.ddl",
        "set search_path to app;\ncreate table if not exists currencies (code text primary key);\n",
    );
    write(
        root,
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\ncreate table if not exists orders (id int primary key, currency text references currencies (code));\n",
    );
    let modules = dbd_core::design::Workspace::load(&root.join("design.yaml"), "dev", Some(root))
        .expect("load")
        .select(None)
        .expect("select");
    let mock = MockAdapter::new();
    for module in &modules {
        module
            .design()
            .check_ownership(&mock)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", module.owner()));
    }
}
