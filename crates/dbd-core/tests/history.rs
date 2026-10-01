//! The changelog the viewer renders (#29): what changed in each snapshot, newest
//! cut last, read from `snapshots/NNN.json` and nothing else.
//!
//! Fixtures are the on-disk JSON, written by hand, because that is the contract —
//! a snapshot written by any earlier dbd has to read back into the same history.

use dbd_core::history;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::path::Path;

fn col(name: &str, ty: &str) -> Value {
    json!({ "name": name, "data_type": ty, "nullable": true, "default_value": null,
            "is_pk": false, "is_unique": false, "comment": null, "inline_fk": null })
}

fn pk(name: &str, ty: &str) -> Value {
    let mut c = col(name, ty);
    c["nullable"] = json!(false);
    c["is_pk"] = json!(true);
    c
}

fn table(schema: &str, name: &str, columns: Vec<Value>) -> Value {
    json!({ "name": name, "schema": schema, "columns": columns, "indexes": [], "table_constraints": [] })
}

fn write(dir: &Path, version: u32, description: &str, tables: Vec<Value>, enums: Vec<Value>) {
    let snapshots = dir.join("snapshots");
    std::fs::create_dir_all(&snapshots).unwrap();
    let snap = json!({ "version": version, "description": description,
                       "timestamp": format!("2026-09-{:02}T10:00:00Z", version),
                       "tables": tables, "enums": enums });
    std::fs::write(
        snapshots.join(format!("{version:03}.json")),
        serde_json::to_string_pretty(&snap).unwrap(),
    )
    .unwrap();
}

fn history_json(dir: &Path) -> Value {
    serde_json::to_value(history::load(dir).expect("history loads")).unwrap()
}

#[test]
fn a_project_with_no_snapshots_has_no_history() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(history_json(dir.path()), json!([]));
}

#[test]
fn the_first_snapshot_is_a_baseline_of_counts_not_a_list_of_everything_added() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "v1 GA",
        vec![
            table("app", "orders", vec![pk("id", "uuid")]),
            table("app", "customers", vec![pk("id", "uuid")]),
        ],
        vec![json!({ "name": "order_status", "schema": "app", "values": ["pending", "paid"] })],
    );
    assert_eq!(
        history_json(dir.path()),
        json!([{
            "version": 1, "description": "v1 GA", "timestamp": "2026-09-01T10:00:00Z",
            "baseline": { "tables": 2, "enums": 1 }, "changes": []
        }])
    );
}

#[test]
fn a_version_lists_what_changed_since_the_one_before_it() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "orders", vec![pk("id", "uuid"), col("status", "text")])],
        vec![],
    );
    let mut status = col("status", "text");
    status["nullable"] = json!(false);
    status["default_value"] = json!("'pending'");
    write(
        dir.path(),
        2,
        "orders get a placed_at",
        vec![
            table(
                "app",
                "orders",
                vec![pk("id", "uuid"), status, col("placed_at", "timestamptz")],
            ),
            table("app", "customers", vec![pk("id", "uuid")]),
        ],
        vec![json!({ "name": "order_status", "schema": "app", "values": ["pending"] })],
    );

    let h = history_json(dir.path());
    assert_eq!(
        h[1],
        json!({
            "version": 2, "description": "orders get a placed_at", "timestamp": "2026-09-02T10:00:00Z",
            "changes": [
                { "kind": "table", "schema": "app", "name": "customers", "op": "added", "fields": [] },
                { "kind": "enum", "schema": "app", "name": "order_status", "op": "added", "fields": [] },
                { "kind": "table", "schema": "app", "name": "orders", "op": "modified", "fields": [
                    { "kind": "column", "name": "placed_at", "op": "added", "to": "timestamp with time zone" },
                    { "kind": "column", "name": "status", "op": "modified",
                      "from": "text", "to": "text not null default 'pending'" }
                ]}
            ]
        })
    );
}

#[test]
fn a_dropped_table_and_column_are_removals() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![
            table("app", "orders", vec![pk("id", "uuid"), col("legacy", "text")]),
            table("app", "old", vec![pk("id", "uuid")]),
        ],
        vec![],
    );
    write(
        dir.path(),
        2,
        "prune",
        vec![table("app", "orders", vec![pk("id", "uuid")])],
        vec![],
    );
    assert_eq!(
        history_json(dir.path())[1]["changes"],
        json!([
            { "kind": "table", "schema": "app", "name": "old", "op": "removed", "fields": [] },
            { "kind": "table", "schema": "app", "name": "orders", "op": "modified", "fields": [
                { "kind": "column", "name": "legacy", "op": "removed", "from": "text" }
            ]}
        ])
    );
}

