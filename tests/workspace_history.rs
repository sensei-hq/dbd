//! A workspace's history, module by module (#7 phase 3, slice 4): `snapshot`,
//! `release`, `format` and `doctor` go over each module, in its folder.
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

fn shop(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: shop\n\nschemas:\n  - app\n\nmodules:\n  shared: {}\n  core:\n    depends_on: [shared]\n",
    );
    write(
        dir,
        "shared/ddl/table/app/currencies.ddl",
        "set search_path to app;\n\ncreate table if not exists currencies (\n  code text primary key\n);\n",
    );
    write(
        dir,
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\n\ncreate table if not exists orders (\n  id integer primary key\n);\n",
    );
}

fn manifest(dir: &Path) -> String {
    fs::read_to_string(dir.join("design.yaml")).unwrap()
}

#[test]
fn snapshot_goes_over_every_module_in_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd(tmp.path(), &["snapshot", "-n", "first"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    for module in ["shared", "core"] {
        assert!(
            tmp.path().join(format!("{module}/snapshots/001.json")).exists(),
            "{module}: {text}"
        );
    }
    assert!(!tmp.path().join("snapshots").exists());
    let out = dbd(tmp.path(), &["inspect"]);
    assert!(out.status.success(), "the workspace loads after: {}", self::text(&out));

    let out = dbd(tmp.path(), &["snapshot", "--list"]);
    let text = self::text(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("module shared (shop/shared)") && text.contains("module core (shop/core)"),
        "{text}"
    );
    assert!(text.contains("001"), "{text}");
}

#[test]
fn snapshot_of_a_module_versions_that_module_alone() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd(tmp.path(), &["snapshot", "--module", "core", "-n", "first"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(tmp.path().join("core/snapshots/001.json").exists(), "{text}");
    assert!(!tmp.path().join("shared/snapshots").exists(), "{text}");
    let manifest = manifest(tmp.path());
    assert!(
        manifest.contains("  core:\n    depends_on: [shared]\n    version: 1"),
        "{manifest}"
    );
}

#[test]
fn release_of_a_module_releases_that_module_alone() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd(tmp.path(), &["release", "--module", "core"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(tmp.path().join("core/snapshots/001.json").exists(), "{text}");
    let manifest = manifest(tmp.path());
    assert!(manifest.contains("released: true"), "{manifest}");
    assert!(!manifest.contains("project:\n  name: shop\n  released"), "{manifest}");

    // Released, core changes by snapshot; shared is still reconciled.
    let out = dbd(tmp.path(), &["reconcile", "--dry-run", "--module", "core"]);
    let text = self::text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("released"), "{text}");
    let out = dbd(tmp.path(), &["release", "--module", "core"]);
    assert!(!out.status.success(), "released once: {}", self::text(&out));
}

#[test]
fn format_goes_over_every_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(
        tmp.path(),
        "core/ddl/table/app/notes.ddl",
        "set search_path to app;\nCREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT);\n",
    );
    let out = dbd(tmp.path(), &["format", "--check"]);
    let text = text(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("notes.ddl"), "{text}");
    let out = dbd(tmp.path(), &["format"]);
    assert!(out.status.success(), "{}", self::text(&out));
    let out = dbd(tmp.path(), &["format", "--check"]);
    assert!(out.status.success(), "{}", self::text(&out));
}

#[test]
fn doctor_goes_over_every_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(
        tmp.path(),
        "core/ddl/tables/app/notes.ddl",
        "set search_path to app;\ncreate table if not exists notes (id integer primary key);\n",
    );
    let out = dbd(tmp.path(), &["doctor"]);
    let text = text(&out);
    assert!(text.contains("tables"), "the plural folder in core: {text}");
    let out = dbd(tmp.path(), &["doctor", "--fix"]);
    assert!(out.status.success(), "{}", self::text(&out));
    assert!(tmp.path().join("core/ddl/table/app/notes.ddl").exists());
}

