//! `dbd split --from-scopes` at the command line (#7 phase 3, slice 6; T23):
//! the plan, then `--write`.
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

/// `sensei`, with a main database's scope and the Dōjō's.
fn sensei_like(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: sensei\n\nschemas:\n  - app\n  - dojo\n\n\
         scopes:\n  default:\n    excludes: [dojo]\n  dojo:\n    includes: [dojo, app.lookups]\n",
    );
    let table = |schema: &str, name: &str, columns: &str| {
        format!("set search_path to {schema};\ncreate table if not exists {name} (\n  {columns}\n);\n")
    };
    write(
        dir,
        "ddl/table/app/lookups.ddl",
        &table("app", "lookups", "code text primary key"),
    );
    write(
        dir,
        "ddl/table/app/orders.ddl",
        &table(
            "app",
            "orders",
            "id integer primary key\n, kind text references lookups (code)",
        ),
    );
    write(
        dir,
        "ddl/table/dojo/sessions.ddl",
        &table(
            "dojo",
            "sessions",
            "id integer primary key\n, kind text references app.lookups (code)",
        ),
    );
    write(dir, "import/app/lookups.csv", "code\nA\n");
}

#[test]
fn split_asks_how_to_divide_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    sensei_like(tmp.path());
    let out = dbd(tmp.path(), &["split"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("--from-scopes"), "{}", text(&out));
}

/// Without `--write` the plan is printed and nothing changes.
#[test]
fn split_prints_its_plan_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sensei_like(dir);
    let manifest = fs::read_to_string(dir.join("design.yaml")).unwrap();
    let out = dbd(dir, &["split", "--from-scopes"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    for wanted in [
        "shared",
        "in default and dojo",
        "core",
        "only in default",
        "dojo",
        "only in dojo",
        "depends on shared",
        "default: modules [core]",
        "dojo: modules [dojo]",
        "--write",
    ] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
    assert_eq!(fs::read_to_string(dir.join("design.yaml")).unwrap(), manifest);
    assert!(dir.join("ddl/table/app/lookups.ddl").exists());
    assert!(!dir.join("shared").exists());
}

/// `--write` makes the project a workspace that loads.
#[test]
fn split_write_makes_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sensei_like(dir);
    let out = dbd(dir, &["split", "--from-scopes", "--write"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(dir.join("shared/ddl/table/app/lookups.ddl").exists());
    assert!(dir.join("shared/import/app/lookups.csv").exists());
    assert!(dir.join("core/ddl/table/app/orders.ddl").exists());
    assert!(dir.join("dojo/ddl/table/dojo/sessions.ddl").exists());

    let out = dbd(dir, &["inspect"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    for module in ["module shared (sensei/shared)", "module core (sensei/core)"] {
        assert!(text.contains(module), "{module}: {text}");
    }
    let out = dbd(dir, &["inspect", "--scope", "dojo"]);
    assert!(out.status.success(), "{}", self::text(&out));
}

/// In a git work tree the files move with `git mv`: the history follows them.
#[test]
fn split_write_moves_files_with_git_in_a_work_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sensei_like(dir);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the project"]);

    let out = dbd(dir, &["split", "--from-scopes", "--write"]);
    assert!(out.status.success(), "{}", text(&out));
    let status = git(&["status", "--porcelain"]);
    assert!(
        status
            .lines()
            .any(|l| l.starts_with("R ") && l.contains("ddl/table/app/orders.ddl -> core/ddl/table/app/orders.ddl")),
        "{status}"
    );
}

#[test]
fn split_renames_a_module() {
    let tmp = tempfile::tempdir().unwrap();
    sensei_like(tmp.path());
    let out = dbd(tmp.path(), &["split", "--from-scopes", "--rename", "shared=common"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("common") && text.contains("depends on common"), "{text}");
}

#[test]
fn split_refuses_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sensei_like(dir);
    assert!(dbd(dir, &["split", "--from-scopes", "--write"]).status.success());
    let out = dbd(dir, &["split", "--from-scopes"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("modules:"), "{text}");
}

/// The plan says what `--write` writes: each scope's `extensions:`, the
/// import scripts apart from the files going to modules, and a scope that
/// is `all`.
#[test]
fn the_plan_shows_what_it_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sensei_like(dir);
    write(
        dir,
        "design.yaml",
        "project:\n  name: sensei\n\nschemas:\n  - app\n  - dojo\n\nimport:\n  after:\n    - import/loader.sql\n\n\
         scopes:\n  default:\n    excludes: [dojo]\n  dojo:\n    includes: [dojo, app.lookups]\n    extensions: [pgcrypto]\n  \
         everything: all\n",
    );
    write(dir, "import/loader.sql", "select 1;\n");
    let out = dbd(dir, &["split", "--from-scopes"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    for wanted in [
        "dojo: modules [dojo], extensions [pgcrypto]",
        "everything: all",
        "4 files move into the modules' folders, and 1 import script to scripts/",
    ] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
}

/// A file git will not move back is moved back by hand, and the run says the
/// index needs fixing — not left where it was moved to.
#[cfg(unix)]
#[test]
fn a_put_back_git_refuses_is_moved_back_by_hand() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sensei");
    sensei_like(&dir);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(&dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the project"]);
    // A git whose `mv` works twice and then fails, forward and back.
    let real = String::from_utf8(std::process::Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let count = tmp.path().join("mv-count");
    fs::write(
        bin.join("git"),
        format!(
            "#!/bin/sh\nif [ \"$1\" = mv ]; then n=$(cat {count} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {count}; \
             if [ $n -gt 2 ]; then echo 'fatal: Unable to create index.lock' >&2; exit 128; fi; fi\nexec {} \"$@\"\n",
            real.trim(),
            count = count.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::cargo_bin("dbd")
        .unwrap()
        .env_remove("DATABASE_URL")
        .env("PATH", path)
        .arg("-s")
        .arg(&dir)
        .args(["split", "--from-scopes", "--write"])
        .output()
        .unwrap();
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    for file in [
        "ddl/table/app/lookups.ddl",
        "ddl/table/app/orders.ddl",
        "ddl/table/dojo/sessions.ddl",
    ] {
        assert!(dir.join(file).exists(), "{file} is back: {text}");
    }
    assert!(text.contains("git add"), "{text}");
}

/// A project a split was interrupted in refuses every command but
/// `dbd split --undo`, which puts it back as it was.
#[test]
fn an_interrupted_split_is_undone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    sensei_like(dir);
    let manifest = fs::read_to_string(dir.join("design.yaml")).unwrap();
    let design = dbd_core::Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();
    let plan = dbd_core::split::plan_from_scopes(&design, &[]).unwrap();
    dbd_core::split::begin_journal(&plan, dir).unwrap();
    let first = &plan.moves[0];
    fs::create_dir_all(dir.join(&first.to).parent().unwrap()).unwrap();
    fs::rename(dir.join(&first.from), dir.join(&first.to)).unwrap();

    // Every command that reads the project, the ones reading only its folders
    // included: format rewrote files of the half-moved tree.
    for args in [
        &["inspect"][..],
        &["format", "--check"],
        &["doctor"],
        &["snapshot", "--list"],
    ] {
        let out = dbd(dir, args);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        assert!(text(&out).contains("dbd split --undo"), "{args:?}: {}", text(&out));
    }

    let out = dbd(dir, &["split", "--undo"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(dir.join(&first.from).exists());
    assert_eq!(fs::read_to_string(dir.join("design.yaml")).unwrap(), manifest);
    assert!(dbd(dir, &["inspect"]).status.success());
}
