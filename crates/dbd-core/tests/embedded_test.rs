//! Integration tests using embedded PostgreSQL.
//!
//! These tests run a full deploy/apply/migrate cycle against a real PostgreSQL
//! instance spun up in-process. No external database required.
//!
//! Run with:
//!   cargo test --features embedded-tests --test embedded_test
//!
//! The first run downloads the PostgreSQL binary (~50 MB, cached in ~/.cache).

#![cfg(feature = "embedded-tests")]

use std::path::PathBuf;

use dbd_core::design::{ApplyStrategy, DeployComplete, Progress};
use dbd_core::{Design, Entity, EntityType, connect};
use postgresql_embedded::{PostgreSQL, Settings};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/embedded")
}

/// Start an embedded PostgreSQL instance and return it with a connection URL.
/// Each test gets its own instance for full isolation.
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

/// Load the embedded fixture design.
fn load_design() -> Design {
    let config = fixture_dir().join("design.yaml");
    Design::from_config_with_dir(&config, "dev", Some(&fixture_dir())).expect("failed to load embedded fixture design")
}

/// Run a catalog existence assertion via an anonymous `DO` block. Raises (and
/// so fails the test) when `predicate` matches in the wrong direction:
/// `should_exist == true` fails when it finds nothing, `false` fails when it
/// finds something. `subject` is the human label, e.g. "table config.lookups".
async fn assert_catalog(adapter: &dyn dbd_core::DatabaseAdapter, should_exist: bool, predicate: &str, subject: &str) {
    let (guard, verb) = if should_exist {
        ("NOT EXISTS", "does not exist")
    } else {
        ("EXISTS", "unexpectedly exists")
    };
    let sql = format!(
        "DO $$ BEGIN \
           IF {guard} ( {predicate} ) \
           THEN RAISE EXCEPTION '{subject} {verb}'; \
           END IF; \
         END $$"
    );
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("assert_catalog({subject}) failed: {e}"));
}

fn table_predicate(schema: &str, table: &str) -> String {
    format!("SELECT 1 FROM pg_catalog.pg_tables WHERE schemaname = '{schema}' AND tablename = '{table}'")
}

fn column_predicate(schema: &str, table: &str, column: &str) -> String {
    format!(
        "SELECT 1 FROM information_schema.columns \
         WHERE table_schema = '{schema}' AND table_name = '{table}' AND column_name = '{column}'"
    )
}

/// Assert that a table exists; panics with a clear message if it doesn't.
async fn assert_table_exists(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str) {
    assert_catalog(
        adapter,
        true,
        &table_predicate(schema, table),
        &format!("table {schema}.{table}"),
    )
    .await;
}

/// Assert that a table does NOT exist; panics if it does.
async fn assert_table_absent(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str) {
    assert_catalog(
        adapter,
        false,
        &table_predicate(schema, table),
        &format!("table {schema}.{table}"),
    )
    .await;
}

/// Assert that a schema exists; panics if it doesn't.
async fn assert_schema_exists(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str) {
    let pred = format!("SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = '{schema}'");
    assert_catalog(adapter, true, &pred, &format!("schema {schema}")).await;
}

/// Assert that a schema is absent; panics if it exists.
async fn assert_schema_absent(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str) {
    let pred = format!("SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = '{schema}'");
    assert_catalog(adapter, false, &pred, &format!("schema {schema}")).await;
}

/// Assert that a column exists on a table; panics if it doesn't.
async fn assert_column_exists(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str, column: &str) {
    let pred = column_predicate(schema, table, column);
    assert_catalog(adapter, true, &pred, &format!("column {schema}.{table}.{column}")).await;
}

/// Assert that a column does NOT exist on a table; panics if it does.
async fn assert_column_absent(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str, column: &str) {
    let pred = column_predicate(schema, table, column);
    assert_catalog(adapter, false, &pred, &format!("column {schema}.{table}.{column}")).await;
}

// ── Test 1: Fresh deploy ──────────────────────────────────────────────────────

#[tokio::test]
async fn fresh_deploy_creates_schema() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    // Postgres/Supabase is the one target with a SQL grant model.
    assert!(adapter.supports_schema_grants());
    let design = load_design();

    let mut summary: Option<DeployComplete> = None;
    design
        .deploy(&*adapter, false, None, |s| summary = Some(s))
        .await
        .expect("deploy failed");

    let s = summary.unwrap();
    assert_eq!(s.apply.strategy, ApplyStrategy::Fresh, "first deploy should be Fresh");
    assert!(s.apply.applied > 0, "should have applied entities");
    assert_eq!(s.apply.from_version, 0, "db starts at v0");
    assert_eq!(s.apply.to_version, 2, "design is at v2");

    assert_table_exists(&*adapter, "app", "items").await;
    assert_table_exists(&*adapter, "app", "orders").await;
}

// ── Test 2: Idempotent redeploy ───────────────────────────────────────────────

#[tokio::test]
async fn redeploy_is_idempotent_and_current() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    design
        .deploy(&*adapter, false, None, |_| {})
        .await
        .expect("first deploy failed");

    let mut summary: Option<DeployComplete> = None;
    design
        .deploy(&*adapter, false, None, |s| summary = Some(s))
        .await
        .expect("second deploy failed");

    let s = summary.unwrap();
    assert_eq!(
        s.apply.strategy,
        ApplyStrategy::Current,
        "second deploy should be Current (already up to date)"
    );
    assert_eq!(
        s.apply.from_version, s.apply.to_version,
        "versions should match on redeploy"
    );
}

// ── Test 3: Tables are queryable after deploy ─────────────────────────────────

#[tokio::test]
async fn deployed_tables_accept_data() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    design
        .deploy(&*adapter, false, None, |_| {})
        .await
        .expect("deploy failed");

    adapter
        .execute_script("INSERT INTO app.items (name, quantity) VALUES ('widget', 10)")
        .await
        .expect("insert into items failed");

    adapter
        .execute_script(
            "INSERT INTO app.orders (item_id, amount) \
             SELECT id, 2 FROM app.items WHERE name = 'widget'",
        )
        .await
        .expect("insert into orders failed");

    // Verify FK is enforced
    let fk_violation = adapter
        .execute_script(
            "INSERT INTO app.orders (item_id, amount) \
             VALUES (gen_random_uuid(), 1)",
        )
        .await;
    assert!(fk_violation.is_err(), "FK constraint should be enforced");
}

// ── Test: COPY import honors the configured null_value sentinel ───────────────

#[tokio::test]
async fn import_data_honors_null_value_sentinel() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    adapter
        .execute_script("CREATE TABLE null_sentinel (id INTEGER, note TEXT)")
        .await
        .expect("create table failed");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.csv");
    // Row 1's `note` cell is the configured sentinel (`\N`) → should load as
    // SQL NULL. Row 2's `note` cell is an empty string, which with a non-empty
    // sentinel configured must load as a literal empty string, NOT NULL.
    std::fs::write(&path, "id,note\n1,\\N\n2,\n").expect("write fixture csv failed");

    let mut entity = Entity::new(EntityType::Table, "null_sentinel");
    entity.file = Some(path);
    entity.format = Some("csv".to_string());

    adapter.import_data(&entity, "\\N", false).await.expect("import failed");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM null_sentinel WHERE id = 1 AND note IS NULL",
        "row 1 (sentinel cell → NULL)",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM null_sentinel WHERE id = 2 AND note = ''",
        "row 2 (empty cell → literal empty string)",
    )
    .await;
}

// ── Test: a CSV/TSV loads by its header, not by position ──────────────────────

/// Create `staging.countries (code text, name text)` and load `body` into it as
/// a `format` data file.
async fn load_countries(
    adapter: &dyn dbd_core::DatabaseAdapter,
    format: &str,
    body: &str,
) -> dbd_core::error::Result<()> {
    adapter
        .execute_script("CREATE SCHEMA IF NOT EXISTS staging; CREATE TABLE staging.countries (code text, name text)")
        .await
        .expect("create table failed");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("countries.{format}"));
    std::fs::write(&path, body).unwrap();
    let mut entity = Entity::new(EntityType::Table, "staging.countries");
    entity.file = Some(path);
    entity.format = Some(format.to_string());
    adapter.import_data(&entity, "", false).await
}

/// COPY skipped the header (`HEADER true`) without reading it, so each value
/// went into the column at its position: a file headed `name,code` loaded
/// `Germany` into `code`. SQLite maps by header name, so the same file loaded
/// differently on the two engines. The header is a column list, quoting and
/// all.
#[tokio::test]
async fn a_csv_loads_each_value_into_the_column_its_header_names() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    load_countries(&*adapter, "csv", "\"name\",code\nGermany,DE\n\"Japan, Nippon\",JP\n")
        .await
        .expect("import failed");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM staging.countries WHERE code = 'DE' AND name = 'Germany'",
        "Germany under name, DE under code",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM staging.countries WHERE code = 'JP' AND name = 'Japan, Nippon'",
        "a quoted value under the column its header names",
    )
    .await;
}

/// TSV is the same load with a tab between columns.
#[tokio::test]
async fn a_tsv_loads_each_value_into_the_column_its_header_names() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    load_countries(&*adapter, "tsv", "name\tcode\nGermany\tDE\n")
        .await
        .expect("import failed");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM staging.countries WHERE code = 'DE' AND name = 'Germany'",
        "Germany under name, DE under code",
    )
    .await;
}

/// Windows line endings: the rows are handed to COPY from just after the
/// header's `\r\n`. Cut after the `\r`, they began with a stray `\n` that COPY
/// reads as a row of its own.
#[tokio::test]
async fn a_csv_with_crlf_line_endings_loads() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    load_countries(&*adapter, "csv", "name,code\r\nGermany,DE\r\n")
        .await
        .expect("import failed");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM staging.countries WHERE code = 'DE' AND name = 'Germany'",
        "Germany under name, DE under code",
    )
    .await;
}

/// A header names a column as Postgres resolves an unquoted name: `Name` is
/// `name`. A spreadsheet's capitalised header loaded before, by position; it
/// must not be refused now that the header is read.
#[tokio::test]
async fn a_header_names_a_column_as_postgres_resolves_an_unquoted_name() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    load_countries(&*adapter, "csv", "Name,CODE\nGermany,DE\n")
        .await
        .expect("import failed");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM staging.countries WHERE code = 'DE' AND name = 'Germany'",
        "Germany under name, DE under code",
    )
    .await;
}

/// A header column the table does not have is a file for another table, or a
/// renamed column — loading the rest by position or dropping it would both
/// load wrong data. Refused, naming the column and what the table has, before
/// a row is loaded.
#[tokio::test]
async fn a_header_column_the_table_does_not_have_is_refused() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    let err = load_countries(&*adapter, "csv", "name,iso_code\nGermany,DE\n")
        .await
        .expect_err("an unknown header column must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("iso_code") && msg.contains("staging.countries") && msg.contains("code, name"),
        "the column, the table, and the columns it has: {msg}"
    );
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM staging.countries",
        "a row loaded from a refused file",
    )
    .await;
}

// ── Test: the default import empties a staging table before loading it ────────

/// `truncate: true` is the default. The step moved behind
/// `DatabaseAdapter::truncate_table` so SQLite and Convex could run it their
/// own way; this pins that Postgres still empties the table.
#[tokio::test]
async fn the_default_import_truncates_a_staging_table_on_postgres() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("design.yaml"), "project:\n  name: t\n").unwrap();
    std::fs::create_dir_all(dir.path().join("import/stage")).unwrap();
    std::fs::write(dir.path().join("import/stage/items.csv"), "id,name\n1,fresh\n").unwrap();
    let design = Design::from_config_with_dir(&dir.path().join("design.yaml"), "dev", Some(dir.path())).unwrap();

    adapter
        .execute_script(
            "CREATE SCHEMA stage; CREATE TABLE stage.items (id integer, name text); \
             INSERT INTO stage.items VALUES (99, 'stale');",
        )
        .await
        .unwrap();

    design
        .import_data(&*adapter, None, false, None, Progress::none())
        .await
        .expect("import failed");

    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM stage.items WHERE id = 99",
        "the stale row",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM stage.items WHERE id = 1 AND name = 'fresh'",
        "the row loaded from the file",
    )
    .await;
}

// ── Test: a function reading a staging table does not fail the import ─────────

/// The import runs each loader with `CALL`, and Postgres refuses `CALL` on a
/// function ("is not a procedure"). A helper function that merely reads a
/// staging table was matched as its loader, so the whole import failed.
#[tokio::test]
async fn a_function_reading_a_staging_table_does_not_fail_the_import() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let write = |rel: &str, body: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write("design.yaml", "project:\n  name: t\nschemas:\n  - stage\n");
    write(
        "ddl/table/stage/items.ddl",
        "set search_path to stage;\ncreate table if not exists items (id integer, name text);\n",
    );
    write(
        "ddl/function/stage/item_count.ddl",
        "set search_path to stage;\n\
         create or replace function item_count() returns bigint language sql\n\
         as $$ select count(*) from stage.items $$;\n",
    );
    write("import/stage/items.csv", "id,name\n1,a\n");
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");
    design
        .import_data(&*adapter, None, false, None, Progress::none())
        .await
        .expect("a helper function must not fail the import");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM stage.items WHERE id = 1",
        "the staged row",
    )
    .await;
}

// ── Test: each staging table's own loader runs ────────────────────────────────

/// The audit's repro on a real database. `load_cities` joins the staged
/// countries to find each city's country, and loader matching took the first
/// procedure reading a staging table — so both staged tables got
/// `load_cities`, it ran twice, `load_countries` never ran, and both targets
/// stayed empty under a summary of "2 procedure(s) called".
#[tokio::test]
async fn each_staging_table_is_loaded_by_its_own_loader() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let write = |rel: &str, body: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    write(
        "design.yaml",
        "project:\n  name: t\nschemas:\n  - staging\n  - app\nimport:\n  staging: [staging]\n",
    );
    write(
        "ddl/table/staging/countries.ddl",
        "set search_path to staging;\ncreate table if not exists countries (code text, name text);\n",
    );
    write(
        "ddl/table/staging/cities.ddl",
        "set search_path to staging;\ncreate table if not exists cities (name text, country text);\n",
    );
    write(
        "ddl/table/app/countries.ddl",
        "set search_path to app;\ncreate table if not exists countries (code text primary key, name text);\n",
    );
    write(
        "ddl/table/app/cities.ddl",
        "set search_path to app;\n\
         create table if not exists cities (name text, country_code text references app.countries (code));\n",
    );
    write(
        "ddl/procedure/staging/load_countries.ddl",
        "set search_path to staging;\n\
         create or replace procedure load_countries() language sql\n\
         as $$ insert into app.countries (code, name) select code, name from staging.countries $$;\n",
    );
    write(
        "ddl/procedure/staging/load_cities.ddl",
        "set search_path to staging;\n\
         create or replace procedure load_cities() language sql\n\
         as $$ insert into app.cities (name, country_code)\n\
               select c.name, k.code from staging.cities c\n\
               join staging.countries s on s.name = c.country\n\
               join app.countries k on k.code = s.code $$;\n",
    );
    write("import/staging/countries.csv", "code,name\nDE,Germany\n");
    write("import/staging/cities.csv", "name,country\nBerlin,Germany\n");
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap();

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");
    design
        .import_data(&*adapter, None, false, None, Progress::none())
        .await
        .expect("import failed");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.countries WHERE code = 'DE' AND name = 'Germany'",
        "the country load_countries moves",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.cities WHERE name = 'Berlin' AND country_code = 'DE'",
        "the city load_cities moves",
    )
    .await;
}

// ── Test: a `.json` file may hold a JSON array ────────────────────────────────

/// A `.json` file is what most tools mean by one: a JSON array of records. The
/// Postgres load split every file into lines and parsed each as a record, so a
/// pretty-printed array failed on its opening `[`.
#[tokio::test]
async fn a_json_array_file_imports_one_row_per_record() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script("CREATE SCHEMA app; CREATE TABLE app.people (id integer, name text);")
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("people.json");
    std::fs::write(
        &path,
        "[\n  {\"id\": 1, \"name\": \"alpha\"},\n  {\"id\": 2, \"name\": \"beta\"}\n]\n",
    )
    .unwrap();
    let mut entity = Entity::new(EntityType::Table, "app.people");
    entity.file = Some(path);
    entity.format = Some("json".to_string());

    adapter
        .import_data(&entity, "", false)
        .await
        .expect("a JSON array must import");

    for (id, name) in [(1, "alpha"), (2, "beta")] {
        assert_catalog(
            &*adapter,
            true,
            &format!("SELECT 1 FROM app.people WHERE id = {id} AND name = '{name}'"),
            &format!("record {id}"),
        )
        .await;
    }
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 WHERE (SELECT count(*) FROM app.people) <> 2",
        "a row count other than 2",
    )
    .await;
}

// ── Test: a JSON import leaves no schema the design did not declare ───────────

/// dbd's JSON loader is its own machinery, and it was installed in a schema
/// called `staging` whatever the project's staging schemas are called — so a
/// JSON import created a schema the design never declared, which the next
/// `reconcile --prune` or `diff` then had to account for.
#[tokio::test]
async fn a_json_import_creates_no_schema_the_design_does_not_declare() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script("CREATE SCHEMA app; CREATE TABLE app.people (id integer, name text);")
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("people.jsonl");
    std::fs::write(&path, "{\"id\": 1, \"name\": \"alpha\"}\n").unwrap();
    let mut entity = Entity::new(EntityType::Table, "app.people");
    entity.file = Some(path);
    entity.format = Some("jsonl".to_string());

    adapter.import_data(&entity, "", false).await.expect("the import runs");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.people WHERE id = 1 AND name = 'alpha'",
        "the imported record",
    )
    .await;
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM pg_namespace WHERE nspname = 'staging'",
        "a `staging` schema the design never declared",
    )
    .await;
}

// ── Test: a Postgres CSV/TSV export imports into SQLite unchanged ─────────────

/// `dbd export` from Postgres and `dbd import` into SQLite is how data moves
/// between the engines, so every value must arrive as it left. Postgres writes
/// real CSV — a comma or a newline quoted, a quote doubled, NULL as an empty
/// field and `''` as `""` — and SQLite split each line on the delimiter and
/// trimmed: the comma was "too many columns", quotes stayed in the value.
#[tokio::test]
async fn a_postgres_export_imports_into_sqlite_unchanged() {
    let (_pg, url) = start_pg().await;
    let pg = connect(&url, "embedded_test").await.unwrap();
    // Everything CSV quoting exists for, plus the NULL/'' distinction.
    let expected: Vec<(i64, Option<String>)> = [
        (1, Some("Japan, Nippon")),
        (2, Some("say \"hi\"")),
        (3, Some("two\nlines")),
        (4, Some("a\ttab")),
        (5, Some("  padded  ")),
        (6, Some("")),
        (7, None),
    ]
    .iter()
    .map(|(id, v)| (*id, v.map(str::to_string)))
    .collect();
    pg.execute_script(
        r#"CREATE SCHEMA app;
           CREATE TABLE app.countries (id integer, name text);
           INSERT INTO app.countries VALUES
             (1, 'Japan, Nippon'), (2, 'say "hi"'), (3, E'two\nlines'), (4, E'a\ttab'),
             (5, '  padded  '), (6, ''), (7, NULL);"#,
    )
    .await
    .unwrap();

    for format in ["csv", "tsv"] {
        let out = tempfile::tempdir().unwrap();
        let mut export = Entity::new(EntityType::Table, "app.countries");
        export.format = Some(format.to_string());
        pg.export_data(&export, Some(out.path())).await.expect("export failed");

        let db = out.path().join("target.db");
        let sqlite_url = format!("sqlite://{}", db.display());
        let sqlite = connect(&sqlite_url, "embedded_test").await.unwrap();
        sqlite
            .execute_script("CREATE TABLE countries (id INTEGER, name TEXT)")
            .await
            .unwrap();
        let mut import = Entity::new(EntityType::Table, "app.countries");
        import.file = Some(out.path().join(format!("countries.{format}")));
        import.format = Some(format.to_string());
        sqlite
            .import_data(&import, "", false)
            .await
            .unwrap_or_else(|e| panic!("{format}: SQLite import failed: {e}"));

        use sqlx::Connection as _;
        let mut conn = sqlx::SqliteConnection::connect(&sqlite_url).await.unwrap();
        let got: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id, name FROM countries ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        assert_eq!(got, expected, "{format}: the values Postgres exported");
    }
}

// ── Test: JSONL export → import is a round trip ───────────────────────────────

/// `dbd export -f jsonl` is how data moves to a new database, so what it writes
/// must import back to the same values. It wrote `COPY … TO STDOUT` in text
/// format, which escapes every backslash in the JSON a second time: a newline
/// came back as the two characters `\n`, a quote as `\"`.
#[tokio::test]
async fn a_jsonl_export_imports_back_to_the_same_values() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    // A backslash, both quotes, a newline and a tab — everything an escaping
    // layer could touch. Kept beside the table as the expected value.
    adapter
        .execute_script(
            r#"CREATE SCHEMA app;
               CREATE TABLE app.notes (id integer, body text);
               INSERT INTO app.notes VALUES (1, E'C:\\temp "double" ''single''\nsecond line\tend');
               CREATE TABLE app.expected AS SELECT * FROM app.notes;"#,
        )
        .await
        .unwrap();

    let out = tempfile::tempdir().unwrap();
    let mut entity = Entity::new(EntityType::Table, "app.notes");
    entity.format = Some("jsonl".to_string());
    adapter
        .export_data(&entity, Some(out.path()))
        .await
        .expect("export failed");

    // The file itself must hold the value, not an escaped rendering of it.
    let written = std::fs::read_to_string(out.path().join("notes.jsonl")).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines.len(), 1, "one row, one line: {written:?}");
    let row: serde_json::Value =
        serde_json::from_str(lines[0]).unwrap_or_else(|e| panic!("each line must be JSON ({e}): {written:?}"));
    assert_eq!(
        row["body"], "C:\\temp \"double\" 'single'\nsecond line\tend",
        "the exported value: {written:?}"
    );

    // And it imports back to exactly what was exported.
    adapter.execute_script("TRUNCATE app.notes").await.unwrap();
    let mut import = Entity::new(EntityType::Table, "app.notes");
    import.file = Some(out.path().join("notes.jsonl"));
    import.format = Some("jsonl".to_string());
    adapter.import_data(&import, "", false).await.expect("import failed");

    assert_catalog(
        &*adapter,
        false,
        "(SELECT * FROM app.notes EXCEPT ALL SELECT * FROM app.expected) \
         UNION ALL (SELECT * FROM app.expected EXCEPT ALL SELECT * FROM app.notes)",
        "a row that differs after the round trip",
    )
    .await;
}

// ── Test: a UTF-16 DDL file applies ───────────────────────────────────────────

/// SSMS writes UTF-16LE with a BOM. The project load decodes it, but `apply`
/// read the file again with `read_to_string`, took the error for "no DDL", and
/// skipped the table while reporting success.
#[tokio::test]
async fn a_utf16_ddl_file_applies_to_postgres() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("design.yaml"),
        "project:\n  name: ssms\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("ddl/table/app")).unwrap();
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in "set search_path to app;\ncreate table if not exists wide (id integer primary key);\n".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(dir.path().join("ddl/table/app/wide.ddl"), utf16).unwrap();
    let design = Design::from_config_with_dir(&dir.path().join("design.yaml"), "dev", Some(dir.path())).unwrap();

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    assert_table_exists(&*adapter, "app", "wide").await;
}

// ── Test: a JSON record is imported as written ────────────────────────────────

