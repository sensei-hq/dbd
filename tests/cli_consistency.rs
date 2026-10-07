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

/// Everything inspect advises on lives in `app`: a string-set CHECK (an enum
/// candidate), a table in an exposed schema with no policy, and a matview whose
/// refresh is scheduled without pg_cron. The `hub` scope builds none of it.
fn advisory_outside_hub_project(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: advisory\n\n\
         source:\n  dialect: postgresql\n\n\
         schemas:\n  - app:\n      exposed: true\n  - hub\n\n\
         materialized_views:\n  options:\n    refresh: \"0 3 * * *\"\n\n\
         scopes:\n  hub:\n    includes:\n      - hub\n",
    );
    write(
        dir,
        "ddl/table/app/orders.ddl",
        "set search_path to app;\ncreate table if not exists orders (\n  id integer primary key\n, \
         state text not null constraint orders_state_chk check (state in ('pending', 'shipped'))\n);\n",
    );
    write(
        dir,
        "ddl/materialized_view/app/order_counts.ddl",
        "set search_path to app;\ncreate materialized view if not exists order_counts as \
         select state, count(*) as n from app.orders group by state;\n",
    );
    write(
        dir,
        "ddl/table/hub/nodes.ddl",
        "set search_path to hub;\ncreate table if not exists nodes (\n  id integer primary key\n);\n",
    );
}

/// Inspect's advisory checks ran over the whole design whatever the scope, so
/// `inspect --scope hub` advised on — and, for the matview, failed on — entities
/// `hub` never builds.
#[test]
fn inspect_under_a_scope_advises_only_on_what_the_scope_builds() {
    let tmp = tempfile::tempdir().unwrap();
    advisory_outside_hub_project(tmp.path());

    let whole = dbd(tmp.path(), &["inspect"]);
    let (whole_out, whole_err) = (stdout(&whole), stderr(&whole));
    assert!(
        whole_out.contains("Suggestions:")
            && whole_err.contains("No RLS policy")
            && whole_out.contains("Materialized view errors"),
        "precondition: unscoped, the project trips every advisory check:\n{whole_out}\n{whole_err}"
    );

    let out = dbd(tmp.path(), &["inspect", "--scope", "hub"]);
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(
        out.status.success(),
        "nothing 'hub' builds is broken, so inspect passes it:\n{text}\n{err}"
    );
    assert!(!text.contains("Suggestions:"), "no enum advice for app.orders: {text}");
    assert!(!err.contains("No RLS policy"), "no RLS advice for app.orders: {err}");
    assert!(
        !text.contains("Materialized view errors"),
        "no matview error for app.order_counts: {text}"
    );
}

// ── one count for one scope ─────────────────────────────────────────────────

