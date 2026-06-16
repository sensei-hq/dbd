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

    // Expression index widget_lower_name_idx — must be skipped (IndexDef cannot represent it)
    let expr_idx = td.indexes.iter().find(|i| i.name.as_deref() == Some("widget_lower_name_idx"));
    assert!(
        expr_idx.is_none(),
        "expression index 'widget_lower_name_idx' should NOT appear in introspect output"
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
                columns: vec![IndexColumn { name: "tags".into(), order: None }],
                unique: false,
                index_type: Some(IndexType::Gin),
            },
            // HASH index on a plain column.
            IndexDef {
                name: Some("doc_title_hash".into()),
                columns: vec![IndexColumn { name: "title".into(), order: None }],
                unique: false,
                index_type: Some(IndexType::Hash),
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

// ── Test 8: Function & procedure introspection (with overloads + extension) ───

/// Reverse-engineer functions and procedures via `pg_get_functiondef`. Creates a
/// plain function, a procedure, an overloaded function (two signatures), and
/// installs `uuid-ossp` whose functions are extension-owned and must be excluded.
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

        -- extension whose functions (uuid_generate_v4, etc.) must be EXCLUDED
        CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\" WITH SCHEMA revfunc;
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
    let has_ext_fn = entities.iter().any(|e| {
        (e.entity_type == dbd_core::EntityType::Function
            || e.entity_type == dbd_core::EntityType::Procedure)
            && e.name.contains("uuid_generate")
    });
    assert!(
        !has_ext_fn,
        "extension-owned functions (uuid_generate_*) must NOT appear in introspect output"
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
