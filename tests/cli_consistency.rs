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

// ── a preview names the hooks its scope skips ───────────────────────────────

/// An `apply.after` hook that writes `app.users`, which scope `hub` leaves out.
fn add_app_only_after_hook(dir: &Path) {
    let yaml = fs::read_to_string(dir.join("design.yaml")).unwrap();
    write(
        dir,
        "design.yaml",
        &format!("{yaml}\napply:\n  after:\n    - sql/seed_users.sql\n"),
    );
    write(dir, "sql/seed_users.sql", "insert into app.users (id) values (1);\n");
}

/// The real run skips a hook whose tables the scope leaves out, and says so —
/// but only in the summary a successful run prints. The previews never named
/// it, so a dry run promised a run that would quietly not seed anything.
#[test]
fn apply_and_deploy_dry_runs_name_a_hook_the_scope_skips() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_app_only_after_hook(tmp.path());

    for command in ["apply", "deploy"] {
        let out = dbd(tmp.path(), &[command, "--dry-run", "--scope", "hub"]);
        assert!(out.status.success(), "{command}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("after-script sql/seed_users.sql skipped") && stderr(&out).contains("app.users"),
            "{command} --dry-run must name the skipped hook and why: {}",
            stderr(&out)
        );
    }
}

/// `deploy` runs the import's `import.after` hooks too, and skips one whose
/// tables the scope leaves out exactly as `import` does. `import --dry-run`
/// named the skip; `deploy --dry-run` said nothing of it.
#[test]
fn deploy_dry_run_names_an_import_after_hook_the_scope_skips() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    let yaml = fs::read_to_string(tmp.path().join("design.yaml")).unwrap();
    write(
        tmp.path(),
        "design.yaml",
        &format!("{yaml}\nimport:\n  after:\n    - sql/after_import.sql\n"),
    );
    write(tmp.path(), "sql/after_import.sql", "update app.users set id = id;\n");

    let out = dbd(tmp.path(), &["deploy", "--dry-run", "--scope", "hub"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("sql/after_import.sql skipped") && stderr(&out).contains("app.users"),
        "deploy --dry-run must name the skipped import.after hook and why: {}",
        stderr(&out)
    );
}

// ── a document says what it leaves out ──────────────────────────────────────

/// `dbml` and `diagram` cannot document a file dbd could not read, and both
/// left it out with an exit of 0 and not a word — a document missing a table,
/// indistinguishable from a complete one. `emit` already reports it; these
/// now name it the same way, and still write what they can.
#[test]
fn dbml_and_diagram_name_a_file_they_leave_out_because_it_does_not_parse() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());

    for args in [
        &["dbml", "-f", "out.dbml"][..],
        &["diagram", "--json", "-f", "out.json"][..],
    ] {
        let out = dbd(tmp.path(), args);
        assert!(out.status.success(), "{args:?} still writes: {}", stderr(&out));
        assert!(
            stderr(&out).contains("broken.ddl") && stderr(&out).contains("could not be parsed"),
            "{args:?} must name the file it leaves out: {}",
            stderr(&out)
        );
    }
}

/// `graph` left the file out of its nodes the same way, and said nothing. Its
/// stdout is a JSON document (`dbd graph | jq`), so the file is named on stderr
/// and stdout stays a document that parses — with `-v` too, whose node count
/// was printed after the JSON on stdout.
#[test]
fn graph_names_a_file_it_leaves_out_on_stderr_and_keeps_stdout_json() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());

    for args in [&["graph"][..], &["graph", "-v"][..]] {
        let out = dbd(tmp.path(), args);
        assert!(out.status.success(), "{args:?} still writes: {}", stderr(&out));
        assert!(
            stderr(&out).contains("broken.ddl") && stderr(&out).contains("could not be parsed"),
            "{args:?} must name the file it leaves out: {}",
            stderr(&out)
        );
        let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
            .unwrap_or_else(|e| panic!("{args:?}: stdout must be one JSON document ({e}):\n{}", stdout(&out)));
        assert!(doc["nodes"].is_array(), "{args:?}: {doc}");
    }

    let scoped = dbd(tmp.path(), &["graph", "--scope", "hub"]);
    assert!(scoped.status.success(), "{}", stderr(&scoped));
    assert!(
        !stderr(&scoped).contains("broken.ddl"),
        "a file outside the scope is not missing from its graph: {}",
        stderr(&scoped)
    );
}