/// A `deps: include` scope whose closure adds an entity, an `extensions: []`
/// allowlist, and an external — every way the counts used to part company.
fn counted_scope_project(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: counts\n\n\
         source:\n  dialect: postgresql\n\n\
         target:\n  postgres:\n    url: $DATABASE_URL\n    extensions:\n      - pgcrypto\n\n\
         schemas:\n  - app\n  - hub\n\n\
         external:\n  - name: auth.users\n\n\
         scopes:\n  hub_edges:\n    includes:\n      - hub.edges\n    deps: include\n    extensions: []\n",
    );
    write(
        dir,
        "ddl/table/app/users.ddl",
        "set search_path to app;\ncreate table if not exists users (\n  id integer primary key\n);\n",
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

/// The `scope 'X': N of M entities` line, wherever a command prints it.
fn scope_line(text: &str) -> Option<String> {
    text.lines().find(|l| l.starts_with("scope '")).map(str::to_string)
}

/// The `N` of a `N entities — …` summary line.
fn summary_count(text: &str) -> Option<usize> {
    text.lines()
        .find(|l| l.contains(" entities — "))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

/// inspect counted the scope before its dependency closure, apply did not
/// print a scope line at all, combine and dbml counted externals and every
/// extension the allowlist drops. Four numbers for one scope; there is one.
#[test]
fn every_command_counts_a_scope_the_same_way() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    counted_scope_project(dir);
    let combined = dir.join("combined.sql");
    let dbml = dir.join("schema.dbml");

    let apply = dbd(dir, &["apply", "--dry-run", "--scope", "hub_edges"]);
    let applied = summary_count(&stdout(&apply)).expect("apply --dry-run prints a summary");
    let unscoped_applied = summary_count(&stdout(&dbd(dir, &["apply", "--dry-run"]))).unwrap();
    let expected = format!("scope 'hub_edges': {applied} of {unscoped_applied} entities");

    let runs = [
        ("inspect", dbd(dir, &["inspect", "--scope", "hub_edges"])),
        ("apply --dry-run", apply),
        (
            "combine",
            dbd(
                dir,
                &["combine", "--scope", "hub_edges", "-f", combined.to_str().unwrap()],
            ),
        ),
        (
            "dbml",
            dbd(dir, &["dbml", "--scope", "hub_edges", "-f", dbml.to_str().unwrap()]),
        ),
        (
            "deploy --dry-run",
            dbd(dir, &["deploy", "--dry-run", "--scope", "hub_edges"]),
        ),
    ];
    for (cmd, out) in &runs {
        assert!(out.status.success(), "{cmd}: {}", stderr(out));
        assert_eq!(
            scope_line(&stdout(out)).as_deref(),
            Some(expected.as_str()),
            "{cmd} must count the scope as apply builds it:\n{}",
            stdout(out)
        );
    }
    assert_eq!(
        summary_count(&stdout(&runs[0].1)),
        Some(applied),
        "inspect's summary counts what the scope builds:\n{}",
        stdout(&runs[0].1)
    );
    assert_eq!(
        summary_count(&stdout(&dbd(dir, &["inspect"]))),
        Some(unscoped_applied),
        "and unscoped, what the design builds"
    );
}

// ── -n names an entity, or the command says why it cannot ───────────────────

/// `two_schema_project` plus a materialized view in `app`, so `refresh` has
/// something to be asked about.
fn named_entity_project(dir: &Path) {
    two_schema_project(dir);
    write(
        dir,
        "ddl/materialized_view/app/user_counts.ddl",
        "set search_path to app;\ncreate materialized view if not exists user_counts as \
         select count(*) as n from app.users;\n",
    );
}

/// A typo in `-n` selected nothing and every one of these reported success:
/// "Everything looks ok", "0 entities — no issues", an empty graph, "No
/// materialized views to refresh", "No tables to export". A selection that
/// matches nothing is a mistake, and the command is the only thing that knows.
#[test]
fn an_unknown_name_is_an_error_on_every_command_that_takes_one() {
    let tmp = tempfile::tempdir().unwrap();
    named_entity_project(tmp.path());

    for args in [
        &["inspect", "-n", "app.nope"][..],
        &["apply", "--dry-run", "-n", "app.nope"],
        &["graph", "-n", "app.nope"],
        &["refresh", "-n", "app.nope"],
        &["export", "-n", "app.nope"],
    ] {
        let out = dbd(tmp.path(), args);
        assert!(!out.status.success(), "{args:?} must fail: {}", stdout(&out));
        assert!(
            stderr(&out).contains("no entity named 'app.nope'"),
            "{args:?} must say the name matches nothing: {}",
            stderr(&out)
        );
    }
}

/// The same, for a name that exists but the scope does not build: the command
/// would act on it on a plane that does not have it, or — as these did —
/// silently act on nothing.
#[test]
fn a_name_outside_the_scope_is_an_error_on_every_command_that_takes_one() {
    let tmp = tempfile::tempdir().unwrap();
    named_entity_project(tmp.path());

    for (args, name) in [
        (&["inspect", "-n", "app.users", "--scope", "hub"][..], "app.users"),
        (
            &["apply", "--dry-run", "-n", "app.users", "--scope", "hub"],
            "app.users",
        ),
        (&["graph", "-n", "app.users", "--scope", "hub"], "app.users"),
        (
            &["refresh", "-n", "app.user_counts", "--scope", "hub"],
            "app.user_counts",
        ),
        (&["export", "-n", "app.users", "--scope", "hub"], "app.users"),
    ] {
        let out = dbd(tmp.path(), args);
        assert!(!out.status.success(), "{args:?} must fail: {}", stdout(&out));
        assert!(
            stderr(&out).contains(&format!("{name} is outside scope 'hub'")),
            "{args:?} must say the scope excludes it: {}",
            stderr(&out)
        );
    }
}

/// The guard is about names that select nothing; a good name still works.
#[test]
fn a_name_the_scope_builds_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    named_entity_project(tmp.path());

    for args in [
        &["inspect", "-n", "hub.nodes", "--scope", "hub"][..],
        &["apply", "--dry-run", "-n", "hub.nodes", "--scope", "hub"],
        &["graph", "-n", "hub.nodes", "--scope", "hub"],
    ] {
        let out = dbd(tmp.path(), args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    }
}

// ── a command that takes --scope honours all of it, or says what it ignores ──

/// `inspect --fix --scope hub` reformatted every DDL file in the project, so
/// a scoped inspect rewrote files the scope does not build.
#[test]
fn inspect_fix_under_a_scope_formats_only_the_scopes_files() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    let unformatted = |schema: &str, table: &str| {
        format!(
            "set search_path to {schema};\nCREATE TABLE IF NOT EXISTS {table} (id integer primary key, name text);\n"
        )
    };
    write(tmp.path(), "ddl/table/app/users.ddl", &unformatted("app", "users"));
    write(tmp.path(), "ddl/table/hub/nodes.ddl", &unformatted("hub", "nodes"));

    let out = dbd(tmp.path(), &["inspect", "--fix", "--scope", "hub"]);
    assert!(out.status.success(), "{}", stderr(&out));

    let read = |rel: &str| fs::read_to_string(tmp.path().join(rel)).unwrap();
    assert_ne!(
        read("ddl/table/hub/nodes.ddl"),
        unformatted("hub", "nodes"),
        "the scope's file is formatted"
    );
    assert_eq!(
        read("ddl/table/app/users.ddl"),
        unformatted("app", "users"),
        "a file outside the scope is left alone"
    );
}

