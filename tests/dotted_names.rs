//! A dot in a schema, table or enum name is not supported (#89): dbd names an
//! entity `schema.name`, so `"a.b".t` read back as schema `a` — diff then took
//! the real table for an orphan, and `reconcile --prune` could drop it. A
//! design holding one is refused, naming it, before anything is written.

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

#[test]
fn a_dotted_schema_or_table_name_is_refused_naming_it() {
    for (schemas, file, ddl) in [
        (
            "  - \"a.b\"\n",
            "ddl/table/a.b/t.ddl",
            "create table if not exists \"a.b\".t (id integer primary key);\n",
        ),
        (
            "  - app\n",
            "ddl/table/app/t.v2.ddl",
            "create table if not exists app.\"t.v2\" (id integer primary key);\n",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            dir,
            "design.yaml",
            &format!("project:\n  name: dotted\nsource:\n  dialect: postgresql\nschemas:\n{schemas}"),
        );
        write(dir, file, ddl);
        for args in [&["inspect"][..], &["apply", "--dry-run"]] {
            let out = dbd(dir, args);
            let said = text(&out);
            assert!(!out.status.success(), "{args:?} on {file}: {said}");
            assert!(said.contains("dot"), "{args:?} on {file}: {said}");
        }
    }
}

/// An ordinary project is untouched.
#[test]
fn names_without_a_dot_are_unaffected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: plain\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    );
    write(
        dir,
        "ddl/table/app/t.ddl",
        "create table if not exists app.t (id integer primary key);\n",
    );
    let out = dbd(dir, &["inspect"]);
    assert!(out.status.success(), "{}", text(&out));
}
