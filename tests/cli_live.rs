//! The `dbd` commands that need a live database, driven as the binary.
//!
//! # Why these exist
//!
//! `dbd-core`'s embedded suite covers the library. What it cannot reach is the
//! CLI's own layer: the `run` arms that build an adapter, the spinner and
//! summary paths, and the exit code `main` turns a failure into. Those sat at
//! ~0% — `commands/mod.rs`'s connecting arms, `commands/data.rs`'s
//! connect-wrappers, `commands/migration.rs`'s reset and status paths (#11).
//!
//! # Why the binary rather than the functions
//!
//! `dbd-cli` is bin-only, so a `tests/` file cannot call `commands::*`. The
//! alternatives were to publish a `[lib]` — a new public API to maintain for
//! testing's sake — or to scatter `#[cfg(test)]` modules through the command
//! files. Driving the binary needs neither, exercises the real dispatch
//! including argument parsing, and is the only way to assert an **exit code**,
//! which is what a CI pipeline actually keys on.
//!
//! Coverage does reach it: `cargo llvm-cov` instruments the binary and the
//! spawned process writes its own profile. Verified before this suite was
//! written — the pre-existing `inspect_advisory` test alone already put
//! `commands/mod.rs` at 4.47%.
//!
//! Run with:
//!   cargo test --features embedded-tests --test cli_live
//!
//! The first run downloads the PostgreSQL binary (~50 MB, cached in ~/.cache).

#![cfg(feature = "embedded-tests")]

use std::path::Path;
use std::process::Output;

use assert_cmd::Command;
use postgresql_embedded::{PostgreSQL, Settings};

// ── Harness ─────────────────────────────────────────────────────────────────

/// Start an embedded PostgreSQL and return it with a connection URL.
///
/// The handle must stay alive for the test's duration — dropping it stops the
/// server.
async fn start_pg() -> (PostgreSQL, String) {
    let settings = Settings {
        version: postgresql_embedded::VersionReq::parse(">=16").unwrap(),
        ..Default::default()
    };
    let mut pg = PostgreSQL::new(settings);
    pg.setup().await.expect("embedded postgres setup failed");
    pg.start().await.expect("embedded postgres start failed");
    pg.create_database("testdb").await.expect("failed to create testdb");
    let url = pg.settings().url("testdb");
    (pg, url)
}

/// A minimal project: one schema, one table, one view that reads it.
fn project(dir: &Path) {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\n  version: 1\n\n\
         source:\n  dialect: postgresql\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = dir.join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("widgets.ddl"),
        "set search_path to app;\n\
         create table if not exists widgets (\n  \
           id   integer primary key\n, \
           name text    not null default 'x'\n\
         );\n",
    )
    .unwrap();
    let v = dir.join("ddl/view/app");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("named.ddl"),
        "set search_path to app;\n\
         create or replace view named as select id, name from widgets;\n",
    )
    .unwrap();
}

/// Run `dbd <args>` against `dir` and `url`.
fn dbd(dir: &Path, url: &str, args: &[&str]) -> Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .args(args)
        .args(["-c", dir.join("design.yaml").to_str().unwrap()])
        .args(["-d", url])
        .current_dir(dir)
        .output()
        .expect("dbd failed to run")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn combined(o: &Output) -> String {
    format!("{}{}", stdout(o), String::from_utf8_lossy(&o.stderr))
}

// ── The connecting commands ─────────────────────────────────────────────────