/// `import -n <table> -f <file> --scope hub` loaded into a table the scope
/// does not build, as if `--scope` had not been given.
#[test]
fn importing_a_file_into_a_table_outside_the_scope_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    write(tmp.path(), "users.csv", "id\n1\n");
    let file = tmp.path().join("users.csv");

    let out = dbd(
        tmp.path(),
        &[
            "import",
            "--dry-run",
            "-n",
            "app.users",
            "-f",
            file.to_str().unwrap(),
            "--scope",
            "hub",
        ],
    );
    assert!(!out.status.success(), "must refuse: {}", stdout(&out));
    assert!(
        stderr(&out).contains("app.users is outside scope 'hub'"),
        "{}",
        stderr(&out)
    );
}

/// `--scope` given to a command that ignores it is warned about; `--deps`,
/// which only means anything to a scope, was accepted in silence.
#[test]
fn deps_given_to_a_command_without_a_scope_is_warned_about() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let out = dbd(tmp.path(), &["snapshot", "--list", "--deps", "include"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("--deps include ignored"),
        "the ignored flag is named: {}",
        stderr(&out)
    );
}

// ── each message says what is true ──────────────────────────────────────────

/// doctor counts issues, not entities, yet ended every run with the entity
/// summary — "0 entities — no issues" on a project with tables in it.
#[test]
fn doctor_reports_issues_not_an_entity_count() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let clean = dbd(tmp.path(), &["doctor"]);
    assert!(clean.status.success(), "{}", stderr(&clean));
    assert!(
        !stdout(&clean).contains("entities"),
        "doctor never counted entities: {}",
        stdout(&clean)
    );

    // A plural type folder is one auto-fixable issue.
    write(
        tmp.path(),
        "ddl/tables/app/extra.ddl",
        "set search_path to app;\ncreate table if not exists extra (id integer primary key);\n",
    );
    let found = dbd(tmp.path(), &["doctor"]);
    let text = stdout(&found);
    assert!(!text.contains("entities"), "{text}");
    assert!(text.contains("1 issue"), "the tally counts issues: {text}");
}

/// Under `deps: include` a gap is not a failure — the closure pulls it in. The
/// dry run printed each one with the ✗ an error gets.
#[test]
fn auto_included_dependencies_are_not_reported_as_failures() {
    let tmp = tempfile::tempdir().unwrap();
    counted_scope_project(tmp.path());

    for args in [
        &["deploy", "--dry-run", "--scope", "hub_edges"][..],
        &["inspect", "--scope", "hub_edges"],
    ] {
        let out = dbd(tmp.path(), args);
        let text = stdout(&out);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert!(
            text.contains("hub.nodes"),
            "{args:?} still names the dependency: {text}"
        );
        assert!(!text.contains('✗'), "{args:?} must not mark it as a failure: {text}");
    }
}

// ── inspect reads the project the way the run does ──────────────────────────

/// With `-s` a relative path other than `.`, inspect reported every DDL file
/// "File not found" and exited 1, while apply read the same files without
/// complaint: each path already starts with the project directory, and the
/// existence check joined the project directory onto it a second time.
#[test]
fn inspect_finds_the_files_of_a_project_given_by_relative_path() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(&tmp.path().join("proj"));

    let run = |args: &[&str]| {
        Command::cargo_bin("dbd")
            .unwrap()
            .env_remove("DATABASE_URL")
            .current_dir(tmp.path())
            .args(["-s", "proj"])
            .args(args)
            .output()
            .unwrap()
    };

    let apply = run(&["apply", "--dry-run"]);
    assert!(
        apply.status.success(),
        "precondition: apply reads it: {}",
        stderr(&apply)
    );

    let out = run(&["inspect"]);
    assert!(
        !stdout(&out).contains("File not found"),
        "every file is where the scan found it: {}",
        stdout(&out)
    );
    assert!(out.status.success(), "{}", stdout(&out));
}