#[test]
fn a_type_spelled_two_ways_is_not_a_change() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![col("code", "varchar(32)")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "respelled",
        vec![table("app", "t", vec![col("code", "character varying(32)")])],
        vec![],
    );
    let h = history_json(dir.path());
    assert_eq!(h[1]["version"], json!(2), "the version is still listed — it was cut");
    assert_eq!(h[1]["changes"], json!([]));
}

#[test]
fn a_two_stage_rename_is_one_version_and_reads_as_a_rename() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![pk("id", "int"), col("nick", "text")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "rename nick (stage 1/2)",
        vec![table(
            "app",
            "t",
            vec![pk("id", "int"), col("nick", "text"), col("handle", "text")],
        )],
        vec![],
    );
    write(
        dir.path(),
        3,
        "rename nick (stage 2/2)",
        vec![table("app", "t", vec![pk("id", "int"), col("handle", "text")])],
        vec![],
    );

    let h = history_json(dir.path());
    assert_eq!(
        h.as_array().unwrap().len(),
        2,
        "baseline + one logical version, not three entries"
    );
    assert_eq!(
        h[1],
        json!({
            "version": 2, "through": 3, "description": "rename nick", "timestamp": "2026-09-03T10:00:00Z",
            "changes": [{ "kind": "table", "schema": "app", "name": "t", "op": "modified", "fields": [
                { "kind": "column", "name": "handle", "op": "renamed", "from": "nick", "to": "handle" }
            ]}]
        })
    );
}

#[test]
fn a_two_stage_type_change_never_shows_the_intermediate_column() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![col("n", "int")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "widen n (stage 1/2)",
        vec![table("app", "t", vec![col("n", "int"), col("n_new", "bigint")])],
        vec![],
    );
    write(
        dir.path(),
        3,
        "widen n (stage 2/2)",
        vec![table("app", "t", vec![col("n", "bigint")])],
        vec![],
    );
    assert_eq!(
        history_json(dir.path())[1]["changes"],
        json!([
            { "kind": "table", "schema": "app", "name": "t", "op": "modified", "fields": [
                { "kind": "column", "name": "n", "op": "modified", "from": "integer", "to": "bigint" }
            ]}
        ])
    );
}

#[test]
fn enum_values_added_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let e = |values: Vec<&str>| json!({ "name": "status", "schema": "app", "values": values });
    write(dir.path(), 1, "baseline", vec![], vec![e(vec!["a", "b", "c"])]);
    write(dir.path(), 2, "values", vec![], vec![e(vec!["a", "c", "d"])]);
    assert_eq!(
        history_json(dir.path())[1]["changes"],
        json!([
            { "kind": "enum", "schema": "app", "name": "status", "op": "modified", "fields": [
                { "kind": "value", "name": "b", "op": "removed" },
                { "kind": "value", "name": "d", "op": "added" }
            ]}
        ])
    );
}

#[test]
fn indexes_and_constraints_are_named_by_what_they_are() {
    let dir = tempfile::tempdir().unwrap();
    let base = table(
        "app",
        "orders",
        vec![pk("id", "uuid"), col("customer_id", "uuid"), col("email", "text")],
    );
    write(
        dir.path(),
        1,
        "baseline",
        vec![base.clone(), table("app", "customers", vec![pk("id", "uuid")])],
        vec![],
    );
    let mut next = base;
    next["indexes"] = json!([{ "name": "orders_email_key", "columns": [{ "name": "email", "is_expression": false,
        "order": null, "nulls_first": null, "opclass": null }], "unique": true, "index_type": null }]);
    next["table_constraints"] = json!([{ "type": "foreign_key", "name": null, "columns": ["customer_id"],
        "ref_schema": "app", "ref_table": "customers", "ref_columns": ["id"], "on_delete": null, "on_update": null }]);
    write(
        dir.path(),
        2,
        "keys",
        vec![next, table("app", "customers", vec![pk("id", "uuid")])],
        vec![],
    );
    assert_eq!(
        history_json(dir.path())[1]["changes"],
        json!([
            { "kind": "table", "schema": "app", "name": "orders", "op": "modified", "fields": [
                { "kind": "constraint", "name": "foreign key (customer_id) → app.customers (id)", "op": "added" },
                { "kind": "index", "name": "orders_email_key", "op": "added", "to": "unique (email)" }
            ]}
        ])
    );
}