/// Each staged JSON record was sent as an interpolated `INSERT` through
/// `execute_script`, which rewrites every `set search_path to …;` it finds to
/// append `, public` — inside a string value too. A note that quoted the
/// statement came back altered.
#[tokio::test]
async fn a_json_record_mentioning_set_search_path_imports_unchanged() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script("CREATE SCHEMA app; CREATE TABLE app.notes (id integer, body text);")
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.jsonl");
    std::fs::write(
        &path,
        "{\"id\": 1, \"body\": \"first run set search_path to app; then load\"}\n",
    )
    .unwrap();
    let mut entity = Entity::new(EntityType::Table, "app.notes");
    entity.file = Some(path);
    entity.format = Some("jsonl".to_string());
    adapter.import_data(&entity, "", false).await.expect("import failed");

    // Read back through the export, not a SQL predicate: a predicate quoting
    // the same text goes through `execute_script` too, and is rewritten to
    // match the damage.
    let out = tempfile::tempdir().unwrap();
    let mut export = Entity::new(EntityType::Table, "app.notes");
    export.format = Some("jsonl".to_string());
    adapter
        .export_data(&export, Some(out.path()))
        .await
        .expect("export failed");
    let written = std::fs::read_to_string(out.path().join("notes.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
    assert_eq!(row["body"], "first run set search_path to app; then load");
}

// ── Test: JSON values and identity ids survive the round trip ─────────────────

/// What a JSONL export lost on its way back in:
/// - a json/jsonb string scalar (`"a string"`) failed the import — its text was
///   cast to json without its quotes;
/// - a JSON `null` in a json/jsonb column came back as SQL NULL;
/// - a json column's own text (spacing, key order) was rewritten;
/// - `GENERATED ALWAYS` identity ids were renumbered, breaking every foreign
///   key pointing at them.
///
/// A JSON null stays the JSON value; an SQL NULL is exported as an absent key
/// and imports as NULL. Ids import as-is, and the identity then continues past
/// them, so the next insert does not collide — after a CSV import too, whose
/// COPY kept the ids and left the sequence behind.
#[tokio::test]
async fn json_values_and_identity_ids_survive_an_export_and_import() {
    const TABLE: &str = "CREATE TABLE app.docs (\n\
                           id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY\n\
                         , j json\n\
                         , jb jsonb\n\
                         , note text\n\
                         );";
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script(&format!(
            r#"CREATE SCHEMA app;
               {TABLE}
               INSERT INTO app.docs (j, jb, note) VALUES
                 ('{{}}', '{{}}', 'deleted, so the ids have a gap'),
                 ('"a string"', '"a string"', 'string scalars'),
                 ('null', 'null', 'json null'),
                 (NULL, NULL, 'sql null'),
                 ('{{"b" : 1,  "a":[1, "x", null]}}', '{{"b": 1, "a": [1, "x", null]}}', 'objects'),
                 ('42', 'true', 'other scalars');
               DELETE FROM app.docs WHERE id = 1;
               CREATE TABLE app.expected AS
                 SELECT id, j::text AS j, j IS NULL AS j_null, jb, jb IS NULL AS jb_null, note FROM app.docs;"#
        ))
        .await
        .unwrap();

    for format in ["jsonl", "csv"] {
        let out = tempfile::tempdir().unwrap();
        let mut entity = Entity::new(EntityType::Table, "app.docs");
        entity.format = Some(format.to_string());
        adapter
            .export_data(&entity, Some(out.path()))
            .await
            .unwrap_or_else(|e| panic!("{format}: export failed: {e}"));

        // A fresh table, as on a new database: its identity starts at 1.
        adapter
            .execute_script(&format!("DROP TABLE app.docs; {TABLE}"))
            .await
            .unwrap();
        entity.file = Some(out.path().join(format!("docs.{format}")));
        adapter
            .import_data(&entity, "", false)
            .await
            .unwrap_or_else(|e| panic!("{format}: import failed: {e}"));

        assert_catalog(
            &*adapter,
            false,
            "(SELECT id, j::text, j IS NULL, jb, jb IS NULL, note FROM app.docs \
               EXCEPT ALL SELECT * FROM app.expected) \
             UNION ALL (SELECT * FROM app.expected \
               EXCEPT ALL SELECT id, j::text, j IS NULL, jb, jb IS NULL, note FROM app.docs)",
            &format!("{format}: a row that differs after the round trip"),
        )
        .await;

        // The identity continues past the loaded ids — not into the gap below
        // them, where a reused id would point a stale reference at a new row.
        adapter
            .execute_script("INSERT INTO app.docs (note) VALUES ('after the import')")
            .await
            .unwrap_or_else(|e| panic!("{format}: the next insert must get a fresh id: {e}"));
        assert_catalog(
            &*adapter,
            true,
            "SELECT 1 FROM app.docs WHERE note = 'after the import' \
               AND id > (SELECT max(id) FROM app.expected)",
            &format!("{format}: an id past the imported ones"),
        )
        .await;
        adapter
            .execute_script("DELETE FROM app.docs WHERE note = 'after the import'")
            .await
            .unwrap();
    }
}

// ── Test: a generated column is computed, not exported or imported ────────────

/// The export wrote a generated column, and the import then refused it (COPY:
/// "column is a generated column") — so no CSV export of a table with one
/// imported back. The JSON load dropped the same key in silence. The export
/// leaves a generated column out, the import recomputes it, and a data file
/// that names one is refused in every format, saying why.
#[tokio::test]
async fn a_generated_column_is_left_out_of_an_export_and_refused_by_an_import() {
    const TABLE: &str = "CREATE TABLE app.items (\n\
                           id integer PRIMARY KEY\n\
                         , price numeric\n\
                         , doubled numeric GENERATED ALWAYS AS (price * 2) STORED\n\
                         );";
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script(&format!(
            "CREATE SCHEMA app; {TABLE} INSERT INTO app.items (id, price) VALUES (1, 1.50), (2, NULL);"
        ))
        .await
        .unwrap();

    for format in ["csv", "tsv", "jsonl"] {
        let out = tempfile::tempdir().unwrap();
        let mut entity = Entity::new(EntityType::Table, "app.items");
        entity.format = Some(format.to_string());
        adapter
            .export_data(&entity, Some(out.path()))
            .await
            .unwrap_or_else(|e| panic!("{format}: export failed: {e}"));
        let file = out.path().join(format!("items.{format}"));
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(!written.contains("doubled"), "{format}: {written}");

        adapter
            .execute_script(&format!("DROP TABLE app.items; {TABLE}"))
            .await
            .unwrap();
        entity.file = Some(file);
        adapter
            .import_data(&entity, "", false)
            .await
            .unwrap_or_else(|e| panic!("{format}: the export imports back: {e}"));
        assert_catalog(
            &*adapter,
            false,
            "SELECT 1 WHERE (SELECT count(*) FROM app.items WHERE \
               (id = 1 AND price = 1.50 AND doubled = 3.00) OR (id = 2 AND price IS NULL AND doubled IS NULL)) <> 2",
            &format!("{format}: rows other than the exported ones"),
        )
        .await;
    }

    let dir = tempfile::tempdir().unwrap();
    for (file, body, format) in [
        ("items.csv", "id,price,doubled\n3,1,2\n", "csv"),
        ("items.jsonl", "{\"id\": 3, \"price\": 1, \"doubled\": 2}\n", "jsonl"),
    ] {
        let path = dir.path().join(file);
        std::fs::write(&path, body).unwrap();
        let mut entity = Entity::new(EntityType::Table, "app.items");
        entity.file = Some(path);
        entity.format = Some(format.to_string());
        let err = adapter
            .import_data(&entity, "", false)
            .await
            .expect_err("a value for a generated column must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("doubled") && msg.contains("generated") && msg.contains(file),
            "{format}: the column, why, and the file: {msg}"
        );
        assert_catalog(
            &*adapter,
            false,
            "SELECT 1 FROM app.items WHERE id = 3",
            "a refused row",
        )
        .await;
    }
}

// ── Test: a JSON key naming no column is refused ──────────────────────────────

/// A CSV header naming a column the table lacks is refused, on Postgres and
/// SQLite, and so is a SQLite JSON key. A Postgres JSON import loaded only the
/// keys that matched a column and dropped the rest without a word — a typo'd
/// key left its column empty and the import reported success.
#[tokio::test]
async fn a_json_key_naming_no_column_is_refused_naming_the_key_and_the_file() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script("CREATE SCHEMA app; CREATE TABLE app.people (id integer, name text);")
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    for (file, body, format) in [
        (
            "people.jsonl",
            "{\"id\": 1, \"name\": \"alpha\"}\n{\"id\": 2, \"nmae\": \"beta\"}\n",
            "jsonl",
        ),
        (
            "people.json",
            "[{\"id\": 1, \"name\": \"alpha\"}, {\"id\": 2, \"nmae\": \"beta\"}]",
            "json",
        ),
    ] {
        let path = dir.path().join(file);
        std::fs::write(&path, body).unwrap();
        let mut entity = Entity::new(EntityType::Table, "app.people");
        entity.file = Some(path);
        entity.format = Some(format.to_string());

        let err = adapter
            .import_data(&entity, "", false)
            .await
            .expect_err("a key naming no column must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("'nmae'") && msg.contains(file) && msg.contains("app.people"),
            "{format}: the key, the file and the table are named: {msg}"
        );
        assert_catalog(
            &*adapter,
            false,
            "SELECT 1 FROM app.people",
            &format!("a row loaded from {file}"),
        )
        .await;
    }
}

// ── Test: an export format dbd cannot write is refused ────────────────────────

/// Any format but `tsv` and `jsonl` fell through to CSV, and the file was still
/// named for the format asked for: `-f json` wrote CSV into `notes.json`, which
/// the import then reads as JSON and rejects. SQLite already refuses.
#[tokio::test]
async fn an_export_format_postgres_cannot_write_is_refused() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script("CREATE SCHEMA app; CREATE TABLE app.notes (id integer); INSERT INTO app.notes VALUES (1);")
        .await
        .unwrap();

    let out = tempfile::tempdir().unwrap();
    let mut entity = Entity::new(EntityType::Table, "app.notes");
    entity.format = Some("json".to_string());
    let err = adapter
        .export_data(&entity, Some(out.path()))
        .await
        .expect_err("a format the export cannot write must be refused");

    let msg = err.to_string();
    assert!(
        msg.contains("json") && msg.contains("jsonl"),
        "say what was asked and what works: {msg}"
    );
    assert!(!out.path().join("notes.json").exists(), "and write nothing");
}

// ── Test 4: Dry-run does not modify schema ────────────────────────────────────

#[tokio::test]
async fn dry_run_does_not_create_tables() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    design
        .deploy(&*adapter, true, None, |_| {})
        .await
        .expect("dry-run failed");

    assert_table_absent(&*adapter, "app", "items").await;
}

// ── Test 5: Migration cycle (v1 → v2) ────────────────────────────────────────

#[tokio::test]
async fn migration_upgrades_schema() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    // --- Phase 1: deploy at v1 ---
    let tmp = tempfile::tempdir().unwrap();
    let v1_dir = tmp.path();

    std::fs::create_dir_all(v1_dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        v1_dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (\n\
           id         uuid primary key default gen_random_uuid()\n\
         , name       text not null\n\
         , quantity   integer not null default 0\n\
         , created_at timestamptz not null default now()\n\
         );\n\
         create unique index if not exists items_name_ukey on items(name);\n",
    )
    .unwrap();
    std::fs::write(
        v1_dir.join("ddl/table/app/orders.ddl"),
        "set search_path to app;\n\
         create table if not exists orders (\n\
           id         uuid primary key default gen_random_uuid()\n\
         , item_id    uuid not null references items(id)\n\
         , amount     integer not null default 1\n\
         , created_at timestamptz not null default now()\n\
         );\n",
    )
    .unwrap();
    std::fs::write(
        v1_dir.join("design.yaml"),
        "project:\n  name: embedded_test\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();

    let v1_design = Design::from_config_with_dir(&v1_dir.join("design.yaml"), "dev", Some(v1_dir))
        .expect("failed to load v1 design");

    let mut v1_summary = None;
    v1_design
        .apply(
            &*adapter,
            None,
            false,
            None,
            Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| v1_summary = Some(s),
            },
        )
        .await
        .expect("v1 apply failed");

    let v1 = v1_summary.unwrap();
    assert_eq!(v1.strategy, ApplyStrategy::Fresh);
    assert_eq!(v1.to_version, 1);

    assert_column_absent(&*adapter, "app", "items", "notes").await;

    // --- Phase 2: apply v2 design (with migration from v1→v2) ---
    let v2_design = load_design();
    let mut v2_summary = None;
    v2_design
        .apply(
            &*adapter,
            None,
            false,
            None,
            Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| v2_summary = Some(s),
            },
        )
        .await
        .expect("v2 apply failed");

    let v2 = v2_summary.unwrap();
    assert_eq!(v2.strategy, ApplyStrategy::Migrate, "should use Migrate strategy");
    assert_eq!(v2.from_version, 1);
    assert_eq!(v2.to_version, 2);
    assert_eq!(v2.migrated, 1, "one entity migrated (items)");

    assert_column_exists(&*adapter, "app", "items", "notes").await;
}

// ── Test: init --from-db keeps a view's and a matview's own comment ───────────

/// `init --from-db` wrote a table's comment and dropped a view's and a
/// materialized view's: neither introspection read them nor the emitter wrote
/// them. A matview comment that is a drift stamp a dbd before 0.25 wrote is
/// dbd's, not the author's, and stays out of the file.
#[tokio::test]
async fn reverse_engineering_keeps_a_views_and_a_matviews_own_comment() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script(
            "CREATE SCHEMA app; CREATE TABLE app.t (id int primary key); \
             CREATE VIEW app.v AS SELECT id FROM app.t; \
             COMMENT ON VIEW app.v IS 'the view''s comment'; \
             CREATE MATERIALIZED VIEW app.mv AS SELECT id FROM app.t; \
             COMMENT ON MATERIALIZED VIEW app.mv IS 'the matview'; \
             CREATE MATERIALIZED VIEW app.stamped AS SELECT id FROM app.t; \
             COMMENT ON MATERIALIZED VIEW app.stamped IS 'dbd:hash=v2:abc123';",
        )
        .await
        .unwrap();

    let entities = adapter.introspect().await.expect("introspect");
    let emitted = |name: &str| {
        let e = entities
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} introspected"));
        dbd_core::emit::emit_entity(e).unwrap_or_default()
    };

    let v = emitted("app.v");
    assert!(
        v.contains(r#"COMMENT ON VIEW "app"."v" IS 'the view''s comment';"#),
        "{v}"
    );
    let mv = emitted("app.mv");
    assert!(
        mv.contains(r#"COMMENT ON MATERIALIZED VIEW "app"."mv" IS 'the matview';"#),
        "{mv}"
    );
    let stamped = emitted("app.stamped");
    assert!(
        !stamped.contains("COMMENT ON"),
        "a legacy stamp is not the author's comment: {stamped}"
    );
}

// ── Test 6: Postgres introspection ───────────────────────────────────────────

#[tokio::test]
async fn introspect_returns_fixture_entities() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "introspect_test").await.unwrap();

    // Create a known fixture in the ephemeral DB
    let fixture_sql = "
        CREATE SCHEMA revtest;

        CREATE TYPE revtest.color AS ENUM ('red', 'green');

        CREATE TYPE revtest.empty_color AS ENUM ();

        CREATE TABLE revtest.owner (
            id uuid PRIMARY KEY DEFAULT gen_random_uuid()
        );

        CREATE TABLE revtest.widget (
            id       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
            owner_id uuid NOT NULL REFERENCES revtest.owner(id) ON DELETE CASCADE,
            name     text NOT NULL,
            qty      int DEFAULT 0,
            tags     text[] NOT NULL DEFAULT '{}'
        );

        ALTER TABLE revtest.widget ADD CONSTRAINT widget_name_key UNIQUE (name);

        CREATE INDEX widget_owner_idx ON revtest.widget (owner_id);
        CREATE INDEX widget_lower_name_idx ON revtest.widget (lower(name));
        CREATE INDEX widget_tags_idx ON revtest.widget USING gin (tags);

        COMMENT ON TABLE revtest.widget IS 'Widget objects';
        COMMENT ON COLUMN revtest.widget.name IS 'Display name';

        CREATE VIEW revtest.active AS SELECT id, name FROM revtest.widget;
    ";
    adapter.execute_script(fixture_sql).await.expect("fixture DDL failed");

    // Run introspect
    let entities = adapter.introspect().await.expect("introspect failed");

    // ── Assert: schema revtest present ───────────────────────────────────────
    let has_revtest_schema = entities
        .iter()
        .any(|e| e.entity_type == dbd_core::EntityType::Schema && e.name == "revtest");
    assert!(has_revtest_schema, "schema 'revtest' not found in introspect output");

    // ── Assert: enum revtest.color present with values [red, green] ──────────
    let color_enum = entities
        .iter()
        .find(|e| e.entity_type == dbd_core::EntityType::Enum && e.name == "revtest.color");
    let color_enum = color_enum.expect("enum 'revtest.color' not found in introspect output");
    let enum_value_names: Vec<&str> = color_enum.enum_values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        enum_value_names,
        vec!["red", "green"],
        "enum values should be [red, green]"
    );

    // ── Assert: label-less enum revtest.empty_color still introspects ─────────
    // `CREATE TYPE e AS ENUM ()` is valid Postgres and creates a real type with
    // zero pg_enum rows. introspect_enums used to INNER JOIN pg_enum, which made
    // such a type invisible here — reconcile then saw it as permanently missing
    // and recreated it on every run, never converging. It must be found, with
    // an empty (not absent) values list.
    let empty_enum = entities
        .iter()
        .find(|e| e.entity_type == dbd_core::EntityType::Enum && e.name == "revtest.empty_color");
    let empty_enum = empty_enum.expect("label-less enum 'revtest.empty_color' not found in introspect output");
    assert!(
        empty_enum.enum_values.is_empty(),
        "empty_color should introspect with zero values, got: {:?}",
        empty_enum.enum_values
    );

    // ── Assert: table revtest.widget present ──────────────────────────────────
    let widget = entities
        .iter()
        .find(|e| e.entity_type == dbd_core::EntityType::Table && e.name == "revtest.widget");
    let widget = widget.expect("table 'revtest.widget' not found in introspect output");
    let td = widget.table_def.as_ref().expect("widget should have a table_def");

    // Columns: id, owner_id, name, qty
    let col_names: Vec<&str> = td.columns.iter().map(|c| c.name.as_str()).collect();
    assert!(col_names.contains(&"id"), "widget must have column 'id'");
    assert!(col_names.contains(&"owner_id"), "widget must have column 'owner_id'");
    assert!(col_names.contains(&"name"), "widget must have column 'name'");
    assert!(col_names.contains(&"qty"), "widget must have column 'qty'");

    // id: not nullable
    let id_col = td.columns.iter().find(|c| c.name == "id").unwrap();
    assert!(!id_col.nullable, "id should be NOT NULL");

    // owner_id: not nullable
    let owner_col = td.columns.iter().find(|c| c.name == "owner_id").unwrap();
    assert!(!owner_col.nullable, "owner_id should be NOT NULL");

    // name: not nullable
    let name_col = td.columns.iter().find(|c| c.name == "name").unwrap();
    assert!(!name_col.nullable, "name should be NOT NULL");

    // qty: has a default value
    let qty_col = td.columns.iter().find(|c| c.name == "qty").unwrap();
    assert!(qty_col.default_value.is_some(), "qty should have a default value");

    // PRIMARY KEY constraint on [id]
    let pk = td.constraints.iter().find_map(|c| match c {
        dbd_core::entity::TableConstraint::PrimaryKey { columns, .. } => Some(columns),
        _ => None,
    });
    let pk_cols = pk.expect("widget should have a PRIMARY KEY constraint");
    assert_eq!(pk_cols, &vec!["id".to_string()], "PK should be on column 'id'");

    // FOREIGN KEY to revtest.owner on CASCADE delete
    let fk = td.constraints.iter().find_map(|c| match c {
        dbd_core::entity::TableConstraint::ForeignKey(fk) => Some(fk),
        _ => None,
    });
    let fk = fk.expect("widget should have a FOREIGN KEY constraint");
    assert_eq!(fk.ref_table, "owner", "FK should reference 'owner'");
    assert_eq!(
        fk.ref_schema.as_deref(),
        Some("revtest"),
        "FK ref_schema should be 'revtest'"
    );
    assert_eq!(
        fk.on_delete,
        Some(dbd_core::entity::FkAction::Cascade),
        "FK on_delete should be CASCADE"
    );

    // UNIQUE constraint on [name]
    let unique = td.constraints.iter().find_map(|c| match c {
        dbd_core::entity::TableConstraint::Unique { columns, .. } => Some(columns),
        _ => None,
    });
    let unique_cols = unique.expect("widget should have a UNIQUE constraint");
    assert!(unique_cols.contains(&"name".to_string()), "UNIQUE should be on 'name'");

    // Non-constraint index widget_owner_idx (plain column — must be captured)
    let idx = td
        .indexes
        .iter()
        .find(|i| i.name.as_deref() == Some("widget_owner_idx"));
    assert!(idx.is_some(), "index 'widget_owner_idx' should be present");

    // GIN index widget_tags_idx — access method must be captured (a GIN index on a
    // text[] column would be invalid as a plain btree, so the method must round-trip).
    let gin_idx = td
        .indexes
        .iter()
        .find(|i| i.name.as_deref() == Some("widget_tags_idx"))
        .expect("GIN index 'widget_tags_idx' should be present");
    assert_eq!(
        gin_idx.index_type,
        Some(dbd_core::entity::IndexType::Gin),
        "widget_tags_idx should be captured as a GIN index"
    );

    // Expression index widget_lower_name_idx — captured, with its key flagged as an
    // expression. It used to be skipped, which made an existing expression index
    // invisible: the design's copy read as missing and every diff asked to create it.
    let expr_idx = td
        .indexes
        .iter()
        .find(|i| i.name.as_deref() == Some("widget_lower_name_idx"))
        .expect("expression index 'widget_lower_name_idx' should be introspected");
    let key = &expr_idx.columns[0];
    assert!(key.is_expression, "lower(name) is an expression, not a column: {key:?}");
    assert!(
        key.name.contains("lower"),
        "the expression text must be captured; got {:?}",
        key.name
    );

    // Table comment
    assert_eq!(
        td.comments.table.as_deref(),
        Some("Widget objects"),
        "table comment should be 'Widget objects'"
    );

    // Column comment on name
    assert_eq!(
        td.comments.columns.get("name").map(String::as_str),
        Some("Display name"),
        "column comment on 'name' should be 'Display name'"
    );

    // ── Assert: view revtest.active present with SELECT body ──────────────────
    let view = entities
        .iter()
        .find(|e| e.entity_type == dbd_core::EntityType::View && e.name == "revtest.active");
    let view = view.expect("view 'revtest.active' not found in introspect output");
    let body = view.body.first().expect("view should have a body in body[0]");
    assert!(
        body.to_uppercase().contains("SELECT"),
        "view body should contain SELECT, got: {body}"
    );
}

// ── Test: Materialized-view introspection ─────────────────────────────────────

/// Reverse-engineer a materialized view from `pg_matviews`. Creates a schema, a
/// `CREATE MATERIALIZED VIEW … WITH DATA`, and a UNIQUE INDEX on it, then asserts
/// `introspect` captures it as `EntityType::MaterializedView` with its SELECT body
/// in `body[0]` and its index attached via `table_def` (matviews carry indexes
/// in `pg_index` exactly like tables). Drops the schema (CASCADE) at the end.
#[tokio::test]
async fn introspect_captures_materialized_views() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "introspect_mv_test").await.unwrap();

    adapter
        .execute_script(
            "CREATE SCHEMA mvtest; \
             CREATE MATERIALIZED VIEW mvtest.mv AS SELECT 1 AS x WITH DATA; \
             CREATE UNIQUE INDEX mv_x_uidx ON mvtest.mv(x);",
        )
        .await
        .expect("fixture DDL failed");

    let entities = adapter.introspect().await.expect("introspect failed");

    // Captured as a MaterializedView entity named "mvtest.mv".
    let mv = entities
        .iter()
        .find(|e| e.name == "mvtest.mv")
        .expect("materialized view 'mvtest.mv' not found in introspect output");
    assert_eq!(
        mv.entity_type,
        dbd_core::EntityType::MaterializedView,
        "mvtest.mv should be a MaterializedView"
    );

    // Body carried in body[0] (same contract as views); contains the SELECT.
    let body = mv.body.first().expect("matview should carry its body in body[0]");
    assert!(
        body.to_lowercase().contains("select"),
        "matview body should contain SELECT (case-insensitive), got: {body}"
    );

    // The UNIQUE INDEX is attached via table_def — exactly one index, and unique.
    let td = mv
        .table_def
        .as_ref()
        .expect("matview should have a table_def carrying its index");
    assert_eq!(
        td.indexes.len(),
        1,
        "matview should have exactly one index, got {:?}",
        td.indexes
    );
    assert!(
        td.indexes[0].unique,
        "the captured index should be UNIQUE, got {:?}",
        td.indexes[0]
    );

    // Clean up the ephemeral schema.
    adapter
        .execute_script("DROP SCHEMA mvtest CASCADE;")
        .await
        .expect("failed to drop schema mvtest");
}

