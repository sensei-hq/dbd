//! `dbd split --from-scopes` (#7 phase 3, slice 6; T23): each region of a
//! project's scopes — its Venn diagram over each scope's entities with their
//! dependencies — becomes a module, and the project's files move into them.

use dbd_core::Design;
use dbd_core::design::Workspace;
use dbd_core::split::{SplitPlan, plan_from_scopes, write_split};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const MANIFEST: &str = "project:\n  name: sensei  # the proving case\n\nschemas:\n  - app\n  - dojo\n  - staging\n\n\
import:\n  after:\n    - import/loader.sql\n\n\
scopes:\n  # the main database\n  default:\n    excludes: [dojo, staging.tenants, app.legacy]\n  \
dojo:\n    includes: [dojo, app.lookups, staging.tenants]\n    deps: include\n";

/// Shaped like sensei: `lookups` both scopes deploy; `orders` and
/// `rule_packs` only the main database's; `sessions` and `tenants` only the
/// Dōjō's; `legacy` neither's.
fn sensei_like(root: &Path) {
    write(root, "design.yaml", MANIFEST);
    let table = |schema: &str, name: &str, columns: &str| {
        format!("set search_path to {schema};\ncreate table if not exists {name} (\n  {columns}\n);\n")
    };
    write(
        root,
        "ddl/table/app/lookups.ddl",
        &table("app", "lookups", "code text primary key"),
    );
    write(
        root,
        "ddl/table/app/orders.ddl",
        &table(
            "app",
            "orders",
            "id integer primary key\n, kind text references lookups (code)",
        ),
    );
    write(
        root,
        "ddl/table/app/rule_packs.ddl",
        &table(
            "app",
            "rule_packs",
            "id integer primary key\n, kind text references lookups (code)",
        ),
    );
    write(
        root,
        "ddl/table/app/legacy.ddl",
        &table("app", "legacy", "id integer primary key"),
    );
    write(
        root,
        "ddl/table/dojo/sessions.ddl",
        &table(
            "dojo",
            "sessions",
            "id integer primary key\n, kind text references app.lookups (code)",
        ),
    );
    write(
        root,
        "ddl/table/staging/tenants.ddl",
        &table("staging", "tenants", "id integer primary key"),
    );
    write(root, "ddl/role/reader.ddl", "create role reader;\n");
    write(root, "import/app/lookups.csv", "code\nA\n");
    write(root, "import/dev/dojo/sessions.csv", "id,kind\n1,A\n");
    write(root, "import/staging/tenants.csv", "id\n1\n");
    write(root, "import/loader.sql", "select 1;\n");
    write(
        root,
        "policies/app/orders.sql",
        "alter table app.orders enable row level security;\n",
    );
}

fn design(root: &Path) -> Design {
    Design::from_config_with_dir(&root.join("design.yaml"), "dev", Some(root)).expect("load")
}

fn plan(root: &Path, renames: &[(&str, &str)]) -> SplitPlan {
    let renames: Vec<(String, String)> = renames.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    plan_from_scopes(&design(root), &renames).expect("plan")
}

/// A module as the tests compare it: name, region, entities, depends_on.
type ModuleRow = (String, Vec<String>, Vec<String>, Vec<String>);

