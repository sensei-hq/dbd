//! `dbd snapshot` refuses a design it could not fully read (#94). A file that
//! fails to parse was left out of the snapshot, so the diff against the one
//! before read its table as removed, and the migration dropped it — CASCADE.

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
fn a_file_that_fails_to_parse_is_never_cut_as_a_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: unparsed\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    );
    write(
        dir,
        "ddl/table/app/t.ddl",
        "create table if not exists app.t (id integer primary key, name text);\n",
    );
    write(
        dir,
        "ddl/table/app/u.ddl",
        "create table if not exists app.u (id integer primary key);\n",
    );
    let out = dbd(dir, &["snapshot"]);
    assert!(out.status.success(), "{}", text(&out));
    let design_before = fs::read_to_string(dir.join("design.yaml")).unwrap();

    // A typo in one file, and a real change in another.
    write(
        dir,
        "ddl/table/app/t.ddl",
        "create table if not exists app.t (id integer primary key, name text,, oops);\n",
    );
    write(
        dir,
        "ddl/table/app/u.ddl",
        "create table if not exists app.u (id integer primary key, extra text);\n",
    );
    let out = dbd(dir, &["snapshot", "-n", "typo"]);
    let said = text(&out);

    assert!(!out.status.success(), "a design with an unread file is refused: {said}");
    assert!(said.contains("t.ddl"), "the file is named: {said}");
    assert!(!dir.join("migrations/002").exists(), "no migration is cut: {said}");
    assert!(
        !dir.join("snapshots/002.json").exists(),
        "no snapshot is written: {said}"
    );
    assert_eq!(
        fs::read_to_string(dir.join("design.yaml")).unwrap(),
        design_before,
        "the version is not bumped"
    );
}

/// A first snapshot of a design dbd could not fully read is refused too: a
/// baseline missing the table would cut it as added later, over a database
/// that has it.
#[test]
fn a_baseline_is_not_cut_from_a_design_missing_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: unparsed\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    );
    write(
        dir,
        "ddl/table/app/t.ddl",
        "create table if not exists app.t (id integer primary key,, oops);\n",
    );
    let out = dbd(dir, &["snapshot"]);
    let said = text(&out);

    assert!(!out.status.success(), "{said}");
    assert!(said.contains("t.ddl"), "{said}");
    assert!(!dir.join("snapshots").exists(), "{said}");
}