/// `sync_refresh_jobs(&[])` against a real Postgres WITHOUT pg_cron must be a
/// no-op that returns `Ok` — proving the pg_cron-presence guard keeps `apply`
/// from touching `cron.job` on databases that lack the extension. The embedded
/// PG has no pg_cron, so this exercises `pg_cron_present()` -> false and
/// `plan_cron_sync(empty, false)` -> `Ok(vec![])` end-to-end on a live DB. (An
/// end-to-end `cron.schedule` test isn't possible here — the embedded PG cannot
/// load pg_cron — so the scheduling SQL is covered by the pure unit tests.)
#[tokio::test]
async fn sync_refresh_jobs_is_noop_without_pg_cron() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "cron_noop_test").await.unwrap();
    adapter
        .sync_refresh_jobs(&[])
        .await
        .expect("sync_refresh_jobs must be a no-op on a DB without pg_cron");
}

// ── Test 7: Emitted index DDL applies to a real Postgres ──────────────────────

/// Close the emit→apply loop: build a minimal `Entity` with a `text[]` column, a
/// GIN index on it, and a HASH index on a plain column, emit DDL via
/// `emit_table`, and execute it against an embedded Postgres. This guards the
/// `CREATE INDEX ... USING <method>` grammar — a prior bug emitted `USING gin`
/// BEFORE `ON table` (invalid Postgres), which only a real apply catches
/// (sqlparser happily parses the invalid ordering). Postgres accepting the DDL
/// proves the emitted output is valid, not merely parseable.
#[tokio::test]
async fn emitted_index_ddl_applies_to_postgres() {
    use dbd_core::entity::{
        ColumnDef, Entity, EntityType, IndexColumn, IndexDef, IndexType, TableConstraint, TableDef,
    };

    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "emit_index_test").await.unwrap();

    // Fresh schema to apply into.
    adapter
        .execute_script("CREATE SCHEMA emit_test;")
        .await
        .expect("failed to create schema emit_test");

    // Minimal standalone table: a plain column + a text[] column, no FKs.
    let mut entity = Entity::new(EntityType::Table, "emit_test.doc");
    entity.schema = Some("emit_test".into());
    entity.table_def = Some(TableDef {
        columns: vec![
            ColumnDef {
                name: "id".into(),
                data_type: "uuid".into(),
                nullable: false,
                default_value: Some("gen_random_uuid()".into()),
                is_pk: false,
                is_unique: false,
                identity: None,
                generated: None,
                comment: None,
                inline_fk: None,
            },
            ColumnDef {
                name: "title".into(),
                data_type: "text".into(),
                nullable: false,
                default_value: None,
                is_pk: false,
                is_unique: false,
                identity: None,
                generated: None,
                comment: None,
                inline_fk: None,
            },
            ColumnDef {
                name: "tags".into(),
                data_type: "text[]".into(),
                nullable: false,
                default_value: Some("'{}'".into()),
                is_pk: false,
                is_unique: false,
                identity: None,
                generated: None,
                comment: None,
                inline_fk: None,
            },
        ],
        constraints: vec![TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["id".into()],
        }],
        indexes: vec![
            // GIN index on the array column — invalid as a btree, so the access
            // method (and its position after `ON table`) must be correct.
            IndexDef {
                name: Some("doc_tags_gin".into()),
                columns: vec![IndexColumn {
                    name: "tags".into(),
                    order: None,
                    ..Default::default()
                }],
                unique: false,
                index_type: Some(IndexType::Gin),
                ..Default::default()
            },
            // HASH index on a plain column.
            IndexDef {
                name: Some("doc_title_hash".into()),
                columns: vec![IndexColumn {
                    name: "title".into(),
                    order: None,
                    ..Default::default()
                }],
                unique: false,
                index_type: Some(IndexType::Hash),
                ..Default::default()
            },
        ],
        comments: Default::default(),
    });

    let sql = dbd_core::emit::emit_table(&entity);
    assert!(
        sql.contains("USING gin"),
        "emitted DDL should carry the GIN access method, got:\n{sql}"
    );

    // The real test: Postgres must accept the emitted DDL verbatim.
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("emitted index DDL rejected by Postgres: {e}\n--- DDL ---\n{sql}"));
}

// ── Test: index round-trip convergence against real Postgres ─────────────────

/// The convergence property reconcile depends on, checked end to end against a
/// real server: introspect an index, re-emit it as DDL, apply that DDL to an
/// identical table, introspect again — and get the SAME `IndexDef`.
///
/// Any attribute the model, the introspector, or the emitter drops shows up here
/// as a mismatch. That is precisely how the reported bug behaved: `DESC` and the
/// `WHERE` predicate were lost, so reconcile recreated an index that differed
/// from the one it had just read, and the next `dbd diff` reported the same
/// `DROP INDEX`/`CREATE INDEX` pair again — forever.
#[tokio::test]
async fn index_definitions_round_trip_through_postgres() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "index_roundtrip_test").await.unwrap();

    // `origin` carries the awkward index shapes; `echo` is the identical table the
    // re-emitted DDL is applied to.
    let columns = "(
           id          uuid not null,
           folder_id   uuid,
           file_path   text,
           status      text,
           created_at  timestamptz,
           context     jsonb,
           tags        text[]
         )";
    adapter
        .execute_script(&format!(
            "CREATE SCHEMA rt;
             CREATE TABLE rt.origin {columns};
             CREATE TABLE rt.echo {columns};

             -- descending key with the implied NULLS FIRST
             CREATE INDEX ix_desc ON rt.origin (folder_id, created_at DESC);
             -- descending key overriding the implied NULLS ordering
             CREATE INDEX ix_desc_nulls_last ON rt.origin (created_at DESC NULLS LAST);
             -- partial unique index with NULLS NOT DISTINCT
             CREATE UNIQUE INDEX ix_partial ON rt.origin (folder_id, file_path)
                 NULLS NOT DISTINCT WHERE file_path IS NOT NULL;
             -- partial index whose predicate Postgres stores with a cast
             CREATE INDEX ix_pred_cast ON rt.origin (id) WHERE status = 'active';
             -- partial index whose predicate Postgres stores as = ANY (ARRAY[…])
             CREATE INDEX ix_pred_any ON rt.origin (id) WHERE status IN ('a', 'b');
             -- expression key
             CREATE INDEX ix_expr ON rt.origin ((context ->> 'module'));
             -- non-btree access method
             CREATE INDEX ix_gin ON rt.origin USING gin (tags);
             -- INCLUDE payload
             CREATE INDEX ix_include ON rt.origin (folder_id) INCLUDE (status);"
        ))
        .await
        .expect("failed to seed rt schema");

    /// Introspected indexes of `rt.<table>`, keyed by name.
    async fn indexes_of(
        adapter: &dyn dbd_core::DatabaseAdapter,
        table: &str,
    ) -> std::collections::BTreeMap<String, dbd_core::entity::IndexDef> {
        adapter
            .introspect()
            .await
            .expect("introspect failed")
            .into_iter()
            .filter(|e| e.name == format!("rt.{table}"))
            .filter_map(|e| e.table_def)
            .flat_map(|td| td.indexes)
            .filter_map(|ix| ix.name.clone().map(|n| (n, ix)))
            .collect()
    }

    let origin = indexes_of(&*adapter, "origin").await;
    assert_eq!(
        origin.len(),
        8,
        "every index must be introspected, including partial and expression ones; got {:?}",
        origin.keys().collect::<Vec<_>>()
    );

    // Re-emit each index onto `echo`, then introspect it back.
    let mut replay = String::new();
    for ix in origin.values() {
        // Rename so origin's names stay free; the shape is what must round-trip.
        let mut echoed = ix.clone();
        echoed.name = Some(format!("{}_echo", ix.name.as_deref().unwrap()));
        replay.push_str(&dbd_core::emit::emit_index_sql(
            &echoed,
            "\"rt\".\"echo\"",
            "echo",
            false,
        ));
        replay.push('\n');
    }
    adapter
        .execute_script(&replay)
        .await
        .unwrap_or_else(|e| panic!("re-emitted index DDL rejected by Postgres: {e}\n--- DDL ---\n{replay}"));

    let echo = indexes_of(&*adapter, "echo").await;
    for (name, before) in &origin {
        let echoed_name = format!("{name}_echo");
        let after = echo.get(&echoed_name).unwrap_or_else(|| {
            panic!(
                "{echoed_name} missing after replay; got {:?}",
                echo.keys().collect::<Vec<_>>()
            )
        });
        // Compare everything but the (deliberately changed) name.
        let expected = dbd_core::entity::IndexDef {
            name: after.name.clone(),
            ..before.clone()
        };
        assert_eq!(
            &expected, after,
            "index {name} did not survive the emit/apply/introspect round trip"
        );
    }
}

// ── Test 8: Function & procedure introspection (with overloads + extension) ───

/// Reverse-engineer functions and procedures via `pg_get_functiondef`. Creates a
/// plain function, a procedure, an overloaded function (two signatures), and
/// installs `tablefunc` whose functions are extension-owned and must be excluded.
/// (`tablefunc` is a pure-SQL contrib extension with no external shared-library
/// dependency, so the embedded server can load it on any runner.)
#[tokio::test]
async fn introspect_captures_functions_and_procedures() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "introspect_fn_test").await.unwrap();

    let fixture_sql = "
        CREATE SCHEMA revfunc;

        -- plain function
        CREATE FUNCTION revfunc.add_one(n integer) RETURNS integer
            LANGUAGE sql IMMUTABLE AS $$ SELECT n + 1 $$;

        -- procedure
        CREATE PROCEDURE revfunc.noop()
            LANGUAGE plpgsql AS $$ BEGIN NULL; END; $$;

        -- overloaded function: two signatures, same name
        CREATE FUNCTION revfunc.greet(name text) RETURNS text
            LANGUAGE sql AS $$ SELECT 'hi ' || name $$;
        CREATE FUNCTION revfunc.greet(name text, loud boolean) RETURNS text
            LANGUAGE sql AS $$ SELECT 'HI ' || name $$;

        -- extension whose functions (crosstab/connectby/normal_rand) must be EXCLUDED
        CREATE EXTENSION IF NOT EXISTS tablefunc WITH SCHEMA revfunc;
    ";
    adapter.execute_script(fixture_sql).await.expect("fixture DDL failed");

    let entities = adapter.introspect().await.expect("introspect failed");

    // ── plain function captured as EntityType::Function ──────────────────────
    let add_one = entities
        .iter()
        .find(|e| e.name == "revfunc.add_one")
        .expect("function 'revfunc.add_one' not found");
    assert_eq!(
        add_one.entity_type,
        dbd_core::EntityType::Function,
        "add_one should be a Function"
    );
    assert_eq!(add_one.body.len(), 1, "add_one has one body");
    assert!(
        add_one.body[0].to_uppercase().contains("FUNCTION"),
        "add_one body should be a CREATE FUNCTION, got: {}",
        add_one.body[0]
    );

    // ── procedure captured as EntityType::Procedure ──────────────────────────
    let noop = entities
        .iter()
        .find(|e| e.name == "revfunc.noop")
        .expect("procedure 'revfunc.noop' not found");
    assert_eq!(
        noop.entity_type,
        dbd_core::EntityType::Procedure,
        "noop should be a Procedure"
    );
    assert!(
        noop.body[0].to_uppercase().contains("PROCEDURE"),
        "noop body should be a CREATE PROCEDURE, got: {}",
        noop.body[0]
    );

    // ── overloaded function: ONE entity with TWO writes ──────────────────────
    let greet_entities: Vec<_> = entities.iter().filter(|e| e.name == "revfunc.greet").collect();
    assert_eq!(
        greet_entities.len(),
        1,
        "overloaded 'revfunc.greet' must collapse into exactly ONE entity, got {}",
        greet_entities.len()
    );
    let greet = greet_entities[0];
    assert_eq!(
        greet.entity_type,
        dbd_core::EntityType::Function,
        "greet should be a Function"
    );
    assert_eq!(
        greet.body.len(),
        2,
        "overloaded greet must hold TWO definitions in writes, got {}",
        greet.body.len()
    );
    // Both signatures present across the two bodies.
    let joined = greet.body.join("\n");
    assert!(joined.contains("name text"), "first overload signature missing");
    assert!(joined.contains("loud boolean"), "second overload signature missing");

    // ── extension-provided functions must be EXCLUDED ────────────────────────
    // `tablefunc` installs regular functions (crosstab/connectby/normal_rand)
    // owned by the extension (pg_depend deptype 'e'); they must be filtered out.
    let has_ext_fn = entities.iter().any(|e| {
        (e.entity_type == dbd_core::EntityType::Function || e.entity_type == dbd_core::EntityType::Procedure)
            && (e.name.contains("crosstab") || e.name.contains("connectby") || e.name.contains("normal_rand"))
    });
    assert!(
        !has_ext_fn,
        "extension-owned functions (crosstab/connectby/normal_rand) must NOT appear in introspect output"
    );
}

// ── Test 9: Emitted routine DDL applies to a real Postgres ────────────────────

/// Round-trip: capture a function via introspect, `emit_routine` it, and execute
/// the emitted DDL into a fresh schema → succeeds (proves emitted routine DDL is
/// valid, mirroring `emitted_index_ddl_applies_to_postgres`).
#[tokio::test]
async fn emitted_routine_ddl_applies_to_postgres() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "emit_routine_test").await.unwrap();

    // Source schema with a function to capture.
    adapter
        .execute_script(
            "CREATE SCHEMA src; \
             CREATE FUNCTION src.double_it(n integer) RETURNS integer \
                 LANGUAGE sql IMMUTABLE AS $$ SELECT n * 2 $$;",
        )
        .await
        .expect("failed to create source function");

    let entities = adapter.introspect().await.expect("introspect failed");
    let func = entities
        .iter()
        .find(|e| e.name == "src.double_it")
        .expect("captured function 'src.double_it' not found");

    // Emit the routine DDL from the captured entity.
    let sql = dbd_core::emit::emit_routine(func);
    assert!(sql.trim_end().ends_with(';'), "emitted routine must end in ';':\n{sql}");

    // Apply into a fresh schema. `pg_get_functiondef` emits a fully-qualified,
    // `CREATE OR REPLACE` definition, so re-running it succeeds.
    adapter
        .execute_script("CREATE SCHEMA dst;")
        .await
        .expect("failed to create dst schema");
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("emitted routine DDL rejected by Postgres: {e}\n--- DDL ---\n{sql}"));
}

// ── Test 10: Role introspection (opt-in) ──────────────────────────────────────

/// Reverse-engineer cluster-global roles via `introspect_roles`. Creates two
/// project roles with a membership, then asserts they are captured as
/// `EntityType::Role`, the membership is preserved in `refers`, and no
/// platform/superuser role (the embedded cluster's bootstrap superuser) leaks in.
#[tokio::test]
async fn introspect_roles_captures_project_roles_and_memberships() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "introspect_roles_test").await.unwrap();

    adapter
        .execute_script(
            "CREATE ROLE app_admin; \
             CREATE ROLE app_ro; \
             GRANT app_admin TO app_ro;",
        )
        .await
        .expect("failed to create roles");

    let roles = adapter.introspect_roles().await.expect("introspect_roles failed");

    // app_admin and app_ro captured as Role entities.
    let app_admin = roles
        .iter()
        .find(|e| e.name == "app_admin")
        .expect("role 'app_admin' not found");
    assert_eq!(app_admin.entity_type, dbd_core::EntityType::Role);
    assert!(
        app_admin.refers().next().is_none(),
        "app_admin should have no memberships, got {:?}",
        app_admin.refers().collect::<Vec<_>>()
    );

    let app_ro = roles
        .iter()
        .find(|e| e.name == "app_ro")
        .expect("role 'app_ro' not found");
    assert_eq!(app_ro.entity_type, dbd_core::EntityType::Role);
    assert_eq!(
        app_ro.refers().collect::<Vec<_>>(),
        vec!["app_admin".to_string()],
        "app_ro should be a member of app_admin"
    );

    // No platform/superuser role (e.g. the bootstrap superuser, or any pg_* role)
    // may appear — they are filtered by `role_is_managed`.
    for e in &roles {
        assert!(
            !e.name.starts_with("pg_"),
            "platform role '{}' must not be captured",
            e.name
        );
    }
    // The embedded cluster's bootstrap superuser is created by name from the
    // current OS user / settings; whatever it is, it is a superuser and must be
    // excluded — assert the kept set is exactly the two project roles.
    let names: Vec<&str> = roles.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["app_admin", "app_ro"],
        "only the two project roles should be captured, got {names:?}"
    );
}

/// Version-safety: a fresh DB with no `_dbd_meta` is foreign (None); once a
/// `_dbd_meta` table exists in ANY schema (here `staging`, off the default
/// search_path), the adapter reports the applied version regardless of which `env`
/// the row was last written with — proving cross-schema detection is env-agnostic.
///
/// The row is seeded with `env='dev'` while the adapter was not constructed with
/// that env value, confirming the read keys on `project` only (not `(project, env)`).
#[tokio::test]
async fn reverse_managed_version_detects_cross_schema_meta() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();

    // (a) Fresh DB — no `_dbd_meta` anywhere → foreign.
    let managed = adapter
        .reverse_managed_version()
        .await
        .expect("reverse_managed_version should not error on a fresh DB");
    assert_eq!(managed, None, "a DB with no _dbd_meta must be foreign (None)");

    // (b) Create `staging._dbd_meta` (NOT on the default search_path) with a row
    //     seeded with env='dev' — deliberately different from any env the caller
    //     might pass. The read must succeed regardless, proving the key is
    //     `project` only.
    adapter
        .execute_script(
            "CREATE SCHEMA staging; \
             CREATE TABLE staging._dbd_meta ( \
                project varchar NOT NULL, \
                env     varchar NOT NULL, \
                version integer NOT NULL \
             ); \
             INSERT INTO staging._dbd_meta (project, env, version) \
             VALUES ('embedded_test', 'dev', 3);",
        )
        .await
        .expect("failed to seed staging._dbd_meta");

    let managed = adapter
        .reverse_managed_version()
        .await
        .expect("reverse_managed_version should read cross-schema _dbd_meta");
    assert_eq!(
        managed,
        Some(3),
        "must read version from staging._dbd_meta regardless of env (project-only key)"
    );
}

// ── Test 12: Role membership GRANTs survive apply (round-trip fix) ────────────

/// Proves that role membership GRANTs written by `generate_role_script` are
/// preserved when the DDL file is re-read and applied — the core bug being
/// fixed. Constructs role entities, emits DDL via `ddl_from_entity`, applies
/// them, and asserts the membership exists in `pg_auth_members`.
#[tokio::test]
async fn role_membership_grant_survives_apply() {
    use dbd_core::entity::{Entity, EntityType};
    use dbd_core::script::ddl_from_entity;

    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "role_grant_test").await.unwrap();

    // Create the parent role first.
    let parent = Entity::new(EntityType::Role, "app_admin");
    let parent_ddl = ddl_from_entity(&parent).expect("ddl_from_entity(app_admin) must return Some");
    adapter
        .execute_script(&parent_ddl)
        .await
        .expect("failed to apply parent role DDL");

    // Create the child role with the parent as a member.
    let mut child = Entity::new(EntityType::Role, "app_ro");
    child.refs = vec![dbd_core::entity::Ref::stated(
        "app_admin",
        dbd_core::entity::RefKind::Member,
    )];
    let child_ddl = ddl_from_entity(&child).expect("ddl_from_entity(app_ro) must return Some");

    // Verify the emitted DDL contains the GRANT.
    assert!(
        child_ddl.contains("GRANT \"app_admin\" TO \"app_ro\""),
        "emitted role DDL missing GRANT line:\n{child_ddl}"
    );

    adapter
        .execute_script(&child_ddl)
        .await
        .expect("failed to apply child role DDL");

    // Query pg_auth_members to assert the membership actually exists.
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF NOT EXISTS ( \
                 SELECT 1 FROM pg_auth_members am \
                 JOIN pg_roles member ON member.oid = am.member \
                 JOIN pg_roles granted ON granted.oid = am.roleid \
                 WHERE member.rolname = 'app_ro' AND granted.rolname = 'app_admin' \
               ) THEN \
                 RAISE EXCEPTION 'membership app_ro -> app_admin not found in pg_auth_members'; \
               END IF; \
             END $$",
        )
        .await
        .expect("role membership GRANT did not take effect in database");
}

// ── Test 12: Standalone sequences + serial / identity columns ─────────────────

/// Reverse-engineer a standalone sequence and serial / identity columns. Creates:
///   1. `CREATE SEQUENCE app.counter` — standalone, must be captured.
///   2. a table with `id bigserial PRIMARY KEY` — owned sequence excluded; column → bigserial.
///   3. a table with `n int GENERATED ALWAYS AS IDENTITY` — owned sequence excluded; column → identity.
///   4. a table with `c int DEFAULT nextval('app.counter')` — keeps its nextval default.
#[tokio::test]
async fn introspect_captures_sequences_and_serial_identity() {
    use dbd_core::entity::{EntityType, IdentityKind};

    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "introspect_seq_test").await.unwrap();

    adapter
        .execute_script(
            "CREATE SCHEMA app; \
             CREATE SEQUENCE app.counter; \
             CREATE TABLE app.with_serial (id bigserial PRIMARY KEY, label text); \
             CREATE TABLE app.with_identity (n int GENERATED ALWAYS AS IDENTITY, label text); \
             CREATE TABLE app.with_ref (id int PRIMARY KEY, c int DEFAULT nextval('app.counter'));",
        )
        .await
        .expect("fixture DDL failed");

    let entities = adapter.introspect().await.expect("introspect failed");

    // (1) app.counter captured as a standalone Sequence.
    let counter = entities
        .iter()
        .find(|e| e.name == "app.counter")
        .expect("standalone sequence 'app.counter' not captured");
    assert_eq!(counter.entity_type, EntityType::Sequence);
    assert!(
        counter.body.first().is_some_and(|w| w.contains("CREATE SEQUENCE")),
        "sequence entity should carry rendered CREATE SEQUENCE in body[0], got {:?}",
        counter.body
    );

    // The bigserial- and identity-owned sequences must NOT appear as standalone
    // Sequence entities (they are column-owned, deptype 'a'/'i').
    let standalone_seqs: Vec<&str> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Sequence)
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        standalone_seqs,
        vec!["app.counter"],
        "only the standalone sequence should be captured, got {standalone_seqs:?}"
    );

    // (2) bigserial column → data_type "bigserial", no nextval default.
    let with_serial = entities
        .iter()
        .find(|e| e.name == "app.with_serial")
        .expect("table 'app.with_serial' not captured");
    let id_col = with_serial
        .table_def
        .as_ref()
        .unwrap()
        .columns
        .iter()
        .find(|c| c.name == "id")
        .expect("column 'id' not found");
    assert_eq!(
        id_col.data_type, "bigserial",
        "bigserial PK should map to data_type bigserial"
    );
    assert!(
        id_col.default_value.is_none(),
        "serial column must drop its owned-sequence default, got {:?}",
        id_col.default_value
    );
    assert!(id_col.identity.is_none(), "serial is not identity");

    // (3) identity column → IdentityKind::Always, no default.
    let with_identity = entities
        .iter()
        .find(|e| e.name == "app.with_identity")
        .expect("table 'app.with_identity' not captured");
    let n_col = with_identity
        .table_def
        .as_ref()
        .unwrap()
        .columns
        .iter()
        .find(|c| c.name == "n")
        .expect("column 'n' not found");
    assert_eq!(
        n_col.identity,
        Some(IdentityKind::Always),
        "n should be GENERATED ALWAYS AS IDENTITY"
    );
    assert!(
        n_col.default_value.is_none(),
        "identity column must carry no default, got {:?}",
        n_col.default_value
    );

    // (4) standalone-referencing column keeps its nextval('app.counter') default.
    let with_ref = entities
        .iter()
        .find(|e| e.name == "app.with_ref")
        .expect("table 'app.with_ref' not captured");
    let c_col = with_ref
        .table_def
        .as_ref()
        .unwrap()
        .columns
        .iter()
        .find(|c| c.name == "c")
        .expect("column 'c' not found");
    assert_ne!(c_col.data_type, "serial", "standalone-referencing column is not serial");
    assert!(
        c_col
            .default_value
            .as_deref()
            .is_some_and(|d| d.contains("nextval(") && d.contains("counter")),
        "standalone-referencing column must keep its nextval('app.counter') default, got {:?}",
        c_col.default_value
    );
}

