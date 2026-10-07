//! A command's preview must agree with the command, and every command must
//! agree with every other about what a scope and a name select.
//!
//! # Why these drive the binary
//!
//! Every defect here is in what a command *prints* or how it *exits*: a dry run
//! listing an extension the real run never installs, a preview that exits 0 on a
//! design the real run refuses, two commands reporting different entity counts
//! for the same scope. Those are only observable from outside, which is also
//! where a CI pipeline observes them — and `dbd-cli` is bin-only, so a `tests/`
//! file cannot call `commands::*` anyway (see `cli_live.rs`).
//!
//! None of these need a database. Every run clears `DATABASE_URL`, so a test
//! that would otherwise reach a developer's real database fails to connect
//! instead.

use std::fs;
use std::path::Path;
use std::process::Output;

use assert_cmd::Command;

// ── Harness ─────────────────────────────────────────────────────────────────

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Run `dbd <args>` against the project in `dir`.
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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Two schemas, two extensions, one table each, one policy each — and a `hub`
/// scope whose `extensions: []` allowlist means no extension installs under it.
fn two_schema_project(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: parity\n\n\
         source:\n  dialect: postgresql\n\n\
         target:\n  postgres:\n    url: $DATABASE_URL\n    extensions:\n      - uuid-ossp\n      - pgcrypto\n\n\
         schemas:\n  - app\n  - hub\n\n\
         scopes:\n  hub:\n    includes:\n      - hub\n    extensions: []\n",
    );
    write(
        dir,
        "ddl/table/hub/nodes.ddl",
        "set search_path to hub;\ncreate table if not exists nodes (\n  id integer primary key\n);\n",
    );
    write(
        dir,
        "ddl/table/app/users.ddl",
        "set search_path to app;\ncreate table if not exists users (\n  id integer primary key\n);\n",
    );
    write(
        dir,
        "policies/hub/nodes.sql",
        "alter table hub.nodes enable row level security;\n",
    );
    write(
        dir,
        "policies/app/users.sql",
        "alter table app.users enable row level security;\n",
    );
}

/// A file PostgreSQL's own grammar rejects, in the `app` schema.
fn add_unparseable_app_table(dir: &Path) {
    write(
        dir,
        "ddl/table/app/broken.ddl",
        "set search_path to app;\ncreate table if not exists broken (\n  id integer primary key,,\n);\n",
    );
}

// ── apply --dry-run agrees with apply ───────────────────────────────────────

/// `extensions: []` means no extension installs under the scope, and the real
/// apply honours it. The preview listed every target extension anyway, so it
/// promised installs the run would never make.
#[test]
fn apply_dry_run_lists_no_extension_the_scope_allowlist_leaves_out() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run", "--scope", "hub"]);
    let text = stdout(&out);

    assert!(out.status.success(), "the scope is valid: {}\n{text}", stderr(&out));
    assert!(text.contains("hub.nodes"), "the scope's table is listed: {text}");
    assert!(
        !text.contains("uuid-ossp") && !text.contains("pgcrypto"),
        "an extension `extensions: []` leaves out must not be listed: {text}"
    );
}

/// The real apply refuses a design with a file dbd could not read. The preview
/// printed "N entities — no issues" and exited 0 over the same design, so a CI
/// gate running the dry run passed what the deploy then rejected.
#[test]
fn apply_dry_run_refuses_an_unparseable_file_like_the_real_run() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run"]);

    assert!(!out.status.success(), "the dry run must refuse: {}", stdout(&out));
    assert!(
        stderr(&out).contains("could not be parsed"),
        "with the real run's reason: {}",
        stderr(&out)
    );
    assert!(
        !stdout(&out).contains("no issues"),
        "and must not claim a clean design: {}",
        stdout(&out)
    );
}

/// The refusal follows the scope, exactly as the real run's does: a file the
/// scope never builds does not block it.
#[test]
fn apply_dry_run_under_a_scope_is_not_blocked_by_a_file_outside_it() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());

    let out = dbd(tmp.path(), &["apply", "--dry-run", "--scope", "hub"]);

    assert!(
        out.status.success(),
        "app/broken.ddl is outside 'hub': {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("hub.nodes"), "{}", stdout(&out));
}

