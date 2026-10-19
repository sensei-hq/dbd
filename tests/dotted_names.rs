//! A dot in a schema name is not supported (#89): dbd names an entity
//! `schema.name` and splits it at the first dot, so `"a.b".t` read back as
//! schema `a` — diff then took the real table for an orphan, and `reconcile
//! --prune` could drop it. A design holding one is refused, naming it, before
//! anything is written. A dot in a table's name is read right — the first dot
//! is the schema's — and stays allowed.

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
fn a_dotted_schema_name_is_refused_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: dotted\nsource:\n  dialect: postgresql\nschemas:\n  - \"a.b\"\n",
    );
    write(
        dir,
        "ddl/table/a.b/t.ddl",
        "create table if not exists \"a.b\".t (id integer primary key);\n",
    );
    for args in [&["inspect"][..], &["apply", "--dry-run"]] {
        let out = dbd(dir, args);
        let said = text(&out);
        assert!(!out.status.success(), "{args:?}: {said}");
        assert!(said.contains("dot") && said.contains("a.b"), "{args:?}: {said}");
    }
}

/// An ordinary project is untouched, and so is a table whose name holds a dot.
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
    write(
        dir,
        "ddl/table/app/t.v2.ddl",
        "create table if not exists app.\"t.v2\" (id integer primary key);\n",
    );
    let out = dbd(dir, &["inspect"]);
    assert!(out.status.success(), "{}", text(&out));
}

/// A dotted schema has no file, so the refusal and the warnings name it as a
/// schema (#99): they read `2 file(s) could not be parsed` over one file, and
/// `⚠ a.b could not be parsed`, as if `a.b` were a path.
#[test]
fn a_dotted_schema_is_named_as_a_schema_not_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: dotted\nsource:\n  dialect: postgresql\n",
    );
    write(
        dir,
        "ddl/table/a.b/t.ddl",
        "create table if not exists \"a.b\".t (id integer primary key);\n",
    );

    let said = text(&dbd(dir, &["snapshot"]));
    assert!(said.contains("1 file(s) could not be parsed"), "{said}");
    assert!(said.contains("1 schema is not valid"), "{said}");
    assert!(said.contains("schema a.b"), "{said}");

    let said = text(&dbd(dir, &["graph"]));
    assert!(said.contains("schema a.b is not valid"), "{said}");
    assert!(!said.contains("a.b could not be parsed"), "{said}");
}

/// `release` refuses a design it could not fully read as the other commands
/// do, naming each file and its error (#99): it said only `Design has 1
/// entity error(s)`.
#[test]
fn release_names_the_file_it_could_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: rel\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    );
    write(
        dir,
        "ddl/table/app/t.ddl",
        "create table if not exists app.t (id integer primary key,, oops);\n",
    );

    let out = dbd(dir, &["release"]);
    let said = text(&out);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("t.ddl"), "the file is named: {said}");
    assert!(said.contains("syntax error"), "its error is given: {said}");
    assert!(!dir.join("snapshots").exists(), "nothing is written: {said}");
}
