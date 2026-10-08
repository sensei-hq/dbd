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

/// Two schemas and a scope that builds one of them.
fn scoped_project(dir: &Path) {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: scoped\n\n\
         source:\n  dialect: postgresql\n\nschemas:\n  - app\n  - hub\n\n\
         scopes:\n  hub:\n    includes:\n      - hub\n",
    )
    .unwrap();
    for (schema, table) in [("app", "users"), ("hub", "nodes")] {
        let t = dir.join(format!("ddl/table/{schema}"));
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(
            t.join(format!("{table}.ddl")),
            format!("set search_path to {schema};\ncreate table if not exists {table} (id integer primary key);\n"),
        )
        .unwrap();
    }
}

/// The `scope 'X': N of M entities` line, wherever a command prints it.
fn scope_line(o: &Output) -> Option<String> {
    stdout(o).lines().find(|l| l.starts_with("scope '")).map(str::to_string)
}

/// Every other scoped command says what it narrowed to. `diff` and `reconcile`
/// said nothing, so a scoped diff reporting no drift read exactly like a whole
/// database in sync — and `apply`, `deploy`, `import` and `policies` said
/// nothing either.
#[tokio::test]
async fn the_connecting_commands_name_the_scope_they_ran_under() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);

    let expected = scope_line(&dbd(dir, &url, &["apply", "--dry-run", "--scope", "hub"]))
        .expect("apply --dry-run names the scope");
    for args in [
        &["diff", "--scope", "hub"][..],
        &["reconcile", "--dry-run", "--scope", "hub"][..],
    ] {
        let o = dbd(dir, &url, args);
        assert!(o.status.success(), "{args:?}: {}", combined(&o));
        assert_eq!(scope_line(&o), Some(expected.clone()), "{args:?}:\n{}", combined(&o));
    }

    // The runs that write: `apply --dry-run` named the scope and the run it
    // previews did not.
    for args in [
        &["apply", "--scope", "hub"][..],
        &["deploy", "--scope", "hub"][..],
        &["import", "--scope", "hub"][..],
        &["policies", "--scope", "hub"][..],
    ] {
        let o = dbd(dir, &url, args);
        assert!(o.status.success(), "{args:?}: {}", combined(&o));
        assert_eq!(scope_line(&o), Some(expected.clone()), "{args:?}:\n{}", combined(&o));
    }

    // `--json` keeps stdout a JSON document; the scope goes to stderr.
    let o = dbd(dir, &url, &["diff", "--json", "--scope", "hub"]);
    assert!(o.status.success(), "{}", combined(&o));
    serde_json::from_slice::<serde_json::Value>(&o.stdout).expect("stdout is JSON alone");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains(&expected),
        "{}",
        combined(&o)
    );
}

/// `export:` lists a table, a view and a table in another schema, and a scope
/// builds only the first schema.
fn listed_export_project(dir: &Path) {
    scoped_project(dir);
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: exported\n\n\
         source:\n  dialect: postgresql\n\nschemas:\n  - app\n  - hub\n\n\
         export:\n  - app.users\n  - app.user_ids\n  - hub.nodes\n\n\
         scopes:\n  app_only:\n    includes:\n      - app\n",
    )
    .unwrap();
    let v = dir.join("ddl/view/app");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("user_ids.ddl"),
        "set search_path to app;\ncreate or replace view user_ids as select id from users;\n",
    )
    .unwrap();
}

