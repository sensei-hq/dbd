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

/// An error in any module fails the run — the first module's included, though
/// a clean module is inspected after it.
#[test]
fn inspect_fails_on_an_error_in_any_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(tmp.path(), "shared/ddl/table/app/broken.ddl", "");

    let out = dbd(tmp.path(), &["inspect"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    let shared = text.find("module shared (shop/shared)").expect(&text);
    let broken = text.find("broken.ddl").expect(&text);
    let core = text.find("module core (shop/core)").expect(&text);
    assert!(
        shared < broken && broken < core,
        "the error is reported under its module:\n{text}"
    );
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

    // A dependency's entity is known to its dependents too — as theirs to
    // reference, not to inspect.
    let out = dbd(tmp.path(), &["inspect", "-n", "app.currencies"]);
    let text = self::text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("module shared (shop/shared)"), "{text}");
    assert!(!text.contains("module core"), "only the module declaring it:\n{text}");

    let out = dbd(tmp.path(), &["inspect", "-n", "app.nothing"]);
    assert!(!out.status.success(), "{}", self::text(&out));
}

/// Scopes select modules in a workspace — not yet: a scope must not quietly
/// select within each module as if it were the whole design.
#[test]
fn inspect_refuses_a_scope_on_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    // `all` is a scope every design has, so only the refusal fails the run.
    let out = dbd(tmp.path(), &["inspect", "--scope", "all"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("--scope") && text.contains("modules:"), "{text}");

    let out = dbd(tmp.path(), &["inspect", "--deps", "include"]);
    let text = self::text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("--deps") && text.contains("modules:"), "{text}");
}

/// The commands that do not run a workspace yet refuse it rather than run
/// the manifest as a design with no DDL. `format`, `doctor` and `snapshot
/// --list` read the project without loading a design, and reported a
/// workspace's empty root clean.
#[test]
fn a_command_that_does_not_run_a_workspace_refuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    for args in [&["format", "--check"][..], &["doctor"], &["snapshot", "--list"]] {
        let out = dbd(tmp.path(), args);
        let text = text(&out);
        assert!(!out.status.success(), "{args:?}: {text}");
        assert!(text.contains("modules:"), "{args:?}: {text}");
    }
}

/// A dry run with no database previews each module in `depends_on` order.
#[test]
fn an_offline_dry_run_previews_each_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    let shared = text.find("module shared (shop/shared)").expect(&text);
    let core = text.find("module core (shop/core)").expect(&text);
    assert!(shared < core, "{text}");

    let out = dbd(tmp.path(), &["apply", "--dry-run", "--module", "shared"]);
    let text = self::text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && !text.contains("module core"),
        "{text}"
    );
}

/// `--module` names a module of a workspace; a design without `modules:` has
/// none to name.
#[test]
fn module_on_a_design_without_modules_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: plain\n\nschemas:\n  - app\n",
    );
    write(tmp.path(), "ddl/table/app/t.ddl", &table("t", "id integer primary key"));

    let out = dbd(tmp.path(), &["apply", "--dry-run", "--module", "core"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("--module") && text.contains("modules:"), "{text}");
}

/// Scopes select modules in a workspace — not yet: a run refuses one rather
/// than select within each module as if it were the whole design.
#[test]
fn a_scope_on_a_workspace_run_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run", "--scope", "all"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("--scope") && text.contains("modules:"), "{text}");
}

/// A module's folder is relative to the project directory (`--source`), as
/// `ddl/` is — not to wherever `-c` puts design.yaml.
#[test]
fn module_folders_are_found_under_the_source_directory() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    fs::create_dir_all(tmp.path().join("config")).unwrap();
    fs::rename(tmp.path().join("design.yaml"), tmp.path().join("config/design.yaml")).unwrap();

    let out = dbd(tmp.path(), &["-c", "config/design.yaml", "inspect"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("module core (shop/core)"), "{text}");
}

/// `migrate` reads no scope, and says so; a workspace does not then refuse the
/// scope it ignores.
#[test]
fn a_scope_a_command_ignores_is_not_refused_on_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    let out = dbd(tmp.path(), &["migrate", "--scope", "all"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("do not select over"), "{text}");
}

/// A workspace is converted from a project with `dbd split`; `init` and
/// `merge` onboard a project and say where a workspace comes from.
#[test]
fn init_and_merge_point_a_workspace_at_split() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());

    for args in [&["merge", "--dry-run"][..], &["init", "--dry-run"]] {
        let out = dbd(tmp.path(), args);
        let text = text(&out);
        assert!(!out.status.success(), "{args:?}: {text}");
        assert!(text.contains("dbd split"), "{args:?}: {text}");
    }
}

/// A SQLite workspace reloads its modules' data as a SQLite project does:
/// what the run reloads is emptied together — a table named in another case
/// than its file's, and a table before the one it references — again and
/// again.
#[test]
fn a_sqlite_workspace_reloads_its_modules_data() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        &format!(
            "project:\n  name: lite\nsource:\n  dialect: sqlite\ntarget:\n  sqlite:\n    url: sqlite://{}\n\
             schemas: []\nmodules:\n  shared: {{}}\n  core:\n    depends_on: [shared]\n",
            dir.join("app.db").display()
        ),
    );
    write(
        dir,
        "shared/ddl/table/aaa.ddl",
        "CREATE TABLE IF NOT EXISTS aaa (id INTEGER PRIMARY KEY);\n",
    );
    write(dir, "shared/import/aaa.csv", "id\n1\n");
    write(
        dir,
        "core/ddl/table/tags.ddl",
        "CREATE TABLE IF NOT EXISTS Tags (name TEXT);\n",
    );
    write(dir, "core/import/tags.csv", "name\na\nb\n");
    write(
        dir,
        "core/ddl/table/parent.ddl",
        "CREATE TABLE IF NOT EXISTS parent (id INTEGER PRIMARY KEY);\n",
    );
    write(
        dir,
        "core/ddl/table/kid.ddl",
        "CREATE TABLE IF NOT EXISTS kid (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES parent (id));\n",
    );
    write(dir, "core/import/parent.csv", "id\n1\n");
    write(dir, "core/import/kid.csv", "id,pid\n1,1\n");

    let o = dbd(dir, &["deploy"]);
    assert!(o.status.success(), "deploy: {}", text(&o));
    for run in ["import", "re-import"] {
        let o = dbd(dir, &["import"]);
        assert!(o.status.success(), "{run}: {}", text(&o));
        let out = dir.join(run);
        let o = dbd(dir, &["export", "-o", out.to_str().unwrap()]);
        assert!(o.status.success(), "export after the {run}: {}", text(&o));
        assert_eq!(
            fs::read_to_string(out.join("Tags.csv")).unwrap(),
            "name\na\nb\n",
            "{run}"
        );
        assert_eq!(
            fs::read_to_string(out.join("kid.csv")).unwrap(),
            "id,pid\n1,1\n",
            "{run}"
        );
    }
}