/// Like apply's refusal, the warning follows the scope: a file the scope does
/// not document is not missing from it.
#[test]
fn dbml_under_a_scope_does_not_warn_about_a_file_outside_it() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());

    let out = dbd(tmp.path(), &["dbml", "-f", "out.dbml", "--scope", "hub"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("broken.ddl"), "{}", stderr(&out));
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

/// "Would apply policies:" headed a list that, under a scope with no policy of
/// its own, was nothing but skips — a preview announcing work above a list of
/// work it will not do. The heading is printed only over what would apply;
/// skips are listed under their own.
#[test]
fn policies_dry_run_heads_what_would_apply_and_what_would_be_skipped_apart() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let out = dbd(tmp.path(), &["policies", "--dry-run", "--scope", "hub"]);
    let text = stdout(&out);
    assert!(out.status.success(), "{}", stderr(&out));
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no `{needle}`:\n{text}"));
    assert!(
        at("Would apply policies:") < at("hub/nodes.sql")
            && at("hub/nodes.sql") < at("Would skip")
            && at("Would skip") < at("app/users.sql"),
        "the applied file under one heading, the skipped one under another:\n{text}"
    );

    std::fs::remove_file(tmp.path().join("policies/hub/nodes.sql")).unwrap();
    let out = dbd(tmp.path(), &["policies", "--dry-run", "--scope", "hub"]);
    let text = stdout(&out);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !text.contains("Would apply policies:"),
        "nothing would apply, so nothing is announced as applying:\n{text}"
    );
    assert!(
        text.contains("Would skip") && text.contains("app/users.sql"),
        "the skip is still listed, under its own heading:\n{text}"
    );
}

// ── a file a command writes stays in the project ───────────────────────────

