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

use dbd_core::design::{ApplyStrategy, DeployComplete};
use dbd_core::{Design, connect};
use postgresql_embedded::{PostgreSQL, Settings};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/embedded")
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
    pg.create_database("testdb")
        .await
        .expect("failed to create testdb");
    let url = pg.settings().url("testdb");
    (pg, url)
}

/// Load the embedded fixture design.
fn load_design() -> Design {
    let config = fixture_dir().join("design.yaml");
    Design::from_config_with_dir(&config, "dev", Some(&fixture_dir()))
        .expect("failed to load embedded fixture design")
}

/// Assert that a table exists; panics with a clear message if it doesn't.
async fn assert_table_exists(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str) {
    let sql = format!(
        "DO $$ BEGIN \
           IF NOT EXISTS ( \
             SELECT 1 FROM pg_catalog.pg_tables \
             WHERE schemaname = '{schema}' AND tablename = '{table}' \
           ) THEN RAISE EXCEPTION 'table {schema}.{table} does not exist'; \
           END IF; \
         END $$"
    );
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("assert_table_exists({schema}.{table}) failed: {e}"));
}

/// Assert that a table does NOT exist; panics if it does.
async fn assert_table_absent(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str) {
    let sql = format!(
        "DO $$ BEGIN \
           IF EXISTS ( \
             SELECT 1 FROM pg_catalog.pg_tables \
             WHERE schemaname = '{schema}' AND tablename = '{table}' \
           ) THEN RAISE EXCEPTION 'table {schema}.{table} unexpectedly exists'; \
           END IF; \
         END $$"
    );
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("assert_table_absent({schema}.{table}) failed: {e}"));
}

/// Assert that a column exists on a table; panics if it doesn't.
async fn assert_column_exists(
    adapter: &dyn dbd_core::DatabaseAdapter,
    schema: &str,
    table: &str,
    column: &str,
) {
    let sql = format!(
        "DO $$ BEGIN \
           IF NOT EXISTS ( \
             SELECT 1 FROM information_schema.columns \
             WHERE table_schema = '{schema}' AND table_name = '{table}' \
               AND column_name = '{column}' \
           ) THEN RAISE EXCEPTION 'column {schema}.{table}.{column} does not exist'; \
           END IF; \
         END $$"
    );
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("assert_column_exists({schema}.{table}.{column}) failed: {e}"));
}

/// Assert that a column does NOT exist on a table; panics if it does.
async fn assert_column_absent(
    adapter: &dyn dbd_core::DatabaseAdapter,
    schema: &str,
    table: &str,
    column: &str,
) {
    let sql = format!(
        "DO $$ BEGIN \
           IF EXISTS ( \
             SELECT 1 FROM information_schema.columns \
             WHERE table_schema = '{schema}' AND table_name = '{table}' \
               AND column_name = '{column}' \
           ) THEN RAISE EXCEPTION 'column {schema}.{table}.{column} unexpectedly exists'; \
           END IF; \
         END $$"
    );
    adapter
        .execute_script(&sql)
        .await
        .unwrap_or_else(|e| panic!("assert_column_absent({schema}.{table}.{column}) failed: {e}"));
}

// ── Test 1: Fresh deploy ──────────────────────────────────────────────────────

#[tokio::test]
async fn fresh_deploy_creates_schema() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    let mut summary: Option<DeployComplete> = None;
    design
        .deploy(&*adapter, false, |s| summary = Some(s))
        .await
        .expect("deploy failed");

    let s = summary.unwrap();
    assert_eq!(
        s.apply.strategy,
        ApplyStrategy::Fresh,
        "first deploy should be Fresh"
    );
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
        .deploy(&*adapter, false, |_| {})
        .await
        .expect("first deploy failed");

    let mut summary: Option<DeployComplete> = None;
    design
        .deploy(&*adapter, false, |s| summary = Some(s))
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
        .deploy(&*adapter, false, |_| {})
        .await
        .expect("deploy failed");

    adapter
        .execute_script(
            "INSERT INTO app.items (name, quantity) VALUES ('widget', 10)",
        )
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

// ── Test 4: Dry-run does not modify schema ────────────────────────────────────

#[tokio::test]
async fn dry_run_does_not_create_tables() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "embedded_test").await.unwrap();
    let design = load_design();

    design
        .deploy(&*adapter, true, |_| {})
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

    let v1_design = Design::from_config_with_dir(
        &v1_dir.join("design.yaml"),
        "dev",
        Some(v1_dir),
    )
    .expect("failed to load v1 design");

    let mut v1_summary = None;
    v1_design
        .apply(&*adapter, None, false, None, |_| {}, |_, _| {}, |s| v1_summary = Some(s))
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
        .apply(&*adapter, None, false, None, |_| {}, |_, _| {}, |s| v2_summary = Some(s))
        .await
        .expect("v2 apply failed");

    let v2 = v2_summary.unwrap();
    assert_eq!(
        v2.strategy,
        ApplyStrategy::Migrate,
        "should use Migrate strategy"
    );
    assert_eq!(v2.from_version, 1);
    assert_eq!(v2.to_version, 2);
    assert_eq!(v2.migrated, 1, "one entity migrated (items)");

    assert_column_exists(&*adapter, "app", "items", "notes").await;
}