/// A table and a view were announced as "Exporting 2 table(s)", and an
/// `export:` entry the scope left out was not mentioned at all — the export
/// simply had one file fewer than the list.
#[tokio::test]
async fn export_counts_what_it_writes_and_names_what_the_scope_left_out() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    listed_export_project(dir);
    assert!(dbd(dir, &url, &["apply"]).status.success());

    let out = dir.join("out");
    let o = dbd(
        dir,
        &url,
        &["export", "--scope", "app_only", "-o", out.to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", combined(&o));
    let text = combined(&o);
    assert!(
        text.contains("Exporting 1 table(s) and 1 view(s)"),
        "a view is counted as a view:\n{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("hub.nodes") && l.contains("outside scope 'app_only'")),
        "the listed entry the scope left out is named:\n{text}"
    );
}

/// Run `dbd <args>` with stderr folded into stdout, in the order it was
/// written — what a terminal shows, and the only way to see whether a warning
/// came before or after a line.
fn dbd_interleaved(dir: &Path, url: &str, args: &[&str]) -> (bool, String) {
    let bin = assert_cmd::cargo::cargo_bin("dbd");
    let config = dir.join("design.yaml");
    let o = std::process::Command::new("sh")
        .arg("-c")
        .arg("\"$0\" \"$@\" 2>&1")
        .arg(bin)
        .args(args)
        .args(["-c", config.to_str().unwrap(), "-d", url])
        .current_dir(dir)
        .output()
        .expect("sh failed to run");
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned())
}

/// A deploy runs schema, then data, then policies. It reported "Applied 1
/// policy file(s)" first and the import's warnings after it, so the output
/// read as if the import ran last — or that a policy applied before a load it
/// was written to protect.
#[tokio::test]
async fn deploy_reports_each_phase_in_the_order_it_ran() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    let p = dir.join("policies/app");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join("widgets.sql"),
        "alter table app.widgets enable row level security;\n",
    )
    .unwrap();

    let (ok, text) = dbd_interleaved(dir, &url, &["deploy"]);
    assert!(ok, "{text}");
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no `{needle}`:\n{text}"));
    assert!(
        at("no import/ directory") < at("Applied 1 policy file(s)"),
        "the import's warning comes before the policy phase:\n{text}"
    );
}

/// A project built before its release has a database with no recorded
/// version. After `release` made it v1, `migrate --status` printed "000" and
/// "001" and then "Database is up to date", and the next `apply` said "Fresh
/// install at v1" over a database that was already built. Each now says what
/// is known: no version is recorded, and the apply recorded one on a database
/// that already held the design.
#[tokio::test]
async fn an_unversioned_database_is_neither_up_to_date_nor_freshly_installed_after_release() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    let yaml = std::fs::read_to_string(dir.join("design.yaml")).unwrap();
    std::fs::write(dir.join("design.yaml"), yaml.replace("  version: 1\n", "")).unwrap();

    assert!(dbd(dir, &url, &["apply"]).status.success(), "the pre-release build");
    let o = dbd(dir, &url, &["release"]);
    assert!(o.status.success(), "{}", combined(&o));

    let o = dbd(dir, &url, &["migrate", "--status"]);
    let status = combined(&o);
    assert!(o.status.success(), "{status}");
    assert!(!status.contains("up to date"), "v1 is not recorded yet:\n{status}");
    assert!(
        status.contains("records v001"),
        "and the next apply is what records it:\n{status}"
    );

    let o = dbd(dir, &url, &["apply"]);
    let applied = combined(&o);
    assert!(o.status.success(), "{applied}");
    assert!(
        !applied.contains("Fresh install"),
        "the database was already built:\n{applied}"
    );
    assert!(applied.contains("v1 recorded"), "{applied}");

    let o = dbd(dir, &url, &["migrate", "--status"]);
    assert!(combined(&o).contains("up to date"), "now it is:\n{}", combined(&o));
}

/// The other side of the check above: an empty database is still a fresh
/// install. Asking whether the design is already there must not find dbd's own
/// bookkeeping, or every first apply would read as an adoption.
#[tokio::test]
async fn an_empty_database_is_still_a_fresh_install() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);

    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "{}", combined(&o));
    assert!(combined(&o).contains("Fresh install at v1"), "{}", combined(&o));
}