/// `dbml` and `diagram` refused a path outside the project; `combine` and
/// `emit` wrote wherever `-f` pointed. One rule now, the safer of the two: a
/// file a command writes lands inside the project, or the command refuses
/// before writing anything.
#[test]
fn every_command_that_writes_a_file_keeps_it_inside_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&outside).unwrap();
    two_schema_project(&project);
    let out = |name: &str| outside.join(name).to_str().unwrap().to_string();
    let inside = project.join("emitted.sql").to_str().unwrap().to_string();

    for args in [
        vec!["combine".to_string(), "-f".into(), out("combined.sql")],
        vec![
            "emit".into(),
            "--dialect".into(),
            "sqlite".into(),
            "-f".into(),
            out("emitted.sql"),
        ],
        vec![
            "emit".into(),
            "--dialect".into(),
            "sqlite".into(),
            "-f".into(),
            inside.clone(),
            "--report".into(),
            out("report.json"),
        ],
        vec!["dbml".into(), "-f".into(), out("schema.dbml")],
        vec!["diagram".into(), "--json".into(), "-f".into(), out("schema.json")],
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = dbd(&project, &args);
        assert!(
            !run.status.success(),
            "{args:?} wrote outside the project: {}",
            stdout(&run)
        );
        assert!(
            stderr(&run).contains("outside project root"),
            "{args:?}: refused for that reason: {}",
            stderr(&run)
        );
    }
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing was written outside"
    );
    assert!(
        !Path::new(&inside).exists(),
        "nor half a run inside: emit checks both paths first"
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

/// `inspect -n` is about one entity, but it counted the whole design ("31
/// entities") and advised on every other table — an RLS warning for tables the
/// reader never asked about, under the name of the one they did.
#[test]
fn inspect_of_a_named_entity_counts_and_advises_on_that_entity_alone() {
    let tmp = tempfile::tempdir().unwrap();
    advisory_outside_hub_project(tmp.path());

    let out = dbd(tmp.path(), &["inspect", "-n", "hub.nodes"]);
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(out.status.success(), "nothing wrong with hub.nodes:\n{text}\n{err}");
    assert_eq!(summary_count(&text), Some(1), "one entity was inspected:\n{text}");
    assert!(!text.contains("Suggestions:"), "no enum advice for app.orders: {text}");
    assert!(!err.contains("No RLS policy"), "no RLS advice for app.orders: {err}");
    assert!(
        !text.contains("Materialized view errors"),
        "no matview error for app.order_counts: {text}"
    );

    // Named, the entity the advice is about still gets it.
    let out = dbd(tmp.path(), &["inspect", "-n", "app.orders"]);
    let (text, err) = (stdout(&out), stderr(&out));
    assert_eq!(summary_count(&text), Some(1), "{text}");
    assert!(text.contains("Suggestions:"), "{text}");
    assert!(err.contains("No RLS policy on 1 table(s)"), "{err}");
}

/// A scope building `core`, and a materialized view outside it that reads a
/// table nobody declares — a warning, in a file the scope never builds.
fn warned_outside_core_project(dir: &Path) {
    write(
        dir,
        "design.yaml",
        "project:\n  name: warned\n\nsource:\n  dialect: postgresql\n\n\
         schemas:\n  - core\n  - other\n\nscopes:\n  coreonly:\n    includes:\n      - core\n",
    );
    write(
        dir,
        "ddl/table/core/customers.ddl",
        "set search_path to core;\ncreate table if not exists customers (id integer primary key);\n",
    );
    write(
        dir,
        "ddl/materialized_view/other/thing_counts.ddl",
        "set search_path to other;\ncreate materialized view if not exists thing_counts as \
         select count(*) from missing_table;\n",
    );
}

/// Errors outside a scope were already kept apart from the scope's own; the
/// warnings were not, so `inspect --scope coreonly` listed and counted a
/// warning for a matview `coreonly` never builds.
#[test]
fn inspect_under_a_scope_counts_only_the_scopes_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    warned_outside_core_project(tmp.path());

    let whole = stdout(&dbd(tmp.path(), &["inspect"]));
    assert!(
        whole.contains("thing_counts.ddl") && whole.contains("1 warning"),
        "precondition: unscoped, the matview's warning is the design's: {whole}"
    );

    let out = dbd(tmp.path(), &["inspect", "--scope", "coreonly"]);
    let text = stdout(&out);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!text.contains("thing_counts.ddl"), "not listed as the scope's: {text}");
    assert!(
        text.contains("2 entities — no issues"),
        "nor counted as the scope's: {text}"
    );
    assert!(
        text.contains("1 warning(s) outside scope 'coreonly'"),
        "but not hidden either — tallied as outside the scope: {text}"
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
/// And `emit`, `reset`, `policies`, `import` and `diagram` printed none, so a
/// scoped run read exactly like the whole design's.
#[test]
fn every_command_counts_a_scope_the_same_way() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    counted_scope_project(dir);
    let combined = dir.join("combined.sql");
    let dbml = dir.join("schema.dbml");
    let emitted = dir.join("emitted.sql");
    let model = dir.join("schema.json");

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
        (
            "emit",
            dbd(
                dir,
                &[
                    "emit",
                    "--dialect",
                    "sqlite",
                    "--scope",
                    "hub_edges",
                    "-f",
                    emitted.to_str().unwrap(),
                ],
            ),
        ),
        (
            "reset --dry-run",
            dbd(dir, &["reset", "--dry-run", "--scope", "hub_edges"]),
        ),
        (
            "policies --dry-run",
            dbd(dir, &["policies", "--dry-run", "--scope", "hub_edges"]),
        ),
        (
            "import --dry-run",
            dbd(dir, &["import", "--dry-run", "--scope", "hub_edges"]),
        ),
        (
            "diagram --json",
            dbd(
                dir,
                &[
                    "diagram",
                    "--json",
                    "--scope",
                    "hub_edges",
                    "-f",
                    model.to_str().unwrap(),
                ],
            ),
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
        &["import", "--dry-run", "-n", "app.nope"],
        // Refused before connecting: no DATABASE_URL is set, so a refusal that
        // waited for the adapter would report the connection instead.
        &["import", "-n", "app.nope"],
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

// ── An unset $DATABASE_URL is named, not parsed ─────────────────────────────

/// `url: $DATABASE_URL` with the variable unset was passed through literally,
/// and the driver reported `relative URL without a base` — true of the string
/// `$DATABASE_URL`, and no help to anyone who just forgot to export it.
#[test]
fn an_unset_url_variable_is_named_with_how_to_set_it() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());

    let out = dbd(tmp.path(), &["apply"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains("DATABASE_URL is not set"),
        "the variable is named as unset: {err}"
    );
    assert!(err.contains("-d"), "and the way past it is given: {err}");
    assert!(!err.contains("relative URL"), "not the parser's complaint: {err}");

    // Exported but empty: clap hands the empty value over as `-d ""`, which
    // names no database any more than an unset one does.
    let out = Command::cargo_bin("dbd")
        .unwrap()
        .env("DATABASE_URL", "")
        .arg("-s")
        .arg(tmp.path())
        .arg("apply")
        .output()
        .unwrap();
    let err = stderr(&out);
    assert!(
        err.contains("DATABASE_URL is empty"),
        "an empty variable is named as empty: {err}"
    );
    assert!(!err.contains("relative URL"), "not the parser's complaint: {err}");
}

// ── A closed pipe ends the output quietly ───────────────────────────────────

/// `dbd inspect | head` panicked — `failed printing to stdout: Broken pipe` —
/// once `head` had what it wanted and closed the pipe. Every command prints
/// through `println!`, so every command did; a reader closing early is how
/// output is meant to be consumed, and the process should simply end.
#[cfg(unix)]
#[test]
fn a_closed_pipe_ends_any_command_without_a_panic() {
    use std::io::Read;
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "design.yaml",
        "project:\n  name: big\nsource:\n  dialect: postgresql\n  search_path: [app]\nschemas:\n  - app\n",
    );
    // Far more output than a pipe buffers: one warning per pathless file.
    for i in 0..2000 {
        write(
            tmp.path(),
            &format!("ddl/table/app/t{i}.ddl"),
            &format!("create table if not exists t{i} (id integer primary key);\n"),
        );
    }

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("dbd"))
        .env_remove("DATABASE_URL")
        .arg("-s")
        .arg(tmp.path())
        .arg("inspect")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = [0u8; 16];
    child.stdout.take().unwrap().read_exact(&mut first).unwrap();
    // The read end is dropped here, as `head -1` closes it.
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "no panic on a closed pipe: {err}");
    assert_ne!(out.status.code(), Some(101), "and no panic exit: {err}");
}

