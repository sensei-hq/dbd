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

/// `shop`, with a scope selecting `shared` alone.
fn shop_with_scopes(dir: &Path) {
    shop(dir);
    let manifest = fs::read_to_string(dir.join("design.yaml")).unwrap();
    fs::write(
        dir.join("design.yaml"),
        format!("{manifest}\nscopes:\n  solo:\n    modules: [shared]\n"),
    )
    .unwrap();
}

/// In a workspace a scope selects modules: `inspect --scope` inspects those it
/// selects — `all`, every one — and `--deps` reaches each module's scope.
#[test]
fn inspect_inspects_the_modules_a_scope_selects() {
    let tmp = tempfile::tempdir().unwrap();
    shop_with_scopes(tmp.path());

    let out = dbd(tmp.path(), &["inspect", "--scope", "solo"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && !text.contains("module core"),
        "{text}"
    );

    for args in [&["inspect", "--scope", "all"][..], &["inspect", "--deps", "include"]] {
        let out = dbd(tmp.path(), args);
        let text = self::text(&out);
        assert!(out.status.success(), "{args:?}: {text}");
        assert!(text.contains("module core (shop/core)"), "{args:?}: {text}");
    }

    let out = dbd(tmp.path(), &["inspect", "--scope", "nope"]);
    let text = self::text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("nope") && text.contains("solo"), "{text}");
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

/// A run under a scope previews the modules the scope selects.
#[test]
fn a_scope_selects_the_modules_a_run_goes_over() {
    let tmp = tempfile::tempdir().unwrap();
    shop_with_scopes(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run", "--scope", "solo"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && !text.contains("module core"),
        "{text}"
    );

    // `--module` within a scope: one of the modules it selects.
    let out = dbd(
        tmp.path(),
        &["apply", "--dry-run", "--scope", "solo", "--module", "core"],
    );
    let text = self::text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("\"core\"") && text.contains("solo"), "{text}");
}

/// Run inside a module's folder, dbd finds the workspace by walking up, as
/// Cargo does, and runs that module — with the modules it depends on.
#[test]
fn a_run_inside_a_module_folder_runs_that_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let run_in = |dir: &Path| {
        Command::cargo_bin("dbd")
            .unwrap()
            .env_remove("DATABASE_URL")
            .current_dir(dir)
            .args(["inspect"])
            .output()
            .unwrap()
    };

    for dir in [tmp.path().join("core"), tmp.path().join("core/ddl/table/app")] {
        let out = run_in(&dir);
        let text = text(&out);
        assert!(out.status.success(), "{}: {text}", dir.display());
        assert!(
            text.contains("module shared (shop/shared)") && text.contains("module core (shop/core)"),
            "{}: {text}",
            dir.display()
        );
    }
    let out = run_in(&tmp.path().join("shared"));
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && !text.contains("module core"),
        "{text}"
    );
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
    assert!(text.contains("--scope all ignored"), "{text}");
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
        let out = dir.join(format!("out-{run}"));
        let o = dbd(dir, &["export", "-o", out.to_str().unwrap()]);
        assert!(o.status.success(), "export after the {run}: {}", text(&o));
        assert_eq!(
            // Exported as its file names it — `tags.ddl` — whatever the case
            // its `CREATE` gives the table.
            fs::read_to_string(out.join("tags.csv")).unwrap(),
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

/// Run `dbd <args>` in `dir`, as a shell started there would.
fn dbd_in(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .env_remove("DATABASE_URL")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

/// `shop`, its manifest's scopes `scopes` added.
fn shop_scoped(dir: &Path, scopes: &str) {
    shop(dir);
    let manifest = fs::read_to_string(dir.join("design.yaml")).unwrap();
    fs::write(dir.join("design.yaml"), format!("{manifest}\nscopes:\n{scopes}")).unwrap();
}

/// Run in a module's folder that the `default` scope leaves out, the module
/// runs on its own — it is what was asked for.
#[test]
fn a_run_in_a_module_folder_the_default_scope_leaves_out_runs_that_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop_scoped(tmp.path(), "  default:\n    modules: [shared]\n");
    let out = dbd_in(&tmp.path().join("core"), &["inspect"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("module core (shop/core)"), "{text}");
}

/// `-n` on an entity of a module the scope leaves out names the module.
#[test]
fn a_named_entity_outside_the_scope_is_refused_naming_its_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop_with_scopes(tmp.path());
    let out = dbd(
        tmp.path(),
        &["apply", "--dry-run", "--scope", "solo", "-n", "app.orders"],
    );
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("\"core\"") && text.contains("solo"), "{text}");
}

