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

    // An ad-hoc `-n X -f F` load is an import under the scope too.
    std::fs::write(dir.join("nodes.jsonl"), "{\"id\": 1}\n").unwrap();
    let o = dbd(
        dir,
        &url,
        &["import", "-n", "hub.nodes", "-f", "nodes.jsonl", "--scope", "hub"],
    );
    assert!(o.status.success(), "{}", combined(&o));
    assert_eq!(
        scope_line(&o),
        Some(expected.clone()),
        "ad-hoc import:\n{}",
        combined(&o)
    );

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
    // Built under the scope it exports: export refuses a database pinned to
    // another scope, as apply does.
    assert!(dbd(dir, &url, &["apply", "--scope", "app_only"]).status.success());

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

/// A table the design declares `external:` is somebody else's — the Supabase
/// scaffold's `auth.users` is the usual one. `merge` wrote it into the project
/// as `ddl/table/auth/users.ddl`, after which `reset` planned `DROP TABLE
/// "auth"."users" CASCADE`; and it warned that schema `auth` — and the empty
/// `public` — were "written but not listed", when nothing of the project's
/// was written to either. A declared external is never captured, and a
/// schema is only reported for files actually written to it.
#[tokio::test]
async fn merge_never_captures_a_declared_external_and_names_only_schemas_it_wrote_to() {
    let (_pg, url) = start_pg().await;
    dbd_core::connect(&url, "platform")
        .await
        .unwrap()
        .execute_script("CREATE SCHEMA auth; CREATE TABLE auth.users (id uuid PRIMARY KEY);")
        .await
        .expect("build the database");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - app\n\n\
         external:\n  - name: auth.users\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("ddl/table/app/profiles.ddl"),
        "set search_path to app;\n\
         create table if not exists profiles (id uuid primary key references auth.users (id));\n",
    )
    .unwrap();
    let o = dbd(dir, &url, &["-e", "dev", "apply"]);
    assert!(o.status.success(), "apply failed: {}", combined(&o));

    let o = dbd(dir, &url, &["merge"]);
    assert!(o.status.success(), "merge failed: {}", combined(&o));
    let said = combined(&o);
    assert!(
        !dir.join("ddl/table/auth").exists(),
        "a declared external is not the project's to capture: {said}"
    );
    for schema in ["auth", "public"] {
        assert!(
            !said.contains(&format!("schema `{schema}` written")),
            "nothing was written to `{schema}`: {said}"
        );
    }
    assert!(dir.join("ddl/table/app/profiles.ddl").exists(), "{said}");
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
    std::fs::read_to_string(out.join("app/widgets.csv")).expect("export wrote app/widgets.csv")
}

// ── Where an export writes ──────────────────────────────────────────────────

/// `core.accounts` and `staging.accounts` — the shape of every project that
/// stages a live table under its own name.
fn same_named_tables_project(dir: &Path) {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: twins\n\n\
         source:\n  dialect: postgresql\n\nschemas:\n  - staging\n  - core\n",
    )
    .unwrap();
    for schema in ["staging", "core"] {
        let t = dir.join(format!("ddl/table/{schema}"));
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(
            t.join("accounts.ddl"),
            format!("set search_path to {schema};\ncreate table if not exists accounts (id integer, name text);\n"),
        )
        .unwrap();
    }
}

/// `-o dir` wrote every table flat as `dir/<table>.<fmt>`, so the second of
/// two same-named tables overwrote the first while the summary counted both.
/// Each table gets `dir/<schema>/<table>.<fmt>` — the layout of the default
/// `export/`, and the `import/<schema>/<table>` layout an import reads.
#[tokio::test]
async fn export_to_a_directory_keeps_same_named_tables_from_different_schemas_apart() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    same_named_tables_project(dir);
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "{}", combined(&o));
    std::fs::write(dir.join("staged.jsonl"), "{\"id\":1,\"name\":\"staged\"}\n").unwrap();
    std::fs::write(dir.join("live.jsonl"), "{\"id\":2,\"name\":\"live\"}\n").unwrap();
    for (table, file) in [("staging.accounts", "staged.jsonl"), ("core.accounts", "live.jsonl")] {
        let o = dbd(dir, &url, &["import", "-n", table, "-f", file]);
        assert!(o.status.success(), "{}", combined(&o));
    }

    let out = dir.join("out");
    let o = dbd(dir, &url, &["export", "-o", out.to_str().unwrap()]);
    assert!(o.status.success(), "{}", combined(&o));
    assert!(combined(&o).contains("Exporting 2 table(s)"), "{}", combined(&o));

    let staged = std::fs::read_to_string(out.join("staging/accounts.csv")).expect("staging/accounts.csv");
    let live = std::fs::read_to_string(out.join("core/accounts.csv")).expect("core/accounts.csv");
    assert_eq!(staged, "id,name\n1,staged\n");
    assert_eq!(live, "id,name\n2,live\n");
    assert!(!out.join("accounts.csv").exists(), "nothing is written flat");
}