fn modules(plan: &SplitPlan) -> Vec<ModuleRow> {
    plan.modules
        .iter()
        .map(|m| {
            (
                m.name.clone(),
                m.region.clone(),
                m.entities.clone(),
                m.depends_on.clone(),
            )
        })
        .collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// T23: a region per set of scopes — the intersection is `shared` — each
/// depending on what its entities reference.
#[test]
fn each_region_of_the_scopes_is_a_module() {
    let tmp = tempfile::tempdir().unwrap();
    sensei_like(tmp.path());
    let plan = plan(tmp.path(), &[]);
    assert_eq!(
        modules(&plan),
        [
            (
                "shared".to_string(),
                strings(&["default", "dojo"]),
                strings(&["app.lookups", "reader"]),
                vec![]
            ),
            (
                "core".to_string(),
                strings(&["default"]),
                strings(&["app.orders", "app.rule_packs"]),
                strings(&["shared"])
            ),
            (
                "dojo".to_string(),
                strings(&["dojo"]),
                strings(&["dojo.sessions", "staging.tenants"]),
                strings(&["shared"])
            ),
            ("unscoped".to_string(), vec![], strings(&["app.legacy"]), vec![]),
        ]
    );
    let scopes: Vec<(String, Option<Vec<String>>)> = plan
        .scopes
        .iter()
        .map(|s| (s.name.clone(), s.modules.clone()))
        .collect();
    assert_eq!(
        scopes,
        [
            ("default".to_string(), Some(strings(&["core"]))),
            ("dojo".to_string(), Some(strings(&["dojo"]))),
        ]
    );
    assert!(plan.cycle.is_none());
}

/// Every file moves to the folder of the module owning its entity or table;
/// the manifest's import script moves out of `import/`.
#[test]
fn each_file_moves_to_its_modules_folder() {
    let tmp = tempfile::tempdir().unwrap();
    sensei_like(tmp.path());
    let plan = plan(tmp.path(), &[]);
    let moves: BTreeSet<(PathBuf, PathBuf)> = plan.moves.iter().map(|m| (m.from.clone(), m.to.clone())).collect();
    let expected: BTreeSet<(PathBuf, PathBuf)> = [
        ("ddl/table/app/lookups.ddl", "shared/ddl/table/app/lookups.ddl"),
        ("ddl/role/reader.ddl", "shared/ddl/role/reader.ddl"),
        ("ddl/table/app/orders.ddl", "core/ddl/table/app/orders.ddl"),
        ("ddl/table/app/rule_packs.ddl", "core/ddl/table/app/rule_packs.ddl"),
        ("ddl/table/dojo/sessions.ddl", "dojo/ddl/table/dojo/sessions.ddl"),
        ("ddl/table/staging/tenants.ddl", "dojo/ddl/table/staging/tenants.ddl"),
        ("ddl/table/app/legacy.ddl", "unscoped/ddl/table/app/legacy.ddl"),
        ("import/app/lookups.csv", "shared/import/app/lookups.csv"),
        ("import/dev/dojo/sessions.csv", "dojo/import/dev/dojo/sessions.csv"),
        ("import/staging/tenants.csv", "dojo/import/staging/tenants.csv"),
        ("policies/app/orders.sql", "core/policies/app/orders.sql"),
        ("import/loader.sql", "scripts/loader.sql"),
    ]
    .iter()
    .map(|(a, b)| (PathBuf::from(a), PathBuf::from(b)))
    .collect();
    assert_eq!(moves, expected);
}

/// `--rename` names a module otherwise, in the plan and its scopes.
#[test]
fn a_module_can_be_renamed() {
    let tmp = tempfile::tempdir().unwrap();
    sensei_like(tmp.path());
    let plan = plan(tmp.path(), &[("shared", "common"), ("unscoped", "attic")]);
    let names: Vec<&str> = plan.modules.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["common", "core", "dojo", "attic"]);
    assert_eq!(plan.modules[1].depends_on, ["common"]);
    assert!(
        plan.moves
            .iter()
            .any(|m| m.to == Path::new("common/ddl/table/app/lookups.ddl"))
    );

    let err = plan_from_scopes(&design(tmp.path()), &[("nope".to_string(), "x".to_string())])
        .expect_err("no module nope")
        .to_string();
    assert!(err.contains("nope"), "{err}");
}