// ── policies --dry-run agrees with policies ─────────────────────────────────

/// The real `policies` (and `deploy`) skip a policy whose table the scope does
/// not build. The preview listed every file under `policies/`, so it promised a
/// policy on a table the plane does not have.
#[test]
fn policies_dry_run_under_a_scope_lists_only_the_scopes_policies() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let out = dbd(tmp.path(), &["policies", "--dry-run", "--scope", "hub"]);
    let text = stdout(&out);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(text.contains("hub/nodes.sql"), "the scope's policy is listed: {text}");
    assert!(
        !text
            .lines()
            .any(|l| l.contains("app/users.sql") && !l.contains("skipped")),
        "a policy outside the scope is never listed as one that would apply: {text}"
    );
}

// ── deploy --dry-run agrees with deploy ─────────────────────────────────────

/// `deploy --dry-run` printed "1 errors" for an unparseable file and exited 0,
/// while the real deploy refuses before writing anything.
#[test]
fn deploy_dry_run_refuses_an_unparseable_file_like_the_real_run() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());

    let out = dbd(tmp.path(), &["deploy", "--dry-run"]);

    assert!(!out.status.success(), "the dry run must refuse: {}", stdout(&out));
    assert!(
        stderr(&out).contains("could not be parsed"),
        "with the real run's reason: {}",
        stderr(&out)
    );
}

/// A scoped deploy builds the scope and applies the scope's policies. Its
/// preview counted the whole design's entities and every policy file, so it
/// described a deploy the scope never runs.
#[test]
fn deploy_dry_run_under_a_scope_reports_the_scope_not_the_design() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let out = dbd(tmp.path(), &["deploy", "--dry-run", "--scope", "hub"]);
    let text = stdout(&out);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        text.contains("scope 'hub': 2 of 6 entities"),
        "the scope is named and counted like every other command counts it: {text}"
    );
    assert!(
        text.contains("2 entities would be applied"),
        "the entity count is the scope's (schema hub + hub.nodes), not the design's: {text}"
    );
    assert!(
        text.contains("1 policy file(s) would be applied"),
        "only the scope's policy would be applied: {text}"
    );
}

// ── inspect --scope agrees with the commands it vets ────────────────────────

/// `hub.edges` references `hub.nodes`; the `edges_only` scope pulls in its
/// dependencies (`deps: include`) yet excludes the one it needs.
fn conflicting_scope_project(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: closure\n\n\
         source:\n  dialect: postgresql\n\n\
         schemas:\n  - hub\n\n\
         scopes:\n  edges_only:\n    includes:\n      - hub.edges\n    excludes:\n      - hub.nodes\n    deps: include\n",
    );
    write(
        dir,
        "ddl/table/hub/nodes.ddl",
        "set search_path to hub;\ncreate table if not exists nodes (\n  id integer primary key\n);\n",
    );
    write(
        dir,
        "ddl/table/hub/edges.ddl",
        "set search_path to hub;\ncreate table if not exists edges (\n  id integer primary key\n, node_id integer references nodes (id)\n);\n",
    );
}

/// apply, dbml and diagram refuse a scope whose closure needs what it
/// excludes. `inspect --scope` — the command meant to vet a scope before a run
/// — printed "will be auto-included" and exited 0 over the same scope.
#[test]
fn inspect_refuses_a_scope_whose_closure_needs_what_it_excludes() {
    let tmp = tempfile::tempdir().unwrap();
    conflicting_scope_project(tmp.path());

    let apply = dbd(tmp.path(), &["apply", "--dry-run", "--scope", "edges_only"]);
    assert!(
        !apply.status.success() && stderr(&apply).contains("excludes 'hub.nodes'"),
        "precondition: apply refuses the scope: {}",
        stderr(&apply)
    );

    let out = dbd(tmp.path(), &["inspect", "--scope", "edges_only"]);
    assert!(!out.status.success(), "inspect must refuse too: {}", stdout(&out));
    assert!(
        stderr(&out).contains("excludes 'hub.nodes'"),
        "for the same reason: {}",
        stderr(&out)
    );
}