/// Round-trip apply: emit the standalone sequence + the serial/identity/standalone-ref
/// tables via `emit_entity` and execute them (sequence before tables) into a fresh
/// schema → succeeds. Proves self-containment: no missing-sequence error, no
/// double-create of the column-owned sequences.
#[tokio::test]
async fn emitted_sequences_and_serial_identity_apply_to_postgres() {
    use dbd_core::entity::EntityType;

    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "emit_seq_test").await.unwrap();

    // Source schema to introspect from.
    adapter
        .execute_script(
            "CREATE SCHEMA src; \
             CREATE SEQUENCE src.counter; \
             CREATE TABLE src.with_serial (id bigserial PRIMARY KEY, label text); \
             CREATE TABLE src.with_identity (n int GENERATED ALWAYS AS IDENTITY, label text); \
             CREATE TABLE src.with_ref (id int PRIMARY KEY, c int DEFAULT nextval('src.counter'));",
        )
        .await
        .expect("source DDL failed");

    let entities = adapter.introspect().await.expect("introspect failed");

    // Fresh destination schema; rewrite each entity's schema/name from src → dst.
    adapter
        .execute_script("CREATE SCHEMA dst;")
        .await
        .expect("failed to create dst schema");

    let retarget = |e: &dbd_core::Entity| -> dbd_core::Entity {
        let mut c = e.clone();
        c.name = c.name.replacen("src.", "dst.", 1);
        c.schema = Some("dst".into());
        // The sequence body in body[0] is fully-qualified to src — retarget it too.
        c.body = c.body.iter().map(|w| w.replace("src", "dst")).collect();
        c
    };

    // Apply in apply order: the sequence MUST come before the tables that
    // reference it via DEFAULT nextval(...).
    let seq = entities
        .iter()
        .find(|e| e.name == "src.counter" && e.entity_type == EntityType::Sequence)
        .expect("standalone sequence not captured");
    let seq = retarget(seq);
    let seq_sql = dbd_core::emit::emit_entity(&seq).expect("sequence should emit");
    adapter
        .execute_script(&seq_sql)
        .await
        .unwrap_or_else(|e| panic!("emitted sequence DDL rejected: {e}\n--- DDL ---\n{seq_sql}"));

    for tname in ["src.with_serial", "src.with_identity", "src.with_ref"] {
        let t = entities
            .iter()
            .find(|e| e.name == tname && e.entity_type == EntityType::Table)
            .unwrap_or_else(|| panic!("table {tname} not captured"));
        let t = retarget(t);
        let sql = dbd_core::emit::emit_entity(&t).expect("table should emit");
        adapter
            .execute_script(&sql)
            .await
            .unwrap_or_else(|e| panic!("emitted table DDL rejected for {tname}: {e}\n--- DDL ---\n{sql}"));
    }

    // Sanity: the dst tables now exist (proves the whole script applied cleanly).
    assert_table_exists(&*adapter, "dst", "with_serial").await;
    assert_table_exists(&*adapter, "dst", "with_identity").await;
    assert_table_exists(&*adapter, "dst", "with_ref").await;
}

// ── Reset: entity-level by default (schemas + extensions survive) ─────────────

#[tokio::test]
async fn reset_default_drops_entities_keeps_schema_and_reapplies() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    design
        .deploy(&*adapter, false, None, |_| {})
        .await
        .expect("deploy failed");
    assert_table_exists(&*adapter, "app", "items").await;

    // Default reset: force past the prod/migrations guards; no schema/extension drops.
    design
        .reset(&*adapter, "postgres", true, false, false, None)
        .await
        .expect("reset failed");

    // Entities are gone …
    assert_table_absent(&*adapter, "app", "items").await;
    assert_table_absent(&*adapter, "app", "orders").await;
    // … but the schema survives — entity-level reset never drops schemas.
    assert_schema_exists(&*adapter, "app").await;

    // Re-apply works after a default reset (db is back to v0 → a Fresh install).
    design
        .deploy(&*adapter, false, None, |_| {})
        .await
        .expect("re-deploy after reset failed");
    assert_table_exists(&*adapter, "app", "items").await;
}

#[tokio::test]
async fn reset_with_schemas_drops_the_schema() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    design
        .deploy(&*adapter, false, None, |_| {})
        .await
        .expect("deploy failed");
    assert_schema_exists(&*adapter, "app").await;

    // `--schemas` on a postgres target also drops the managed schema itself.
    design
        .reset(&*adapter, "postgres", true, true, false, None)
        .await
        .expect("reset --schemas failed");
    assert_schema_absent(&*adapter, "app").await;
}

// ── Test: reconcile (declarative, pre-release apply) ──────────────────────────

/// Reconcile creates missing objects, ALTERs an existing table to add a column,
/// refuses a destructive column drop unless `allow_destructive` is set, and
/// prunes an orphaned table only when `prune` is set — all in place, without
/// snapshots or a version bump.
#[tokio::test]
async fn reconcile_creates_alters_and_drops_in_place() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();

    let write_design = |version: u32| {
        std::fs::write(
            dir.join("design.yaml"),
            format!(
                "project:\n  name: reconcile_test\n  version: {version}\n\
                 source:\n  dialect: postgresql\nschemas:\n  - app\n"
            ),
        )
        .unwrap();
    };
    let write_items = |body: &str| {
        std::fs::write(dir.join("ddl/table/app/items.ddl"), body).unwrap();
    };
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    write_design(1);

    // ── Phase 1: initial table → reconcile creates schema + table ──
    write_items(
        "set search_path to app;\n\
         create table if not exists items (\n\
           id   uuid primary key default gen_random_uuid()\n\
         , name text not null\n\
         );\n",
    );
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 failed");
    assert_schema_exists(&*adapter, "app").await;
    assert_table_exists(&*adapter, "app", "items").await;
    assert_column_exists(&*adapter, "app", "items", "name").await;

    // ── Phase 2: add a column → reconcile ALTERs in place ──
    write_items(
        "set search_path to app;\n\
         create table if not exists items (\n\
           id   uuid primary key default gen_random_uuid()\n\
         , name text not null\n\
         , qty  integer not null default 0\n\
         );\n",
    );
    let mut summary = None;
    load()
        .reconcile(
            &*adapter,
            false,
            false,
            false,
            None,
            Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| summary = Some(s),
            },
        )
        .await
        .expect("reconcile phase 2 failed");
    assert_column_exists(&*adapter, "app", "items", "qty").await;
    assert_eq!(summary.unwrap().altered, 1, "exactly one table altered");

    // ── Phase 3: drop a column → refused without allow_destructive ──
    write_items(
        "set search_path to app;\n\
         create table if not exists items (\n\
           id  uuid primary key default gen_random_uuid()\n\
         , qty integer not null default 0\n\
         );\n",
    );
    let refused = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await;
    assert!(
        refused.is_err(),
        "destructive reconcile must be refused without allow_destructive"
    );
    assert_column_exists(&*adapter, "app", "items", "name").await; // still present

    // ── Phase 4: same drop with allow_destructive → applied ──
    load()
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("reconcile phase 4 failed");
    assert_column_absent(&*adapter, "app", "items", "name").await;

    // ── Phase 5: orphaned table pruned only with `prune` ──
    // Add a second managed table, reconcile creates it.
    std::fs::write(
        dir.join("ddl/table/app/notes.ddl"),
        "set search_path to app;\n\
         create table if not exists notes (id uuid primary key, body text);\n",
    )
    .unwrap();
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile creating notes failed");
    assert_table_exists(&*adapter, "app", "notes").await;

    // Remove it from the design → it is now an orphan in a managed schema.
    std::fs::remove_file(dir.join("ddl/table/app/notes.ddl")).unwrap();

    // Without prune: the orphan is reported in the plan but left in place.
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile without prune failed");
    assert_eq!(plan.dropped.len(), 1, "notes should be reported as an orphan");
    assert_eq!(plan.dropped[0].entity_name, "app.notes");
    assert_table_exists(&*adapter, "app", "notes").await; // still present

    // With prune: the orphan is dropped.
    let mut prune_summary = None;
    load()
        .reconcile(
            &*adapter,
            false,
            false,
            true,
            None,
            Progress {
                on_start: |_: &str| {},
                on_done: |_: &str, _: Option<&str>| {},
                on_complete: |s| prune_summary = Some(s),
            },
        )
        .await
        .expect("reconcile with prune failed");
    assert_eq!(prune_summary.unwrap().dropped, 1, "one table pruned");
    assert_table_absent(&*adapter, "app", "notes").await;
}

/// Source-hash drift handling for materialized views, end to end:
/// 1. First reconcile CREATEs the matview and stamps a `dbd:hash=` comment.
/// 2. Reconciling the SAME design SKIPs (no warning; oid unchanged).
/// 3. Reconciling a CHANGED definition WARNs and LEAVES THE MATVIEW UNTOUCHED —
///    proven by the `pg_class` oid being UNCHANGED (a drop+recreate would change
///    it). dbd never auto-drops a materialized view. This oid-unchanged-on-drift
///    check is the key proof of the warn-only behavior.
#[tokio::test]
async fn reconcile_warns_on_matview_definition_change() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_mv_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_mv_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    let write_mv = |select: &str| {
        std::fs::write(
            dir.join("ddl/materialized_view/app/mv.ddl"),
            format!("create materialized view app.mv as {select} with data;\n"),
        )
        .unwrap();
    };
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");
    // Persist the current matview oid in a table (survives across pooled
    // connections) so a later DO block can compare against it. A drop+recreate
    // changes the oid; an untouched matview keeps it.
    //
    // Schema-qualified deliberately: the fixture DDL runs `set search_path to
    // app`, which sticks to whichever pooled connection ran it, so an
    // unqualified name here resolves inconsistently depending on which
    // connection the pool hands back.
    async fn stash_oid(adapter: &dyn dbd_core::DatabaseAdapter) {
        adapter
            .execute_script(
                "DROP TABLE IF EXISTS public._mv_oid; \
                 CREATE TABLE public._mv_oid AS SELECT 'app.mv'::regclass::oid AS oid;",
            )
            .await
            .expect("stash oid failed");
    }
    async fn assert_oid_unchanged(adapter: &dyn dbd_core::DatabaseAdapter, msg: &str) {
        adapter
            .execute_script(&format!(
                "DO $$ BEGIN \
                   IF (SELECT oid FROM public._mv_oid) <> 'app.mv'::regclass::oid \
                   THEN RAISE EXCEPTION '{msg}'; END IF; END $$;"
            ))
            .await
            .unwrap_or_else(|e| panic!("oid-unchanged assertion failed: {e}"));
    }

    // ── Phase 1: create the matview + sentinel comment; no warning. ──
    write_mv("select id from app.items");
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 (create matview) failed");
    assert_table_exists(&*adapter, "app", "items").await;
    assert!(
        plan.warnings.is_empty(),
        "create must not warn; got {:?}",
        plan.warnings
    );
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM dbd.matview_stamps \
                              WHERE matview = 'app.mv' AND stamp LIKE 'dbd:hash=v2:%') \
               THEN RAISE EXCEPTION 'matview app.mv has no dbd:hash stamp recorded'; \
               END IF; END $$;",
        )
        .await
        .expect("stamp assertion failed");

    // ── Phase 2: SAME design → SKIP. No warning; oid unchanged. ──
    stash_oid(&*adapter).await;
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 2 (no-change) failed");
    assert!(
        plan.warnings.is_empty(),
        "unchanged matview must not warn; got {:?}",
        plan.warnings
    );
    assert_oid_unchanged(
        &*adapter,
        "matview app.mv oid changed but an unchanged design must not touch it",
    )
    .await;

    // ── Phase 3: change the definition → WARN, matview left untouched (oid same). ──
    stash_oid(&*adapter).await;
    write_mv("select id, name from app.items");
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 3 (drift) failed");
    assert_oid_unchanged(
        &*adapter,
        "matview app.mv was recreated on drift but dbd must only warn",
    )
    .await;
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("app.mv") && w.contains("differs")),
        "a drifted matview must produce a warning; got {:?}",
        plan.warnings
    );
}

/// `reconcile --dry-run` must PREVIEW a materialized-view create WITHOUT writing:
/// the returned plan lists the matview under `matview_creates`, yet the object is
/// NOT created in the database (a dry run writes nothing). A follow-up
/// `dry_run=false` reconcile then actually creates it. This is the key proof that
/// matview detection was hoisted *before* the dry-run early-return — previously
/// `--dry-run` returned before detection ran, so it showed nothing about matviews.
#[tokio::test]
async fn reconcile_dry_run_previews_matview_create_without_writing() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_mv_dryrun_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_mv_dryrun_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/materialized_view/app/mv.ddl"),
        "create materialized view app.mv as select id from app.items with data;\n",
    )
    .unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // ── Dry-run: the plan PREVIEWS the matview create, but writes nothing. ──
    let plan = load()
        .reconcile(&*adapter, /*dry_run*/ true, false, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed");
    assert!(
        plan.matview_creates.iter().any(|n| n == "app.mv"),
        "dry-run plan must preview the matview create; got {:?}",
        plan.matview_creates
    );
    // A dry run writes nothing: neither the matview nor its source table exists.
    assert!(
        !adapter.matview_states().await.unwrap().contains_key("app.mv"),
        "dry-run must NOT create the matview"
    );
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF to_regclass('app.mv') IS NOT NULL \
               THEN RAISE EXCEPTION 'app.mv must not exist after a dry-run'; END IF; END $$;",
        )
        .await
        .expect("post-dry-run existence check failed");

    // ── Real reconcile: now the matview is actually created. ──
    let plan = load()
        .reconcile(&*adapter, /*dry_run*/ false, false, false, None, Progress::none())
        .await
        .expect("reconcile (create matview) failed");
    assert!(
        plan.matview_creates.iter().any(|n| n == "app.mv"),
        "the real reconcile plan should still list the matview create; got {:?}",
        plan.matview_creates
    );
    assert!(
        adapter.matview_states().await.unwrap().contains_key("app.mv"),
        "reconcile with dry_run=false must create the matview"
    );
}

/// End-to-end materialized-view drift reporting in the READ-ONLY `diff_live`:
/// after a matview is created + stamped by reconcile, `diff_live` reports
/// Drifted (design definition changed), Missing (design matview absent from the
/// DB) and Orphan (DB matview absent from the design), in name-sorted order —
/// and writes NOTHING (proven by the drifted matview's oid being unchanged, the
/// Missing matview still not existing, and the Orphan still existing afterward).
#[tokio::test]
async fn diff_live_reports_matview_drift_read_only() {
    use dbd_core::schema_diff::MatviewDriftKind;

    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "diff_mv_drift_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: diff_mv_drift_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    let write_mv = |select: &str| {
        std::fs::write(
            dir.join("ddl/materialized_view/app/mv.ddl"),
            format!("create materialized view app.mv as {select} with data;\n"),
        )
        .unwrap();
    };
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // ── Setup: reconcile creates + stamps app.mv (and its source table). ──
    write_mv("select id from app.items");
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile (create matview) failed");

    // ── (a) SAME design → no matview drift (stored hash matches the design). ──
    let d = load().diff_live(&*adapter, None).await.expect("diff (in sync) failed");
    assert!(
        d.matview_drift.is_empty(),
        "an in-sync matview must not report drift; got {:?}",
        d.matview_drift
    );

    // ── Introduce all three drift kinds at once: ──
    // Drifted: change app.mv's definition in the design.
    write_mv("select id, name from app.items");
    // Missing: a second design matview the DB doesn't have.
    std::fs::write(
        dir.join("ddl/materialized_view/app/mv2.ddl"),
        "create materialized view app.mv2 as select id from app.items with data;\n",
    )
    .unwrap();
    // Orphan: a DB matview with no design counterpart (created directly here).
    adapter
        .execute_script("create materialized view app.orphan as select id from app.items with data;")
        .await
        .expect("create orphan matview failed");

    // Snapshot app.mv's oid so we can prove diff_live never recreated it.
    adapter
        .execute_script(
            "DROP TABLE IF EXISTS public._mv_oid; \
             CREATE TABLE public._mv_oid AS SELECT 'app.mv'::regclass::oid AS oid;",
        )
        .await
        .expect("stash oid failed");

    // ── Run the read-only diff. ──
    let d = load().diff_live(&*adapter, None).await.expect("diff (drift) failed");

    let kind_of = |name: &str| d.matview_drift.iter().find(|m| m.name == name).map(|m| m.kind);
    assert_eq!(
        kind_of("app.mv"),
        Some(MatviewDriftKind::Drifted),
        "got {:?}",
        d.matview_drift
    );
    assert_eq!(
        kind_of("app.mv2"),
        Some(MatviewDriftKind::Missing),
        "got {:?}",
        d.matview_drift
    );
    assert_eq!(
        kind_of("app.orphan"),
        Some(MatviewDriftKind::Orphan),
        "got {:?}",
        d.matview_drift
    );
    assert!(!d.is_empty(), "matview drift must make the diff non-empty");
    // Deterministic (name-sorted) order.
    let names: Vec<&str> = d.matview_drift.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["app.mv", "app.mv2", "app.orphan"],
        "matview drift must be name-sorted"
    );

    // ── Read-only proof: diff wrote nothing. ──
    // The drifted matview was NOT recreated (oid unchanged).
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF (SELECT oid FROM public._mv_oid) <> 'app.mv'::regclass::oid \
               THEN RAISE EXCEPTION 'diff_live recreated app.mv but it must be read-only'; END IF; END $$;",
        )
        .await
        .expect("oid-unchanged assertion failed");
    // The Missing matview was NOT created; the Orphan was NOT dropped.
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF to_regclass('app.mv2') IS NOT NULL \
               THEN RAISE EXCEPTION 'diff_live created app.mv2 but it must be read-only'; END IF; \
               IF to_regclass('app.orphan') IS NULL \
               THEN RAISE EXCEPTION 'diff_live dropped app.orphan but it must be read-only'; END IF; \
             END $$;",
        )
        .await
        .expect("read-only existence assertions failed");
}

/// Applying a matview's DDL TWICE (as `dbd apply` does when it re-applies the
/// current design) must be idempotent. Postgres has no `CREATE OR REPLACE
/// MATERIALIZED VIEW`, so authored matview DDL uses `CREATE MATERIALIZED VIEW
/// IF NOT EXISTS` + `CREATE UNIQUE INDEX IF NOT EXISTS` (see the fixture and
/// README). Without those clauses the second apply errors `42P07 relation
/// already exists`; this test drives the real `apply_entity` path (which reads
/// the DDL file verbatim) twice and asserts the second succeeds.
#[tokio::test]
async fn matview_ddl_reapply_is_idempotent() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "matview_idem_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: matview_idem_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    // Authored per the documented idempotent convention: both the matview and
    // its unique index carry IF NOT EXISTS so a second apply of the same DDL is
    // a clean no-op rather than a 42P07 error.
    std::fs::write(
        dir.join("ddl/materialized_view/app/mv.ddl"),
        "create materialized view if not exists app.mv as \
         select id from app.items with data;\n\
         create unique index if not exists mv_id_uidx on app.mv(id);\n",
    )
    .unwrap();

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // Prerequisites: the schema and the table the matview selects from.
    adapter
        .execute_script("CREATE SCHEMA IF NOT EXISTS app")
        .await
        .expect("create schema failed");
    let items = design
        .entities()
        .iter()
        .find(|e| e.name == "app.items")
        .expect("table entity discovered");
    adapter.apply_entity(items).await.expect("apply table failed");

    let mv = design
        .entities()
        .iter()
        .find(|e| e.name == "app.mv")
        .expect("matview entity discovered");

    // First apply CREATEs the matview + its unique index.
    adapter.apply_entity(mv).await.expect("first matview apply failed");
    // Second apply re-runs the SAME DDL — must be a clean no-op, not 42P07.
    adapter
        .apply_entity(mv)
        .await
        .expect("second matview apply must succeed (IF NOT EXISTS idempotency)");

    // The matview still exists after both applies.
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_matviews WHERE schemaname = 'app' AND matviewname = 'mv'",
        "materialized view app.mv",
    )
    .await;
}

/// Coherence proof for `apply` + `reconcile` on materialized views (Gap 2):
/// `Design::apply` must stamp the `dbd:hash` sentinel on a matview it creates,
/// so a subsequent `reconcile` of the SAME unchanged design recognizes it as
/// dbd-managed and does NOT warn "exists but is not stamped by dbd".
///
/// Before the fix, `apply` ran the DDL verbatim without stamping, so reconcile
/// warned about the unstamped matview forever; this test is red then. It also
/// structurally proves `apply` now invokes `sync_refresh_jobs`: the run
/// completes `Ok` against embedded PG (which has no pg_cron), so the sync
/// must have no-op'd rather than errored.
#[tokio::test]
async fn apply_stamps_matview_so_later_reconcile_does_not_warn() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "apply_stamp_mv_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: apply_stamp_mv_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    // Idempotent-apply convention (IF NOT EXISTS) so re-applies are clean no-ops.
    std::fs::write(
        dir.join("ddl/materialized_view/app/mv.ddl"),
        "create materialized view if not exists app.mv as \
         select id from app.items with data;\n\
         create unique index if not exists mv_id_uidx on app.mv(id);\n",
    )
    .unwrap();

    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // ── apply the whole design (creates the matview + stamps the sentinel). ──
    load()
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed (sync_refresh_jobs must no-op without pg_cron)");
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_matviews WHERE schemaname = 'app' AND matviewname = 'mv'",
        "materialized view app.mv",
    )
    .await;
    // apply must have recorded the dbd:hash stamp in dbd's bookkeeping.
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM dbd.matview_stamps \
                              WHERE matview = 'app.mv' AND stamp LIKE 'dbd:hash=v2:%') \
               THEN RAISE EXCEPTION 'apply did not stamp app.mv with a dbd:hash sentinel'; \
               END IF; END $$;",
        )
        .await
        .expect("apply should have stamped the dbd:hash sentinel");

    // ── reconcile the SAME unchanged design: matview is recognized, no warning. ──
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile after apply failed");
    assert!(
        !plan.warnings.iter().any(|w| w.contains("app.mv")),
        "apply-created matview must not warn on the next reconcile; got {:?}",
        plan.warnings
    );
}

/// Newly-created-only correctness (Gap 2), end to end: a matview that ALREADY
/// exists (stamped with an OLD hash) must NOT be re-stamped by `apply` — `apply`
/// uses `CREATE MATERIALIZED VIEW IF NOT EXISTS`, a no-op on it, so its deployed
/// definition may be stale. Re-stamping a "current" hash would mask that drift.
/// Here the design's matview definition changes between the first and second
/// apply; the second apply must leave the ORIGINAL sentinel in place so a later
/// reconcile still detects the drift and warns.
#[tokio::test]
async fn apply_does_not_restamp_pre_existing_matview() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "apply_no_restamp_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: apply_no_restamp_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    // Same object name, IF NOT EXISTS so the 2nd apply is a no-op on the object;
    // only the SELECT body differs between the two writes.
    let write_mv = |select: &str| {
        std::fs::write(
            dir.join("ddl/materialized_view/app/mv.ddl"),
            format!("create materialized view if not exists app.mv as {select} with data;\n"),
        )
        .unwrap();
    };
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // ── First apply: create + stamp the v1 hash. ──
    write_mv("select id from app.items");
    load()
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("first apply failed");
    // Capture the stamped hash for later comparison.
    let hash_after_first = adapter
        .matview_states()
        .await
        .expect("matview_states failed")
        .get("app.mv")
        .cloned()
        .flatten()
        .expect("first apply must have stamped a hash");

    // ── Second apply with a DIFFERENT definition: IF NOT EXISTS makes the object
    //    a no-op, and the pre-existing matview must NOT be re-stamped. ──
    write_mv("select id, name from app.items");
    load()
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("second apply failed");
    let hash_after_second = adapter
        .matview_states()
        .await
        .expect("matview_states failed")
        .get("app.mv")
        .cloned()
        .flatten()
        .expect("matview should still carry the original sentinel");

    assert_eq!(
        hash_after_first, hash_after_second,
        "apply must NOT re-stamp an already-existing matview (would mask drift)"
    );

    // A reconcile of the changed design still detects the drift and warns —
    // proving the un-restamped sentinel preserved drift detection.
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile failed");
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.contains("app.mv") && w.contains("differs")),
        "drift on a pre-existing matview must still warn; got {:?}",
        plan.warnings
    );
}

