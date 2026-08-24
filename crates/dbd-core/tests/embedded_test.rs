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

/// Run a catalog existence assertion via an anonymous `DO` block. Raises (and
/// so fails the test) when `predicate` matches in the wrong direction:
/// `should_exist == true` fails when it finds nothing, `false` fails when it
/// finds something. `subject` is the human label, e.g. "table config.lookups".
async fn assert_catalog(
    adapter: &dyn dbd_core::DatabaseAdapter,
    should_exist: bool,
    predicate: &str,
    subject: &str,
) {
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
    assert_catalog(adapter, true, &table_predicate(schema, table), &format!("table {schema}.{table}")).await;
}

/// Assert that a table does NOT exist; panics if it does.
async fn assert_table_absent(adapter: &dyn dbd_core::DatabaseAdapter, schema: &str, table: &str) {
    assert_catalog(adapter, false, &table_predicate(schema, table), &format!("table {schema}.{table}")).await;
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
async fn assert_column_exists(
    adapter: &dyn dbd_core::DatabaseAdapter,
    schema: &str,
    table: &str,
    column: &str,
) {
    let pred = column_predicate(schema, table, column);
    assert_catalog(adapter, true, &pred, &format!("column {schema}.{table}.{column}")).await;
}

/// Assert that a column does NOT exist on a table; panics if it does.
async fn assert_column_absent(
    adapter: &dyn dbd_core::DatabaseAdapter,
    schema: &str,
    table: &str,
    column: &str,
) {
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

    adapter
        .import_data(&entity, "\\N", false)
        .await
        .expect("import failed");

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
        .apply(&*adapter, None, false, None, Progress { on_start: |_: &str| {}, on_done: |_: &str, _: Option<&str>| {}, on_complete: |s| v1_summary = Some(s) })
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
        .apply(&*adapter, None, false, None, Progress { on_start: |_: &str| {}, on_done: |_: &str, _: Option<&str>| {}, on_complete: |s| v2_summary = Some(s) })
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

    // ── Assert: label-less enum revtest.empty_color still introspects ─────────
    // `CREATE TYPE e AS ENUM ()` is valid Postgres and creates a real type with
    // zero pg_enum rows. introspect_enums used to INNER JOIN pg_enum, which made
    // such a type invisible here — reconcile then saw it as permanently missing
    // and recreated it on every run, never converging. It must be found, with
    // an empty (not absent) values list.
    let empty_enum = entities.iter().find(|e| {
        e.entity_type == dbd_core::EntityType::Enum && e.name == "revtest.empty_color"
    });
    let empty_enum =
        empty_enum.expect("label-less enum 'revtest.empty_color' not found in introspect output");
    assert!(
        empty_enum.enum_values.is_empty(),
        "empty_color should introspect with zero values, got: {:?}",
        empty_enum.enum_values
    );

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

    // Non-constraint index widget_owner_idx (plain column — must be captured)
    let idx = td.indexes.iter().find(|i| i.name.as_deref() == Some("widget_owner_idx"));
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

// ── Test: Materialized-view introspection ─────────────────────────────────────

/// Reverse-engineer a materialized view from `pg_matviews`. Creates a schema, a
/// `CREATE MATERIALIZED VIEW … WITH DATA`, and a UNIQUE INDEX on it, then asserts
/// `introspect` captures it as `EntityType::MaterializedView` with its SELECT body
/// in `writes[0]` and its index attached via `table_def` (matviews carry indexes
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

    // Body carried in writes[0] (same contract as views); contains the SELECT.
    let body = mv
        .writes
        .first()
        .expect("matview should carry its body in writes[0]");
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
                columns: vec![IndexColumn { name: "tags".into(), order: None, ..Default::default() }],
                unique: false,
                index_type: Some(IndexType::Gin),
                ..Default::default()
            },
            // HASH index on a plain column.
            IndexDef {
                name: Some("doc_title_hash".into()),
                columns: vec![IndexColumn { name: "title".into(), order: None, ..Default::default() }],
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
        let after = echo
            .get(&echoed_name)
            .unwrap_or_else(|| panic!("{echoed_name} missing after replay; got {:?}", echo.keys().collect::<Vec<_>>()));
        // Compare everything but the (deliberately changed) name.
        let expected = dbd_core::entity::IndexDef { name: after.name.clone(), ..before.clone() };
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
    assert_eq!(add_one.writes.len(), 1, "add_one has one body");
    assert!(
        add_one.writes[0].to_uppercase().contains("FUNCTION"),
        "add_one body should be a CREATE FUNCTION, got: {}",
        add_one.writes[0]
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
        noop.writes[0].to_uppercase().contains("PROCEDURE"),
        "noop body should be a CREATE PROCEDURE, got: {}",
        noop.writes[0]
    );

    // ── overloaded function: ONE entity with TWO writes ──────────────────────
    let greet_entities: Vec<_> = entities
        .iter()
        .filter(|e| e.name == "revfunc.greet")
        .collect();
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
        greet.writes.len(),
        2,
        "overloaded greet must hold TWO definitions in writes, got {}",
        greet.writes.len()
    );
    // Both signatures present across the two bodies.
    let joined = greet.writes.join("\n");
    assert!(joined.contains("name text") , "first overload signature missing");
    assert!(joined.contains("loud boolean"), "second overload signature missing");

    // ── extension-provided functions must be EXCLUDED ────────────────────────
    // `tablefunc` installs regular functions (crosstab/connectby/normal_rand)
    // owned by the extension (pg_depend deptype 'e'); they must be filtered out.
    let has_ext_fn = entities.iter().any(|e| {
        (e.entity_type == dbd_core::EntityType::Function
            || e.entity_type == dbd_core::EntityType::Procedure)
            && (e.name.contains("crosstab")
                || e.name.contains("connectby")
                || e.name.contains("normal_rand"))
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

    let roles = adapter
        .introspect_roles()
        .await
        .expect("introspect_roles failed");

    // app_admin and app_ro captured as Role entities.
    let app_admin = roles
        .iter()
        .find(|e| e.name == "app_admin")
        .expect("role 'app_admin' not found");
    assert_eq!(app_admin.entity_type, dbd_core::EntityType::Role);
    assert!(
        app_admin.refers.is_empty(),
        "app_admin should have no memberships, got {:?}",
        app_admin.refers
    );

    let app_ro = roles
        .iter()
        .find(|e| e.name == "app_ro")
        .expect("role 'app_ro' not found");
    assert_eq!(app_ro.entity_type, dbd_core::EntityType::Role);
    assert_eq!(
        app_ro.refers,
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
    child.refers = vec!["app_admin".to_string()];
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
        counter.writes.first().is_some_and(|w| w.contains("CREATE SEQUENCE")),
        "sequence entity should carry rendered CREATE SEQUENCE in writes[0], got {:?}",
        counter.writes
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
    assert_eq!(id_col.data_type, "bigserial", "bigserial PK should map to data_type bigserial");
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
    assert_eq!(n_col.identity, Some(IdentityKind::Always), "n should be GENERATED ALWAYS AS IDENTITY");
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
        c_col.default_value.as_deref().is_some_and(|d| d.contains("nextval(") && d.contains("counter")),
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
        // The sequence body in writes[0] is fully-qualified to src — retarget it too.
        c.writes = c.writes.iter().map(|w| w.replace("src", "dst")).collect();
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
    let load = || {
        Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
            .expect("load design")
    };

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
        .reconcile(&*adapter, false, false, false, None, Progress { on_start: |_: &str| {}, on_done: |_: &str, _: Option<&str>| {}, on_complete: |s| summary = Some(s) })
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
        .reconcile(&*adapter, false, false, true, None, Progress { on_start: |_: &str| {}, on_done: |_: &str, _: Option<&str>| {}, on_complete: |s| prune_summary = Some(s) })
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
    let load = || {
        Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
            .expect("load design")
    };
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
    assert!(plan.warnings.is_empty(), "create must not warn; got {:?}", plan.warnings);
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF obj_description('app.mv'::regclass, 'pg_class') NOT LIKE 'dbd:hash=%' \
               THEN RAISE EXCEPTION 'matview app.mv is missing its dbd:hash sentinel comment'; \
               END IF; END $$;",
        )
        .await
        .expect("sentinel-comment assertion failed");

    // ── Phase 2: SAME design → SKIP. No warning; oid unchanged. ──
    stash_oid(&*adapter).await;
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 2 (no-change) failed");
    assert!(plan.warnings.is_empty(), "unchanged matview must not warn; got {:?}", plan.warnings);
    assert_oid_unchanged(&*adapter, "matview app.mv oid changed but an unchanged design must not touch it").await;

    // ── Phase 3: change the definition → WARN, matview left untouched (oid same). ──
    stash_oid(&*adapter).await;
    write_mv("select id, name from app.items");
    let plan = load()
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await
        .expect("reconcile phase 3 (drift) failed");
    assert_oid_unchanged(&*adapter, "matview app.mv was recreated on drift but dbd must only warn").await;
    assert!(
        plan.warnings.iter().any(|w| w.contains("app.mv") && w.contains("differs")),
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
    let load = || {
        Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design")
    };

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
    let load = || {
        Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load design")
    };

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
    assert_eq!(kind_of("app.mv"), Some(MatviewDriftKind::Drifted), "got {:?}", d.matview_drift);
    assert_eq!(kind_of("app.mv2"), Some(MatviewDriftKind::Missing), "got {:?}", d.matview_drift);
    assert_eq!(kind_of("app.orphan"), Some(MatviewDriftKind::Orphan), "got {:?}", d.matview_drift);
    assert!(!d.is_empty(), "matview drift must make the diff non-empty");
    // Deterministic (name-sorted) order.
    let names: Vec<&str> = d.matview_drift.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["app.mv", "app.mv2", "app.orphan"], "matview drift must be name-sorted");

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

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load design");

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
    adapter
        .apply_entity(mv)
        .await
        .expect("first matview apply failed");
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

    let load = || {
        Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
            .expect("load design")
    };

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
    // apply must have stamped the dbd:hash sentinel onto the matview's comment.
    adapter
        .execute_script(
            "DO $$ BEGIN \
               IF obj_description('app.mv'::regclass, 'pg_class') NOT LIKE 'dbd:hash=%' \
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
    let load = || {
        Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
            .expect("load design")
    };

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
        plan.warnings.iter().any(|w| w.contains("app.mv") && w.contains("differs")),
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

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load design");
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

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load design");
    let diff = design.diff_live(&*adapter, None).await.expect("diff_live failed");

    assert!(!diff.is_empty(), "a design table absent from the live DB must show drift");
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
    use dbd_core::diff::{DiffAction, FieldType};
    use dbd_core::SchemaDiff;

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

    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load design");

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
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load design");

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
    let design2 = Design::from_config_with_dir(&dir2.join("design.yaml"), "dev", Some(dir2))
        .expect("load design2");

    // Without --allow-destructive → refused, FK untouched.
    let refused = design2
        .reconcile(&*adapter, false, false, false, None, Progress::none())
        .await;
    assert!(refused.is_err(), "dropping an FK without --allow-destructive must be refused");
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
    let design = Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir))
        .expect("load design");

    design
        .apply(&*adapter, None, false, None, Progress::none())
        .await
        .expect("apply failed");

    /// The reconcile plan's ALTER SQL, or an empty vec when it is in sync.
    async fn plan_sql(
        design: &Design,
        adapter: &dyn dbd_core::DatabaseAdapter,
    ) -> Vec<String> {
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
                .and_then(|td| td.columns.iter().find(|c| c.name == col).and_then(|c| c.comment.clone()))
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
async fn assert_dbd_meta_version(
    adapter: &dyn dbd_core::DatabaseAdapter,
    project: &str,
    expected: i32,
) {
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
    adapter.execute_script(
        "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('nolscope','prod',4);"
    ).await.unwrap();
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
    adapter.execute_script(
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
         INSERT INTO dojo._dbd_migrations   (project, version) VALUES ('p',2);"
    ).await.unwrap();

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
    adapter.execute_script(
        "CREATE TABLE public._dbd_meta ( \
            project varchar NOT NULL PRIMARY KEY, env varchar NOT NULL DEFAULT 'dev', \
            version integer NOT NULL DEFAULT 0, scope varchar, \
            created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now() ); \
         INSERT INTO public._dbd_meta (project, env, version) VALUES ('statusonly','prod',5);"
    ).await.unwrap();
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