fn read(dir: &Path) -> dbd_core::config::DesignConfig {
    dbd_core::config::read(&dir.join("design.yaml")).unwrap()
}

#[test]
fn release_of_a_module_leaves_its_dependencies_unreleased() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd(tmp.path(), &["release", "--module", "core"]);
    assert!(out.status.success(), "{}", text(&out));
    let config = read(tmp.path());
    assert!(config.modules["core"].released);
    assert!(!config.modules["shared"].released);
}

/// A whole-workspace release is checked for every module before any is
/// released: one refused leaves design.yaml as it was.
#[test]
fn a_whole_workspace_release_is_refused_whole() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let out = dbd(tmp.path(), &["snapshot", "-n", "first"]);
    assert!(out.status.success(), "{}", text(&out));
    write(
        tmp.path(),
        "core/ddl/table/app/orders.ddl",
        "set search_path to app;\n\ncreate table if not exists orders (\n  id integer primary key\n, note text\n);\n",
    );
    let before = manifest(tmp.path());
    let out = dbd(tmp.path(), &["release"]);
    assert!(
        !out.status.success(),
        "core has changes no snapshot records: {}",
        text(&out)
    );
    assert_eq!(manifest(tmp.path()), before);
}

/// A whole-workspace release releases what is not released yet, and refuses
/// when nothing is left.
#[test]
fn a_whole_workspace_release_skips_what_is_released() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    assert!(dbd(tmp.path(), &["release", "--module", "core"]).status.success());
    let out = dbd(tmp.path(), &["release"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(read(tmp.path()).modules["shared"].released);
    let out = dbd(tmp.path(), &["release"]);
    assert!(!out.status.success(), "{}", text(&out));
}

/// `format` and `doctor` go over the module asked for — `--module`, or the
/// folder they are run in — and refuse a module there is none of.
#[test]
fn format_and_doctor_take_a_module() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    write(
        tmp.path(),
        "shared/ddl/table/app/rates.ddl",
        "set search_path to app;\nCREATE TABLE IF NOT EXISTS rates (id INTEGER PRIMARY KEY);\n",
    );
    write(
        tmp.path(),
        "shared/ddl/views/app/v.ddl",
        "set search_path to app;\ncreate or replace view v as select 1 as one;\n",
    );
    let out = dbd(tmp.path(), &["--module", "core", "format", "--check"]);
    assert!(out.status.success(), "core's files are formatted: {}", text(&out));
    let out = dbd(tmp.path(), &["--module", "nosuch", "format", "--check"]);
    assert!(!out.status.success(), "{}", text(&out));
    let out = dbd(tmp.path(), &["--module", "core", "doctor"]);
    assert!(
        !text(&out).contains("views"),
        "shared's folder is not core's: {}",
        text(&out)
    );
}

/// A module folder that is not there is refused, not audited clean.
#[test]
fn doctor_refuses_a_missing_module_folder() {
    let tmp = tempfile::tempdir().unwrap();
    shop(tmp.path());
    let manifest = manifest(tmp.path());
    fs::write(
        tmp.path().join("design.yaml"),
        manifest.replace(
            "    depends_on: [shared]\n",
            "    depends_on: [shared]\n    path: src/core\n",
        ),
    )
    .unwrap();
    let out = dbd(tmp.path(), &["doctor"]);
    let text = text(&out);
    assert!(!text.contains("No issues found") && text.contains("src/core"), "{text}");
}

/// `doctor` audits — and migrates — a design.yaml written for an older dbd,
/// which does not parse as today's: it is the command for one.
#[test]
fn doctor_audits_a_config_it_cannot_parse() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: legacy\n  version: v2\n  database: postgresql\n",
    );
    write(
        tmp.path(),
        "ddl/tables/app/t.ddl",
        "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
    );
    let out = dbd(tmp.path(), &["doctor"]);
    let text = text(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("project.database"), "{text}");
}