/// A dry-run reconcile computes a plan but writes nothing.
#[tokio::test]
async fn reconcile_dry_run_is_read_only() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_dry").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_dry\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id uuid primary key);\n",
    )
    .unwrap();

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");
    let plan = design
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed");

    assert_eq!(plan.added, vec!["app.items".to_string()], "plan proposes the new table");
    assert_table_absent(&*adapter, "app", "items").await; // nothing created
}

/// `diff_live` reports drift against a live database and writes nothing.
#[tokio::test]
async fn diff_live_reports_drift_and_writes_nothing() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "diff_live_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: diff_live_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\n\
         create table if not exists items (id uuid primary key);\n",
    )
    .unwrap();

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");
    let diff = design.diff_live(&*adapter, None).await.expect("diff_live failed");

    assert!(
        !diff.is_empty(),
        "a design table absent from the live DB must show drift"
    );
    assert!(
        diff.changes.iter().any(|c| c.entity_name == "app.items"),
        "drift should name the missing table, got {:?}",
        diff.changes
    );
    assert_table_absent(&*adapter, "app", "items").await; // diff wrote nothing
}

/// Issue #8, end-to-end: `dbd diff` must see foreign keys with REAL Postgres
/// introspection. Apply a design whose child table carries an inline FK, then:
///   1. an in-sync live DB (real, named FK) shows NO FK drift vs the design's
///      inline unnamed FK — the cross-representation match works through the
///      full pipeline, and
///   2. dropping the live FK out from under the design surfaces as drift.
/// Before the fix, `diff_live` stripped FKs before comparing, so neither the
/// match nor the drift was ever computed.
#[tokio::test]
async fn diff_live_sees_foreign_keys_with_real_introspection() {
    use dbd_core::SchemaDiff;
    use dbd_core::diff::{DiffAction, FieldType};

    /// Whether the diff reports any constraint-level change on `table`.
    fn constraint_change_on(d: &SchemaDiff, table: &str) -> bool {
        d.changes.iter().any(|c| {
            c.entity_name == table
                && matches!(&c.action, DiffAction::Change(fcs)
                    if fcs.iter().any(|f| f.field_type == FieldType::Constraint))
        })
    }

    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "diff_fk_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: diff_fk_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/parents.ddl"),
        "set search_path to app;\n\
         create table if not exists parents (id uuid primary key);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/children.ddl"),
        "set search_path to app;\n\
         create table if not exists children (\n\
           id uuid primary key\n\
         , parent_id uuid references parents(id)\n\
         );\n",
    )
    .unwrap();

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // Apply the design → the live DB now has the FK (named children_parent_id_fkey).
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    // 1. In-sync: the real named live FK must match the design's inline unnamed FK.
    let d = design.diff_live(&*adapter, None).await.expect("diff_live failed");
    assert!(
        !constraint_change_on(&d, "app.children"),
        "in-sync FK must not surface as drift; got {:?}",
        d.changes
    );

    // 2. Drop the FK out from under the design → drift must be reported.
    adapter
        .execute_script("ALTER TABLE app.children DROP CONSTRAINT children_parent_id_fkey;")
        .await
        .expect("drop fk failed");
    let d = design.diff_live(&*adapter, None).await.expect("diff_live failed");
    assert!(
        constraint_change_on(&d, "app.children"),
        "a live FK dropped out from under the design must surface as drift; got {:?}",
        d.changes
    );
}

/// Issue #8, end-to-end: `dbd reconcile` must MANAGE foreign keys against a real
/// database — add a declared FK the live DB lacks (non-destructive), leave an
/// in-sync FK alone, and drop a removed FK only under `--allow-destructive`.
#[tokio::test]
async fn reconcile_converges_foreign_keys() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_fk_test").await.unwrap();

    // Design with an inline FK on the child table.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_fk_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/parents.ddl"),
        "set search_path to app;\ncreate table if not exists parents (id uuid primary key);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/children.ddl"),
        "set search_path to app;\n\
         create table if not exists children (\n\
           id uuid primary key\n\
         , parent_id uuid references parents(id)\n\
         );\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    let fk_pred = "SELECT 1 FROM pg_constraint c \
         JOIN pg_class cls ON cls.oid = c.conrelid \
         JOIN pg_namespace ns ON ns.oid = cls.relnamespace \
         WHERE c.contype = 'f' AND ns.nspname = 'app' AND cls.relname = 'children'";

    // Apply → the live DB has the FK.
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");
    assert_catalog(&*adapter, true, fk_pred, "FK on app.children").await;

    // In-sync: reconcile must not churn the FK.
    let plan = design
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed");
    assert!(
        !plan.altered.iter().any(|s| s.sql.contains("FOREIGN KEY")),
        "in-sync FK must not produce reconcile churn; got {:?}",
        plan.altered
    );

    // Drop the FK out from under the design.
    adapter
        .execute_script("ALTER TABLE app.children DROP CONSTRAINT children_parent_id_fkey;")
        .await
        .expect("drop fk failed");
    assert_catalog(&*adapter, false, fk_pred, "FK on app.children").await;

    // Reconcile (non-destructive) must re-add the declared FK.
    design
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile re-adding FK failed");
    assert_catalog(&*adapter, true, fk_pred, "FK on app.children").await;

    // A second design without the FK: dropping it is destructive.
    let tmp2 = tempfile::tempdir().unwrap();
    let dir2 = tmp2.path();
    std::fs::create_dir_all(dir2.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir2.join("design.yaml"),
        "project:\n  name: reconcile_fk_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir2.join("ddl/table/app/parents.ddl"),
        "set search_path to app;\ncreate table if not exists parents (id uuid primary key);\n",
    )
    .unwrap();
    std::fs::write(
        dir2.join("ddl/table/app/children.ddl"),
        "set search_path to app;\n\
         create table if not exists children (id uuid primary key, parent_id uuid);\n",
    )
    .unwrap();
    let design2 = Design::from_config_with_dir(&dir2.join("design.yaml"), "dev", Some(dir2)).expect("load design2");

    // Without --allow-destructive → refused, FK untouched.
    let refused = design2
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await;
    assert!(
        refused.is_err(),
        "dropping an FK without --allow-destructive must be refused"
    );
    assert_catalog(&*adapter, true, fk_pred, "FK on app.children").await;

    // With --allow-destructive → the FK is dropped.
    design2
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("destructive reconcile failed");
    assert_catalog(&*adapter, false, fk_pred, "FK on app.children").await;
}

// ── Reconcile convergence: comments and keyword defaults ──────────────────────

/// Apply a design, then require reconcile to report NOTHING — the property both
/// of these fixes exist for, checked against a real server.
///
/// Two spellings Postgres rewrites used to churn on every run:
/// - `default current_date` comes back from `pg_get_expr` as `CURRENT_DATE`, so
///   reconcile emitted a `SET DEFAULT` that Postgres immediately re-spelled;
/// - column comments were cleared by `canonicalize` with no convergence pass, so
///   `dbd diff` reported comment drift that reconcile could never act on.
///
/// The test then perturbs the live database three ways — changed comment, removed
/// comment, changed default — and requires reconcile to restore each and settle.
#[tokio::test]
async fn reconcile_converges_comments_and_keyword_defaults() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_comment_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_comment_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    // Lowercase keyword defaults throughout — Postgres stores every one of these
    // in its own casing, which is what used to read as drift.
    std::fs::write(
        dir.join("ddl/table/app/docs.ddl"),
        "set search_path to app;\n\
         create table if not exists docs (\n\
           id          uuid        primary key\n\
         , effective   date        not null default current_date\n\
         , created_at  timestamptz not null default current_timestamp\n\
         , author      text        not null default current_user\n\
         , touched_at  timestamptz not null default now()\n\
         , label       text        not null default 'Mixed Case'\n\
         , title       text\n\
         , body        text\n\
         );\n\
         comment on column docs.title is 'Display name';\n\
         comment on column docs.body is 'The document''s body';\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    /// The reconcile plan's ALTER SQL, or an empty vec when it is in sync.
    async fn plan_sql(design: &Design, adapter: &dyn dbd_core::DatabaseAdapter) -> Vec<String> {
        design
            .reconcile(adapter, true, true, false, None, Progress::none())
            .await
            .expect("dry-run reconcile failed")
            .altered
            .into_iter()
            .map(|s| s.sql)
            .collect()
    }

    // Freshly applied → nothing to do. This is the assertion that used to fail:
    // `SET DEFAULT current_date` and every `COMMENT ON COLUMN` reappeared forever.
    let sql = plan_sql(&design, &*adapter).await;
    assert!(
        sql.is_empty(),
        "a freshly applied design must reconcile to no change; got {sql:?}"
    );

    // Reading the live comment back must give exactly what the design declared,
    // including the escaped apostrophe.
    let comment_of = |col: &'static str| {
        let adapter = &*adapter;
        async move {
            adapter
                .introspect()
                .await
                .expect("introspect failed")
                .into_iter()
                .find(|e| e.name == "app.docs")
                .and_then(|e| e.table_def)
                .and_then(|td| {
                    td.columns
                        .iter()
                        .find(|c| c.name == col)
                        .and_then(|c| c.comment.clone())
                })
        }
    };
    assert_eq!(comment_of("body").await.as_deref(), Some("The document's body"));

    // Perturb: change one comment, remove another, and change a default.
    adapter
        .execute_script(
            "COMMENT ON COLUMN app.docs.title IS 'drifted text';
             COMMENT ON COLUMN app.docs.body IS NULL;
             ALTER TABLE app.docs ALTER COLUMN effective SET DEFAULT '2020-01-01';",
        )
        .await
        .expect("perturb failed");

    let sql = plan_sql(&design, &*adapter).await.join("\n");
    assert!(sql.contains("app.docs"), "drift must be detected; got: {sql}");

    design
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("reconcile failed");

    // Restored to the design.
    assert_eq!(comment_of("title").await.as_deref(), Some("Display name"));
    assert_eq!(comment_of("body").await.as_deref(), Some("The document's body"));
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_attrdef ad \
         JOIN pg_class c ON c.oid = ad.adrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ad.adnum \
         WHERE n.nspname = 'app' AND c.relname = 'docs' AND a.attname = 'effective' \
           AND pg_get_expr(ad.adbin, ad.adrelid) = 'CURRENT_DATE'",
        "app.docs.effective default restored to CURRENT_DATE",
    )
    .await;

    // And it SETTLES: a second reconcile has nothing left to do. Restoring drift
    // but never converging is exactly the reported failure.
    let sql = plan_sql(&design, &*adapter).await;
    assert!(
        sql.is_empty(),
        "reconcile must converge, not churn on every run; got {sql:?}"
    );
}

// ── Batch transaction (atomic apply) ──────────────────────────────────────────

#[tokio::test]
async fn batch_commit_persists_ddl() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "batch_test").await.unwrap();

    adapter.begin_batch().await.expect("begin_batch");
    adapter
        .execute_script("CREATE SCHEMA committed_schema")
        .await
        .expect("ddl inside batch");
    adapter.commit_batch().await.expect("commit_batch");

    // After commit the schema is durable and visible on a fresh pool connection.
    assert_schema_exists(&*adapter, "committed_schema").await;
}

#[tokio::test]
async fn batch_rollback_discards_ddl() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "batch_test").await.unwrap();

    adapter.begin_batch().await.expect("begin_batch");
    adapter
        .execute_script("CREATE SCHEMA rolled_back_schema")
        .await
        .expect("ddl inside batch");
    adapter.rollback_batch().await.expect("rollback_batch");

    // The schema created inside the batch must be gone after rollback.
    assert_schema_absent(&*adapter, "rolled_back_schema").await;
}

#[tokio::test]
async fn batch_failure_mid_plan_rolls_back_prior_ddl() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "batch_test").await.unwrap();

    // Simulate an interrupted upgrade: an early object succeeds, a later one
    // fails. The whole batch must roll back — the prior object leaves no trace.
    adapter.begin_batch().await.expect("begin_batch");
    adapter
        .execute_script("CREATE SCHEMA partial_schema")
        .await
        .expect("first object applies");
    let failed = adapter
        .execute_script("CREATE TABLE partial_schema.t (id nonexistent_type)")
        .await;
    assert!(failed.is_err(), "invalid DDL should error");
    adapter.rollback_batch().await.expect("rollback_batch");

    assert_schema_absent(&*adapter, "partial_schema").await;
}

// ── Bookkeeping lives in the `dbd` schema; heal folds legacy `_dbd_*` copies ───

/// Assert `dbd.meta.version` for `project` equals `expected`.
async fn assert_dbd_meta_version(adapter: &dyn dbd_core::DatabaseAdapter, project: &str, expected: i32) {
    let sql = format!(
        "DO $$ DECLARE v integer; BEGIN \
           SELECT version INTO v FROM dbd.meta WHERE project = '{project}'; \
           IF v IS DISTINCT FROM {expected} THEN \
             RAISE EXCEPTION 'dbd.meta[{project}].version = %, expected {expected}', v; \
           END IF; END $$"
    );
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("assert_dbd_meta_version({project}) failed: {e}"));
}

/// A scoped apply can leave `_dbd_meta` in a non-`public` schema (pooled
/// connections don't share `search_path`). The read-only detection path must
/// still recognise the DB as managed BEFORE heal runs (both-names awareness),
/// and `heal_bookkeeping` must fold every row into `dbd.meta` — preserving
/// unrelated rows — then drop the stray copy, so later access can't miss it
/// (which surfaced as `relation "_dbd_meta" does not exist` during reconcile).
#[tokio::test]
async fn heal_relocates_stray_meta_and_preserves_rows() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "meta_heal_test").await.unwrap();

    // Simulate the leak: `_dbd_meta` created in `dojo`, not `public`, with a row
    // for this project (v7) and an unrelated project's row (v99) to prove the
    // heal moves data rather than recreating an empty table.
    adapter
        .execute_script(
            "CREATE SCHEMA dojo; \
             CREATE TABLE dojo._dbd_meta ( \
                project     varchar NOT NULL PRIMARY KEY, \
                env         varchar NOT NULL DEFAULT 'dev', \
                version     integer NOT NULL DEFAULT 0, \
                created_at  timestamptz NOT NULL DEFAULT now(), \
                updated_at  timestamptz NOT NULL DEFAULT now() \
             ); \
             INSERT INTO dojo._dbd_meta (project, env, version) \
                VALUES ('meta_heal_test', 'prod', 7), ('other_project', 'dev', 99)",
        )
        .await
        .expect("seed stray dojo._dbd_meta");

    // Before heal the new layout doesn't exist; detection still sees it as managed.
    assert_eq!(adapter.reverse_managed_version().await.unwrap(), Some(7));

    adapter.heal_bookkeeping().await.unwrap();

    // `_dbd_meta` now lives in `dbd.meta`; the stray `dojo` copy is gone.
    assert_table_exists(&*adapter, "dbd", "meta").await;
    assert_table_absent(&*adapter, "dojo", "_dbd_meta").await;

    // This project's row rode along (v7), and the unrelated row too (v99) —
    // proving data was moved, not dropped.
    assert_dbd_meta_version(&*adapter, "meta_heal_test", 7).await;
    assert_dbd_meta_version(&*adapter, "other_project", 99).await;

    // Post-heal writes target `dbd.meta` and reads resolve against it.
    adapter.set_project_meta("prod", 8, None).await.unwrap();
    assert_eq!(adapter.get_db_version().await.unwrap(), 8);
    let meta = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(meta.version, 8);
    assert_eq!(meta.env, "prod");
    assert_eq!(meta.project, "meta_heal_test");
}

/// The CLI scope/prod guard reads `get_project_meta()` BEFORE the core op heals.
/// On a legacy, not-yet-healed DB whose `_dbd_meta` predates the `scope` column,
/// that read must still return the row via `get_meta`'s SQLSTATE-42703
/// (undefined_column) fallback — otherwise the guard silently disables. This is
/// the pre-heal path `heal_folds_legacy_meta_without_scope_column` does NOT hit
/// (it reads only after heal, against `dbd.meta`, which has the scope column).
#[tokio::test]
async fn get_meta_reads_legacy_scopeless_meta_before_heal() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "preheal").await.unwrap();
    // Legacy public._dbd_meta WITHOUT the scope column, prod row — and DO NOT heal.
    adapter
        .execute_script(
            "CREATE TABLE public._dbd_meta ( \
                project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
                version integer NOT NULL DEFAULT 0, \
                created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
             INSERT INTO public._dbd_meta (project, env, version) VALUES ('preheal','prod',4);",
        )
        .await
        .unwrap();
    // The guard reads meta BEFORE core heals — must return the prod row via the 42703 fallback.
    let m = adapter
        .get_project_meta()
        .await
        .unwrap()
        .expect("pre-heal legacy meta must be readable");
    assert_eq!(m.env, "prod");
    assert_eq!(m.version, 4);
    assert_eq!(m.scope, None);
}

// ── heal_bookkeeping: move to `dbd` schema + fold legacy `public._dbd_*` ───────

#[tokio::test]
async fn heal_fresh_db_creates_dbd_schema() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "fresh").await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    assert_table_exists(&*adapter, "dbd", "meta").await;
    assert_table_exists(&*adapter, "dbd", "migrations").await;
    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "public", "_dbd_migrations").await;
}

#[tokio::test]
async fn heal_folds_legacy_public_meta_into_dbd() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "legacy").await.unwrap();
    adapter.execute_script(
        "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         CREATE TABLE public._dbd_migrations ( \
            project varchar NOT NULL, version integer NOT NULL, applied_at timestamptz NOT NULL DEFAULT now(), \
            description text, checksum text, PRIMARY KEY (project, version) ); \
         INSERT INTO public._dbd_meta (project, env, version, scope) VALUES ('legacy','prod',4,'public'); \
         INSERT INTO public._dbd_migrations (project, version, description, checksum) VALUES ('legacy',1,'init','abc');"
    ).await.unwrap();

    adapter.heal_bookkeeping().await.unwrap();

    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "public", "_dbd_migrations").await;
    assert_dbd_meta_version(&*adapter, "legacy", 4).await;
    let m = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m.env, "prod");
    assert_eq!(m.scope.as_deref(), Some("public"));
    assert_eq!(adapter.get_db_version().await.unwrap(), 4);
}

#[tokio::test]
async fn heal_folds_legacy_meta_without_scope_column() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "nolscope").await.unwrap();
    adapter
        .execute_script(
            "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('nolscope','prod',4);",
        )
        .await
        .unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    let m = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m.version, 4);
    assert_eq!(m.env, "prod");
    assert_eq!(m.scope, None);
    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
}

#[tokio::test]
async fn heal_folds_multiple_stray_copies_public_wins() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "p").await.unwrap();
    // public (canonical) v1 + stray dojo v2 for the same project.
    adapter
        .execute_script(
            "CREATE SCHEMA dojo; \
         CREATE TABLE public._dbd_meta (project varchar PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()); \
         CREATE TABLE dojo._dbd_meta (LIKE public._dbd_meta INCLUDING ALL); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('p','prod',1); \
         INSERT INTO dojo._dbd_meta   (project, env, version) VALUES ('p','prod',2); \
         CREATE TABLE public._dbd_migrations (project varchar, version integer, applied_at timestamptz DEFAULT now(), \
            description text, checksum text, PRIMARY KEY (project, version)); \
         CREATE TABLE dojo._dbd_migrations (LIKE public._dbd_migrations INCLUDING ALL); \
         INSERT INTO public._dbd_migrations (project, version) VALUES ('p',1); \
         INSERT INTO dojo._dbd_migrations   (project, version) VALUES ('p',2);",
        )
        .await
        .unwrap();

    adapter.heal_bookkeeping().await.unwrap();

    // Canonical public row wins (v1), matching today's read-prefers-public semantics.
    assert_dbd_meta_version(&*adapter, "p", 1).await;
    // Migrations union both copies (composite PK).
    assert_table_absent(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "dojo", "_dbd_meta").await;
    assert_table_absent(&*adapter, "dojo", "_dbd_migrations").await;
    let n: i64 = 2; // versions 1 and 2 present
    let sql = format!(
        "DO $$ DECLARE c bigint; BEGIN SELECT count(*) INTO c FROM dbd.migrations WHERE project='p'; \
         IF c <> {n} THEN RAISE EXCEPTION 'dbd.migrations count = %, expected {n}', c; END IF; END $$"
    );
    adapter.execute_script(&sql).await.unwrap();
}

#[tokio::test]
async fn heal_is_idempotent() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "idem").await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    adapter.set_project_meta("prod", 3, Some("public")).await.unwrap();
    adapter.heal_bookkeeping().await.unwrap(); // second heal — no-op
    let m = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m.version, 3);
    assert_eq!(m.scope.as_deref(), Some("public"));
}

/// `dbd migrate --status` is read-only: it must resolve the version through
/// the both-names-aware catalog read WITHOUT ever invoking `heal_bookkeeping`,
/// which would relocate/drop a legacy DB's bookkeeping as a side effect of a
/// status check. Regression test for a status command that used to heal.
#[tokio::test]
async fn get_db_version_reads_legacy_without_healing() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "statusonly").await.unwrap();
    // Legacy public._dbd_meta at v5 — a read (as migrate --status does) must NOT relocate/drop it.
    adapter
        .execute_script(
            "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('statusonly','prod',5);",
        )
        .await
        .unwrap();
    // The read returns the legacy version via the both-names path...
    assert_eq!(adapter.get_db_version().await.unwrap(), 5);
    // ...and does NOT heal: legacy table still present, dbd.meta NOT created.
    assert_table_exists(&*adapter, "public", "_dbd_meta").await;
    assert_table_absent(&*adapter, "dbd", "meta").await;
}