// ── A file that does not parse is not also missing a search path ────────────

/// A file that fails to parse states no search path the parser could record,
/// so it also got "states no `SET search_path`" — beside its parse error, for
/// a file whose first line was `set search_path to app;`. The parse error is
/// the finding; a pathless file that parses still gets its warning.
#[test]
fn a_file_that_does_not_parse_gets_no_search_path_warning() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    add_unparseable_app_table(tmp.path());
    write(
        tmp.path(),
        "ddl/table/app/pathless.ddl",
        "create table if not exists pathless (id integer primary key);\n",
    );

    let out = dbd(tmp.path(), &["inspect"]);
    let report = format!("{}{}", stdout(&out), stderr(&out));
    let warned = |file: &str| {
        report
            .lines()
            .any(|l| l.contains(file) && l.contains("states no `SET search_path`"))
    };
    assert!(report.contains("broken.ddl"), "the parse error is reported: {report}");
    assert!(!warned("broken.ddl"), "and nothing else about that file: {report}");
    assert!(
        warned("pathless.ddl"),
        "a pathless file that parses still warns: {report}"
    );
}

// ── doctor counts what it fixes as it counts what it finds ──────────────────

/// `doctor` found "2 issues" — two plural folders — and `--fix` then reported
/// "4 issues fixed": it counted every file it moved out of a folder whose
/// singular already existed, and a folder it renamed whole as one.
#[test]
fn doctor_fixes_as_many_issues_as_it_finds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(dir, "design.yaml", "project:\n  name: d\nschemas:\n  - app\n");
    write(
        dir,
        "ddl/table/app/x.ddl",
        "set search_path to app;\ncreate table if not exists x (id integer);\n",
    );
    for t in ["a", "b", "c"] {
        write(
            dir,
            &format!("ddl/tables/app/{t}.ddl"),
            &format!("set search_path to app;\ncreate table if not exists {t} (id integer);\n"),
        );
    }
    write(
        dir,
        "ddl/views/app/v.ddl",
        "set search_path to app;\ncreate or replace view v as select 1 as one;\n",
    );

    let found = stdout(&dbd(dir, &["doctor"]));
    assert!(found.contains("2 issues found."), "{found}");
    let fixed = stdout(&dbd(dir, &["doctor", "--fix"]));
    assert!(fixed.contains("2 issues fixed"), "the same two, fixed: {fixed}");
}

// ── an external name is not something inspect can check ─────────────────────

/// `inspect -n auth.users` said "Everything looks ok — 1 entities, no issues"
/// for a name declared `external:`, while `apply -n auth.users` refused it.
/// apply is right: dbd has no DDL for an external and reads nothing of it, so
/// there is nothing to inspect, and reporting it checked is the false all-clear
/// a `-n` that selects nothing used to give.
#[test]
fn inspect_refuses_an_external_name_as_apply_does() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: ext\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
         external:\n  - name: auth.users\n",
    );
    write(
        dir,
        "ddl/table/app/t.ddl",
        "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
    );

    let apply = dbd(dir, &["apply", "--dry-run", "-n", "auth.users"]);
    assert!(!apply.status.success(), "precondition: apply refuses it");
    let out = dbd(dir, &["inspect", "-n", "auth.users"]);
    assert!(!out.status.success(), "inspect refuses it too: {}", stdout(&out));
    assert!(stderr(&out).contains("external"), "saying why: {}", stderr(&out));
    assert!(!stdout(&out).contains("Everything looks ok"), "{}", stdout(&out));
}

// ── an import counts tables ─────────────────────────────────────────────────

/// A table with a shared data file and a dev-only one is one table. The
/// import's preview counted its plan's data files, as the run's summary did.
#[test]
fn import_dry_run_counts_a_table_with_two_data_files_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: twofiles\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - staging\n",
    );
    write(
        dir,
        "ddl/table/staging/items.ddl",
        "set search_path to staging;\ncreate table if not exists items (id integer);\n",
    );
    write(dir, "import/staging/items.csv", "id\n1\n");
    write(dir, "import/dev/staging/items.csv", "id\n2\n");

    let out = dbd(dir, &["import", "--dry-run", "-e", "dev"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).matches("import staging.items").count(),
        2,
        "precondition: both files load: {}",
        stdout(&out)
    );
    assert!(stdout(&out).contains("1 entities"), "one table: {}", stdout(&out));
}