/// The documented way to move a long-running system's data: export a live
/// table straight into a staging schema's import folder, so the new
/// database's import loads it there — `export -n core.accounts -o
/// import/staging` → `import/staging/accounts.csv` → `staging.accounts`. Once
/// `-o` grew a schema folder of its own (so same-named tables stay apart), the
/// file landed at `import/staging/core/accounts.csv`, a layout no import
/// reads, and the round trip loaded nothing. Into a schema folder of the
/// project's `import/` tree, a table is written as the import reads it; two
/// selected tables that would share a file there are refused, naming both,
/// with nothing written.
#[tokio::test]
async fn export_into_an_import_schema_folder_writes_what_the_import_loads() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    same_named_tables_project(dir);
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "{}", combined(&o));
    std::fs::write(dir.join("live.jsonl"), "{\"id\":2,\"name\":\"live\"}\n").unwrap();
    let o = dbd(dir, &url, &["import", "-n", "core.accounts", "-f", "live.jsonl"]);
    assert!(o.status.success(), "{}", combined(&o));

    let staging = dir.join("import/staging");
    let o = dbd(
        dir,
        &url,
        &["export", "-n", "core.accounts", "-o", staging.to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", combined(&o));
    assert_eq!(
        std::fs::read_to_string(staging.join("accounts.csv")).unwrap_or_default(),
        "id,name\n2,live\n",
        "written where the import loads it: {}",
        combined(&o)
    );
    let o = dbd(dir, &url, &["import", "--dry-run"]);
    assert!(o.status.success(), "{}", combined(&o));
    assert!(
        combined(&o).contains("staging.accounts") && combined(&o).contains("import/staging/accounts.csv"),
        "and the import plans it into staging.accounts: {}",
        combined(&o)
    );

    // Both `accounts` tables into one schema folder would be one file.
    std::fs::remove_file(staging.join("accounts.csv")).unwrap();
    let o = dbd(dir, &url, &["export", "-o", staging.to_str().unwrap()]);
    assert!(!o.status.success(), "two tables, one file — refused: {}", combined(&o));
    let said = combined(&o);
    assert!(
        said.contains("core.accounts") && said.contains("staging.accounts"),
        "naming both: {said}"
    );
    assert!(
        std::fs::read_dir(&staging).unwrap().next().is_none(),
        "with nothing written: {said}"
    );
}

/// `-s` says where the project is, and the default `export/` is the
/// project's — it was written under whatever directory dbd ran in.
#[tokio::test]
async fn the_default_export_directory_is_the_projects() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    project(&dir);
    assert!(dbd(&dir, &url, &["apply"]).status.success());

    let o = Command::cargo_bin("dbd")
        .unwrap()
        .args(["export", "-s", dir.to_str().unwrap(), "-d", &url])
        .current_dir(&elsewhere)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", combined(&o));
    assert!(
        dir.join("export/app/widgets.csv").exists(),
        "written under the project: {}",
        combined(&o)
    );
    assert!(!elsewhere.join("export").exists(), "and not where dbd ran");
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
    let (pg, url) = start_pg().await;
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
    // A dev database of its own: `-e` defaults to prod, and the prod guard must
    // not be what fires — and a dev run may not re-mark the prod one above.
    pg.create_database("pinned").await.unwrap();
    let url = pg.settings().url("pinned");
    let o = dbd(dir, &url, &["apply", "-e", "dev", "--scope", "hub"]);
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

// ── staging tables linked by a foreign key import together ──────────────────

/// Two staging tables, one referencing the other, each with a data file. The
/// import emptied them one `TRUNCATE` at a time, and Postgres refuses to
/// truncate a table another references — so the import failed on every run,
/// saying "cannot truncate a table referenced in a foreign key constraint"
/// without naming either table. Truncated together, with the referenced table
/// loaded first, they import; a table outside the import that still holds a
/// key to one is named, and nothing is emptied.
#[tokio::test]
async fn staging_tables_linked_by_a_foreign_key_import_together_and_a_blocker_is_named() {
    let (pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - staging\n  - app\n\n\
         import:\n  staging: [staging]\n",
    )
    .unwrap();
    let write = |rel: &str, body: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        "ddl/table/staging/parent.ddl",
        "set search_path to staging;\ncreate table if not exists parent (id int primary key);\n",
    );
    write(
        "ddl/table/staging/child.ddl",
        "set search_path to staging;\ncreate table if not exists child (id int primary key, pid int references parent (id));\n",
    );
    write("import/staging/parent.csv", "id\n1\n");
    write("import/staging/child.csv", "id,pid\n1,1\n");
    let o = dbd(dir, &url, &["-e", "dev", "apply"]);
    assert!(o.status.success(), "apply failed: {}", combined(&o));

    for run in 1..=2 {
        let o = dbd(dir, &url, &["-e", "dev", "import"]);
        assert!(o.status.success(), "import run {run} failed: {}", combined(&o));
    }

    // A live table outside the import holding a key to a staging table.
    write(
        "ddl/table/app/orders.ddl",
        "set search_path to app;\ncreate table if not exists orders (id int primary key, pid int references staging.parent (id));\n",
    );
    let o = dbd(dir, &url, &["-e", "dev", "apply"]);
    assert!(o.status.success(), "apply failed: {}", combined(&o));
    let o = dbd(dir, &url, &["-e", "dev", "import"]);
    assert!(!o.status.success(), "a truncate the database refuses fails the import");
    let said = combined(&o);
    assert!(
        said.contains("staging.parent") && said.contains("app.orders"),
        "naming the table it empties and the one whose key blocks it: {said}"
    );
    let o = psql(&pg, &url, &["-Atc", "SELECT count(*) FROM staging.child"]);
    assert_eq!(stdout(&o).trim(), "1", "and nothing was emptied: {said}");
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

// ── reconcile runs the design's grants and apply hooks ───────────────────────

/// `reconcile` is the pre-release `apply`, and a reconciled database must be the
/// one an apply builds. It never ran the design's grants or its `apply.before`
/// / `apply.after` hooks, so a role had no access to a schema it was granted
/// and a hook's setup or seed data never happened. The after-hook writes a table
/// only reconcile creates, so it can only succeed after the writes.
#[tokio::test]
async fn reconcile_runs_the_designs_grants_and_apply_hooks() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\nsource:\n  dialect: postgresql\n\
         schemas:\n  - app:\n      grants:\n        app_writer: [usage, select, insert]\n\
         target:\n  postgres:\n    url: $DATABASE_URL\n    roles:\n      - name: app_writer\n      - name: app_reader\n\
         \x20   grants:\n      app:\n        app_reader: [usage, select]\n\
         apply:\n  before:\n    - sql/before.sql\n  after:\n    - sql/after.sql\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("sql")).unwrap();
    std::fs::write(
        dir.join("sql/before.sql"),
        "create schema if not exists hooks;\ncreate table if not exists hooks.log (step text);\n\
         insert into hooks.log values ('before');\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sql/after.sql"),
        "insert into app.t (id) values (1);\ninsert into hooks.log values ('after');\n",
    )
    .unwrap();

    let o = dbd(dir, &url, &["reconcile"]);
    assert!(o.status.success(), "reconcile failed: {}", combined(&o));

    // Each predicate must hold, checked in the database through dbd-core's own
    // adapter: a DO block that raises names what is missing.
    let adapter = dbd_core::connect(&url, "clilive").await.unwrap();
    for (predicate, what) in [
        (
            "SELECT 1 WHERE (SELECT string_agg(step, ',' ORDER BY step DESC) FROM hooks.log) = 'before,after'",
            "both hooks' rows",
        ),
        (
            "SELECT 1 FROM app.t WHERE id = 1",
            "the after-hook's row in a table reconcile created",
        ),
        (
            "SELECT 1 WHERE has_schema_privilege('app_writer', 'app', 'USAGE') \
             AND has_schema_privilege('app_reader', 'app', 'USAGE')",
            "the schema grants",
        ),
    ] {
        let sql = format!(
            "DO $$ BEGIN IF NOT EXISTS ({predicate}) THEN RAISE EXCEPTION '{} missing'; END IF; END $$",
            what.replace('\'', "''")
        );
        adapter
            .execute_script(&sql)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e}\n{}", combined(&o)));
    }
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