/// A design without modules is not looked for above where dbd runs.
#[test]
fn a_design_without_modules_is_not_looked_for_above() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: plain\n\nschemas:\n  - app\n",
    );
    write(tmp.path(), "ddl/table/app/t.ddl", &table("t", "id integer primary key"));
    fs::create_dir_all(tmp.path().join("reports")).unwrap();
    let out = dbd_in(&tmp.path().join("reports"), &["inspect"]);
    assert!(!out.status.success(), "{}", text(&out));
}

/// Walking up re-points the run at the workspace — a deploy's source too.
#[test]
fn a_deploy_inside_a_module_folder_runs_that_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd_in(&tmp.path().join("core"), &["deploy", "--dry-run"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("module core (shop/core)"), "{text}");
}

/// `init` starts a project where it is run, and `--module` says which module
/// to run wherever dbd is run from.
#[test]
fn init_does_not_walk_up_and_module_overrides_the_folder() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd_in(&tmp.path().join("core"), &["inspect", "--module", "shared"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && !text.contains("module core"),
        "{text}"
    );

    let out = dbd_in(&tmp.path().join("core"), &["init", "--name", "fresh"]);
    let text = self::text(&out);
    assert!(out.status.success(), "{text}");
    assert!(tmp.path().join("core/design.yaml").exists(), "{text}");
}

/// Run in a folder of the workspace no module holds, the whole workspace runs;
/// `-s` naming a module folder runs that module.
#[test]
fn a_run_in_a_folder_no_module_holds_runs_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    let out = dbd_in(&tmp.path().join("docs"), &["inspect"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && text.contains("module core (shop/core)"),
        "{text}"
    );

    let out = dbd_in(tmp.path(), &["-s", "core", "inspect"]);
    let text = self::text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("module core (shop/core)"), "{text}");
}

/// `--deps` reaches each module's scope: a gap the scope reports is refused,
/// and taken in under `--deps include`.
#[test]
fn deps_reaches_each_modules_scope() {
    let tmp = tempfile::tempdir().unwrap();
    shop_scoped(tmp.path(), "  notes_only:\n    includes: [app.notes]\n");
    write(
        tmp.path(),
        "core/ddl/table/app/notes.ddl",
        &table(
            "notes",
            "id integer primary key\n, order_id integer references orders (id)",
        ),
    );
    let out = dbd(tmp.path(), &["inspect", "--scope", "notes_only"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("dependency gap") && text.contains("app.orders"), "{text}");
    let out = dbd(tmp.path(), &["inspect", "--scope", "notes_only", "--deps", "include"]);
    assert!(out.status.success(), "{}", self::text(&out));
}

/// `--deps include` on a workspace run is checked for gaps as the run
/// resolves it: what it takes in may need what the scope leaves out of
/// another module — here notes take in orders, which need currencies.
#[test]
fn a_deps_override_reaches_the_workspace_gap_check() {
    let tmp = tempfile::tempdir().unwrap();
    shop_scoped(
        tmp.path(),
        "  s:\n    includes: [app.notes]\n    excludes: [app.currencies]\n",
    );
    write(
        tmp.path(),
        "core/ddl/table/app/notes.ddl",
        &table(
            "notes",
            "id integer primary key\n, order_id integer references orders (id)",
        ),
    );
    let out = dbd(tmp.path(), &["inspect", "--scope", "s", "--deps", "include"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("app.orders (module \"core\") requires app.currencies"),
        "{text}"
    );
}

/// `--module`'s help names every command that takes the module alone.
#[test]
fn modules_help_names_what_takes_the_module_alone() {
    let out = Command::cargo_bin("dbd").unwrap().arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).to_string();
    let module = help
        .lines()
        .skip_while(|l| !l.contains("--module"))
        .take_while(|l| !l.trim_start().starts_with("--") || l.contains("--module"))
        .collect::<Vec<_>>()
        .join(" ");
    for command in ["reset", "export", "snapshot", "release"] {
        assert!(module.contains(command), "{command}: {module}");
    }
}