/// `apply`, `deploy`, `reconcile` and `reset` refuse to run another scope on a
/// database pinned to one. `policies` did not: unscoped, on a database built
/// as `hub`, it applied every schema's policies — onto tables this database
/// does not have, or, where they existed, securing a plane it does not serve.
#[tokio::test]
async fn policies_refuse_another_scope_on_a_pinned_database() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);
    for (schema, table) in [("app", "users"), ("hub", "nodes")] {
        let p = dir.join(format!("policies/{schema}"));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join(format!("{table}.sql")),
            format!("alter table {schema}.{table} enable row level security;\n"),
        )
        .unwrap();
    }
    let o = dbd(dir, &url, &["apply", "--scope", "hub"]);
    assert!(o.status.success(), "{}", combined(&o));

    let o = dbd(dir, &url, &["policies"]);
    assert!(
        !o.status.success(),
        "unscoped policies on a hub database: {}",
        combined(&o)
    );
    assert!(
        combined(&o).contains("pinned to scope 'hub'"),
        "refused by the scope guard, naming the pin: {}",
        combined(&o)
    );

    let o = dbd(dir, &url, &["policies", "--scope", "hub"]);
    assert!(o.status.success(), "its own scope still runs: {}", combined(&o));
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

// ── Ad-hoc data files ───────────────────────────────────────────────────────

/// Export `app.widgets` as CSV through the binary and return the file's text.
fn exported_widgets(dir: &Path, url: &str) -> String {
    let out = dir.join("out");
    let o = dbd(
        dir,
        url,
        &["export", "-n", "app.widgets", "-f", "csv", "-o", out.to_str().unwrap()],
    );
    assert!(o.status.success(), "export failed: {}", combined(&o));
    std::fs::read_to_string(out.join("widgets.csv")).expect("export wrote widgets.csv")
}

/// `dbd import -n <table> -f <file>.jsonl` loads one file outside the import
/// plan, and JSON rows reach their table through dbd's internal
/// `dbd.import_jsonb_to_table` procedure. Only the full import installed
/// it, so on a database that had never run one the ad-hoc load failed asking
/// for a procedure the user never wrote.
#[tokio::test]
async fn an_ad_hoc_jsonl_import_loads_into_a_database_that_never_ran_a_full_import() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    assert!(dbd(dir, &url, &["apply"]).status.success());

    std::fs::write(
        dir.join("rows.jsonl"),
        "{\"id\":1,\"name\":\"alpha\"}\n{\"id\":2,\"name\":\"beta\"}\n",
    )
    .unwrap();
    let o = dbd(dir, &url, &["import", "-n", "app.widgets", "-f", "rows.jsonl"]);
    assert!(o.status.success(), "ad-hoc jsonl import failed: {}", combined(&o));

    let csv = exported_widgets(dir, &url);
    assert!(
        csv.contains("1,alpha") && csv.contains("2,beta"),
        "rows not loaded: {csv}"
    );
}

/// `-f rows.json` must be read as JSON. The format is inferred from the
/// extension, and `.json` fell through to the CSV default — the JSON text was
/// handed to `COPY … (FORMAT csv)`.
#[tokio::test]
async fn an_ad_hoc_json_file_is_imported_as_json() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    assert!(dbd(dir, &url, &["apply"]).status.success());

    std::fs::write(
        dir.join("rows.json"),
        "[\n  {\"id\": 1, \"name\": \"alpha\"},\n  {\"id\": 2, \"name\": \"beta\"}\n]\n",
    )
    .unwrap();
    let o = dbd(dir, &url, &["import", "-n", "app.widgets", "-f", "rows.json"]);
    assert!(o.status.success(), "ad-hoc json import failed: {}", combined(&o));

    let csv = exported_widgets(dir, &url);
    assert!(
        csv.contains("1,alpha") && csv.contains("2,beta"),
        "rows not loaded: {csv}"
    );
}

// ── reset --dry-run refuses where the real run does ─────────────────────────

/// An unversioned project with a scope, so neither the applied-migrations guard
/// nor an unknown scope gets in the way of the guard under test.
fn unversioned_scoped_project(dir: &Path) {
    project(dir);
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\n\n\
         source:\n  dialect: postgresql\n\nschemas:\n  - app\n\n\
         scopes:\n  hub:\n    includes: [app]\n",
    )
    .unwrap();
}