// ── an ad-hoc import names its scope ────────────────────────────────────────

/// Every scoped command prints `scope 'X': N of M entities`, and `import`
/// does — except for an ad-hoc `-n X -f F` load, which said nothing about the
/// scope it was checked against.
#[test]
fn an_adhoc_import_dry_run_names_its_scope() {
    let tmp = tempfile::tempdir().unwrap();
    two_schema_project(tmp.path());
    write(tmp.path(), "nodes.csv", "id\n1\n");

    let expected = dbd(tmp.path(), &["import", "--dry-run", "--scope", "hub"]);
    let line = stdout(&expected)
        .lines()
        .find(|l| l.starts_with("scope 'hub'"))
        .map(str::to_string)
        .expect("precondition: the plan's dry run names the scope");

    let nodes = tmp.path().join("nodes.csv");
    let out = dbd(
        tmp.path(),
        &[
            "import",
            "--dry-run",
            "-n",
            "hub.nodes",
            "-f",
            nodes.to_str().unwrap(),
            "--scope",
            "hub",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).lines().any(|l| l == line),
        "{line:?} in:\n{}",
        stdout(&out)
    );
}

// ── schemas and extensions belong in design.yaml ────────────────────────────

/// `init --from-db` wrote each extension as `ddl/extension/<name>.ddl`, a
/// folder the scanner does not read: `inspect` refused the project while
/// `doctor` said "No issues found". Extensions and schemas are declared in
/// design.yaml, and both commands must say so about a file in either folder.
#[test]
fn inspect_and_doctor_agree_that_extensions_and_schemas_are_declared_in_design_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(dir, "design.yaml", "project:\n  name: folders\n\nschemas:\n  - app\n");
    write(
        dir,
        "ddl/table/app/t.ddl",
        "set search_path to app;\ncreate table if not exists t (id int);\n",
    );
    write(
        dir,
        "ddl/extension/citext.ddl",
        "create extension if not exists \"citext\" with schema \"public\";\n",
    );
    write(dir, "ddl/schema/app.ddl", "create schema if not exists \"app\";\n");

    let inspect = dbd(dir, &["inspect"]);
    let said = format!("{}{}", stdout(&inspect), stderr(&inspect));
    assert!(!inspect.status.success(), "inspect must refuse the file: {said}");
    assert!(
        said.contains("ddl/extension/citext.ddl") && said.contains("extensions:"),
        "inspect must say where an extension is declared: {said}"
    );

    let doctor = dbd(dir, &["doctor"]);
    let said = format!("{}{}", stdout(&doctor), stderr(&doctor));
    assert!(
        !said.contains("No issues found"),
        "doctor must agree with inspect: {said}"
    );
    assert!(
        said.contains("ddl/extension/citext.ddl") && said.contains("`citext`") && said.contains("extensions:"),
        "doctor must name the extension file and the entry that replaces it: {said}"
    );
    assert!(
        said.contains("ddl/schema/app.ddl") && said.contains("schemas:"),
        "doctor must name the schema file dbd does not read: {said}"
    );
}

// ── reset: the targets its help names, SQLite included ──────────────────────

/// `reset --help` named postgres or supabase, `--target` accepted any word,
/// and on a SQLite project — the design's own target — every reset failed on
/// PostgreSQL's `"public"."items" CASCADE`. The values the help names are the
/// ones accepted, and a SQLite database resets.
#[test]
fn reset_accepts_the_targets_its_help_names_and_resets_a_sqlite_database() {
    let help = Command::cargo_bin("dbd")
        .unwrap()
        .args(["reset", "--help"])
        .output()
        .unwrap();
    let help = stdout(&help);
    for target in ["postgres", "supabase", "sqlite"] {
        assert!(help.contains(target), "--help must name `{target}`: {help}");
    }

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: lite\n\nsource:\n  dialect: sqlite\n\ntarget:\n  sqlite:\n    url: $DATABASE_URL\n\nschemas: []\n",
    );
    write(
        dir,
        "ddl/table/items.ddl",
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY);\n",
    );
    write(
        dir,
        "ddl/table/parts.ddl",
        "CREATE TABLE IF NOT EXISTS parts (id INTEGER PRIMARY KEY, item_id INTEGER REFERENCES items(id));\n",
    );
    let url = format!("sqlite://{}", dir.join("app.db").display());

    let o = dbd(dir, &["reset", "--target", "mysql", "--dry-run"]);
    assert!(!o.status.success(), "an unknown --target must be refused");
    assert!(
        stderr(&o).contains("sqlite"),
        "and the refusal names what is accepted: {}",
        stderr(&o)
    );

    let o = dbd(dir, &["-e", "dev", "apply", "-d", &url]);
    assert!(o.status.success(), "apply: {}{}", stdout(&o), stderr(&o));
    let o = dbd(dir, &["-e", "dev", "reset", "--dry-run"]);
    assert!(o.status.success(), "reset --dry-run: {}{}", stdout(&o), stderr(&o));
    assert!(
        !stdout(&o).contains("CASCADE") && !stdout(&o).contains("\"public\""),
        "SQLite has neither CASCADE nor schemas: {}",
        stdout(&o)
    );
    let o = dbd(dir, &["-e", "dev", "reset", "-d", &url]);
    assert!(o.status.success(), "reset: {}{}", stdout(&o), stderr(&o));
    let o = dbd(dir, &["-e", "dev", "apply", "-d", &url]);
    assert!(
        o.status.success(),
        "the reset database applies again: {}{}",
        stdout(&o),
        stderr(&o)
    );
}