/// `diff` and `migrate --status` only read, so a database pinned to another
/// scope is not refused — but they said nothing of the pin, and an unscoped
/// diff of a `hub` database listed everything `hub` does not build as missing,
/// with no word of why. Each names the pin in one line, and stays quiet where
/// the run's scope is the pinned one. Under `--json` the note goes to stderr,
/// leaving stdout the document.
#[tokio::test]
async fn diff_and_migrate_status_name_the_pin_of_a_database_pinned_to_another_scope() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);
    let o = dbd(dir, &url, &["apply", "--scope", "hub"]);
    assert!(o.status.success(), "{}", combined(&o));

    let mut failures: Vec<String> = Vec::new();
    for args in [&["diff"][..], &["migrate", "--status"]] {
        let o = dbd(dir, &url, args);
        let notes = combined(&o)
            .lines()
            .filter(|l| l.contains("pinned to scope 'hub'"))
            .count();
        if !o.status.success() || notes != 1 {
            failures.push(format!("{args:?} does not name the pin in one line: {}", combined(&o)));
        }
    }
    let o = dbd(dir, &url, &["diff", "--json"]);
    if serde_json::from_slice::<serde_json::Value>(&o.stdout).is_err()
        || !String::from_utf8_lossy(&o.stderr).contains("pinned to scope 'hub'")
    {
        failures.push(format!(
            "diff --json: stdout the document, the note on stderr: {}",
            combined(&o)
        ));
    }
    let o = dbd(dir, &url, &["diff", "--scope", "hub"]);
    if !o.status.success() || combined(&o).contains("pinned to scope") {
        failures.push(format!("diff --scope hub notes its own pin: {}", combined(&o)));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
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

// ── a dev run against a prod database is refused ────────────────────────────

/// The audit's repro: `apply -e prod`, then a dev run of apply, deploy,
/// reconcile or import. Each re-marked the database `dev`, so `reset` — which
/// refuses a prod database — went ahead. Each must exit non-zero naming the
/// database's env and the way out, and `reset` must stay refused.
#[tokio::test]
async fn a_dev_run_against_a_database_marked_prod_exits_non_zero_and_reset_stays_refused() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    std::fs::create_dir_all(dir.join("import/dev/app")).unwrap();
    std::fs::write(dir.join("import/dev/app/widgets.csv"), "id,name\n7,dev-seed\n").unwrap();

    let o = dbd(dir, &url, &["-e", "prod", "apply"]);
    assert!(o.status.success(), "the prod apply failed: {}", combined(&o));

    for command in [&["apply"][..], &["deploy"], &["reconcile"], &["import"]] {
        let args = [&["-e", "dev"][..], command].concat();
        let o = dbd(dir, &url, &args);
        assert!(
            !o.status.success(),
            "`dbd {}` must be refused on a prod database: {}",
            args.join(" "),
            combined(&o)
        );
        assert!(
            combined(&o).contains("marked prod") && combined(&o).contains("-e prod"),
            "`dbd {}` must name the env and what to do: {}",
            args.join(" "),
            combined(&o)
        );
    }

    let o = dbd(dir, &url, &["-e", "dev", "reset"]);
    assert!(
        !o.status.success() && combined(&o).contains("prod"),
        "reset must stay refused on the prod database: {}",
        combined(&o)
    );
    let csv = exported_widgets(dir, &url);
    assert!(!csv.contains("dev-seed"), "no dev seed data in prod: {csv}");
}

// ── promoting a dev database to prod is said ────────────────────────────────