/// `reset --dry-run` never connected, so on a database the real run refuses — one
/// marked prod, or pinned to another scope — it printed the full plan and exited
/// 0. A preview that promises a reset the real command will not perform is
/// worse than none. With a database to ask, it now asks, and refuses exactly
/// where the real run does, `--force` and `--allow-scope-change` honoured the
/// same way. Without one it stays a database-free preview.
#[tokio::test]
async fn reset_dry_run_refuses_where_the_real_reset_would() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    unversioned_scoped_project(dir);

    // ── A prod database ──
    let o = dbd(dir, &url, &["apply", "-e", "prod"]);
    assert!(o.status.success(), "apply -e prod failed: {}", combined(&o));

    let real = dbd(dir, &url, &["reset"]);
    assert!(!real.status.success(), "the real reset refuses a prod database");
    let o = dbd(dir, &url, &["reset", "--dry-run"]);
    assert!(
        !o.status.success(),
        "the dry run must refuse a prod database too: {}",
        combined(&o)
    );
    assert!(combined(&o).contains("prod"), "and say why: {}", combined(&o));
    assert!(
        !stdout(&o).contains("DROP TABLE"),
        "a refused dry run prints no plan: {}",
        combined(&o)
    );

    let o = dbd(dir, &url, &["reset", "--dry-run", "--force"]);
    assert!(o.status.success(), "--force previews past the guard: {}", combined(&o));
    assert!(
        stdout(&o).contains("DROP TABLE"),
        "and prints the plan: {}",
        combined(&o)
    );

    // ── A database pinned to another scope ──
    // `-e dev`: `-e` defaults to prod, and the prod guard must not be what fires.
    let o = dbd(
        dir,
        &url,
        &["apply", "-e", "dev", "--scope", "hub", "--allow-scope-change"],
    );
    assert!(o.status.success(), "apply --scope hub failed: {}", combined(&o));

    let real = dbd(dir, &url, &["reset"]);
    assert!(
        !real.status.success(),
        "the real reset refuses another scope's database"
    );
    let o = dbd(dir, &url, &["reset", "--dry-run"]);
    assert!(
        !o.status.success(),
        "the dry run must refuse another scope's database too: {}",
        combined(&o)
    );
    assert!(
        combined(&o).contains("hub"),
        "and name the pinned scope: {}",
        combined(&o)
    );

    let o = dbd(dir, &url, &["reset", "--dry-run", "--allow-scope-change"]);
    assert!(
        o.status.success(),
        "--allow-scope-change previews past the scope guard: {}",
        combined(&o)
    );
    assert!(
        stdout(&o).contains("DROP TABLE"),
        "and prints the plan: {}",
        combined(&o)
    );

    // ── No database configured: still a database-free preview ──
    let o = Command::cargo_bin("dbd")
        .unwrap()
        .args(["reset", "--dry-run", "-c", dir.join("design.yaml").to_str().unwrap()])
        .env_remove("DATABASE_URL")
        .current_dir(dir)
        .output()
        .expect("dbd failed to run");
    assert!(o.status.success(), "no database, no guard to check: {}", combined(&o));
    assert!(
        stdout(&o).contains("DROP TABLE"),
        "the plan is printed: {}",
        combined(&o)
    );
}

// ── import.staging is enforced ──────────────────────────────────────────────

/// The audit's repro. `import.staging` lists `staging`, and a data file sits
/// under `import/app/`: `dbd import` TRUNCATEd the live `app.widgets` and
/// replaced its rows. The docs say the import fails for any schema the list
/// leaves out — so it must exit non-zero, and the live row must survive.
#[tokio::test]
async fn an_import_refuses_a_data_file_outside_the_staging_schemas_and_keeps_the_live_rows() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    assert!(dbd(dir, &url, &["apply"]).status.success());

    // A live row, loaded before the project declares its staging schemas.
    std::fs::write(dir.join("live.jsonl"), "{\"id\":1,\"name\":\"live\"}\n").unwrap();
    let o = dbd(dir, &url, &["import", "-n", "app.widgets", "-f", "live.jsonl"]);
    assert!(o.status.success(), "seeding the live row failed: {}", combined(&o));

    let design = std::fs::read_to_string(dir.join("design.yaml")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        format!("{design}\nimport:\n  staging: [staging]\n"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("import/app")).unwrap();
    std::fs::write(dir.join("import/app/widgets.csv"), "id,name\n2,replaced\n").unwrap();

    let o = dbd(dir, &url, &["import"]);
    assert!(!o.status.success(), "the import must be refused: {}", combined(&o));
    assert!(
        combined(&o).contains("import/app/widgets.csv"),
        "and name the file it refused: {}",
        combined(&o)
    );

    let csv = exported_widgets(dir, &url);
    assert!(csv.contains("1,live"), "the live row must survive: {csv}");
    assert!(!csv.contains("replaced"), "and nothing may replace it: {csv}");
}

