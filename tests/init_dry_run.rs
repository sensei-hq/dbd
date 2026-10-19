//! `dbd init --dry-run` writes nothing (#85): it said "Initialized" and wrote
//! the whole project, as `init` does.

use std::path::Path;
use std::process::Output;

use assert_cmd::Command;

fn dbd(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .env_remove("DATABASE_URL")
        .current_dir(dir)
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

fn entries(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, at: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            found.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.is_dir() {
                walk(root, &path, found);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

#[test]
fn init_dry_run_writes_nothing_and_says_what_init_would_write() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let out = dbd(dir, &["init", "--dry-run"]);
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert!(entries(dir).is_empty(), "wrote {:?}:\n{said}", entries(dir));
    assert!(!said.contains("Initialized"), "{said}");
    for path in ["design.yaml", "ddl/table/public/example.ddl"] {
        assert!(said.contains(path), "{path} not named:\n{said}");
    }

    // Without the flag, the same tree is written.
    let out = dbd(dir, &["init"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(dir.join("design.yaml").exists() && dir.join("ddl/table/public/example.ddl").exists());

    // And a dry run over a project refuses as init does.
    let out = dbd(dir, &["init", "--dry-run"]);
    assert!(!out.status.success(), "{}", text(&out));
}