/// `-e` defaults to prod, so a run without it promotes a dev database to
/// prod — allowed, since it only tightens what `reset` allows, but silent,
/// and every later `-e dev` run is refused. Each command that records the
/// environment says so when it promotes one, and only then.
#[tokio::test]
async fn a_run_that_promotes_a_dev_database_to_prod_says_so() {
    let (pg, _url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);

    let mut failures: Vec<String> = Vec::new();
    for command in ["apply", "deploy", "reconcile"] {
        pg.create_database(command).await.unwrap();
        let url = pg.settings().url(command);
        let o = dbd(dir, &url, &["-e", "dev", command]);
        assert!(o.status.success(), "-e dev {command}: {}", combined(&o));

        let o = dbd(dir, &url, &[command]);
        let out = combined(&o);
        if !o.status.success() || !(out.contains("was marked dev") && out.contains("now marked prod")) {
            failures.push(format!(
                "{command} without -e does not say it promoted the database: {out}"
            ));
        }
        let o = dbd(dir, &url, &[command]);
        if combined(&o).contains("was marked dev") {
            failures.push(format!(
                "{command} on a prod database speaks of a promotion: {}",
                combined(&o)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ── a file with no SET search_path is named by the runs that execute it ─────

/// A DDL file that states no `SET search_path` resolves its unqualified names
/// against a path nobody wrote into it. The design says that is never silent —
/// `inspect` counts these files and `apply`/`deploy` print them — but only
/// `inspect` did. The runs that execute the file, and their previews, name it.
#[tokio::test]
async fn apply_and_deploy_name_a_file_that_states_no_search_path() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    std::fs::write(
        dir.join("ddl/table/app/pathless.ddl"),
        "create table if not exists app.pathless (id integer primary key);\n",
    )
    .unwrap();

    let mut failures: Vec<String> = Vec::new();
    for args in [
        &["apply", "--dry-run"][..],
        &["apply"],
        &["deploy", "--dry-run"],
        &["deploy"],
    ] {
        let o = dbd(dir, &url, args);
        let out = combined(&o);
        if !o.status.success() {
            failures.push(format!("{args:?} failed: {out}"));
        } else if !out.contains("pathless.ddl states no `SET search_path`") {
            failures.push(format!("{args:?} does not name the file: {out}"));
        } else if out.contains("widgets.ddl states no") {
            failures.push(format!("{args:?} names a file that states one: {out}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ── a dry run checks every guard its real run checks ────────────────────────

/// The audit's repro. With a database to ask, `apply`, `deploy` and `import
/// --dry-run -e dev` on a database marked prod said "no issues" and exited 0
/// while the real runs refused; `import --dry-run` ignored the scope pin the
/// real import refuses; and an ad-hoc `import -n -f` — which loads data into
/// the database — skipped the environment guard altogether. Each must refuse
/// where its real run does.
#[tokio::test]
async fn a_dry_run_checks_every_guard_its_real_run_checks() {
    let (pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);
    for (schema, table) in [("app", "users"), ("hub", "nodes")] {
        std::fs::create_dir_all(dir.join(format!("import/dev/{schema}"))).unwrap();
        std::fs::write(dir.join(format!("import/dev/{schema}/{table}.csv")), "id\n7\n").unwrap();
    }
    std::fs::write(dir.join("users.csv"), "id\n8\n").unwrap();

    let mut failures: Vec<String> = Vec::new();

    // ── A database marked prod: the environment guard ──
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "the prod apply failed: {}", combined(&o));
    for args in [
        &["-e", "dev", "apply", "--dry-run"][..],
        &["-e", "dev", "deploy", "--dry-run"],
        &["-e", "dev", "import", "--dry-run"],
        &["-e", "dev", "import", "-n", "app.users", "-f", "users.csv", "--dry-run"],
        &["-e", "dev", "import", "-n", "app.users", "-f", "users.csv"],
    ] {
        // Collected, so one run's failure does not hide another's.
        let o = dbd(dir, &url, args);
        if o.status.success() || !(combined(&o).contains("marked prod") && combined(&o).contains("-e prod")) {
            failures.push(format!(
                "{args:?} not refused by the environment guard: {}",
                combined(&o)
            ));
        }
    }
    let rows = psql(&pg, &url, &["-tA", "-c", "select count(*) from app.users"]);
    if stdout(&rows).trim() != "0" {
        failures.push(format!("dev data loaded into the prod database: {}", stdout(&rows)));
    }

    // ── A database pinned to another scope: the scope pin ──
    pg.create_database("pinned").await.unwrap();
    let url = pg.settings().url("pinned");
    let o = dbd(dir, &url, &["-e", "dev", "apply", "--scope", "hub"]);
    assert!(o.status.success(), "apply --scope hub failed: {}", combined(&o));
    for args in [
        &["-e", "dev", "import", "--dry-run"][..],
        &["-e", "dev", "import", "-n", "app.users", "-f", "users.csv", "--dry-run"],
    ] {
        let o = dbd(dir, &url, args);
        if o.status.success() || !combined(&o).contains("pinned to scope 'hub'") {
            failures.push(format!("{args:?} not refused by the scope guard: {}", combined(&o)));
        }
    }
    let o = dbd(dir, &url, &["-e", "dev", "import", "--dry-run", "--scope", "hub"]);
    if !o.status.success() {
        failures.push(format!("its own scope does not preview: {}", combined(&o)));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ── a renamed project.name does not lift the guards ─────────────────────────

/// The audit's repro, through the binary: a production database pinned to
/// `hub`, then `project.name` renamed. Every guard read the record by name, so
/// `reset` with no `--force` dropped the tables, a dev run passed the env
/// guard, and an unscoped apply passed the scope pin. Each — and the dry runs
/// that check the same guards — must be refused, naming the project the
/// database records and the likely rename. `--force` and
/// `--allow-scope-change` are honoured as they are on the project's own mark.
#[tokio::test]
async fn a_renamed_project_is_refused_where_its_old_name_would_be() {
    let (pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scoped_project(dir);
    let o = dbd(dir, &url, &["apply", "--scope", "hub"]);
    assert!(o.status.success(), "the prod apply failed: {}", combined(&o));

    let yaml = std::fs::read_to_string(dir.join("design.yaml")).unwrap();
    std::fs::write(dir.join("design.yaml"), yaml.replace("name: scoped", "name: renamed")).unwrap();

    for args in [
        &["reset", "--scope", "hub"][..],
        &["reset", "--scope", "hub", "--dry-run"],
        &["-e", "dev", "apply", "--scope", "hub"],
        &["apply"],
        &["apply", "--dry-run"],
    ] {
        let o = dbd(dir, &url, args);
        assert!(
            !o.status.success(),
            "{args:?} must be refused for the renamed project: {}",
            combined(&o)
        );
        assert!(
            combined(&o).contains("'scoped'") && combined(&o).contains("project.name"),
            "{args:?} names the project the database records, and the likely rename: {}",
            combined(&o)
        );
    }
    assert_eq!(schemas_of(&pg, &url, "nodes"), "hub", "the reset dropped nothing");

    let o = dbd(dir, &url, &["reset", "--scope", "hub", "--dry-run", "--force"]);
    assert!(o.status.success(), "--force is honoured: {}", combined(&o));
    let o = dbd(dir, &url, &["apply", "--dry-run", "--allow-scope-change"]);
    assert!(o.status.success(), "--allow-scope-change is honoured: {}", combined(&o));
}

// ── init --from-db writes a project that applies ────────────────────────────

/// A database with every kind of object `init --from-db` captures.
const BROWNFIELD: &str = "\
    CREATE EXTENSION \"uuid-ossp\";
    CREATE EXTENSION citext;
    CREATE SCHEMA app;
    CREATE TYPE app.mood AS ENUM ('happy', 'sad');
    CREATE SEQUENCE app.ticket_seq START 100;
    CREATE TABLE app.regions (id int PRIMARY KEY, name citext NOT NULL);
    CREATE TABLE app.people (
      id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
      region_id int,
      mood app.mood,
      ticket int DEFAULT nextval('app.ticket_seq'),
      CONSTRAINT people_region_fk FOREIGN KEY (region_id) REFERENCES app.regions (id)
    );
    CREATE INDEX people_mood_idx ON app.people (mood);
    CREATE TABLE app.counters (id serial PRIMARY KEY, n int NOT NULL DEFAULT 0);
    CREATE VIEW app.people_v AS SELECT id, mood FROM app.people;
    CREATE MATERIALIZED VIEW app.mood_counts AS SELECT mood, count(*) AS n FROM app.people GROUP BY mood;
    CREATE INDEX mood_counts_idx ON app.mood_counts (mood);";

/// The documented brownfield path, end to end. `init --from-db` wrote
/// `ddl/extension/<name>.ddl` — a folder the scanner does not read, so
/// `inspect` and `apply` exited 1 on any database with an extension — and DDL
/// that was not idempotent (`create sequence`, `CREATE TABLE` with no
/// `IF NOT EXISTS`), so applying the project back to the database it came
/// from failed with `already exists`. The reversed project must inspect
/// clean, apply to its own database and to a fresh one, and leave no drift on
/// either.
#[tokio::test]
async fn a_reverse_engineered_project_applies_to_its_own_database_and_to_a_fresh_one_without_drift() {
    let (pg, url) = start_pg().await;
    dbd_core::connect(&url, "brownfield")
        .await
        .unwrap()
        .execute_script(BROWNFIELD)
        .await
        .expect("build the brownfield database");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let o = dbd(dir, &url, &["init", "--from-db"]);
    assert!(o.status.success(), "init --from-db failed: {}", combined(&o));
    assert!(
        !dir.join("ddl/extension").exists(),
        "extensions belong in design.yaml, not a folder the scanner does not read"
    );

    let o = dbd(dir, &url, &["inspect"]);
    assert!(
        o.status.success(),
        "inspect of the reversed project failed: {}",
        combined(&o)
    );

    // Back to the database it came from.
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "apply to its own database failed: {}", combined(&o));
    let o = dbd(dir, &url, &["diff", "--exit-code"]);
    assert!(o.status.success(), "drift on its own database: {}", combined(&o));

    // And to a database that has none of it.
    pg.create_database("fresh").await.unwrap();
    let fresh = pg.settings().url("fresh");
    let o = dbd(dir, &fresh, &["apply"]);
    assert!(o.status.success(), "apply to a fresh database failed: {}", combined(&o));
    let o = dbd(dir, &fresh, &["diff", "--exit-code"]);
    assert!(o.status.success(), "drift on the fresh database: {}", combined(&o));
}

// ── init --from-db says what it leaves out ──────────────────────────────────

/// On a database with Supabase's `auth` and `storage` schemas, the platform
/// schemas are left out — and `init --from-db` dropped them without a word,
/// and wrote nothing for the foreign key into `auth.users`. Every schema left
/// out is named with the reason, and the referenced table is declared
/// `external:`, so the project inspects clean.
#[tokio::test]
async fn init_from_db_names_every_schema_it_leaves_out_and_declares_what_a_foreign_key_reaches() {
    let (_pg, url) = start_pg().await;
    dbd_core::connect(&url, "platform")
        .await
        .unwrap()
        .execute_script(
            "CREATE SCHEMA auth; CREATE TABLE auth.users (id uuid PRIMARY KEY);
             CREATE SCHEMA storage; CREATE TABLE storage.objects (id uuid PRIMARY KEY);
             CREATE SCHEMA extensions; CREATE EXTENSION pgcrypto WITH SCHEMA extensions;
             CREATE SCHEMA audit; CREATE TABLE audit.log (id int PRIMARY KEY);
             CREATE SCHEMA app;
             CREATE TABLE app.profiles (id uuid PRIMARY KEY REFERENCES auth.users (id));",
        )
        .await
        .expect("build the database");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let o = dbd(dir, &url, &["init", "--from-db", "--exclude-schema", "audit"]);
    assert!(o.status.success(), "init --from-db failed: {}", combined(&o));
    let said = combined(&o);
    for (schema, why) in [
        ("auth", "Supabase"),
        ("storage", "Supabase"),
        ("extensions", "Supabase"),
        ("audit", "--exclude-schema"),
    ] {
        assert!(
            said.lines()
                .any(|l| l.contains(&format!("`{schema}`")) && l.contains(why)),
            "`{schema}` must be named as left out ({why}): {said}"
        );
    }
    assert!(
        said.lines().any(|l| l.contains("`pgcrypto`")),
        "an extension left out with its schema must be named too: {said}"
    );

    let config = dbd_core::config::read(&dir.join("design.yaml")).unwrap();
    let externals: Vec<&str> = config.external.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(externals, vec!["auth.users"]);

    let o = dbd(dir, &url, &["inspect"]);
    assert!(o.status.success(), "inspect failed: {}", combined(&o));
    assert!(
        !combined(&o).contains("auth.users"),
        "the declared external must resolve: {}",
        combined(&o)
    );
}

// ── init --from-db --schema declares or names what the capture needs ────────

/// `init --from-db --schema app` captured `app` and nothing it depends on:
/// the `citext` its columns use was left out with `public` (so the project
/// failed on a new database with `type "citext" does not exist`), and the
/// enum from `ref`, the function from `util` and the extension installed in
/// `extensions` were not mentioned at all. An extension the captured objects
/// use is declared — it is the database's, not a schema's — when it installs
/// into `public` or a captured schema; everything else they need from outside
/// is named, with what needs it, so the user can capture it too.
#[tokio::test]
async fn init_from_db_scoped_declares_the_extensions_its_capture_uses_and_names_what_else_it_needs() {
    let (pg, url) = start_pg().await;
    const OUTSIDE: &str = "CREATE SCHEMA extensions; CREATE EXTENSION pgcrypto WITH SCHEMA extensions; \
         CREATE SCHEMA ref; CREATE TYPE ref.mood AS ENUM ('ok', 'meh'); \
         CREATE SCHEMA util; CREATE FUNCTION util.prio() RETURNS int LANGUAGE sql AS 'SELECT 1';";
    dbd_core::connect(&url, "scoped")
        .await
        .unwrap()
        .execute_script(&format!(
            "CREATE EXTENSION citext; {OUTSIDE} \
             CREATE SCHEMA other; CREATE TABLE other.unused (id int); \
             CREATE SCHEMA app; \
             CREATE TABLE app.t (id int PRIMARY KEY, email citext, mood ref.mood DEFAULT 'ok', \
               prio int DEFAULT util.prio(), token bytea DEFAULT extensions.gen_random_bytes(8));"
        ))
        .await
        .expect("build the database");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let o = dbd(dir, &url, &["init", "--from-db", "--schema", "app"]);
    assert!(o.status.success(), "init --from-db failed: {}", combined(&o));
    let said = combined(&o);

    let config = dbd_core::config::read(&dir.join("design.yaml")).unwrap();
    let extensions: Vec<&str> = config.target["postgres"].extensions.iter().map(|e| e.name()).collect();
    assert_eq!(extensions, ["citext"], "the extension app's columns use: {said}");
    for (needed, by) in [("ref.mood", "app.t"), ("util.prio()", "app.t"), ("pgcrypto", "app.t")] {
        assert!(
            said.lines().any(|l| l.contains(needed) && l.contains(by)),
            "{needed}, needed by {by}, is named: {said}"
        );
    }
    assert!(
        !said.contains("other.unused"),
        "what nothing captured uses is not: {said}"
    );

    // A database that has what the report named takes the project.
    pg.create_database("fresh").await.unwrap();
    let fresh = pg.settings().url("fresh");
    dbd_core::connect(&fresh, "scoped")
        .await
        .unwrap()
        .execute_script(OUTSIDE)
        .await
        .unwrap();
    let o = dbd(dir, &fresh, &["-e", "dev", "apply"]);
    assert!(
        o.status.success(),
        "apply to a database with what was named: {}",
        combined(&o)
    );
    let o = dbd(dir, &fresh, &["diff", "--exit-code"]);
    assert!(o.status.success(), "drift on the fresh database: {}", combined(&o));
}

// ── reset says what it did ──────────────────────────────────────────────────

/// `dbd reset` printed nothing on success, so a reset that dropped the whole
/// schema and one that found nothing to drop looked the same. It counts what
/// it dropped, by kind — and says when there was nothing.
#[tokio::test]
async fn reset_reports_what_it_dropped() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    unversioned_scoped_project(dir);
    let o = dbd(dir, &url, &["-e", "dev", "apply"]);
    assert!(o.status.success(), "apply failed: {}", combined(&o));

    let o = dbd(dir, &url, &["-e", "dev", "reset"]);
    assert!(o.status.success(), "reset failed: {}", combined(&o));
    let said = stdout(&o);
    assert!(
        said.contains("dropped") && said.contains("1 table") && said.contains("1 view"),
        "reset must count what it dropped: {said}"
    );

    let o = dbd(dir, &url, &["-e", "dev", "reset"]);
    assert!(o.status.success(), "a second reset failed: {}", combined(&o));
    assert!(
        stdout(&o).contains("nothing"),
        "a reset that finds nothing to drop says so: {}",
        stdout(&o)
    );
}

// ── reset keeps a role another database holds, and says so ──────────────────

/// Roles are server-wide, so two databases built from one design share them.
/// `dbd reset` of one failed claiming the role "holds privileges dbd did not
/// grant", which dbd had granted — in the other database. The reset keeps the
/// role, completes, and says which role it kept and the database that holds it.
#[tokio::test]
async fn reset_keeps_a_role_another_database_holds_and_names_that_database() {
    let (pg, url) = start_pg().await;
    pg.create_database("other_db").await.unwrap();
    let other = pg.settings().url("other_db");
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(dir);
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\n\nsource:\n  dialect: postgresql\n\n\
         schemas:\n  - app:\n      grants:\n        live_writer: [usage, select]\n\n\
         target:\n  postgres:\n    roles:\n      - name: live_writer\n",
    )
    .unwrap();

    for db in [&url, &other] {
        let o = dbd(dir, db, &["-e", "dev", "apply"]);
        assert!(o.status.success(), "apply failed: {}", combined(&o));
    }

    let o = dbd(dir, &url, &["-e", "dev", "reset"]);
    assert!(o.status.success(), "reset failed: {}", combined(&o));
    assert!(
        stdout(&o)
            .lines()
            .any(|l| l.contains("live_writer") && l.contains("kept") && l.contains("other_db")),
        "reset must say which role it kept and which database holds it: {}",
        combined(&o)
    );
}

// ── Dry runs say what their run does ────────────────────────────────────────

/// `reconcile --dry-run` listed the schema changes and stopped there, though
/// the run goes on to its `apply.before` and `apply.after` hooks and the
/// design's grants. The preview now names them.
#[tokio::test]
async fn reconcile_dry_run_names_the_hooks_and_grants_the_run_applies() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: clilive\nsource:\n  dialect: postgresql\n\
         schemas:\n  - app:\n      grants:\n        app_writer: [usage, select]\n\
         target:\n  postgres:\n    roles:\n      - name: app_writer\n\
         apply:\n  before:\n    - sql/before.sql\n  after:\n    - sql/after.sql\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("sql")).unwrap();
    std::fs::write(dir.join("sql/before.sql"), "select 1;\n").unwrap();
    std::fs::write(dir.join("sql/after.sql"), "select 2;\n").unwrap();

    let o = dbd(dir, &url, &["reconcile", "--dry-run"]);
    assert!(o.status.success(), "reconcile --dry-run failed: {}", combined(&o));
    let out = stdout(&o);
    for named in ["sql/before.sql", "sql/after.sql", "app_writer"] {
        assert!(out.contains(named), "the preview names {named}: {out}");
    }
}

/// Under a scope that none of v2's changes touch, `apply --dry-run` said "No
/// pending migration to run (database at v1, design at v2)", and the run then
/// said "Migrated v1 → v2". The preview now says what the run does: v2 is
/// pending, has nothing to run in this scope, and is recorded.
#[tokio::test]
async fn a_scoped_dry_run_says_the_run_records_a_migration_it_has_nothing_to_run_for() {
    let (_pg, url) = start_pg().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let write = |people: &str| {
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: clilive\nsource:\n  dialect: postgresql\nschemas:\n  - app\n  - core\n\
             scopes:\n  core_only:\n    includes: [core]\n",
        )
        .unwrap();
        for d in ["ddl/table/app", "ddl/table/core"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(
            dir.join("ddl/table/app/people.ddl"),
            format!("set search_path to app;\ncreate table if not exists people (id integer primary key{people});\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/core/zones.ddl"),
            "set search_path to core;\ncreate table if not exists zones (id integer primary key);\n",
        )
        .unwrap();
    };
    write("");
    let o = dbd(dir, &url, &["snapshot", "--name", "v1"]);
    assert!(o.status.success(), "snapshot v1: {}", combined(&o));
    let o = dbd(dir, &url, &["apply", "--scope", "core_only"]);
    assert!(o.status.success(), "apply v1: {}", combined(&o));
    write(", nick text");
    let o = dbd(dir, &url, &["snapshot", "--name", "v2"]);
    assert!(o.status.success(), "snapshot v2: {}", combined(&o));

    let o = dbd(dir, &url, &["apply", "--scope", "core_only", "--dry-run"]);
    assert!(o.status.success(), "dry run: {}", combined(&o));
    let preview = stdout(&o);
    assert!(!preview.contains("No pending migration"), "{preview}");
    assert!(preview.contains("v2"), "the preview names v2: {preview}");

    let o = dbd(dir, &url, &["apply", "--scope", "core_only"]);
    assert!(o.status.success(), "apply v2: {}", combined(&o));
    assert!(stdout(&o).contains("Migrated v1 → v2"), "{}", stdout(&o));
}

// ── Awkward names through DBML and a real database ─────────────────────────

/// The characters a quoted identifier may hold that DBML or SQL must escape,
/// or that a splitter may take for a delimiter.
const AWKWARD: [&str; 6] = ["\"", "\\", " ", ".", "'", "é名"];

/// A project whose every name holds one of [`AWKWARD`], one schema per
/// character: an enum, two tables, named primary, foreign, UNIQUE and CHECK
/// keys, an index with an INCLUDE column, notes and a default holding it.
///
/// A schema, table or enum name is a file or directory name too: `path_safe`
/// refuses a backslash in one by design, and dbd's model joins a schema and a
/// name with a dot and splits them apart again throughout, so those two are
/// left out of those three kinds of name and kept in every other.
///
/// The default leaves a backslash out too: the deparser spells `'d\'` as
/// `E'd\\'` and Postgres reports `'d\'`, which `reconcile::canonical_default`
/// does not settle, so any such default reads as drift — DBML or not.
fn awkward_project(dir: &Path) {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let lit = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut schemas = Vec::new();
    for (i, w) in AWKWARD.iter().enumerate() {
        let file_name = |base: &str| {
            if *w == "\\" || *w == "." {
                format!("{base}_{i}")
            } else {
                format!("{base}{w}")
            }
        };
        let schema = file_name("s");
        let (enum_name, parent, child) = (file_name("e"), file_name("p"), file_name("c"));
        let (s, e, p, c) = (q(&schema), q(&enum_name), q(&parent), q(&child));
        let name = |base: &str| q(&format!("{base}{w}"));
        let files = [
            (
                format!("ddl/enum/{schema}/{enum_name}.ddl"),
                format!(
                    "create type {s}.{e} as enum ({}, {});\n",
                    lit(&format!("v{w}")),
                    lit(&format!("it's {w}"))
                ),
            ),
            (
                format!("ddl/table/{schema}/{parent}.ddl"),
                format!(
                    "create table if not exists {s}.{p} ({id} int, constraint {pk} primary key ({id}));\n",
                    id = name("id"),
                    pk = name("pk")
                ),
            ),
            (
                format!("ddl/table/{schema}/{child}.ddl"),
                format!(
                    "create table if not exists {s}.{c} (\n  \
                       id int,\n  \
                       {r} int not null,\n  \
                       {m} {s}.{e},\n  \
                       {n} text not null default {d},\n  \
                       constraint {cpk} primary key (id),\n  \
                       constraint {fk} foreign key ({r}) references {s}.{p} ({pid}) on delete cascade,\n  \
                       constraint {uq} unique ({n}),\n  \
                       constraint {ck} check (char_length({n}) > 0)\n\
                     );\n\
                     create index if not exists {ix} on {s}.{c} ({n}, {m}) include ({r});\n\
                     comment on table {s}.{c} is {tnote};\n\
                     comment on column {s}.{c}.{n} is {cnote};\n",
                    r = name("ref"),
                    m = name("m"),
                    n = name("n"),
                    d = lit(&format!("d{}", if *w == "\\" { "" } else { w })),
                    cpk = name("cpk"),
                    fk = name("fk"),
                    pid = name("id"),
                    uq = name("uq"),
                    ck = name("ck"),
                    ix = name("ix"),
                    tnote = lit(&format!("table {w} \" odd")),
                    cnote = lit(&format!("note {w} \" odd")),
                ),
            ),
        ];
        for (path, sql) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, sql).unwrap();
        }
        schemas.push(format!(
            "  - \"{}\"\n",
            schema.replace('\\', "\\\\").replace('"', "\\\"")
        ));
    }
    std::fs::write(
        dir.join("design.yaml"),
        format!(
            "project:\n  name: awkward\nsource:\n  dialect: postgresql\nschemas:\n{}",
            schemas.concat()
        ),
    )
    .unwrap();
}

/// Names holding a quote, a backslash, a space, a dot, a single quote or
/// non-ASCII text survive `dbml` → `init --from-dbml` → `apply`: the project
/// the document rebuilds inspects clean, applies to a real database, and the
/// database then matches it. The enum type was written in SQL quoting, the
/// rebuilt table lost its primary key to a `"` inside a `'…'` setting, and
/// apply failed on an index over a column named `id [pk, name: …]`.
#[tokio::test]
async fn awkward_names_survive_dbml_init_from_dbml_and_a_real_apply() {
    let (_pg, url) = start_pg().await;
    let source = tempfile::tempdir().unwrap();
    awkward_project(source.path());
    let o = dbd_offline(source.path(), &["inspect"]);
    assert!(o.status.success(), "the source project is valid: {}", combined(&o));
    let o = dbd_offline(source.path(), &["dbml", "-f", "design.dbml"]);
    assert!(o.status.success(), "dbml: {}", combined(&o));
    let dbml = std::fs::read_to_string(source.path().join("design.dbml")).unwrap();

    let rebuilt = tempfile::tempdir().unwrap();
    let dir = rebuilt.path();
    std::fs::write(dir.join("in.dbml"), &dbml).unwrap();
    let o = Command::cargo_bin("dbd")
        .unwrap()
        .args(["init", "--from-dbml", "in.dbml", "-n", "awkward"])
        .env_remove("DATABASE_URL")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "init --from-dbml: {}\n{dbml}", combined(&o));
    let o = dbd_offline(dir, &["inspect"]);
    assert!(o.status.success(), "inspect: {}\n{dbml}", combined(&o));
    let o = dbd(dir, &url, &["apply"]);
    assert!(o.status.success(), "apply: {}\n{dbml}", combined(&o));
    let o = dbd(dir, &url, &["diff", "--exit-code"]);
    assert!(o.status.success(), "diff: {}\n{dbml}", combined(&o));
}

/// A project declaring an entity another owns is refused by the commands that
/// claim nothing, too — `refresh` and `policies` — naming the entity and its
/// owner, before either touches the database (#7).
#[tokio::test]
async fn refresh_and_policies_refuse_a_design_declaring_another_owners_entity() {
    let (_pg, url) = start_pg().await;
    let owner = tempfile::tempdir().unwrap();
    project(owner.path());
    let applied = dbd(owner.path(), &url, &["apply"]);
    assert!(applied.status.success(), "{}", combined(&applied));

    let intruder = tempfile::tempdir().unwrap();
    let dir = intruder.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: intruder\n\nsource:\n  dialect: postgresql\n\nschemas:\n  - app\n",
    )
    .unwrap();
    for (rel, sql) in [
        (
            "ddl/table/app/widgets.ddl",
            "set search_path to app;\ncreate table if not exists widgets (id integer primary key);\n",
        ),
        (
            "ddl/materialized_view/app/counts.ddl",
            "create materialized view if not exists app.counts as select count(*) as n from app.widgets with data;\n",
        ),
        (
            "policies/app/widgets.sql",
            "alter table app.widgets enable row level security;\n",
        ),
    ] {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, sql).unwrap();
    }

    for command in ["refresh", "policies"] {
        let out = dbd(dir, &url, &[command]);
        assert!(!out.status.success(), "{command} succeeded: {}", combined(&out));
        assert!(
            combined(&out).contains("relation app.widgets is owned by \"clilive\""),
            "{command} did not name the entity and its owner: {}",
            combined(&out)
        );
    }
}
