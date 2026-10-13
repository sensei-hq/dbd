//! `dbd inspect` on a workspace (#7 phase 3, slice 1): it validates every
//! module, and refuses — before any database is involved — modules that
//! declare the same entity (T19) or reference a module they do not depend on
//! (T20).
//!
//! None of these need a database. Every run clears `DATABASE_URL`.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn dbd(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .env_remove("DATABASE_URL")
        .arg("-s")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn table(name: &str, columns: &str) -> String {
    format!("set search_path to app;\n\ncreate table if not exists {name} (\n  {columns}\n);\n")
}

/// `shop`: `shared` holds currencies, `core` holds orders referencing them.
fn shop(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\n\
         modules:\n  shared: {}\n  core:\n    depends_on: [shared]\n",
    );
    write(
        dir,
        "shared/ddl/table/app/currencies.ddl",
        &table("currencies", "code text primary key"),
    );
    write(
        dir,
        "core/ddl/table/app/orders.ddl",
        &table(
            "orders",
            "id integer primary key\n, currency text references currencies (code)",
        ),
    );
}

#[test]
fn inspect_validates_every_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["inspect"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    let shared = text.find("module shared (shop/shared)").expect(&text);
    let core = text.find("module core (shop/core)").expect(&text);
    assert!(shared < core, "modules are inspected in depends_on order:\n{text}");
    assert!(
        !text.contains("Unresolved reference"),
        "a dependency's entities resolve:\n{text}"
    );
}

#[test]
fn inspect_fails_on_an_error_in_any_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(tmp.path(), "core/ddl/table/app/broken.ddl", "");

    let out = dbd(tmp.path(), &["inspect"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    let core = text.find("module core (shop/core)").expect(&text);
    let broken = text.find("broken.ddl").expect(&text);
    assert!(broken > core, "the error is reported under its module:\n{text}");
}

/// T19.
#[test]
fn inspect_refuses_two_modules_declaring_the_same_entity() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(
        tmp.path(),
        "core/ddl/table/app/currencies.ddl",
        &table("currencies", "code text primary key"),
    );

    let out = dbd(tmp.path(), &["inspect"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("relation app.currencies") && text.contains("\"core\"") && text.contains("\"shared\""),
        "{text}"
    );
}

/// T20.
#[test]
fn inspect_reports_a_reference_outside_depends_on_as_a_gap() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\nmodules:\n  shared: {}\n  core: {}\n",
    );

    let out = dbd(tmp.path(), &["inspect"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("dependency gap") && text.contains("app.orders") && text.contains("app.currencies"),
        "{text}"
    );
}

/// `-n` inspects the entity in the module that declares it.
#[test]
fn inspect_of_a_named_entity_inspects_it_in_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["inspect", "-n", "app.orders"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("module core (shop/core)"), "{text}");
    assert!(!text.contains("module shared"), "only the module declaring it:\n{text}");

    let out = dbd(tmp.path(), &["inspect", "-n", "app.nothing"]);
    assert!(!out.status.success(), "{}", self::text(&out));
}

/// Scopes select modules in a workspace — not yet: a scope must not quietly
/// select within each module as if it were the whole design.
#[test]
fn inspect_refuses_a_scope_on_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["inspect", "--scope", "default"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("--scope") && text.contains("modules:"), "{text}");
}

/// The other commands do not run a workspace yet; each refuses it rather
/// than run the manifest as a design with no DDL.
#[test]
fn a_command_that_does_not_run_a_workspace_refuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("modules:"), "{text}");
}