/// Written, the project is a workspace that deploys under each scope what
/// the project deployed under it.
#[test]
fn written_the_workspace_deploys_what_each_scope_did() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sensei_like(root);
    let before: Vec<BTreeSet<String>> = ["default", "dojo"]
        .iter()
        .map(|scope| {
            let design = design(root);
            let resolved = design.resolve_scope(Some(scope), None).unwrap();
            design
                .scoped_entities(&resolved)
                .unwrap()
                .into_iter()
                .filter(|e| e.entity_type != dbd_core::EntityType::External)
                .map(|e| e.name)
                .collect()
        })
        .collect();

    let plan = plan(root, &[]);
    write_split(&plan, &root.join("design.yaml"), root).expect("write");

    for (scope, before) in ["default", "dojo"].iter().zip(&before) {
        let after: BTreeSet<String> = Workspace::load(&root.join("design.yaml"), "dev", Some(root))
            .expect("the workspace loads")
            .reading(None, Some(scope), None)
            .expect("reading")
            .entities()
            .unwrap()
            .into_iter()
            .filter(|e| e.entity_type != dbd_core::EntityType::External)
            .map(|e| e.name)
            .collect();
        assert_eq!(&after, before, "scope {scope}");
    }

    let text = std::fs::read_to_string(root.join("design.yaml")).unwrap();
    for kept in ["# the proving case", "  - staging", "    - scripts/loader.sql"] {
        assert!(text.contains(kept), "{kept}: {text}");
    }
    assert!(!text.contains("import/loader.sql"), "{text}");
    let manifest = dbd_core::config::read(&root.join("design.yaml")).unwrap();
    let declared: Vec<(&str, Vec<String>)> = manifest
        .modules
        .iter()
        .map(|(name, m)| (name.as_str(), m.depends_on.clone()))
        .collect();
    assert_eq!(
        declared,
        [
            ("shared", vec![]),
            ("core", strings(&["shared"])),
            ("dojo", strings(&["shared"])),
            ("unscoped", vec![]),
        ]
    );
    for folder in ["ddl", "import", "policies"] {
        let left: Vec<_> = walkdir(root.join(folder));
        assert!(left.is_empty(), "{folder}/ is left holding {left:?}");
    }
    assert!(root.join("scripts/loader.sql").exists());
}

fn walkdir(dir: PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// What cannot divide is refused before anything moves, each naming why.
#[test]
fn what_cannot_divide_is_refused_before_anything_moves() {
    let refused = |setup: &dyn Fn(&Path), wanted: &str| {
        let tmp = tempfile::tempdir().unwrap();
        sensei_like(tmp.path());
        setup(tmp.path());
        let err = match Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path()))
            .map_err(|e| e.to_string())
            .and_then(|d| plan_from_scopes(&d, &[]).map_err(|e| e.to_string()))
        {
            Ok(_) => panic!("{wanted}: refused"),
            Err(e) => e,
        };
        assert!(err.contains(wanted), "{wanted}: {err}");
        assert!(tmp.path().join("ddl/table/app/lookups.ddl").exists(), "nothing moved");
    };
    refused(&|root| write(root, "snapshots/001.json", "{}"), "snapshots");
    refused(&|root| write(root, "migrations/002/up.sql", "select 1;"), "migrations");
    refused(&|root| write(root, "ddl/README.md", "notes"), "ddl/README.md");
    refused(
        &|root| write(root, "import/app/ghost.csv", "id\n"),
        "import/app/ghost.csv",
    );
    refused(&|root| write(root, "import/notes.sql", "select 1;"), "import/notes.sql");
    refused(&|root| write(root, "core/notes.txt", "x"), "core");
    refused(
        &|root| {
            let text = std::fs::read_to_string(root.join("design.yaml")).unwrap();
            write(
                root,
                "design.yaml",
                &text.replace("  dojo:\n    includes", "  the dojo:\n    includes"),
            );
        },
        "--rename",
    );
    refused(
        &|root| {
            write(
                root,
                "design.yaml",
                "project:\n  name: sensei\n\nschemas:\n  - app\n  - dojo\n  - staging\n",
            )
        },
        "scopes",
    );
}

/// What `--write` refuses before it moves anything, and what it puts back
/// when it cannot finish — the split is whole or not at all (#7).
mod writing {
    use super::*;