// ── a skipped hook is named even when the run fails ─────────────────────────

/// A scope skips a hook whose tables it leaves out, and said so only in the
/// summary a successful run prints. When the run then failed — often *because*
/// the hook did not prepare what the DDL needed — the error came with no word
/// of the skip, and nothing explained it.
#[tokio::test]
async fn a_scoped_apply_that_fails_still_names_the_hook_it_skipped() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);
    let yaml = std::fs::read_to_string(dir.join("design.yaml")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        format!("{yaml}\napply:\n  before:\n    - sql/seed_users.sql\n"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("sql")).unwrap();
    std::fs::write(
        dir.join("sql/seed_users.sql"),
        "insert into app.users (id) values (1);\n",
    )
    .unwrap();
    // Parses, and fails when Postgres runs it.
    std::fs::write(
        dir.join("ddl/table/hub/broken.ddl"),
        "set search_path to hub;\ncreate table if not exists broken (id integer primary key default hub.no_such_fn());\n",
    )
    .unwrap();

    let o = dbd(dir, &url, &["apply", "--scope", "hub"]);

    assert!(!o.status.success(), "the apply must fail: {}", combined(&o));
    assert!(
        combined(&o).contains("before-script sql/seed_users.sql skipped"),
        "the failure must still name the skipped hook: {}",
        combined(&o)
    );
}

// ── source.search_path is the path a pathless file runs on ──────────────────

/// `psql` from the embedded server's own installation, so a script is run the
/// way a user runs `combine`'s output — not through dbd's adapter, which
/// rewrites what it executes.
fn psql(pg: &PostgreSQL, url: &str, args: &[&str]) -> Output {
    std::process::Command::new(pg.settings().binary_dir().join("psql"))
        .arg(url)
        .args(["-X", "-q", "-v", "ON_ERROR_STOP=1"])
        .args(args)
        .output()
        .expect("psql failed to run")
}

/// The schema a table named `name` landed in, one per line.
fn schemas_of(pg: &PostgreSQL, url: &str, name: &str) -> String {
    let o = psql(
        pg,
        url,
        &[
            "-tA",
            "-c",
            &format!("select schemaname from pg_tables where tablename = '{name}' order by 1"),
        ],
    );
    assert!(o.status.success(), "psql query failed: {}", combined(&o));
    stdout(&o).trim().to_string()
}

/// `source.search_path` is what the parser resolves a file with no `SET
/// search_path` against: `ddl/table/app/a.ddl` holding a bare `create table a`
/// is `app.a`, and `inspect` says so. Nothing ran it there. The DDL ran on the
/// session's own path, so the table landed in `public`, `diff` then reported
/// `+ create app.a`, and reconcile never converged on its own diff.
///
/// Every path that runs or writes the file has to use the path the parser
/// used: `apply`, `reconcile` on a fresh database, and the script `combine`
/// writes, run as a user runs it — with `psql`.
#[tokio::test]
async fn a_file_with_no_search_path_runs_on_source_search_path() {
    let (pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: pathless\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("ddl/table/app/a.ddl"),
        "create table if not exists a (id integer primary key);\n",
    )
    .unwrap();

    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "apply failed: {}", combined(&o));
    assert_eq!(
        schemas_of(&pg, &url, "a"),
        "app",
        "apply builds it where the parser read it"
    );

    let o = dbd(dir, &url, &["diff"]);
    assert!(o.status.success(), "diff failed: {}", combined(&o));
    assert!(
        stdout(&o).contains("in sync"),
        "and the design's own diff is then empty: {}",
        combined(&o)
    );

    let o = dbd(dir, &url, &["reconcile", "--dry-run"]);
    assert!(o.status.success(), "reconcile --dry-run failed: {}", combined(&o));
    assert!(
        stdout(&o).contains("Already in sync"),
        "so reconcile has nothing to do: {}",
        combined(&o)
    );

    // A fresh database, built by reconcile.
    pg.create_database("by_reconcile").await.unwrap();
    let reconciled = pg.settings().url("by_reconcile");
    let o = dbd(dir, &reconciled, &["reconcile"]);
    assert!(o.status.success(), "reconcile failed: {}", combined(&o));
    assert_eq!(
        schemas_of(&pg, &reconciled, "a"),
        "app",
        "reconcile builds it there too"
    );

    // A fresh database, built from the combined script by psql.
    let o = dbd(dir, &url, &["combine", "-f", "combined.sql"]);
    assert!(o.status.success(), "combine failed: {}", combined(&o));
    pg.create_database("by_psql").await.unwrap();
    let scripted = pg.settings().url("by_psql");
    let o = psql(&pg, &scripted, &["-f", dir.join("combined.sql").to_str().unwrap()]);
    assert!(o.status.success(), "psql -f combined.sql failed: {}", combined(&o));
    assert_eq!(
        schemas_of(&pg, &scripted, "a"),
        "app",
        "the combined script runs like apply"
    );
}