// ── a SQLite import follows the foreign keys ────────────────────────────────

/// SQLite enforces foreign keys on dbd's connections, and a SQLite project's
/// DDL is read verbatim, with no references — so an import ordered `kid`
/// before `parent` alphabetically and failed on `kid`'s key, both tables
/// left empty, and a re-import emptied `parent` first and failed the same
/// way. The import loads a referenced table first and empties it last, by the
/// foreign keys the database itself reports; its preview lists that order.
#[test]
fn a_sqlite_import_of_tables_linked_by_a_foreign_key_loads_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        &format!(
            "project:\n  name: lite\nsource:\n  dialect: sqlite\ntarget:\n  sqlite:\n    url: sqlite://{}\nschemas: []\n",
            dir.join("app.db").display()
        ),
    );
    write(
        dir,
        "ddl/table/parent.ddl",
        "CREATE TABLE IF NOT EXISTS parent (id INTEGER PRIMARY KEY, name TEXT);\n",
    );
    write(
        dir,
        "ddl/table/kid.ddl",
        "CREATE TABLE IF NOT EXISTS kid (id INTEGER PRIMARY KEY, pid INTEGER REFERENCES parent (id));\n",
    );
    write(dir, "import/parent.csv", "id,name\n1,a\n2,b\n");
    write(dir, "import/kid.csv", "id,pid\n1,1\n2,2\n");

    let o = dbd(dir, &["apply"]);
    assert!(o.status.success(), "apply: {}{}", stdout(&o), stderr(&o));

    let o = dbd(dir, &["import", "--dry-run"]);
    assert!(o.status.success(), "import --dry-run: {}{}", stdout(&o), stderr(&o));
    let preview = stdout(&o);
    let at = |table: &str| preview.find(&format!("import {table} ")).unwrap_or(usize::MAX);
    assert!(
        at("parent") < at("kid"),
        "the preview loads parent before the table referencing it: {preview}"
    );

    // Loaded into an empty pair, then reloaded over a populated one — which
    // empties both first, `kid` before the `parent` its rows point at.
    for run in ["first import", "re-import"] {
        let o = dbd(dir, &["import"]);
        assert!(o.status.success(), "{run}: {}{}", stdout(&o), stderr(&o));
        let out = dir.join("out");
        let o = dbd(dir, &["export", "-o", out.to_str().unwrap()]);
        assert!(
            o.status.success(),
            "export after the {run}: {}{}",
            stdout(&o),
            stderr(&o)
        );
        assert_eq!(
            fs::read_to_string(out.join("parent.csv")).unwrap(),
            "id,name\n1,a\n2,b\n",
            "parent after the {run}"
        );
        assert_eq!(
            fs::read_to_string(out.join("kid.csv")).unwrap(),
            "id,pid\n1,1\n2,2\n",
            "kid after the {run}"
        );
    }
}

// ── a dry run creates nothing ───────────────────────────────────────────────

