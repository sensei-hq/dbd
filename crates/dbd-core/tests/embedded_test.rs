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
        .deploy(&*adapter, false, None, |s| summary = Some(s))
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

// ── Test 6: Postgres introspection ───────────────────────────────────────────

#[tokio::test]
async fn introspect_returns_fixture_entities() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "introspect_test").await.unwrap();

    // Create a known fixture in the ephemeral DB
    let fixture_sql = "
        CREATE SCHEMA revtest;

        CREATE TYPE revtest.color AS ENUM ('red', 'green');

        CREATE TABLE revtest.owner (
            id uuid PRIMARY KEY DEFAULT gen_random_uuid()
        );

        CREATE TABLE revtest.widget (
            id       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
            owner_id uuid NOT NULL REFERENCES revtest.owner(id) ON DELETE CASCADE,
            name     text NOT NULL,
            qty      int DEFAULT 0
        );

        ALTER TABLE revtest.widget ADD CONSTRAINT widget_name_key UNIQUE (name);

        CREATE INDEX widget_owner_idx ON revtest.widget (owner_id);

        COMMENT ON TABLE revtest.widget IS 'Widget objects';
        COMMENT ON COLUMN revtest.widget.name IS 'Display name';

        CREATE VIEW revtest.active AS SELECT id, name FROM revtest.widget;
    ";
    adapter.execute_script(fixture_sql).await.expect("fixture DDL failed");

    // Run introspect
    let entities = adapter.introspect().await.expect("introspect failed");

    // ── Assert: schema revtest present ───────────────────────────────────────
    let has_revtest_schema = entities.iter().any(|e| {
        e.entity_type == dbd_core::EntityType::Schema && e.name == "revtest"
    });
    assert!(has_revtest_schema, "schema 'revtest' not found in introspect output");

    // ── Assert: enum revtest.color present with values [red, green] ──────────
    let color_enum = entities.iter().find(|e| {
        e.entity_type == dbd_core::EntityType::Enum && e.name == "revtest.color"
    });
    let color_enum = color_enum.expect("enum 'revtest.color' not found in introspect output");
    let enum_value_names: Vec<&str> = color_enum.enum_values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(enum_value_names, vec!["red", "green"], "enum values should be [red, green]");

    // ── Assert: table revtest.widget present ──────────────────────────────────
    let widget = entities.iter().find(|e| {
        e.entity_type == dbd_core::EntityType::Table && e.name == "revtest.widget"
    });
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
    assert_eq!(fk.ref_schema.as_deref(), Some("revtest"), "FK ref_schema should be 'revtest'");
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

    // Non-constraint index widget_owner_idx
    let idx = td.indexes.iter().find(|i| i.name.as_deref() == Some("widget_owner_idx"));
    assert!(idx.is_some(), "index 'widget_owner_idx' should be present");

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
    let view = entities.iter().find(|e| {
        e.entity_type == dbd_core::EntityType::View && e.name == "revtest.active"
    });
    let view = view.expect("view 'revtest.active' not found in introspect output");
    let body = view.writes.first().expect("view should have a body in writes[0]");
    assert!(
        body.to_uppercase().contains("SELECT"),
        "view body should contain SELECT, got: {body}"
    );
}