// ── Every command that reaches a pinned database checks the pin ─────────────

/// `apply`, `deploy`, `reconcile`, `reset` and `policies` refuse another scope
/// on a database pinned to one. `refresh` did not: on a `hub` database it
/// refreshed the other scope's matview and failed partway. And the `policies`,
/// `apply` and `deploy` dry runs, given a database with `-d`, never asked it
/// for its pin — previewing a run the guard then refused.
///
/// Each now checks the pin whenever it has a database to ask, and lets the
/// pinned scope (or `--allow-scope-change`, where the run takes it) through.
#[tokio::test]
async fn refresh_and_the_dry_runs_refuse_another_scope_on_a_pinned_database() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);
    let mv = dir.join("ddl/materialized_view/app");
    std::fs::create_dir_all(&mv).unwrap();
    std::fs::write(
        mv.join("user_count.ddl"),
        "set search_path to app;\ncreate materialized view if not exists user_count as select count(*) as n from users;\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("policies/app")).unwrap();
    std::fs::write(
        dir.join("policies/app/users.sql"),
        "alter table app.users enable row level security;\n",
    )
    .unwrap();
    let o = dbd(dir, &url, &["apply", "--scope", "hub"]);
    assert!(o.status.success(), "{}", combined(&o));

    for args in [
        &["refresh"][..],
        &["policies", "--dry-run"],
        &["apply", "--dry-run"],
        &["deploy", "--dry-run"],
    ] {
        let o = dbd(dir, &url, args);
        assert!(!o.status.success(), "{args:?} on a hub database: {}", combined(&o));
        assert!(
            combined(&o).contains("pinned to scope 'hub'"),
            "{args:?} is refused by the scope guard, naming the pin: {}",
            combined(&o)
        );
    }

    for args in [
        &["refresh", "--scope", "hub"][..],
        &["policies", "--dry-run", "--scope", "hub"],
        &["apply", "--dry-run", "--scope", "hub"],
        &["deploy", "--dry-run", "--scope", "hub"],
        &["apply", "--dry-run", "--allow-scope-change"],
        &["deploy", "--dry-run", "--allow-scope-change"],
    ] {
        let o = dbd(dir, &url, args);
        assert!(o.status.success(), "{args:?} is let through: {}", combined(&o));
    }
}

// ── apply --dry-run names the pending migrations ────────────────────────────

/// `dbd <args>` with no database: no `-d`, and no `$DATABASE_URL` to fall back on.
fn dbd_offline(dir: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("dbd")
        .unwrap()
        .args(args)
        .args(["-c", dir.join("design.yaml").to_str().unwrap()])
        .env_remove("DATABASE_URL")
        .current_dir(dir)
        .output()
        .expect("dbd failed to run")
}