/// A dry run asks the configured database for its guards — and the SQLite
/// adapter opens its file with `create_if_missing`, so a preview of a project
/// whose database did not exist yet created it: `reset --dry-run` left an
/// empty database file behind. A dry run writes nothing, so it opens only a
/// file that is already there, and without one previews as it does with no
/// database at all.
#[test]
fn a_dry_run_does_not_create_a_sqlite_database_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let db = dir.join("not-yet.db");
    write(
        dir,
        "design.yaml",
        &format!(
            "project:\n  name: lite\nsource:\n  dialect: sqlite\ntarget:\n  sqlite:\n    url: sqlite://{}\nschemas: []\n",
            db.display()
        ),
    );
    write(
        dir,
        "ddl/table/items.ddl",
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY);\n",
    );

    let mut failures: Vec<String> = Vec::new();
    for args in [
        &["reset", "--dry-run"][..],
        &["apply", "--dry-run"],
        &["deploy", "--dry-run"],
        &["import", "--dry-run"],
        &["policies", "--dry-run"],
    ] {
        let out = dbd(dir, args);
        if !out.status.success() {
            failures.push(format!("{args:?} failed: {}{}", stdout(&out), stderr(&out)));
        }
        if db.exists() {
            failures.push(format!("{args:?} created {}", db.display()));
            fs::remove_file(&db).unwrap();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ── reset refuses a --target the connection is not ──────────────────────────

/// `reset --target postgres` on a SQLite project was accepted, connected, and
/// failed on PostgreSQL's `CASCADE` — after it had read the database, and,
/// with `--force`, with nothing said about why. A `--target` that is not the
/// kind of database the connection reaches is refused before connecting.
/// Each URL here leads nowhere, so a reset that connects fails differently.
#[test]
fn reset_refuses_a_target_the_connection_is_not() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cases = [
        // (the project's target block, the --target it is refused)
        (
            format!("sqlite:\n    url: sqlite://{}/no/such/dir/app.db", dir.display()),
            "postgres",
            "SQLite",
        ),
        (
            format!("sqlite:\n    url: sqlite://{}/no/such/dir/app.db", dir.display()),
            "supabase",
            "SQLite",
        ),
        (
            "postgres:\n    url: postgres://nobody@127.0.0.1:1/absent".to_string(),
            "sqlite",
            "PostgreSQL",
        ),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (target, flag, kind) in &cases {
        write(
            dir,
            "design.yaml",
            &format!(
                "project:\n  name: mismatch\nsource:\n  dialect: postgresql\ntarget:\n  {target}\nschemas:\n  - app\n"
            ),
        );
        write(
            dir,
            "ddl/table/app/t.ddl",
            "set search_path to app;\ncreate table if not exists t (id int primary key);\n",
        );
        for args in [
            &["reset", "--force", "--target", flag][..],
            &["reset", "--dry-run", "--target", flag],
        ] {
            let out = dbd(dir, args);
            let err = stderr(&out);
            if out.status.success() || !(err.contains(&format!("--target {flag}")) && err.contains(kind)) {
                failures.push(format!("{args:?} on a {kind} connection: {}{err}", stdout(&out)));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ── an external name has no place in the graph ──────────────────────────────

/// `graph -n auth.users` printed an empty graph and exited 0 for a name
/// declared `external:` — a valid document indistinguishable from an entity
/// with no dependencies — while `inspect -n` and `apply -n` refuse it: dbd has
/// no DDL for an external, so it is not a node of the graph.
#[test]
fn graph_refuses_an_external_name_as_inspect_and_apply_do() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: ext\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
         external:\n  - name: auth.users\n",
    );
    write(
        dir,
        "ddl/table/app/t.ddl",
        "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
    );

    let out = dbd(dir, &["graph", "-n", "auth.users"]);
    assert!(!out.status.success(), "graph refuses it: {}", stdout(&out));
    assert!(stderr(&out).contains("external"), "saying why: {}", stderr(&out));
    let out = dbd(dir, &["graph", "-n", "app.t"]);
    assert!(out.status.success(), "a managed name still graphs: {}", stderr(&out));
}

// ── merge --from-dbml of a scoped document ──────────────────────────────────

/// A DBML document scoped to some schemas stands a stub in for every table a
/// foreign key reaches outside them — whether the project builds that table
/// or somebody else does. `merge --from-dbml` told the user to declare
/// `core.accounts` under `external:` when the project manages it: an external
/// is somebody else's table, and declaring one the project builds would stop
/// it being built. A table the project manages is never called external; one
/// it does not is still named.
#[test]
fn merge_from_a_scoped_dbml_document_never_calls_a_table_the_project_manages_external() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: scoped\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - core\n  - app\n",
    );
    write(
        dir,
        "ddl/table/core/accounts.ddl",
        "set search_path to core;\ncreate table if not exists accounts (id uuid primary key);\n",
    );
    write(
        dir,
        "app.dbml",
        "Table \"app\".\"people\" {\n  \"id\" int [pk]\n  \"account_id\" uuid\n  \"invoice_id\" uuid\n}\n\n\
         Ref: \"app\".\"people\".\"account_id\" > \"core\".\"accounts\".\"id\"\n\
         Ref: \"app\".\"people\".\"invoice_id\" > \"billing\".\"invoices\".\"id\"\n\n\
         Table \"core\".\"accounts\" {\n  \"id\" uuid\n\n  indexes {\n    id [unique]\n  }\n\n\
         \x20 Note: 'Defined in this project, outside this document — only the referenced columns are shown'\n}\n\n\
         Table \"billing\".\"invoices\" {\n  \"id\" uuid\n\n  indexes {\n    id [unique]\n  }\n\n\
         \x20 Note: 'External entity — managed outside this project'\n}\n",
    );

    let o = dbd(
        dir,
        &[
            "merge",
            "--from-dbml",
            dir.join("app.dbml").to_str().unwrap(),
            "--dry-run",
        ],
    );
    assert!(o.status.success(), "merge: {}{}", stdout(&o), stderr(&o));
    let said = format!("{}{}", stdout(&o), stderr(&o));
    assert!(
        !said
            .lines()
            .any(|l| l.contains("core.accounts") && l.contains("external")),
        "core.accounts is the project's own table: {said}"
    );
    assert!(
        said.lines()
            .any(|l| l.contains("billing.invoices") && l.contains("external")),
        "a table the project does not build is still named: {said}"
    );
}

// ── merge keeps an authored table file's triggers ───────────────────────────

/// A DBML document cannot hold a trigger, and `merge --from-dbml` rewrote the
/// table files it changed from the document alone — so a trigger written in
/// `notes.ddl` was deleted without a word, and so was the `DROP TRIGGER IF
/// EXISTS` that keeps a constraint trigger re-runnable. A source that cannot
/// hold triggers leaves the file's own in place.
#[test]
fn merge_from_dbml_keeps_the_triggers_written_in_a_table_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "design.yaml",
        "project:\n  name: trig\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - app\n",
    );
    write(
        dir,
        "ddl/function/app/touch.ddl",
        "set search_path to app;\ncreate or replace function touch() returns trigger language plpgsql \
         as $$ begin new.updated_at = now(); return new; end; $$;\n",
    );
    write(
        dir,
        "ddl/table/app/notes.ddl",
        "set search_path to app;\n\
         create table if not exists notes (id int primary key, body text, updated_at timestamptz);\n\
         create or replace trigger notes_touch before update on notes for each row execute function touch();\n\
         drop trigger if exists notes_guard on notes;\n\
         create constraint trigger notes_guard after insert on notes for each row execute function touch();\n",
    );
    write(
        dir,
        "notes.dbml",
        "Table \"app\".\"notes\" {\n  \"id\" int [pk]\n  \"body\" text\n  \"updated_at\" timestamptz\n  \"extra\" text\n}\n",
    );

    let o = dbd(dir, &["merge", "--from-dbml", dir.join("notes.dbml").to_str().unwrap()]);
    assert!(o.status.success(), "merge: {}{}", stdout(&o), stderr(&o));
    let file = fs::read_to_string(dir.join("ddl/table/app/notes.ddl")).unwrap();
    assert!(file.contains("\"extra\""), "the document's change is written: {file}");
    for kept in [
        "notes_touch",
        "drop trigger if exists notes_guard",
        "constraint trigger notes_guard",
    ] {
        assert!(file.to_lowercase().contains(kept), "the file keeps `{kept}`: {file}");
    }
    let o = dbd(dir, &["inspect"]);
    assert!(
        o.status.success(),
        "the merged file still reads: {}{}",
        stdout(&o),
        stderr(&o)
    );
}