/// The `dbd` bookkeeping schema must be invisible to reverse-engineering /
/// introspection — it's dbd's own internal state, never a project object —
/// while still surviving as a real schema in the database.
#[tokio::test]
async fn dbd_schema_excluded_from_introspect_and_survives() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "excl").await.unwrap();
    adapter.heal_bookkeeping().await.unwrap();
    // introspect() must not surface dbd.meta / dbd.migrations (or the dbd schema)
    let ents = adapter.introspect().await.unwrap();
    assert!(
        ents.iter().all(|e| !e.name.starts_with("dbd.") && e.name != "dbd"),
        "dbd.* leaked into introspect: {:?}",
        ents.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(
        ents.iter().all(|e| e.schema.as_deref() != Some("dbd")),
        "an entity with schema = dbd leaked into introspect: {:?}",
        ents.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    // list_entities() (the `dbd inspect` refcache path) must also exclude dbd.*
    let names = adapter.list_entities().await.unwrap();
    assert!(
        names.iter().all(|n| !n.starts_with("dbd.")),
        "dbd.* leaked into list_entities: {names:?}"
    );
    // dbd bookkeeping is still present (it's dbd-internal, not a project object)
    assert_table_exists(&*adapter, "dbd", "meta").await;
    assert_table_exists(&*adapter, "dbd", "migrations").await;
}

// ── Reconcile convergence: PRIMARY KEY replacement ────────────────────────────

/// End-to-end: `dbd reconcile` must be able to REPLACE a composite PRIMARY KEY on
/// a table that already holds rows.
///
/// It could not. Reconcile diffs name-stripped snapshots, so a PK's identity was
/// the internal matching key `pk:<columns>` — and that key was then emitted as a
/// SQL identifier, producing `DROP CONSTRAINT pk:tenant_id,metric_id`. Postgres
/// rejects it, and because reconcile applies in one transaction the whole change
/// was blocked; the only way through was dropping and recreating the table, which
/// a table with data cannot afford. Two further defects sat behind that one: the
/// replacement PK was emitted as `ADD CONSTRAINT unnamed PRIMARY KEY` (a PK's
/// backing index takes the constraint name, and index names are schema-scoped, so
/// the second table to converge this way collided), and the add was ordered before
/// the drop, which Postgres rejects with "multiple primary keys".
///
/// This asserts against a real server, with real rows: the new PK exists, the old
/// one is gone, every row survived, and a second reconcile is a no-op.
#[tokio::test]
async fn reconcile_replaces_primary_key_on_table_with_data() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_pk_test").await.unwrap();

    let write_design = |dir: &std::path::Path, pk: &str| {
        std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
        std::fs::write(
            dir.join("design.yaml"),
            "project:\n  name: reconcile_pk_test\n  version: 1\n\
             source:\n  dialect: postgresql\nschemas:\n  - app\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ddl/table/app/metrics.ddl"),
            format!(
                "set search_path to app;\n\
                 create table if not exists metrics (\n\
                   tenant_id uuid not null\n\
                 , metric_id uuid not null\n\
                 , bucket date not null\n\
                 , value integer\n\
                 , primary key ({pk})\n\
                 );\n"
            ),
        )
        .unwrap();
    };

    // A predicate matching the PK of app.metrics over exactly `cols`, in order.
    let pk_over = |cols: &str| {
        format!(
            "SELECT 1 FROM pg_constraint c \
               JOIN pg_class cls ON cls.oid = c.conrelid \
               JOIN pg_namespace ns ON ns.oid = cls.relnamespace \
             WHERE c.contype = 'p' AND ns.nspname = 'app' AND cls.relname = 'metrics' \
               AND (SELECT array_agg(a.attname ORDER BY k.ord) \
                    FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) \
                    JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum) \
                   = ARRAY[{cols}]::name[]"
        )
    };
    let old_pk = pk_over("'tenant_id','metric_id'");
    let new_pk = pk_over("'tenant_id','metric_id','bucket'");

    // ── Apply the original design, then put real data behind the PK ──
    let tmp = tempfile::tempdir().unwrap();
    write_design(tmp.path(), "tenant_id, metric_id");
    let design =
        Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).expect("load design");
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");
    assert_catalog(&*adapter, true, &old_pk, "PK on (tenant_id, metric_id)").await;

    adapter
        .execute_script(
            "INSERT INTO app.metrics (tenant_id, metric_id, bucket, value) VALUES \
               ('11111111-1111-1111-1111-111111111111','22222222-2222-2222-2222-222222222222','2026-01-01',1), \
               ('11111111-1111-1111-1111-111111111111','33333333-3333-3333-3333-333333333333','2026-01-02',2);",
        )
        .await
        .expect("seed rows failed");

    // In-sync: the live PK Postgres auto-named must match the design's unnamed
    // declaration, or reconcile churns a destructive drop+add every run.
    let plan = design
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed");
    assert!(
        !plan.altered.iter().any(|s| s.sql.contains("PRIMARY KEY")),
        "an in-sync PK must not produce reconcile churn; got {:?}",
        plan.altered
    );

    // ── Widen the PK to (tenant_id, metric_id, bucket) ──
    let tmp2 = tempfile::tempdir().unwrap();
    write_design(tmp2.path(), "tenant_id, metric_id, bucket");
    let design2 =
        Design::from_config_with_dir(&tmp2.path().join("design.yaml"), "dev", Some(tmp2.path())).expect("load design2");

    // Replacing a PK drops a constraint, so it is gated as destructive.
    let refused = design2
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await;
    assert!(
        refused.is_err(),
        "replacing a PK without --allow-destructive must be refused"
    );
    assert_catalog(&*adapter, true, &old_pk, "PK on (tenant_id, metric_id)").await;

    // With --allow-destructive it must actually apply — this is what the
    // `DROP CONSTRAINT pk:tenant_id,metric_id` defect made impossible.
    design2
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("PK replacement reconcile failed");
    assert_catalog(&*adapter, true, &new_pk, "PK on (tenant_id, metric_id, bucket)").await;
    assert_catalog(&*adapter, false, &old_pk, "old PK on (tenant_id, metric_id)").await;

    // The rows the table was carrying are still there — a PK replacement must not
    // be a table rebuild.
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.metrics HAVING count(*) = 2",
        "both seeded rows",
    )
    .await;

    // And it converges: a second reconcile has nothing left to do.
    let settled = design2
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("post-replacement dry-run reconcile failed");
    assert!(
        settled.is_empty(),
        "reconcile must settle after replacing the PK; got {settled:?}"
    );
}

/// Issue #12.1: `unique nulls not distinct` must survive the ALTER path.
///
/// `dbd apply` on a fresh database ran the DDL file verbatim and kept the clause;
/// `dbd reconcile` altering an existing table re-derived the constraint from the
/// parsed model, which had nowhere to hold it. The constraint that landed was
/// materially weaker than the one declared — NULL rows no longer collided — and
/// `dbd diff` then compared UNIQUE by name alone and certified the result as
/// in sync, so nothing anywhere reported the difference.
///
/// The check is `pg_index.indnullsnotdistinct` rather than a duplicate-insert
/// attempt, because it pins the property directly: an insert test would also pass
/// against a constraint that merely happened to reject that one pair of rows.
#[tokio::test]
async fn reconcile_preserves_nulls_not_distinct_on_a_unique_constraint() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "nnd_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: nnd_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    let write_thing = |body: &str| std::fs::write(dir.join("ddl/table/app/thing.ddl"), body).unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    // `indnullsnotdistinct` is PG15+; read through `to_jsonb` so the predicate is
    // false rather than an error on an older server.
    let nnd_on = |cols: &str| {
        format!(
            "SELECT 1 FROM pg_index ix \
               JOIN pg_class i ON i.oid = ix.indexrelid \
               JOIN pg_class t ON t.oid = ix.indrelid \
               JOIN pg_namespace ns ON ns.oid = t.relnamespace \
              WHERE ns.nspname = 'app' AND t.relname = 'thing' \
                AND pg_get_indexdef(ix.indexrelid) LIKE '%({cols})%' \
                AND coalesce((to_jsonb(ix) ->> 'indnullsnotdistinct')::boolean, false)"
        )
    };

    // ── Phase 1: fresh create — the clause is preserved (this always worked) ──
    write_thing(
        "set search_path to app;\n\
         create table if not exists thing (\n\
           id   int primary key\n\
         , grp  text\n\
         , name text not null\n\
         , unique nulls not distinct (grp, name)\n\
         );\n",
    );
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 failed");
    assert_catalog(
        &*adapter,
        true,
        &nnd_on("grp, name"),
        "NULLS NOT DISTINCT on (grp, name)",
    )
    .await;

    // ── Phase 2: add a column and re-key onto it, keeping the clause ──
    // This is the ALTER path: the constraint is dropped and re-added from the
    // parsed model, so the clause has to survive parse → diff → generate.
    write_thing(
        "set search_path to app;\n\
         create table if not exists thing (\n\
           id    int primary key\n\
         , grp   text\n\
         , scope text\n\
         , name  text not null\n\
         , unique nulls not distinct (grp, scope, name)\n\
         );\n",
    );
    load()
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("reconcile phase 2 failed");
    assert_catalog(
        &*adapter,
        true,
        &nnd_on("grp, scope, name"),
        "NULLS NOT DISTINCT on (grp, scope, name)",
    )
    .await;

    // The declared constraint permits exactly one (NULL, NULL, 'x') row. Proving
    // the second insert is rejected is what the issue actually cared about.
    //
    // Columns are named rather than positional: `ALTER TABLE ADD COLUMN` appends,
    // so the live order is (id, grp, name, scope) while the DDL file declares
    // (id, grp, scope, name).
    adapter
        .execute_script("INSERT INTO app.thing (id, grp, scope, name) VALUES (1, NULL, NULL, 'x')")
        .await
        .expect("first row must be accepted");
    assert!(
        adapter
            .execute_script("INSERT INTO app.thing (id, grp, scope, name) VALUES (2, NULL, NULL, 'x')")
            .await
            .is_err(),
        "a second (NULL, NULL, 'x') row violates the declared constraint and must be rejected"
    );

    // ── Phase 3: it converges — a re-run has nothing left to do ──
    let settled = load()
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("post-alter dry-run reconcile failed");
    assert!(
        settled.is_empty(),
        "reconcile must settle after preserving the clause; got {settled:?}"
    );
}

/// The converse of the above: a live `NULLS NOT DISTINCT` constraint against a
/// design that declares a *plain* unique is drift, and reconcile must weaken it
/// rather than leave the stronger constraint in place. Without this, the fix
/// would only be half a fix — convergence has to work in both directions.
#[tokio::test]
async fn reconcile_drops_nulls_not_distinct_when_the_design_stops_declaring_it() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "nnd_drop_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: nnd_drop_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    let write_thing = |body: &str| std::fs::write(dir.join("ddl/table/app/thing.ddl"), body).unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    let any_nnd = "SELECT 1 FROM pg_index ix \
           JOIN pg_class t ON t.oid = ix.indrelid \
           JOIN pg_namespace ns ON ns.oid = t.relnamespace \
          WHERE ns.nspname = 'app' AND t.relname = 'thing' \
            AND coalesce((to_jsonb(ix) ->> 'indnullsnotdistinct')::boolean, false)";

    write_thing(
        "set search_path to app;\n\
         create table if not exists thing (\n\
           id int primary key, grp text, name text not null\n\
         , unique nulls not distinct (grp, name)\n\
         );\n",
    );
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 failed");
    assert_catalog(&*adapter, true, any_nnd, "NULLS NOT DISTINCT constraint").await;

    // Same columns, clause removed → the constraint must be replaced, not kept.
    write_thing(
        "set search_path to app;\n\
         create table if not exists thing (\n\
           id int primary key, grp text, name text not null\n\
         , unique (grp, name)\n\
         );\n",
    );
    load()
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("reconcile phase 2 failed");
    assert_catalog(&*adapter, false, any_nnd, "NULLS NOT DISTINCT constraint").await;

    let settled = load()
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("post-alter dry-run reconcile failed");
    assert!(settled.is_empty(), "reconcile must settle; got {settled:?}");
}

/// Issue #12.2: `reconcile` must converge when an enum loses a value.
///
/// Postgres has no `ALTER TYPE … DROP VALUE`, so the diff engine emitted no SQL,
/// `plan_reconcile` skipped the empty statement, and reconcile reported
/// `0 altered` while `dbd diff` flagged the same drift on every subsequent run —
/// it could not converge and never said so.
///
/// Covers the parts that make the swap non-trivial: a dependent view (which
/// blocks `ALTER … TYPE` and must be dropped and re-created), a column default
/// (typed against the old type), and the rows surviving the rewrite.
#[tokio::test]
async fn reconcile_removes_an_enum_value_by_recreating_the_type() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "enum_value_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/enum/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/view/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: enum_value_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    let write_enum = |values: &str| {
        std::fs::write(
            dir.join("ddl/enum/app/colour.ddl"),
            format!("set search_path to app;\ncreate type colour as enum ({values});\n"),
        )
        .unwrap();
    };
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\n\
         create table if not exists t (\n\
           id int primary key\n\
         , c  colour not null default 'red'\n\
         );\n",
    )
    .unwrap();
    // A view over the enum column: this is what blocks `ALTER … TYPE` and has to
    // be dropped first, then restored when reconcile re-applies its views.
    std::fs::write(
        dir.join("ddl/view/app/t_v.ddl"),
        "set search_path to app;\n\
         create or replace view t_v as select id, c from t;\n",
    )
    .unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    let enum_is = |values: &str| format!("SELECT 1 WHERE (SELECT enum_range(NULL::app.colour)::text) = '{{{values}}}'");

    // ── Phase 1: create the three-value enum, the table, and the view ──
    write_enum("'red', 'green', 'blue'");
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 failed");
    assert_catalog(&*adapter, true, &enum_is("red,green,blue"), "enum app.colour").await;
    adapter
        .execute_script("INSERT INTO app.t (id, c) VALUES (1, 'red'), (2, 'green')")
        .await
        .expect("seed rows failed");

    // ── Phase 2: drop a value — refused without allow_destructive ──
    write_enum("'red', 'green'");
    let refused = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await;
    assert!(
        refused.is_err(),
        "removing an enum value can reject existing rows, so it must be gated"
    );
    assert_catalog(&*adapter, true, &enum_is("red,green,blue"), "enum app.colour").await;

    // ── Phase 3: with allow_destructive it actually applies ──
    load()
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("reconcile phase 3 failed");
    assert_catalog(&*adapter, true, &enum_is("red,green"), "enum app.colour").await;

    // The displaced type must not be left behind.
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM pg_type ty JOIN pg_namespace ns ON ns.oid = ty.typnamespace \
          WHERE ns.nspname = 'app' AND ty.typname = 'colour__dbd_old'",
        "displaced type app.colour__dbd_old",
    )
    .await;

    // The rows survive the rewrite, the default survives the drop/restore, and
    // the view reconcile re-applied is queryable again.
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.t WHERE id = 1 AND c = 'red' AND EXISTS (SELECT 1 FROM app.t WHERE id = 2 AND c = 'green')",
        "both seeded rows with their values",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_attrdef d \
           JOIN pg_class cl ON cl.oid = d.adrelid \
           JOIN pg_namespace ns ON ns.oid = cl.relnamespace \
           JOIN pg_attribute a ON a.attrelid = cl.oid AND a.attnum = d.adnum \
          WHERE ns.nspname = 'app' AND cl.relname = 't' AND a.attname = 'c' \
            AND pg_get_expr(d.adbin, d.adrelid) LIKE '%red%'",
        "default on app.t.c",
    )
    .await;
    assert_catalog(&*adapter, true, "SELECT 1 FROM app.t_v WHERE id = 1", "view app.t_v").await;

    // ── Phase 4: it converges — the drift `diff` reported forever is gone ──
    let settled = load()
        .reconcile(&*adapter, true, false, false, None, Progress::none())
        .await
        .expect("post-recreation dry-run reconcile failed");
    assert!(
        settled.is_empty(),
        "reconcile must settle after recreating the enum; got {settled:?}"
    );
    // `dbd diff` is what the issue reported as stuck, and it computes its own
    // snapshots rather than reusing the plan — so assert on it directly.
    let live_diff = load().diff_live(&*adapter, None).await.expect("diff_live failed");
    assert!(
        live_diff.is_empty(),
        "diff must stop reporting the enum drift; got {:?}",
        live_diff.changes
    );
}

/// A row still holding the removed value cannot be silently rewritten — there is
/// no value to rewrite it to. The `USING` cast fails, and because the whole
/// recreation runs in one implicit transaction the database is left exactly as it
/// was: the old type, the old rows, and the view still in place.
#[tokio::test]
async fn removing_an_enum_value_still_in_use_fails_without_half_applying() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "enum_in_use_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/enum/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/view/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: enum_in_use_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    let write_enum = |values: &str| {
        std::fs::write(
            dir.join("ddl/enum/app/colour.ddl"),
            format!("set search_path to app;\ncreate type colour as enum ({values});\n"),
        )
        .unwrap();
    };
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table if not exists t (id int primary key, c colour not null);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/view/app/t_v.ddl"),
        "set search_path to app;\ncreate or replace view t_v as select id, c from t;\n",
    )
    .unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    write_enum("'red', 'green', 'blue'");
    load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 failed");
    adapter
        .execute_script("INSERT INTO app.t (id, c) VALUES (1, 'blue')")
        .await
        .expect("seed row failed");

    write_enum("'red', 'green'");
    let failed = load()
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await;
    assert!(failed.is_err(), "a row holding the removed value must fail the run");

    // Nothing half-applied: the type, the row and the view are all as they were.
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 WHERE (SELECT enum_range(NULL::app.colour)::text) = '{red,green,blue}'",
        "enum app.colour unchanged",
    )
    .await;
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM pg_type ty JOIN pg_namespace ns ON ns.oid = ty.typnamespace \
          WHERE ns.nspname = 'app' AND ty.typname = 'colour__dbd_old'",
        "displaced type app.colour__dbd_old",
    )
    .await;
    assert_catalog(&*adapter, true, "SELECT 1 FROM app.t WHERE c = 'blue'", "the blue row").await;
    assert_catalog(&*adapter, true, "SELECT 1 FROM app.t_v WHERE id = 1", "view app.t_v").await;
}

// ── Reconcile convergence: generated columns and varchar CHECKs ───────────────

/// The reconcile plan's ALTER SQL, or an empty vec when it is in sync.
async fn altered_sql(design: &Design, adapter: &dyn dbd_core::DatabaseAdapter) -> Vec<String> {
    design
        .reconcile(adapter, true, true, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed")
        .altered
        .into_iter()
        .map(|s| s.sql)
        .collect()
}

/// Issues #16 and #17, checked against a real server because neither is
/// reproducible from unit tests: both come from the asymmetry between what the
/// design says and what Postgres stores, which only a live catalog exhibits.
///
/// - **#16** — `GENERATED ALWAYS AS (…) STORED` keeps its expression in
///   `pg_attrdef`, the same catalog an ordinary `DEFAULT` uses. Read as a default,
///   reconcile planned `ALTER COLUMN … DROP DEFAULT`, which Postgres refuses
///   ("column … is a generated column"), aborting the run.
/// - **#17** — a `CHECK`/index predicate over a `varchar` column is stored with
///   the `::text` cast Postgres inserts, so the design key and the live key never
///   matched and the constraint was dropped and re-added on every run.
///
/// `n::text <> ''` on an `integer` column is in the fixture deliberately: that
/// cast is the author's and must survive, so this also pins the line between the
/// two cases.
#[tokio::test]
async fn reconcile_converges_generated_columns_and_varchar_checks() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_gen_check_test").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_gen_check_test\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/chk.ddl"),
        "set search_path to app;\n\
         create table if not exists chk (\n\
           name varchar(100) not null\n\
         , kind varchar(20)  not null\n\
         , n    integer\n\
         , constraint chk_name_lower check (name = lower(name))\n\
         , constraint chk_kind_in    check (kind in ('a', 'b'))\n\
         , constraint chk_name_len   check (length(name) > 2)\n\
         , constraint chk_n_text     check (n::text <> '')\n\
         );\n\
         create index if not exists chk_partial_ix on chk (n) where name = lower(name);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/gen.ddl"),
        "set search_path to app;\n\
         create table if not exists gen (\n\
           content text\n\
         , tsv tsvector generated always as (to_tsvector('english', coalesce(content, ''))) stored\n\
         );\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    // Freshly applied → nothing to do. Before the fix this produced a
    // `DROP DEFAULT` on the generated column plus a drop/re-add of all three
    // varchar CHECKs and a drop/re-create of the partial index.
    let sql = altered_sql(&design, &*adapter).await;
    assert!(
        sql.is_empty(),
        "a freshly applied design must reconcile to no change; got {sql:?}"
    );

    // The generated column really is generated on the server — proving the
    // convergence came from reading it correctly, not from dbd dropping the
    // clause on both sides.
    let generated: Option<String> = adapter
        .introspect()
        .await
        .expect("introspect failed")
        .into_iter()
        .find(|e| e.name == "app.gen")
        .and_then(|e| e.table_def)
        .and_then(|td| td.columns.iter().find(|c| c.name == "tsv").cloned())
        .and_then(|c| c.generated);
    assert!(
        generated.is_some_and(|g| g.contains("to_tsvector")),
        "the STORED expression must be read as `generated`, not as a default"
    );

    // Convergence is a two-run property: one pass can restore drift while still
    // churning, and a single clean run looks identical to a real fix.
    design
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("reconcile failed");
    let sql = altered_sql(&design, &*adapter).await;
    assert!(sql.is_empty(), "reconcile must be idempotent; second pass got {sql:?}");
}

/// Defaults PostgreSQL rewrites when it stores them (#18).
///
/// Reconcile compared defaults as text, so a literal the server re-spells read
/// as drift on every run: it emitted a `SET DEFAULT`, the server rewrote it
/// again, and the next run saw the same change. Non-destructive, but it meant
/// reconcile never reached a clean state — so no CI drift-guard could be built
/// on it, and real drift hid among the permanent noise.
///
/// The four rewrites, all measured on a live server before any code changed:
///
/// | written | stored |
/// |---|---|
/// | `'epoch'` (timestamptz) | `'1969-12-31 18:00:00-06'` — *in the session timezone* |
/// | `'{"b":1,"a":2}'` (jsonb) | `'{"a": 2, "b": 1}'` |
/// | `'t'` (boolean) | `true` |
/// | `'infinity'` (timestamptz) | `'infinity'` — already converged |
///
/// `'now'` and `'today'` are deliberately absent: the server resolves them at
/// DDL time and freezes the result, so no comparison can converge and
/// normalising them would mean inventing a value. See
/// `a_frozen_special_input_is_not_normalised`.
#[tokio::test]
async fn reconcile_converges_defaults_the_server_rewrites() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "reconcile_rewritten_defaults").await.unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: reconcile_rewritten_defaults\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/cursors.ddl"),
        "set search_path to app;\n\
         create table if not exists cursors (\n\
           id            uuid        primary key\n\
         , last_seen     timestamptz not null default 'epoch'\n\
         , never         timestamptz not null default 'infinity'\n\
         , sync_policy   jsonb       not null default '{\"config_pull\":\"realtime\",\"pull_interval_s\":300,\"offline_grace_h\":72,\"buffer_flush\":\"on_reconnect\"}'\n\
         , enabled       boolean     not null default 't'\n\
         , explicit_ts   timestamptz not null default '2024-03-01 07:00:00-05'\n\
         );\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design");

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    async fn plan_sql(design: &Design, adapter: &dyn dbd_core::DatabaseAdapter) -> Vec<String> {
        design
            .reconcile(adapter, true, true, false, None, Progress::none())
            .await
            .expect("dry-run reconcile failed")
            .altered
            .into_iter()
            .map(|s| s.sql)
            .collect()
    }

    // The assertion that used to fail: every one of these came back as a
    // `SET DEFAULT`, forever.
    let sql = plan_sql(&design, &*adapter).await;
    assert!(
        sql.is_empty(),
        "a freshly applied design must reconcile to no change; got {sql:?}"
    );

    // And it must still SEE a real change — a normaliser that made everything
    // equal would also pass the assertion above.
    std::fs::write(
        dir.join("ddl/table/app/cursors.ddl"),
        "set search_path to app;\n\
         create table if not exists cursors (\n\
           id            uuid        primary key\n\
         , last_seen     timestamptz not null default 'epoch'\n\
         , never         timestamptz not null default 'infinity'\n\
         , sync_policy   jsonb       not null default '{\"config_pull\":\"batched\"}'\n\
         , enabled       boolean     not null default 'f'\n\
         , explicit_ts   timestamptz not null default '2024-03-01 07:00:00-05'\n\
         );\n",
    )
    .unwrap();
    let changed = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("reload");
    let sql = plan_sql(&changed, &*adapter).await;
    let joined = sql.join("\n");
    assert!(
        joined.contains("sync_policy") && joined.contains("enabled"),
        "a genuinely changed jsonb and boolean default must still be reported; got {sql:?}"
    );
    assert!(
        !joined.contains("last_seen") && !joined.contains("never") && !joined.contains("explicit_ts"),
        "and the unchanged ones must stay quiet; got {sql:?}"
    );
}

/// `reconcile --prune` must not drop a table the platform owns (#7).
///
/// Prune is the only dbd operation that drops something the design does not
/// declare. On a Supabase target, one project table declared in `auth` puts
/// the whole schema into `managed_schemas` — and every table Supabase keeps
/// there then reads as an orphan. Dropping them loses the policies and grants
/// the environment runs on.
///
/// This needs a real database: the drop only appears in the plan when the live
/// side actually holds an undeclared table in the protected schema, so a test
/// against an empty target passes whether the filter is wired up or not. That
/// is exactly what happened to the first version of this test.
#[tokio::test]
async fn reconcile_does_not_prune_a_platform_owned_table() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "prune_guard").await.unwrap();

    // Stand in for what Supabase provisions: an `auth` schema with its own
    // table that no dbd design declares.
    adapter
        .execute_script(
            "CREATE SCHEMA IF NOT EXISTS auth; CREATE TABLE IF NOT EXISTS auth.users (id uuid PRIMARY KEY);",
        )
        .await
        .expect("seed the platform schema");

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: prune_guard\n  version: 1\n\
         source:\n  dialect: postgresql\n\
         target:\n  supabase:\n    url: $DATABASE_URL\n\
         schemas:\n  - auth\n",
    )
    .unwrap();
    // The project's own table, inside the platform's schema — this is what
    // pulls `auth` into the managed set.
    std::fs::create_dir_all(dir.join("ddl/table/auth")).unwrap();
    std::fs::write(
        dir.join("ddl/table/auth/app_profiles.ddl"),
        "set search_path to auth;\ncreate table if not exists app_profiles (id integer primary key);",
    )
    .unwrap();

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    assert_eq!(design.target_name(), "supabase");
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    let plan = design
        .reconcile(&*adapter, true, true, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed");

    let dropped: Vec<&str> = plan.dropped.iter().map(|s| s.entity_name.as_str()).collect();
    assert!(
        !dropped.iter().any(|n| n.starts_with("auth.")),
        "a Supabase-owned table was planned for pruning: {dropped:?}"
    );

    // And the guard must be narrow: an undeclared table in a schema the
    // project DOES own is still prunable, or the fix has just disabled prune.
    adapter
        .execute_script("CREATE SCHEMA IF NOT EXISTS app; CREATE TABLE IF NOT EXISTS app.orphan (id integer);")
        .await
        .expect("seed an ownable orphan");
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: prune_guard\n  version: 1\n\
         source:\n  dialect: postgresql\n\
         target:\n  supabase:\n    url: $DATABASE_URL\n\
         schemas:\n  - auth\n  - app\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("ddl/table/app/kept.ddl"),
        "set search_path to app;\ncreate table if not exists kept (id integer primary key);",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("reload");
    let plan = design
        .reconcile(&*adapter, true, true, false, None, Progress::none())
        .await
        .expect("dry-run reconcile failed");
    let dropped: Vec<&str> = plan.dropped.iter().map(|s| s.entity_name.as_str()).collect();
    assert!(
        dropped.iter().any(|n| n.starts_with("app.")),
        "an orphan in the project's own schema must still be prunable: {dropped:?}"
    );
}