    fn split_err(root: &Path, renames: &[(&str, &str)]) -> String {
        let renames: Vec<(String, String)> = renames.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        match plan_from_scopes(&design(root), &renames) {
            Ok(plan) => match write_split(&plan, &root.join("design.yaml"), root) {
                Ok(()) => panic!("refused"),
                Err(e) => e.to_string(),
            },
            Err(e) => e.to_string(),
        }
    }

    fn untouched(root: &Path, manifest: &str) {
        assert_eq!(
            std::fs::read_to_string(root.join("design.yaml")).unwrap(),
            manifest,
            "design.yaml as it was"
        );
        for file in [
            "ddl/table/app/lookups.ddl",
            "ddl/table/app/orders.ddl",
            "ddl/table/dojo/sessions.ddl",
            "import/app/lookups.csv",
        ] {
            assert!(root.join(file).exists(), "{file} where it was");
        }
        for module in ["shared", "core", "dojo"] {
            assert!(walkdir(root.join(module)).is_empty(), "{module}/ holds nothing");
        }
    }

    /// An earlier split's leftovers — a module folder holding files the plan
    /// does not make a module of — are refused, named, not split around.
    #[test]
    fn an_earlier_splits_leftovers_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sensei_like(root);
        move_into(root, "ddl/table/app/orders.ddl", "core/ddl/table/app/orders.ddl");
        move_into(
            root,
            "ddl/table/app/rule_packs.ddl",
            "core/ddl/table/app/rule_packs.ddl",
        );
        let err = split_err(root, &[]);
        assert!(err.contains("core/"), "{err}");
    }

    /// A project with nothing left to divide is refused, not written as a
    /// workspace of no modules.
    #[test]
    fn a_plan_of_no_modules_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "design.yaml",
            "project:\n  name: sensei\n\nschemas:\n  - app\n\nscopes:\n  default:\n    includes: [app]\n",
        );
        let err = split_err(root, &[]);
        assert!(err.contains("no module"), "{err}");
        assert!(
            !std::fs::read_to_string(root.join("design.yaml"))
                .unwrap()
                .contains("modules:")
        );
    }

    /// Two files planned onto one path are refused, naming both — neither
    /// overwrites the other.
    #[test]
    fn two_files_onto_one_path_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "design.yaml",
            "project:\n  name: sensei\n\nschemas:\n  - app\n\nimport:\n  after:\n    - import/ddl/table/app/lookups.ddl\n\n\
             scopes:\n  scripts:\n    includes: [app]\n",
        );
        write(
            root,
            "ddl/table/app/lookups.ddl",
            "set search_path to app;\ncreate table if not exists lookups (code text primary key);\n",
        );
        write(
            root,
            "import/ddl/table/app/lookups.ddl",
            "-- a loader script\nselect 1;\n",
        );
        let err = split_err(root, &[]);
        assert!(
            err.contains("ddl/table/app/lookups.ddl") && err.contains("import/ddl/table/app/lookups.ddl"),
            "{err}"
        );
        assert!(
            std::fs::read_to_string(root.join("ddl/table/app/lookups.ddl"))
                .unwrap()
                .contains("create table"),
            "the DDL is not overwritten"
        );
    }

    /// Module names equal but for case would share a folder on a
    /// case-insensitive filesystem.
    #[test]
    fn names_equal_but_for_case_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        sensei_like(tmp.path());
        let err = split_err(tmp.path(), &[("dojo", "Core")]);
        assert!(err.contains("Core") && err.contains("core"), "{err}");
    }

    /// A DDL file the scanner reads is moved, whatever its name — a dotfile
    /// included: left behind, its entity was gone from the workspace.
    #[test]
    fn every_file_an_entity_is_declared_in_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sensei_like(root);
        write(
            root,
            "ddl/table/app/.audit.ddl",
            "set search_path to app;\ncreate table if not exists audit (id integer primary key);\n",
        );
        let entities = |root: &Path| design(root).entities().iter().filter(|e| e.file.is_some()).count();
        if !design(root)
            .entities()
            .iter()
            .any(|e| e.file.as_ref().is_some_and(|f| f.ends_with(".audit.ddl")))
        {
            return; // the scanner does not read it: nothing to move
        }
        let before = entities(root);
        let plan = plan(root, &[]);
        assert!(
            plan.moves
                .iter()
                .any(|m| m.from == Path::new("ddl/table/app/.audit.ddl")),
            "{:?}",
            plan.moves
        );
        write_split(&plan, &root.join("design.yaml"), root).expect("write");
        let after: usize = Workspace::load(&root.join("design.yaml"), "dev", Some(root))
            .unwrap()
            .modules()
            .iter()
            .map(|m| m.design().entities().iter().filter(|e| e.file.is_some()).count())
            .sum();
        assert_eq!(after, before);
    }

    /// The manifest is written aside first: a split that cannot write it
    /// moves nothing.
    #[test]
    fn a_manifest_that_cannot_be_written_moves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sensei_like(root);
        let manifest = std::fs::read_to_string(root.join("design.yaml")).unwrap();
        // Where the new manifest is written first, taken by a folder.
        std::fs::create_dir_all(root.join("design.yaml.dbd-split")).unwrap();
        let err = split_err(root, &[]);
        assert!(err.contains("design.yaml"), "{err}");
        untouched(root, &manifest);
    }

    /// A move that fails puts back every move before it, and design.yaml is
    /// as it was.
    #[cfg(unix)]
    #[test]
    fn a_failed_move_puts_back_every_move_before_it() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sensei_like(root);
        let manifest = std::fs::read_to_string(root.join("design.yaml")).unwrap();
        let plan = plan(root, &[]);
        // dojo's files sort after app's: its folder, unwritable, fails them.
        let dojo = root.join("ddl/table/dojo");
        std::fs::set_permissions(&dojo, std::fs::Permissions::from_mode(0o555)).unwrap();
        let outcome = write_split(&plan, &root.join("design.yaml"), root);
        std::fs::set_permissions(&dojo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = outcome.expect_err("dojo's folder is not writable").to_string();
        assert!(err.contains("dojo/sessions.ddl"), "{err}");
        assert!(err.contains("put back"), "{err}");
        untouched(root, &manifest);
    }

    /// A folder the split cannot read is refused: walked past, its files
    /// stayed at the root, and the workspace was refused once they were read.
    #[cfg(unix)]
    #[test]
    fn a_folder_it_cannot_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sensei_like(root);
        let policies = root.join("policies/app");
        std::fs::set_permissions(&policies, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = plan_from_scopes(&design(root), &[]);
        std::fs::set_permissions(&policies, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = outcome.expect_err("policies/app cannot be read").to_string();
        assert!(err.contains("policies"), "{err}");
    }

    /// In a git work tree a file git will not move fails the split, saying
    /// why — not moved by hand behind git's back — and nothing is left moved.
    #[test]
    fn a_file_git_will_not_move_fails_the_split() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        sensei_like(root);
        let manifest = std::fs::read_to_string(root.join("design.yaml")).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(root)
                .output()
                .expect("git")
        };
        assert!(git(&["init", "-q"]).status.success());
        assert!(git(&["add", "-A"]).status.success());
        assert!(git(&["commit", "-q", "-m", "the project"]).status.success());
        std::fs::write(root.join(".git/index.lock"), "").unwrap();
        let plan = plan(root, &[]);
        let err = write_split(&plan, &root.join("design.yaml"), root)
            .expect_err("git's index is locked")
            .to_string();
        std::fs::remove_file(root.join(".git/index.lock")).unwrap();
        assert!(err.contains("index.lock"), "{err}");
        untouched(root, &manifest);
    }

    fn move_into(root: &Path, from: &str, to: &str) {
        let to = root.join(to);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(root.join(from), to).unwrap();
    }
}