/// SQLite's introspection captured a table and its indexes and left its
/// triggers out, so `merge` from a SQLite database rewrote a table file
/// without the trigger written in it — and the database still had it. The
/// trigger is captured into the file, as a PostgreSQL table's are.
#[test]
fn merge_from_a_sqlite_database_keeps_a_tables_trigger() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let url = format!("sqlite://{}", dir.join("app.db").display());
    write(
        dir,
        "design.yaml",
        &format!(
            "project:\n  name: lite\nsource:\n  dialect: sqlite\ntarget:\n  sqlite:\n    url: {url}\nschemas: []\n"
        ),
    );
    write(
        dir,
        "ddl/table/notes.ddl",
        "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT, n INTEGER DEFAULT 0);\n\
         CREATE TRIGGER IF NOT EXISTS notes_bump AFTER UPDATE OF body ON notes \
         BEGIN UPDATE notes SET n = n + 1 WHERE id = new.id; END;\n",
    );
    let o = dbd(dir, &["apply"]);
    assert!(o.status.success(), "apply: {}{}", stdout(&o), stderr(&o));

    let o = dbd(dir, &["merge", "-d", &url]);
    assert!(o.status.success(), "merge: {}{}", stdout(&o), stderr(&o));
    let file = fs::read_to_string(dir.join("ddl/table/notes.ddl")).unwrap();
    assert!(
        file.to_lowercase().contains("trigger notes_bump") && file.to_lowercase().contains("set n = n + 1"),
        "the file keeps its trigger: {file}"
    );
}