/// `apply --dry-run` listed the entities it would apply and never the pending
/// migrations — not even one that drops a table — though the run executes them
/// first. With a database to read the recorded version from, the preview lists
/// each migration step the run would take and the statements it runs, a drop
/// marked; with none, it says it cannot.
#[tokio::test]
async fn apply_dry_run_lists_the_pending_migrations() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    let legacy = dir.join("ddl/table/app/legacy.ddl");
    std::fs::write(
        &legacy,
        "set search_path to app;\ncreate table if not exists legacy (id integer primary key);\n",
    )
    .unwrap();
    let o = dbd(dir, &url, &["snapshot", "-n", "v1"]);
    assert!(o.status.success(), "{}", combined(&o));
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "{}", combined(&o));
    std::fs::remove_file(&legacy).unwrap();
    let o = dbd(dir, &url, &["snapshot", "-n", "drop legacy"]);
    assert!(o.status.success(), "{}", combined(&o));

    let o = dbd(dir, &url, &["apply", "--dry-run"]);
    assert!(o.status.success(), "apply --dry-run failed: {}", combined(&o));
    let out = stdout(&o);
    assert!(
        out.lines()
            .any(|l| l.contains("DROP TABLE") && l.contains("app.legacy") && l.trim_start().starts_with('⚠')),
        "the drop is listed, and marked: {out}"
    );
    assert!(
        out.lines().any(|l| l.contains("v2") && l.contains("app.legacy")),
        "under the version that drops it: {out}"
    );

    let o = dbd_offline(dir, &["apply", "--dry-run"]);
    assert!(o.status.success(), "the offline preview still runs: {}", combined(&o));
    assert!(
        combined(&o).contains("pending migrations are not listed"),
        "and says what it cannot show: {}",
        combined(&o)
    );
}

// ── An older project does not apply to a newer database ─────────────────────

/// A branch at v1 pointed at a database at v2 planned as "current": it
/// re-applied v1's DDL over v2's schema and printed "Already up to date (v2)".
/// `apply`, `deploy` and their dry runs given the database refuse it before
/// anything runs, naming both versions and the way out, as the scope and prod
/// guards refuse. The database keeps v2; an offline dry run, which cannot know
/// the version, still previews.
#[tokio::test]
async fn an_older_project_is_refused_on_a_newer_database() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    let o = dbd(dir, &url, &["snapshot", "-n", "v1"]);
    assert!(o.status.success(), "{}", combined(&o));
    let gadgets = dir.join("ddl/table/app/gadgets.ddl");
    std::fs::write(
        &gadgets,
        "set search_path to app;\ncreate table if not exists gadgets (id integer primary key);\n",
    )
    .unwrap();
    let o = dbd(dir, &url, &["snapshot", "-n", "v2"]);
    assert!(o.status.success(), "{}", combined(&o));
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "{}", combined(&o));

    // Check out the v1 revision: its design.yaml, snapshots and DDL.
    let yaml = std::fs::read_to_string(dir.join("design.yaml")).unwrap();
    assert!(yaml.contains("version: 2"), "{yaml}");
    std::fs::write(dir.join("design.yaml"), yaml.replace("version: 2", "version: 1")).unwrap();
    std::fs::remove_file(dir.join("snapshots/002.json")).unwrap();
    std::fs::remove_dir_all(dir.join("migrations/002")).unwrap();
    std::fs::remove_file(&gadgets).unwrap();

    for args in [
        &["apply"][..],
        &["deploy"],
        &["apply", "--dry-run"],
        &["deploy", "--dry-run"],
    ] {
        let o = dbd(dir, &url, args);
        assert!(!o.status.success(), "{args:?} must refuse: {}", combined(&o));
        let out = combined(&o);
        assert!(
            out.contains("at v2") && out.contains("at v1") && out.contains("newer revision"),
            "{args:?} names both versions and what to do: {out}"
        );
    }

    let o = dbd(dir, &url, &["migrate", "--status"]);
    assert!(
        stdout(&o).contains("Database version: 002"),
        "the database keeps v2: {}",
        combined(&o)
    );
    let o = dbd_offline(dir, &["apply", "--dry-run"]);
    assert!(o.status.success(), "offline, the preview still runs: {}", combined(&o));
}