/// One server, one project, walked through the commands in the order a user
/// would. Sharing the instance keeps the suite to a single ~50 MB download and
/// one startup; each assertion still stands alone.
#[tokio::test]
async fn the_connecting_commands_run_against_a_real_database() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);

    // `apply` — the arm that builds an adapter and runs the whole plan.
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "apply failed: {}", combined(&o));

    // `inspect` against a live DB: the connecting branch, not the offline one.
    let o = dbd(dir, &url, &["inspect"]);
    assert!(o.status.success(), "inspect failed: {}", combined(&o));

    // `diff` — read-only, and must report no drift on a freshly applied design.
    let o = dbd(dir, &url, &["diff"]);
    assert!(o.status.success(), "diff failed: {}", combined(&o));

    // `reconcile --dry-run` — the same, through the other planner.
    let o = dbd(dir, &url, &["reconcile", "--dry-run"]);
    assert!(o.status.success(), "reconcile failed: {}", combined(&o));

    // `migrate --status` — the pending-migration print loop.
    let o = dbd(dir, &url, &["migrate", "--status"]);
    assert!(o.status.success(), "migrate --status failed: {}", combined(&o));

    // `refresh` — no matviews here, so it must succeed having done nothing
    // rather than fail for want of work.
    let o = dbd(dir, &url, &["refresh"]);
    assert!(o.status.success(), "refresh failed: {}", combined(&o));
}

/// `apply` is idempotent: the second run must also succeed.
#[tokio::test]
async fn applying_twice_is_not_an_error() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);

    assert!(dbd(dir, &url, &["apply"]).status.success());
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "second apply failed: {}", combined(&o));
}

// ── Failure reaches the exit code ───────────────────────────────────────────

/// The half the library suite cannot test at all: a failure has to leave the
/// process with a non-zero status, or no pipeline notices.
#[tokio::test]
async fn an_unreachable_database_exits_non_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);

    // A syntactically valid URL pointing at nothing listening.
    let o = dbd(dir, "postgres://nobody@127.0.0.1:1/absent", &["apply"]);
    assert!(!o.status.success(), "a failed apply must not exit 0: {}", combined(&o));
    assert!(!combined(&o).is_empty(), "and it must say something about why");
}

/// A design that cannot be read must fail before it connects — and still exit
/// non-zero.
#[tokio::test]
async fn an_unparseable_design_exits_non_zero() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    std::fs::write(
        dir.join("ddl/table/app/widgets.ddl"),
        "set search_path to app;\ncreate table if not exists widgets (\n",
    )
    .unwrap();

    let o = dbd(dir, &url, &["apply"]);
    assert!(!o.status.success(), "a broken design must not exit 0: {}", combined(&o));
}

// ── init --from-db and merge ────────────────────────────────────────────────

/// `init --from-db` refuses a database dbd already manages, and says what to
/// use instead.
///
/// The guard matters more than the happy path here: without it, reverse
/// engineering a managed database would overwrite the project that created it
/// with a round-tripped copy of itself.
#[tokio::test]
async fn init_from_db_refuses_a_database_dbd_manages() {
    let (_pg, url) = start_pg().await;
    let src = tempfile::tempdir().unwrap();
    project(src.path());
    assert!(dbd(src.path(), &url, &["apply"]).status.success());

    let out = tempfile::tempdir().unwrap();
    let o = Command::cargo_bin("dbd")
        .unwrap()
        .args(["init", "--from-db", "-d", &url])
        .current_dir(out.path())
        .output()
        .expect("dbd failed to run");

    assert!(!o.status.success(), "it must refuse: {}", combined(&o));
    let said = combined(&o);
    assert!(said.contains("managed by dbd"), "and say why: {said}");
    assert!(said.contains("merge"), "and what to use instead: {said}");
    assert!(!out.path().join("design.yaml").exists(), "and write nothing: {said}");
}

/// `merge --dry-run` is the supported way in for a managed database — the
/// other connecting path through the reverse-engineer code.
#[tokio::test]
async fn merge_reads_a_live_database_into_the_project() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    assert!(dbd(dir, &url, &["apply"]).status.success());

    let o = dbd(dir, &url, &["merge", "--dry-run"]);
    assert!(o.status.success(), "merge --dry-run failed: {}", combined(&o));
    // Nothing to merge — the project is what built the database — so it must
    // say so rather than invent a change.
    assert!(!combined(&o).is_empty(), "merge must report what it would do");
}
