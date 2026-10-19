//! `dbd snapshot` says what it took a change for (0.27.2 verification): a
//! table's one column dropped and one added, of one type, is cut as a rename —
//! and a type change is made in place, refusing a value the new type cannot
//! hold. The snapshot's warnings said so, and the command printed none of them.

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
fn a_snapshot_says_what_it_took_a_change_for() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: warn\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    );
    let line = |name: &str, ty: &str| {
        format!("create table if not exists app.line (\n  id integer primary key\n, {name} integer\n, total {ty}\n);\n")
    };
    write(dir, "ddl/table/app/line.ddl", &line("qty", "integer"));
    let out = dbd(dir, &["release"]);
    assert!(out.status.success(), "{}", text(&out));
    write(dir, "ddl/table/app/line.ddl", &line("quantity", "bigint"));
    let out = dbd(dir, &["snapshot", "-n", "rename and widen"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("'qty' renamed to 'quantity'"), "{said}");
    assert!(said.contains("type change"), "{said}");
}
