//! `SchemaModel` v3 carries the project's history (#29).
//!
//! Additive, as v2 was: `history` is one more optional array, omitted when a
//! project has no snapshots, so a consumer that never heard of it reads a v3
//! payload exactly as it read v2. The version moves so that the next consumer
//! does not have to sniff for the field.

use dbd_core::Design;
use dbd_core::history;
use dbd_core::schema_model::{SchemaModel, build};
use serde_json::{Value, json};
use std::path::Path;

fn project(dir: &Path) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: m3\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = dir.join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("people.ddl"),
        "set search_path to app;\ncreate table if not exists people (id integer primary key);",
    )
    .unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).unwrap()
}

fn snapshot(dir: &Path) {
    let snapshots = dir.join("snapshots");
    std::fs::create_dir_all(&snapshots).unwrap();
    let snap = json!({ "version": 1, "description": "v1", "timestamp": "2026-09-01T10:00:00Z",
        "tables": [{ "name": "people", "schema": "app", "indexes": [], "table_constraints": [],
            "columns": [{ "name": "id", "data_type": "integer", "nullable": false, "default_value": null,
                "is_pk": true, "is_unique": false, "comment": null, "inline_fk": null }] }],
        "enums": [] });
    std::fs::write(snapshots.join("001.json"), snap.to_string()).unwrap();
}

#[test]
fn the_model_is_version_3() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(build(&project(tmp.path()), None).version, 3);
}

#[test]
fn a_model_without_history_omits_the_field() {
    let tmp = tempfile::tempdir().unwrap();
    let json = serde_json::to_value(build(&project(tmp.path()), None)).unwrap();
    assert!(json.get("history").is_none(), "an empty history is absent, not []");
}

#[test]
fn a_model_carries_the_history_it_is_given() {
    let tmp = tempfile::tempdir().unwrap();
    let mut model = build(&project(tmp.path()), None);
    snapshot(tmp.path());
    model.history = history::load(tmp.path()).unwrap();
    let json = serde_json::to_value(&model).unwrap();
    assert_eq!(json["history"][0]["baseline"], json!({ "tables": 1, "enums": 0 }));
}

#[test]
fn a_v2_payload_still_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v2: Value = serde_json::to_value(build(&project(tmp.path()), None)).unwrap();
    v2["version"] = json!(2);
    let back: SchemaModel = serde_json::from_value(v2).unwrap();
    assert!(back.history.is_empty());
}