#[test]
fn a_corrupt_snapshot_is_an_error_not_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 1, "baseline", vec![], vec![]);
    std::fs::write(dir.path().join("snapshots/002.json"), "{ not json").unwrap();
    assert!(
        history::load(dir.path()).is_err(),
        "a history with a silent hole would misreport what changed"
    );
}

#[test]
fn a_scoped_history_counts_and_lists_only_its_schemas() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![
            table("app", "orders", vec![pk("id", "uuid")]),
            table("billing", "invoices", vec![pk("id", "uuid")]),
        ],
        vec![],
    );
    write(
        dir.path(),
        2,
        "both schemas move",
        vec![
            table("app", "orders", vec![pk("id", "uuid"), col("note", "text")]),
            table(
                "billing",
                "invoices",
                vec![pk("id", "uuid"), col("paid_at", "timestamptz")],
            ),
        ],
        vec![],
    );
    let h = serde_json::to_value(history::load_scoped(dir.path(), |schema, _| schema == "app").unwrap()).unwrap();
    assert_eq!(
        h[0]["baseline"],
        json!({ "tables": 1, "enums": 0 }),
        "the baseline counts the scope, not the project"
    );
    assert_eq!(
        h[1]["changes"],
        json!([{ "kind": "table", "schema": "app", "name": "orders", "op": "modified", "fields": [
            { "kind": "column", "name": "note", "op": "added", "to": "text" }
        ]}])
    );
}

/// `migrations/NNN/graph.json`, the way `dbd snapshot` writes it since #29 — with
/// its place in a multi-stage run.
fn graph(dir: &Path, version: u32, index: u32, of: u32) {
    let d = dir.join("migrations").join(format!("{version:03}"));
    std::fs::create_dir_all(&d).unwrap();
    let g = json!({ "fromVersion": version - 1, "toVersion": version, "added": [], "altered": [], "dropped": [],
                    "stage": { "index": index, "of": of } });
    std::fs::write(d.join("graph.json"), g.to_string()).unwrap();
}

#[test]
fn an_extension_index_names_its_access_method() {
    let dir = tempfile::tempdir().unwrap();
    let t = table("app", "docs", vec![pk("id", "uuid"), col("embedding", "vector(3)")]);
    write(dir.path(), 1, "baseline", vec![t.clone()], vec![]);
    let mut next = t;
    next["indexes"] = json!([{ "name": "docs_embedding_idx", "columns": [{ "name": "embedding", "is_expression": false,
        "order": null, "nulls_first": null, "opclass": null }], "unique": false, "index_type": { "other": "hnsw" } }]);
    write(dir.path(), 2, "ann", vec![next], vec![]);
    assert_eq!(
        history_json(dir.path())[1]["changes"][0]["fields"][0]["to"],
        json!("(embedding) using hnsw")
    );
}

#[test]
fn a_rename_that_also_tightens_the_column_says_both() {
    let dir = tempfile::tempdir().unwrap();
    let mut handle = col("handle", "text");
    handle["nullable"] = json!(false);
    handle["default_value"] = json!("''");
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![pk("id", "int"), col("nick", "text")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "rename nick (stage 1/2)",
        vec![table(
            "app",
            "t",
            vec![pk("id", "int"), col("nick", "text"), handle.clone()],
        )],
        vec![],
    );
    write(
        dir.path(),
        3,
        "rename nick (stage 2/2)",
        vec![table("app", "t", vec![pk("id", "int"), handle])],
        vec![],
    );
    assert_eq!(
        history_json(dir.path())[1]["changes"][0]["fields"],
        json!([
            { "kind": "column", "name": "handle", "op": "modified", "from": "text", "to": "text not null default ''" },
            { "kind": "column", "name": "handle", "op": "renamed", "from": "nick", "to": "handle" }
        ])
    );
}