// ── #40: a scope narrows what is applied, not what the project owns ─────────

/// A project with two tables in one schema — `orders` references `items` — and a
/// scope that selects only `items`. Deployed whole, plus one live table the design
/// does not declare at all.
async fn scoped_project(adapter: &dyn dbd_core::DatabaseAdapter) -> (tempfile::TempDir, Design) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: scoped_prune\n  version: 1\n\
         source:\n  dialect: postgresql\nschemas:\n  - app\n\
         scopes:\n  items_only:\n    includes:\n      - app.items\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\ncreate table if not exists items (id integer primary key, name text);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/orders.ddl"),
        "set search_path to app;\ncreate table if not exists orders (\n  id integer primary key\n, item_id integer references app.items (id)\n);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    design
        .reconcile(adapter, false, false, false, None, Progress::none())
        .await
        .expect("create the whole design");
    adapter
        .execute_script("create table app.stray (id integer);")
        .await
        .expect("an undeclared table");
    (tmp, design)
}

fn orders_fk() -> String {
    "SELECT 1 FROM information_schema.table_constraints \
     WHERE table_schema = 'app' AND table_name = 'orders' AND constraint_type = 'FOREIGN KEY'"
        .to_string()
}

/// `reconcile --prune --scope items_only` dropped `app.orders` — the design's own
/// table, outside the scope but in the scope's schema (#40).
#[tokio::test]
async fn a_scoped_reconcile_prunes_only_what_the_design_does_not_declare() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "scoped_prune").await.unwrap();
    let (_tmp, design) = scoped_project(&*adapter).await;
    let scope = design.resolve_scope(Some("items_only"), None).expect("scope");

    let plan = design
        .reconcile(&*adapter, false, false, true, Some(&scope), Progress::none())
        .await
        .expect("scoped reconcile");

    let dropped: Vec<&str> = plan.dropped.iter().map(|d| d.entity_name.as_str()).collect();
    assert_eq!(
        dropped,
        vec!["app.stray"],
        "only what no part of the design declares is an orphan"
    );
    assert_table_exists(&*adapter, "app", "orders").await;
    assert_catalog(&*adapter, true, &orders_fk(), "app.orders foreign key").await;
    assert_table_absent(&*adapter, "app", "stray").await;
}

/// `dbd diff --scope` must show what reconcile would do — no drop for `app.orders`.
#[tokio::test]
async fn a_scoped_diff_reports_no_drop_for_the_designs_own_tables() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "scoped_diff").await.unwrap();
    let (_tmp, design) = scoped_project(&*adapter).await;
    let scope = design.resolve_scope(Some("items_only"), None).expect("scope");

    let diff = design.diff_live(&*adapter, Some(&scope)).await.expect("scoped diff");
    let touched: Vec<String> = diff
        .changes
        .iter()
        .map(|c| format!("{:?} {}", c.action, c.entity_name))
        .collect();
    assert_eq!(
        touched,
        vec!["Drop app.stray"],
        "the scoped diff names the orphan and nothing else"
    );
}

/// Unscoped, nothing changes: the stray is the only orphan and both tables stay.
#[tokio::test]
async fn an_unscoped_reconcile_still_prunes_orphans_and_keeps_every_declared_table() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "unscoped_prune").await.unwrap();
    let (_tmp, design) = scoped_project(&*adapter).await;

    let plan = design
        .reconcile(&*adapter, false, false, true, None, Progress::none())
        .await
        .expect("unscoped reconcile");
    let dropped: Vec<&str> = plan.dropped.iter().map(|d| d.entity_name.as_str()).collect();
    assert_eq!(dropped, vec!["app.stray"]);
    assert_table_exists(&*adapter, "app", "items").await;
    assert_table_exists(&*adapter, "app", "orders").await;
}

/// A role, or an extension declared without `schema:`, made `public` a managed
/// schema, so `reconcile --prune` dropped tables in `public` that the design
/// never declared — data loss for anything else living there.
#[tokio::test]
async fn a_role_or_bare_extension_does_not_let_prune_reach_public() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "role_public").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: role_public\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
         target:\n  postgres:\n    url: $DATABASE_URL\n    roles:\n      - name: prune_probe_role\n    \
         extensions:\n      - plpgsql\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\ncreate table if not exists items (id integer primary key);\n",
    )
    .unwrap();
    adapter
        .execute_script("create table public.not_mine (id integer);")
        .await
        .expect("a table the design does not own");
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");

    let plan = design
        .reconcile(&*adapter, false, false, true, None, Progress::none())
        .await
        .expect("reconcile --prune");

    let dropped: Vec<&str> = plan.dropped.iter().map(|d| d.entity_name.as_str()).collect();
    assert!(
        dropped.is_empty(),
        "nothing outside `app` is the design's to prune: {dropped:?}"
    );
    assert_table_exists(&*adapter, "public", "not_mine").await;
    assert_table_exists(&*adapter, "app", "items").await;
}

/// `serial` declares no key. The model claimed one, so after reconcile created
/// the table, the next diff wanted `ADD PRIMARY KEY (seq_no)` beside the real
/// key, and every real reconcile failed with "multiple primary keys".
#[tokio::test]
async fn reconcile_converges_on_a_serial_column_that_is_not_the_key() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "serial_converge").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: serial_converge\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table if not exists t (id uuid primary key, seq_no serial, label text);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    design
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("create");

    let diff = design.diff_live(&*adapter, None).await.expect("diff");
    let touched: Vec<String> = diff
        .changes
        .iter()
        .map(|c| format!("{:?} {}", c.action, c.entity_name))
        .collect();
    assert!(touched.is_empty(), "nothing left to change: {touched:?}");
}

/// A materialized view's own `COMMENT ON` belongs to its author. dbd kept its
/// `dbd:hash` drift stamp in that same comment: the first apply overwrote the
/// author's text, and the next one — re-running the file's COMMENT — erased the
/// stamp, after which every reconcile warned the view was "not stamped by dbd".
#[tokio::test]
async fn a_matview_keeps_its_own_comment_and_dbd_still_tracks_its_drift() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "mv_comment").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: mv_comment\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\ncreate table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/materialized_view/app/mv.ddl"),
        "create materialized view if not exists app.mv as select id from app.items with data;\n\
         comment on materialized view app.mv is 'Revenue per day';\n",
    )
    .unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");

    for _ in 0..2 {
        load()
            .apply(&*adapter, None, false, None, Progress::none())
            .await
            .expect("apply");
    }

    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF obj_description('app.mv'::regclass, 'pg_class') IS DISTINCT FROM 'Revenue per day' \
               THEN RAISE EXCEPTION 'the author''s comment was replaced: %', \
                    obj_description('app.mv'::regclass, 'pg_class'); \
               END IF; END $$;",
        )
        .await
        .expect("the matview keeps the comment its file sets");

    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile");
    assert!(
        !plan.warnings.iter().any(|w| w.contains("app.mv")),
        "dbd still recognises the view it created: {:?}",
        plan.warnings
    );
}

/// Postgres stores `nextval('core.invoice_seq')` as an OID and reports it back
/// as `nextval('core.invoice_seq'::regclass)`, so a design that leaves the cast
/// off read as a changed default on every run: diff always showed a SET DEFAULT
/// and reconcile always reported one alteration.
#[tokio::test]
async fn a_nextval_default_converges() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "nextval_converge").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    for d in ["ddl/sequence/core", "ddl/table/sales"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: nextval_converge\nsource:\n  dialect: postgresql\nschemas:\n  - core\n  - sales\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/sequence/core/invoice_seq.ddl"),
        "create sequence if not exists core.invoice_seq;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/sales/orders.ddl"),
        "set search_path to sales;\ncreate table if not exists orders (\n  id integer primary key\n\
         , invoice_no integer default nextval('core.invoice_seq')\n);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply");

    let diff = design.diff_live(&*adapter, None).await.expect("diff");
    let touched: Vec<String> = diff
        .changes
        .iter()
        .map(|c| format!("{:?} {}", c.action, c.entity_name))
        .collect();
    assert!(
        touched.is_empty(),
        "the default is already what the design says: {touched:?}"
    );
}

/// A project with no `project.version` has applied no migration. Reconcile
/// recorded version 1 for it (apply records 0), after which a dev `reset`
/// refused with "database has applied migrations".
#[tokio::test]
async fn reconcile_without_a_version_records_none_applied() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "unversioned").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: unversioned\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    design
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile");

    let meta = adapter.get_project_meta().await.unwrap().expect("meta");
    assert_eq!(meta.version, 0, "no migration has been applied");
    design
        .reset(&*adapter, "postgres", false, false, false, None)
        .await
        .expect("a dev reset of an unversioned project is allowed");
}

/// A database a dbd before 0.25 stamped keeps its drift tracking on upgrade.
/// That dbd kept the `dbd:hash` stamp in the matview's comment; 0.25 reads the
/// comment only while no stamp row exists, and the first apply re-ran the
/// file's own `COMMENT ON`, erasing the old stamp before anything recorded it.
/// Every reconcile afterwards warned "not stamped by dbd".
#[tokio::test]
async fn an_upgraded_database_keeps_its_matview_drift_stamp() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "mv_upgrade").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::create_dir_all(dir.join("ddl/materialized_view/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: mv_upgrade\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/items.ddl"),
        "set search_path to app;\ncreate table if not exists items (id int primary key, name text);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/materialized_view/app/mv.ddl"),
        "create materialized view if not exists app.mv as select id from app.items with data;\n\
         comment on materialized view app.mv is 'Revenue per day';\n",
    )
    .unwrap();
    let load = || Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load");
    load()
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("first apply");

    // Put the database back the way 0.24 left it: the stamp in the comment,
    // no stamp row.
    adapter
        .execute_script(
            "DO $$ DECLARE s text; BEGIN \
               SELECT stamp INTO s FROM dbd.matview_stamps WHERE matview = 'app.mv'; \
               EXECUTE format('COMMENT ON MATERIALIZED VIEW app.mv IS %L', s); \
               DELETE FROM dbd.matview_stamps; END $$;",
        )
        .await
        .expect("simulate a 0.24 stamp");

    load()
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply with 0.25");
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile");
    assert!(
        !plan.warnings.iter().any(|w| w.contains("app.mv")),
        "the upgraded view is still tracked: {:?}",
        plan.warnings
    );
}

// ── Reconcile creates in the dependency order apply uses ──────────────────────

/// Write `files` (path relative to `dir`, SQL) under a fresh project at `dir`
/// whose `design.yaml` is `yaml`, and load it.
fn write_project(dir: &std::path::Path, yaml: &str, files: &[(&str, &str)]) -> Design {
    std::fs::write(dir.join("design.yaml"), yaml).unwrap();
    for (rel, sql) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, sql).unwrap();
    }
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design")
}

/// Reconcile ran every table create before any function, so
/// on a fresh database a table whose default calls a project function failed
/// with `function core.default_tier() does not exist`, and a trigger declared in
/// a table file failed on its trigger function — leaving the tables created
/// before it behind. `apply` has always created both in dependency order; the
/// entity refs record the default's call and the trigger's function, so the
/// order is there to follow.
///
/// The second phase is the same mistake on the ALTER path: a column added to an
/// existing table with a default calling a function that is new in this run.
#[tokio::test]
async fn reconcile_creates_functions_before_the_tables_whose_defaults_and_triggers_call_them() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "fn_order_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let yaml = "project:\n  name: fn_order_test\nsource:\n  dialect: postgresql\nschemas:\n  - core\n";
    let default_tier = (
        "ddl/function/core/default_tier.ddl",
        "create or replace function core.default_tier() returns text\n\
         language sql immutable as $$ select 'basic'::text $$;\n",
    );
    let touch = (
        "ddl/function/core/touch.ddl",
        "create or replace function core.touch() returns trigger\n\
         language plpgsql as $$ begin new.touched_at := now(); return new; end $$;\n",
    );
    let accounts = (
        "ddl/table/core/accounts.ddl",
        "create table if not exists core.accounts (\n\
           id   integer primary key\n\
         , tier text    not null default core.default_tier()\n\
         );\n",
    );
    let events = (
        "ddl/table/core/events.ddl",
        "create table if not exists core.events (\n\
           id         integer primary key\n\
         , touched_at timestamptz\n\
         );\n\
         create or replace trigger events_touch before insert or update on core.events\n\
           for each row execute function core.touch();\n",
    );

    // ── Phase 1: a fresh database ──
    let design = write_project(dir, yaml, &[default_tier, touch, accounts, events]);
    design
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile on a fresh database must create the functions before the tables that call them");
    adapter
        .execute_script("INSERT INTO core.accounts (id) VALUES (1); INSERT INTO core.events (id) VALUES (1);")
        .await
        .expect("insert");
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM core.accounts WHERE id = 1 AND tier = 'basic'",
        "the default the function supplies",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM core.events WHERE id = 1 AND touched_at IS NOT NULL",
        "touched_at set by the trigger",
    )
    .await;

    // ── Phase 2: a new column on an existing table, defaulting to a new function ──
    let design = write_project(
        dir,
        yaml,
        &[
            (
                "ddl/function/core/default_region.ddl",
                "create or replace function core.default_region() returns text\n\
                 language sql immutable as $$ select 'eu'::text $$;\n",
            ),
            (
                "ddl/table/core/accounts.ddl",
                "create table if not exists core.accounts (\n\
                   id     integer primary key\n\
                 , tier   text    not null default core.default_tier()\n\
                 , region text    not null default core.default_region()\n\
                 );\n",
            ),
        ],
    );
    design
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("the ALTER adding the column must run after the function its default calls");
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM core.accounts WHERE id = 1 AND region = 'eu'",
        "the new column default on the existing row",
    )
    .await;
}

// ── Enum value removal: only views are dropped ────────────────────────────────

/// Removing an enum value recreates the type, and every view over a column of
/// it has to go first. Every *entity* that transitively referenced the enum's
/// table was emitted as `DROP VIEW`: the table with a foreign key to it, and the
/// procedure and function whose bodies read it. The run failed with
/// `"order_notes" is not a view`, so the value could not be removed from any
/// enum whose table anything else referenced.
///
/// End to end: the tables keep their rows, the routines are not dropped, and
/// the views — one over the column, one over that view — come back.
#[tokio::test]
async fn removing_an_enum_value_drops_only_the_views_over_its_column() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "enum_dependents_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let yaml =
        "project:\n  name: enum_dependents_test\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - app\n";
    let enum_file = |values: &str| {
        (
            "ddl/enum/app/order_status.ddl",
            format!("set search_path to app;\ncreate type order_status as enum ({values});\n"),
        )
    };
    let rest: Vec<(&str, &str)> = vec![
        (
            "ddl/table/app/orders.ddl",
            "set search_path to app;\n\
             create table if not exists orders (\n\
               id     integer primary key\n\
             , status order_status not null default 'new'\n\
             );\n",
        ),
        (
            "ddl/table/app/order_notes.ddl",
            "set search_path to app;\n\
             create table if not exists order_notes (\n\
               id       integer primary key\n\
             , order_id integer not null references orders (id)\n\
             , note     text\n\
             );\n",
        ),
        (
            "ddl/view/app/open_orders.ddl",
            "set search_path to app;\n\
             create or replace view open_orders as select id, status from orders where status <> 'void';\n",
        ),
        (
            "ddl/view/app/open_order_ids.ddl",
            "set search_path to app;\n\
             create or replace view open_order_ids as select id from open_orders;\n",
        ),
        (
            "ddl/procedure/app/void_order.ddl",
            "set search_path to app;\n\
             create or replace procedure void_order(p_id integer)\n\
             language plpgsql as $$\n\
             begin\n\
               update orders set status = 'void' where id = p_id;\n\
             end $$;\n",
        ),
        (
            "ddl/function/app/order_count.ddl",
            "set search_path to app;\n\
             create or replace function order_count() returns bigint\n\
             language sql stable as $$ select count(*) from orders $$;\n",
        ),
    ];
    let load = |values: &str| {
        let (path, sql) = enum_file(values);
        let mut files = rest.clone();
        files.push((path, sql.as_str()));
        write_project(dir, yaml, &files)
    };

    // ── Phase 1: build it, with rows in both tables ──
    load("'new', 'paid', 'void'")
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 1 failed");
    adapter
        .execute_script(
            "INSERT INTO app.orders (id, status) VALUES (1, 'new'), (2, 'void'); \
             INSERT INTO app.order_notes (id, order_id, note) VALUES (1, 1, 'keep me');",
        )
        .await
        .expect("seed rows failed");

    // ── Phase 2: remove the unused value ──
    load("'new', 'void'")
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("removing an enum value must not drop the tables and routines that reference its table");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 WHERE (SELECT enum_range(NULL::app.order_status)::text) = '{new,void}'",
        "enum app.order_status without paid",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.order_notes WHERE id = 1 AND note = 'keep me'",
        "the row in app.order_notes",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.orders WHERE id = 2 AND status = 'void'",
        "the rows in app.orders",
    )
    .await;
    for routine in ["void_order", "order_count"] {
        assert_catalog(
            &*adapter,
            true,
            &format!(
                "SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                 WHERE n.nspname = 'app' AND p.proname = '{routine}'"
            ),
            &format!("routine app.{routine}"),
        )
        .await;
    }
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.open_order_ids WHERE id = 1",
        "the views, recreated over the new type",
    )
    .await;
    // The body names `orders` bare, as its file's `set search_path to app`
    // allows, so it resolves on the caller's path. Set here, in the same script
    // and so on the same connection: a pooled one may carry any earlier path.
    adapter
        .execute_script("SET search_path TO app; CALL app.void_order(1)")
        .await
        .expect("the procedure still runs against the recreated type");
}

// ── Reset with schema grants ──────────────────────────────────────────────────

/// A design whose roles hold schema grants: one through `schemas:`, one through
/// the target's `grants:`.
const GRANTS_YAML: &str = "project:\n  name: grants_reset_test\nsource:\n  dialect: postgresql\n\
    schemas:\n  - app:\n      grants:\n        app_writer: [usage, select, insert]\n\
    target:\n  postgres:\n    roles:\n      - name: app_writer\n      - name: app_reader\n\
    \x20   grants:\n      app:\n        app_reader: [usage, select]\n";

const GRANTS_TABLE: (&str, &str) = (
    "ddl/table/app/t.ddl",
    "set search_path to app;\ncreate table if not exists t (id integer primary key);\n",
);

fn role_predicate(role: &str) -> String {
    format!("SELECT 1 FROM pg_roles WHERE rolname = '{role}'")
}

/// The default `dbd reset` of any design with schema grants failed: it dropped
/// the tables and then ran `DROP ROLE` on roles that still held USAGE on the
/// surviving schema and the default privileges dbd set there, so Postgres
/// refused and the whole reset rolled back. Only `--clean`, which drops the
/// schema and its grants with it, worked.
#[tokio::test]
async fn the_default_reset_drops_roles_that_hold_the_designs_schema_grants() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "grants_reset_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(tmp.path(), GRANTS_YAML, &[GRANTS_TABLE]);

    design.deploy(&*adapter, false, None, |_| {}).await.expect("deploy");
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_default_acl d JOIN pg_namespace n ON n.oid = d.defaclnamespace WHERE n.nspname = 'app'",
        "the default privileges deploy set on app",
    )
    .await;

    design
        .reset(&*adapter, "postgres", false, false, false, None)
        .await
        .expect("the default reset must drop roles that hold the design's grants");

    assert_table_absent(&*adapter, "app", "t").await;
    assert_schema_exists(&*adapter, "app").await;
    for role in ["app_writer", "app_reader"] {
        assert_catalog(&*adapter, false, &role_predicate(role), &format!("role {role}")).await;
    }
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM pg_default_acl d JOIN pg_namespace n ON n.oid = d.defaclnamespace WHERE n.nspname = 'app'",
        "default privileges on app",
    )
    .await;

    // The database is back to where apply starts from.
    design
        .deploy(&*adapter, false, None, |_| {})
        .await
        .expect("re-deploy after reset");
    assert_table_exists(&*adapter, "app", "t").await;
}

/// Reset revokes only what dbd granted. A privilege someone else gave one of
/// the design's roles still blocks `DROP ROLE` — dbd must not guess at it, nor
/// reach for `DROP OWNED BY`, which would drop objects the project does not
/// own — but the error has to say which role and what it still holds, where
/// Postgres's own message names the role and leaves the reason in a DETAIL the
/// driver does not show. And the reset is one transaction: nothing is dropped.
#[tokio::test]
async fn a_role_holding_a_privilege_dbd_did_not_grant_fails_the_reset_naming_the_role_and_privilege() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "grants_reset_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(tmp.path(), GRANTS_YAML, &[GRANTS_TABLE]);

    design.deploy(&*adapter, false, None, |_| {}).await.expect("deploy");
    adapter
        .execute_script(
            "CREATE SCHEMA other; CREATE TABLE other.x (id integer); GRANT SELECT ON other.x TO app_writer;",
        )
        .await
        .expect("grant outside the design");

    let err = design
        .reset(&*adapter, "postgres", false, false, false, None)
        .await
        .expect_err("a privilege dbd did not grant must still block the role drop")
        .to_string();
    assert!(err.contains("app_writer"), "the error must name the role: {err}");
    assert!(
        err.contains("privileges for table other.x"),
        "the error must say what the role still holds: {err}"
    );
    // A grant reset does not revoke is not one dbd did not make — a hook, a
    // policy file or another database's apply makes them too — so the message
    // does not claim to know which.
    assert!(!err.contains("dbd did not grant"), "{err}");

    assert_table_exists(&*adapter, "app", "t").await;
    assert_catalog(&*adapter, true, &role_predicate("app_writer"), "role app_writer").await;
}

// ── Reset forgets the stamps of the matviews it drops ─────────────────────────

/// `reset --clean` dropped the materialized view and left its row in
/// `dbd.matview_stamps`. A stamp outliving its view describes something that no
/// longer exists — and a later view of the same name, built by hand, would read
/// as one dbd created. Both resets that drop the view must forget its stamp:
/// the default one, which drops it by name, and `--clean`, which drops its
/// schema.
#[tokio::test]
async fn reset_forgets_the_drift_stamp_of_every_materialized_view_it_drops() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "mv_stamp_reset").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(
        tmp.path(),
        "project:\n  name: mv_stamp_reset\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
        &[
            (
                "ddl/table/app/items.ddl",
                "set search_path to app;\ncreate table if not exists items (id int primary key);\n",
            ),
            (
                "ddl/materialized_view/app/mv.ddl",
                "create materialized view if not exists app.mv as select id from app.items with data;\n",
            ),
        ],
    );
    let stamp = "SELECT 1 FROM dbd.matview_stamps WHERE project = 'mv_stamp_reset' AND matview = 'app.mv'";

    for (drop_schemas, drop_extensions, label) in [(false, false, "the default reset"), (true, true, "reset --clean")] {
        design.deploy(&*adapter, false, None, |_| {}).await.expect("deploy");
        assert_catalog(&*adapter, true, stamp, "the stamp deploy recorded for app.mv").await;

        design
            .reset(&*adapter, "postgres", false, drop_schemas, drop_extensions, None)
            .await
            .unwrap_or_else(|e| panic!("{label} failed: {e}"));

        assert_catalog(
            &*adapter,
            false,
            "SELECT 1 FROM pg_matviews WHERE schemaname = 'app' AND matviewname = 'mv'",
            "matview app.mv",
        )
        .await;
        assert_catalog(&*adapter, false, stamp, &format!("the app.mv stamp after {label}")).await;
    }
}