#[test]
fn an_inline_foreign_key_action_is_part_of_the_column() {
    let dir = tempfile::tempdir().unwrap();
    let fk = |on_delete: Value| {
        json!({ "name": null, "columns": ["customer_id"], "ref_schema": "app",
        "ref_table": "customers", "ref_columns": ["id"], "on_delete": on_delete, "on_update": null })
    };
    let mut before = col("customer_id", "uuid");
    before["inline_fk"] = fk(json!(null));
    let mut after = col("customer_id", "uuid");
    after["inline_fk"] = fk(json!("cascade"));
    after["comment"] = json!("Owner.");
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "orders", vec![before])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "cascade",
        vec![table("app", "orders", vec![after])],
        vec![],
    );
    assert_eq!(
        history_json(dir.path())[1]["changes"][0]["fields"][0],
        json!({ "kind": "column", "name": "customer_id", "op": "modified",
                "from": "uuid references app.customers (id)",
                "to": "uuid references app.customers (id) on delete cascade" })
    );
}

#[test]
fn a_changed_constraint_is_one_modification_not_a_removal_and_an_addition() {
    let dir = tempfile::tempdir().unwrap();
    let with_fk = |on_delete: Value| {
        let mut t = table("app", "orders", vec![pk("id", "uuid"), col("customer_id", "uuid")]);
        t["table_constraints"] = json!([{ "type": "foreign_key", "name": null, "columns": ["customer_id"],
            "ref_schema": "app", "ref_table": "customers", "ref_columns": ["id"], "on_delete": on_delete, "on_update": null }]);
        t
    };
    write(dir.path(), 1, "baseline", vec![with_fk(json!(null))], vec![]);
    write(dir.path(), 2, "cascade", vec![with_fk(json!("cascade"))], vec![]);
    assert_eq!(
        history_json(dir.path())[1]["changes"][0]["fields"],
        json!([{ "kind": "constraint", "name": "foreign key (customer_id) → app.customers (id)", "op": "modified",
                 "from": "foreign key (customer_id) → app.customers (id)",
                 "to": "foreign key (customer_id) → app.customers (id) on delete cascade" }])
    );
}

#[test]
fn versions_marked_single_stage_are_never_grouped_whatever_their_descriptions_say() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![pk("id", "int"), col("a", "text")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "cleanup (stage 1/2)",
        vec![table("app", "t", vec![pk("id", "int")])],
        vec![],
    );
    write(
        dir.path(),
        3,
        "cleanup (stage 2/2)",
        vec![table("app", "t", vec![pk("id", "int"), col("b", "text")])],
        vec![],
    );
    graph(dir.path(), 2, 1, 1);
    graph(dir.path(), 3, 1, 1);
    let h = history_json(dir.path());
    assert_eq!(
        h.as_array().unwrap().len(),
        3,
        "two ordinary versions, not one invented rename"
    );
    assert_eq!(h[1]["changes"][0]["fields"][0]["op"], json!("removed"));
    assert_eq!(h[2]["changes"][0]["fields"][0]["op"], json!("added"));
}

#[test]
fn stages_marked_in_the_graph_are_grouped_whatever_their_descriptions_say() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![col("n", "int")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "widen n",
        vec![table("app", "t", vec![col("n", "int"), col("n_new", "bigint")])],
        vec![],
    );
    write(
        dir.path(),
        3,
        "widen n",
        vec![table("app", "t", vec![col("n", "bigint")])],
        vec![],
    );
    graph(dir.path(), 2, 1, 2);
    graph(dir.path(), 3, 2, 2);
    let h = history_json(dir.path());
    assert_eq!(h.as_array().unwrap().len(), 2);
    assert_eq!(h[1]["through"], json!(3));
}

#[test]
fn an_unmarked_stage_run_that_drops_before_it_adds_is_not_dbds_and_is_not_grouped() {
    // A snapshot from before stages were marked: only the description says "stage". dbd
    // always expands before it contracts — stage 1 still holds everything the run removes —
    // so a "stage 1" that already dropped the column is two ordinary versions.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        1,
        "baseline",
        vec![table("app", "t", vec![pk("id", "int"), col("a", "text")])],
        vec![],
    );
    write(
        dir.path(),
        2,
        "cleanup (stage 1/2)",
        vec![table("app", "t", vec![pk("id", "int")])],
        vec![],
    );
    write(
        dir.path(),
        3,
        "cleanup (stage 2/2)",
        vec![table("app", "t", vec![pk("id", "int"), col("b", "text")])],
        vec![],
    );
    assert_eq!(history_json(dir.path()).as_array().unwrap().len(), 3);
}