// ── source.search_path: reconcile creates a pathless matview on it ────────────

/// Reconcile creates an absent materialized view from its model, not its file,
/// and ran that `CREATE` on a path of every managed schema in name order. A
/// matview file that states no path was read against `source.search_path`, so a
/// bare `a` in its body meant `app.a`; created on `"aaa", "app", public` it
/// meant `aaa.a` instead — here a table without the column, so reconcile failed
/// building a view `apply` builds without complaint.
#[tokio::test]
async fn reconcile_creates_a_pathless_matview_on_source_search_path() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "pathless_mv_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(
        tmp.path(),
        "project:\n  name: pathless_mv_test\nsource:\n  dialect: postgresql\n  search_path: [app]\n\
         schemas:\n  - aaa\n  - app\n",
        &[
            (
                "ddl/table/aaa/a.ddl",
                "set search_path to aaa;\ncreate table if not exists a (id integer primary key);\n",
            ),
            (
                "ddl/table/app/a.ddl",
                "create table if not exists a (id integer primary key, x integer);\n",
            ),
            (
                "ddl/materialized_view/app/m.ddl",
                "create materialized view if not exists m as select x from a;\n",
            ),
        ],
    );

    design
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("the matview's body resolves on source.search_path, as apply runs its file");
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_matviews WHERE schemaname = 'app' AND matviewname = 'm'",
        "materialized view app.m",
    )
    .await;
}

// ── A scoped migration keeps the always-on roles and extensions ──────────────

/// v2 adds `citext` to the target's extensions, a role, and a table with a
/// `citext` column. Unscoped, the migration worked; under a scope it failed with
/// `type "citext" does not exist` and never created the role, though
/// `apply --dry-run --scope` listed both: the migrate plan filtered the run's
/// entities by the scope's working set a second time, and roles and extensions
/// are never in it — they are always on.
#[tokio::test]
async fn a_scoped_migration_installs_its_new_extension_and_role() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "scoped_migrate_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let config = dir.join("design.yaml");
    let people = (
        "ddl/table/app/people.ddl",
        "set search_path to app;\ncreate table if not exists people (id integer primary key);\n",
    );
    let v1 = "project:\n  name: scoped_migrate_test\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
              target:\n  postgres:\n    roles:\n      - name: app_reader\n\
              scopes:\n  app_only:\n    includes: [app]\n";
    let design = write_project(dir, v1, &[people]);
    dbd_core::snapshot::create_snapshot(design.entities(), dir, &config, "v1").unwrap();
    let design = Design::from_config_with_dir(&config, "dev", Some(dir)).unwrap();
    let scope = design.resolve_scope(Some("app_only"), None).unwrap();
    design
        .apply(&*adapter, None, false, Some(&scope), Progress::none())
        .await
        .expect("v1 applies under the scope");

    let v2 = "project:\n  name: scoped_migrate_test\n  version: 1\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
              target:\n  postgres:\n    extensions:\n      - citext\n    roles:\n      - name: app_reader\n      - name: app_writer\n\
              scopes:\n  app_only:\n    includes: [app]\n";
    let emails = (
        "ddl/table/app/emails.ddl",
        "set search_path to app;\ncreate table if not exists emails (id integer primary key, addr citext);\n",
    );
    let design = write_project(dir, v2, &[people, emails]);
    dbd_core::snapshot::create_snapshot(design.entities(), dir, &config, "v2").unwrap();
    let design = Design::from_config_with_dir(&config, "dev", Some(dir)).unwrap();
    assert_eq!(design.config().project.version, Some(2));
    let scope = design.resolve_scope(Some("app_only"), None).unwrap();
    design
        .apply(&*adapter, None, false, Some(&scope), Progress::none())
        .await
        .expect("the scoped migration installs the extension its new table needs");

    assert_table_exists(&*adapter, "app", "emails").await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_extension WHERE extname = 'citext'",
        "extension citext",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_roles WHERE rolname = 'app_writer'",
        "role app_writer",
    )
    .await;
}

// ── Enum value removal: indexes that read the column ─────────────────────────

/// `ALTER COLUMN … TYPE` rebuilds every index on the column from its stored
/// definition. A partial index comparing the column with an enum literal —
/// stored as `mood <> 'bad'::app.mood` — is re-read after the type swap's
/// rename as a comparison with the old type, and reconcile failed with
/// `operator does not exist: mood <> mood__dbd_old`, naming no index. An
/// expression key comparing the column fails the same way.
///
/// The swap rebuilds both around it, the rows survive, and the design then
/// diffs clean — the indexes included.
#[tokio::test]
async fn removing_an_enum_value_rebuilds_the_indexes_that_read_its_column() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "enum_index_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let yaml = "project:\n  name: enum_index_test\nsource:\n  dialect: postgresql\nschemas:\n  - app\n";
    let people = (
        "ddl/table/app/people.ddl",
        "set search_path to app;\n\
         create table if not exists people (\n  id integer primary key\n, mood mood not null default 'ok'\n);\n\
         create index if not exists people_not_bad on people (id) where mood <> 'bad';\n\
         create index if not exists people_is_ok on people ((mood = 'ok'));\n",
    );
    let load = |values: &str| {
        let enum_sql = format!("set search_path to app;\ncreate type mood as enum ({values});\n");
        write_project(dir, yaml, &[people, ("ddl/enum/app/mood.ddl", enum_sql.as_str())])
    };

    load("'ok', 'meh', 'bad'")
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("phase 1 builds the table and its indexes");
    adapter
        .execute_script("INSERT INTO app.people (id, mood) VALUES (1, 'ok'), (2, 'bad');")
        .await
        .expect("seed rows");

    let design = load("'ok', 'bad'");
    design
        .reconcile(&*adapter, false, true, false, None, Progress::none())
        .await
        .expect("removing an enum value rebuilds the indexes over its column");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 WHERE (SELECT enum_range(NULL::app.mood)::text) = '{ok,bad}'",
        "enum app.mood without meh",
    )
    .await;
    for index in ["people_not_bad", "people_is_ok"] {
        assert_catalog(
            &*adapter,
            true,
            &format!("SELECT 1 FROM pg_indexes WHERE schemaname = 'app' AND indexname = '{index}'"),
            &format!("index app.{index}"),
        )
        .await;
    }
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.people WHERE id = 2 AND mood = 'bad'",
        "the rows in app.people",
    )
    .await;
    let diff = design.diff_live(&*adapter, None).await.expect("diff");
    assert!(diff.is_empty(), "the design diffs clean afterwards: {:?}", diff.changes);
}

// ── Enum value removal: the migration `dbd snapshot` writes applies ──────────

/// Removing an enum value from a released project wrote a three-stage migration
/// that could not apply:
///
/// - `ALTER … TYPE TEXT` failed on any column a view or matview selects
///   ("cannot alter type of a column used by a view or rule");
/// - `DROP TYPE` failed on any column with an enum default — the normal case
///   for a status column;
/// - the stage-one `data.sql` mapping the removed value was never run, its
///   table being in no stage's `altered` list;
/// - and apply, running each entity's migrations at its place in dependency
///   order, re-pointed the column at the old type before the drop ran.
///
/// The migration now swaps the type the way reconcile does, around the views,
/// the matview, the default and an index over the column; the mapped row is
/// kept, and the design then diffs clean and snapshots with no changes.
#[tokio::test]
async fn the_enum_value_removal_migration_applies() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "enum_migration_test").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let config = dir.join("design.yaml");
    let yaml = "project:\n  name: enum_migration_test\nsource:\n  dialect: postgresql\nschemas:\n  - app\n";
    let rest: Vec<(&str, &str)> = vec![
        (
            "ddl/table/app/people.ddl",
            "set search_path to app;\n\
             create table if not exists people (\n  id integer primary key\n, mood mood not null default 'ok'\n);\n\
             create index if not exists people_not_bad on people (id) where mood <> 'bad';\n",
        ),
        (
            "ddl/view/app/happy.ddl",
            "set search_path to app;\ncreate or replace view happy as select id, mood from people where mood = 'ok';\n",
        ),
        (
            "ddl/materialized_view/app/mood_counts.ddl",
            "set search_path to app;\n\
             create materialized view if not exists mood_counts as select mood, count(*) as n from people group by mood;\n",
        ),
    ];
    let load = |values: &str| {
        let enum_sql = format!("set search_path to app;\ncreate type mood as enum ({values});\n");
        let mut files = rest.clone();
        files.push(("ddl/enum/app/mood.ddl", enum_sql.as_str()));
        write_project(dir, yaml, &files)
    };

    // ── v1, released and applied, with a row holding the value to remove ──
    let design = load("'ok', 'meh', 'bad'");
    dbd_core::snapshot::create_snapshot(design.entities(), dir, &config, "v1").unwrap();
    let design = Design::from_config_with_dir(&config, "dev", Some(dir)).unwrap();
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("v1 applies");
    adapter
        .execute_script("INSERT INTO app.people (id, mood) VALUES (1, 'meh'), (2, 'bad');")
        .await
        .expect("seed rows");

    // ── remove 'meh', snapshot, and resolve the data TODO as a user would ──
    let design = load("'ok', 'bad'");
    dbd_core::snapshot::create_snapshot(design.entities(), dir, &config, "drop meh").unwrap();
    let todos = dbd_core::snapshot::scan_data_sql_todos(dir).unwrap();
    assert!(!todos.is_empty(), "the removal asks for its rows to be mapped");
    for todo in &todos {
        let sql = std::fs::read_to_string(&todo.file).unwrap();
        let resolved: String = sql
            .lines()
            .filter(|l| !l.trim_start().starts_with("-- TODO"))
            .map(|l| format!("{}\n", l.replace("'???'", "'ok'")))
            .collect();
        std::fs::write(&todo.file, resolved).unwrap();
    }

    let design = Design::from_config_with_dir(&config, "dev", Some(dir)).unwrap();
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("the generated migration applies");

    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 WHERE (SELECT enum_range(NULL::app.mood)::text) = '{ok,bad}'",
        "enum app.mood without meh",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.people WHERE id = 1 AND mood = 'ok'",
        "the mapped row",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM app.happy WHERE id = 1",
        "the view, over the new type",
    )
    .await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_matviews WHERE schemaname = 'app' AND matviewname = 'mood_counts'",
        "the matview",
    )
    .await;
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
         WHERE n.nspname = 'app' AND t.typname <> 'mood' AND t.typtype = 'e'",
        "a leftover enum type",
    )
    .await;

    let diff = design.diff_live(&*adapter, None).await.expect("diff");
    assert!(
        diff.is_empty(),
        "the design diffs clean: {:?} {:?}",
        diff.changes,
        diff.matview_drift
    );
    let again = dbd_core::snapshot::create_snapshot(design.entities(), dir, &config, "again").unwrap();
    assert!(
        again.snapshots.len() == 1 && again.snapshots[0].no_changes,
        "and the snapshots end where the design is"
    );
}

// ── A dev run against a prod database is refused ──────────────────────────────

const ENV_GUARD_YAML: &str = "project:\n  name: env_guard\nsource:\n  dialect: postgresql\nschemas:\n  - app\n";

const ENV_GUARD_FILES: &[(&str, &str)] = &[
    (
        "ddl/table/app/items.ddl",
        "set search_path to app;\ncreate table if not exists items (id int primary key, name text);\n",
    ),
    // Seed data only a dev database should ever get.
    ("import/dev/app/items.csv", "id,name\n1,dev-seed\n"),
];

fn env_guard_design(dir: &std::path::Path, env: &str) -> Design {
    write_project(dir, ENV_GUARD_YAML, ENV_GUARD_FILES);
    Design::from_config_with_dir(&dir.join("design.yaml"), env, Some(dir)).expect("load design")
}

/// `apply -e prod` and then `apply -e dev` rewrote `dbd.meta.env` to `dev`,
/// which lifted the prod guard on `reset` — and a dev deploy or import loaded
/// `import/dev/` seed data into the production database. A dev run against a
/// database marked prod is itself the mistake, so every command that records
/// the environment (and import, which picks its data by it) refuses it before
/// writing anything, and the database stays marked prod.
#[tokio::test]
async fn a_dev_run_against_a_database_marked_prod_is_refused_before_any_write() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "env_guard").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let prod = env_guard_design(tmp.path(), "prod");
    let dev = env_guard_design(tmp.path(), "dev");

    prod.apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("the prod apply");
    let meta = adapter.get_project_meta().await.unwrap().expect("apply records meta");
    assert_eq!(meta.env, "prod");

    let refusals = [
        (
            "apply",
            dev.apply(&*adapter, None, false, None, Progress::none()).await.err(),
        ),
        ("deploy", dev.deploy(&*adapter, false, None, |_| {}).await.err()),
        (
            "reconcile",
            dev.reconcile(&*adapter, false, false, false, None, Progress::none())
                .await
                .err(),
        ),
        (
            "import",
            dev.import_data(&*adapter, None, false, None, Progress::none())
                .await
                .err(),
        ),
    ];
    for (command, err) in refusals {
        let err = err
            .unwrap_or_else(|| panic!("a dev {command} against a prod database must be refused"))
            .to_string();
        assert!(
            err.contains("marked prod") && err.contains("-e prod"),
            "the {command} refusal must name the database's env and what to do: {err}"
        );
    }

    let meta = adapter.get_project_meta().await.unwrap().expect("meta");
    assert_eq!(meta.env, "prod", "a refused dev run must not re-mark the database");
    assert!(
        Design::check_reset_guard(Some(&meta)).is_err(),
        "reset must stay blocked on the prod database"
    );
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM app.items WHERE name = 'dev-seed'",
        "dev seed data in the prod database",
    )
    .await;
}

/// The way out for a database a run without `-e` marked prod: reset it, with
/// `--force`, and build it again with `-e dev`. The reset dropped the objects
/// and the migrations but kept the `dbd.meta` row, so the rebuilt database was
/// still marked prod and the dev run was refused — there was no way out short
/// of a new database. A full reset leaves nothing of the project behind, its
/// record of the database included.
#[tokio::test]
async fn a_forced_full_reset_lets_a_mis_marked_database_be_rebuilt_as_dev() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "env_guard").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let prod = env_guard_design(tmp.path(), "prod");
    let dev = env_guard_design(tmp.path(), "dev");

    prod.apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("the prod apply");
    prod.reset(&*adapter, "postgres", false, false, false, None)
        .await
        .expect_err("an unforced reset of a prod database is refused");
    prod.reset(&*adapter, "postgres", true, false, false, None)
        .await
        .expect("a forced reset");
    assert!(
        adapter.get_project_meta().await.unwrap().is_none(),
        "a full reset forgets the database's record of the project"
    );

    dev.apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("the reset database is rebuilt with -e dev");
    let meta = adapter.get_project_meta().await.unwrap().expect("apply records meta");
    assert_eq!(meta.env, "dev");
}

/// The guard is one-way. A database marked dev may be promoted to prod: that
/// only tightens what `reset` allows.
#[tokio::test]
async fn a_database_marked_dev_may_be_promoted_to_prod() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "env_guard").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let dev = env_guard_design(tmp.path(), "dev");
    let prod = env_guard_design(tmp.path(), "prod");

    dev.apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("the dev apply");
    prod.apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("promoting a dev database to prod");

    let meta = adapter.get_project_meta().await.unwrap().expect("meta");
    assert_eq!(meta.env, "prod");
}

// ── A brownfield matview is adopted only when it is the design's ──────────────

/// The brownfield path: `init --from-db` reads a materialized view dbd did not
/// create, so the first apply finds it already there. Apply stamps only the
/// matviews it creates, so this one stayed unstamped and every `dbd diff`
/// reported it "not stamped by dbd" — the round trip never converged. Apply
/// adopts a pre-existing, unstamped matview whose deployed definition is the
/// design's, and only that one: a different deployed definition stays
/// unstamped, since stamping it would vouch for a definition the design does
/// not have.
#[tokio::test]
async fn apply_stamps_an_unstamped_matview_only_when_its_deployed_definition_is_the_designs() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "adopt_mv").await.unwrap();
    adapter
        .execute_script(
            "CREATE SCHEMA app; CREATE TABLE app.items (id int PRIMARY KEY, name text); \
             CREATE MATERIALIZED VIEW app.same AS SELECT id, name FROM app.items; \
             CREATE INDEX same_id ON app.same (id); \
             CREATE MATERIALIZED VIEW app.other AS SELECT id FROM app.items;",
        )
        .await
        .expect("the brownfield database");

    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(
        tmp.path(),
        "project:\n  name: adopt_mv\nsource:\n  dialect: postgresql\nschemas:\n  - app\n",
        &[
            (
                "ddl/table/app/items.ddl",
                "create table if not exists app.items (id int primary key, name text);\n",
            ),
            // The same query as deployed, written the way a person (or the
            // formatter) writes it rather than the way Postgres prints it.
            (
                "ddl/materialized_view/app/same.ddl",
                "create materialized view if not exists app.same as\n  select id\n       , name\n    from app.items\n  with data;\n\
                 create index if not exists same_id on app.same (id);\n",
            ),
            // Not what is deployed: the database has `select id` only.
            (
                "ddl/materialized_view/app/other.ddl",
                "create materialized view if not exists app.other as select id, name from app.items with data;\n",
            ),
        ],
    );

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply");

    let drift = design.diff_live(&*adapter, None).await.expect("diff").matview_drift;
    let kind = |name: &str| drift.iter().find(|d| d.name == name).map(|d| d.kind);
    assert_eq!(
        kind("app.same"),
        None,
        "the deployed definition is the design's: {drift:?}"
    );
    assert_eq!(
        kind("app.other"),
        Some(dbd_core::schema_diff::MatviewDriftKind::Unstamped),
        "a different deployed definition must stay unverified: {drift:?}"
    );
}

// ── init --from-db keeps key names and an enum's comment ──────────────────────

/// `init --from-db` rebuilt `people_region_fk` as Postgres's own
/// `people_region_id_fkey`, and dropped the enum's `COMMENT ON TYPE` — neither
/// of which `diff` can see. Introspection reads both, and the emitted DDL must
/// carry both, as it carries a CHECK's name and a view's comment.
#[tokio::test]
async fn reverse_engineering_keeps_key_names_and_an_enums_own_comment() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    adapter
        .execute_script(
            "CREATE SCHEMA app; \
             CREATE TYPE app.mood AS ENUM ('happy', 'sad'); \
             COMMENT ON TYPE app.mood IS 'how a person''s day went'; \
             CREATE TABLE app.regions (id int CONSTRAINT regions_pk PRIMARY KEY); \
             CREATE TABLE app.people (id int PRIMARY KEY, code text, region_id int, mood app.mood, \
               CONSTRAINT people_code_uq UNIQUE (code), \
               CONSTRAINT people_region_fk FOREIGN KEY (region_id) REFERENCES app.regions (id));",
        )
        .await
        .unwrap();

    let entities = adapter.introspect().await.expect("introspect");
    let emitted = |name: &str| {
        let e = entities
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} introspected"));
        dbd_core::emit::emit_entity(e).unwrap_or_default()
    };

    let mood = emitted("app.mood");
    assert!(
        mood.contains(r#"COMMENT ON TYPE "app"."mood" IS 'how a person''s day went';"#),
        "{mood}"
    );
    let people = emitted("app.people");
    for named in [
        r#"CONSTRAINT "people_region_fk" FOREIGN KEY"#,
        r#"CONSTRAINT "people_code_uq" UNIQUE"#,
        r#"CONSTRAINT "people_pkey" PRIMARY KEY"#,
    ] {
        assert!(people.contains(named), "{named} in:\n{people}");
    }
    let regions = emitted("app.regions");
    assert!(regions.contains(r#"CONSTRAINT "regions_pk" PRIMARY KEY"#), "{regions}");
}

// ── A scoped reset destroys nothing outside its scope ─────────────────────────

const MOODS_YAML: &str = "project:\n  name: scoped_reset\nsource:\n  dialect: postgresql\nschemas:\n  - app\n\
    scopes:\n  moods:\n    includes: [app.mood, app.mood_names]\n  names:\n    includes: [app.mood_names]\n";

const MOODS_FILES: &[(&str, &str)] = &[
    (
        "ddl/enum/app/mood.ddl",
        "create type app.mood as enum ('happy', 'sad');\n",
    ),
    (
        "ddl/table/app/people.ddl",
        "set search_path to app;\ncreate table if not exists people (id int primary key, mood app.mood);\n",
    ),
    (
        "ddl/view/app/mood_names.ddl",
        "set search_path to app;\ncreate or replace view mood_names as select unnest(enum_range(null::app.mood))::text as name;\n",
    ),
];

/// The audit's repro: a scope holding the enum `app.mood` but not the table
/// `app.people` whose column uses it. The reset dropped the enum `CASCADE`, and
/// with it the `app.people.mood` column — out of scope, and its data with it.
/// A scoped reset drops without `CASCADE`, all or nothing: refused, it names
/// what outside the scope depends on what it would drop, and drops nothing.
#[tokio::test]
async fn a_scoped_reset_refuses_rather_than_cascade_into_what_is_outside_its_scope() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "scoped_reset").await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(tmp.path(), MOODS_YAML, MOODS_FILES);
    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply");

    let moods = design.resolve_scope(Some("moods"), None).unwrap();
    let err = design
        .reset(&*adapter, "postgres", false, false, false, Some(&moods))
        .await
        .expect_err("the enum is used outside the scope")
        .to_string();
    assert!(
        err.contains("app.mood") && err.contains("column mood of table app.people"),
        "the refusal must name the dependent outside the scope: {err}"
    );

    assert_column_exists(&*adapter, "app", "people", "mood").await;
    assert_catalog(
        &*adapter,
        true,
        "SELECT 1 FROM pg_views WHERE schemaname = 'app' AND viewname = 'mood_names'",
        "view app.mood_names (the refused reset drops nothing)",
    )
    .await;

    // A scope that nothing outside it depends on still resets.
    let names = design.resolve_scope(Some("names"), None).unwrap();
    design
        .reset(&*adapter, "postgres", false, false, false, Some(&names))
        .await
        .expect("a self-contained scope resets");
    assert_catalog(
        &*adapter,
        false,
        "SELECT 1 FROM pg_views WHERE schemaname = 'app' AND viewname = 'mood_names'",
        "view app.mood_names",
    )
    .await;
    assert_column_exists(&*adapter, "app", "people", "mood").await;
}

// ── Roles are server-wide: a reset keeps one another database holds ───────────

/// Two databases on one server built from the same design share its roles,
/// which are server-wide. Resetting one failed — "role app_writer … holds
/// privileges dbd did not grant … objects in database a1", though dbd granted
/// them, in the other database — and had it gone ahead it would have dropped
/// a role the other database still uses. A role that holds anything in
/// another database is kept, and the reset continues; the last database to
/// reset drops it.
#[tokio::test]
async fn a_reset_keeps_a_role_another_database_on_the_server_holds() {
    let (pg, url) = start_pg().await;
    pg.create_database("second").await.unwrap();
    let first = connect(&url, "grants_reset_test").await.unwrap();
    let second = connect(&pg.settings().url("second"), "grants_reset_test")
        .await
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let design = write_project(tmp.path(), GRANTS_YAML, &[GRANTS_TABLE]);
    design
        .deploy(&*first, false, None, |_| {})
        .await
        .expect("deploy the first");
    design
        .deploy(&*second, false, None, |_| {})
        .await
        .expect("deploy the second");

    design
        .reset(&*second, "postgres", false, false, false, None)
        .await
        .expect("the second database resets, keeping the roles the first holds");
    assert_table_absent(&*second, "app", "t").await;
    assert_catalog(&*second, true, &role_predicate("app_writer"), "role app_writer").await;
    assert_catalog(
        &*first,
        true,
        "SELECT 1 WHERE has_table_privilege('app_writer', 'app.t', 'INSERT')",
        "the grant to app_writer in the first database",
    )
    .await;

    design
        .reset(&*first, "postgres", false, false, false, None)
        .await
        .expect("the first database resets");
    assert_catalog(&*first, false, &role_predicate("app_writer"), "role app_writer").await;
    assert_catalog(&*first, false, &role_predicate("app_reader"), "role app_reader").await;
}
