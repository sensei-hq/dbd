# Schema Diagram Viewer v1 — Plan 1: `SchemaModel` + `dbd diagram --json`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a dbd-native `SchemaModel` (the JSON contract the diagram viewer will consume) and a `dbd diagram --json` command that emits it. This is Plan 1 of 2 for v1 — Plan 2 adds the Svelte+Rokkit viewer and the default self-contained HTML output.

**Architecture:** A pure builder `schema_model::build(&Design, Option<&ResolvedScope>) -> SchemaModel` derives the model from loaded entities + `TableDef`s (columns, PK/null/enum flags, comments) + FK relationships (from inline + table-level constraints). It serializes to the proven `DBD_SCHEMA` JSON shape (`project`/`schemas`/`tables`/`refs` with `{s,t,c}` column-level refs). A new `dbd diagram --json` command writes it via `safe_write`, scope-aware via `Design::scoped_entities`.

**Tech Stack:** Rust, `serde`/`serde_json`, `insta` (snapshot tests). Spec: `docs/superpowers/specs/2026-06-12-schema-diagram-viewer-design.md`. Reference data shape: `docs/mockup/designs/schema-data.js` (`window.DBD_SCHEMA`).

**Note on v1 split:** In Plan 1, `dbd diagram` emits **JSON only** (the model). Plan 2 adds the viewer bundle + HTML template, makes HTML the default, and makes `--json` the opt-in flag.

---

## File structure

- **Create** `crates/dbd-core/src/schema_model.rs` — `SchemaModel` types + `build()` + tests. One responsibility: turn a `Design` into the viewer JSON model.
- **Modify** `crates/dbd-core/src/lib.rs` — `pub mod schema_model;` + re-export `SchemaModel`.
- **Create** `crates/dbd-cli/src/commands/diagram.rs` — `cmd_diagram` (JSON output). (New small command file; mirrors `commands/project.rs` style.)
- **Modify** `crates/dbd-cli/src/cli.rs` — add `Commands::Diagram { file, json }`.
- **Modify** `crates/dbd-cli/src/commands/mod.rs` — declare `mod diagram;` + dispatch arm.
- **Modify** docs: `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt`, `README.md`.

---

## Task 1: `SchemaModel` types

**Files:**
- Create: `crates/dbd-core/src/schema_model.rs`
- Modify: `crates/dbd-core/src/lib.rs`

- [ ] **Step 1: Write the failing test** (append to a `tests` module at the bottom of the new file)

Create `crates/dbd-core/src/schema_model.rs` with the types and this test:

```rust
//! The `SchemaModel` — a dbd-native JSON model of a schema, consumed by the
//! diagram viewer. Serializes to the `DBD_SCHEMA` shape (see
//! docs/mockup/designs/schema-data.js). Boolean column flags (`pk`/`nn`/`en`)
//! are emitted only when true; the viewer reads them truthily.

use serde::Serialize;

use crate::design::Design;
use crate::entity::{EntityType, FkAction, TableConstraint};
use crate::scope::ResolvedScope;

#[derive(Debug, PartialEq, Serialize)]
pub struct SchemaModel {
    pub project: ProjectInfo,
    pub schemas: Vec<SchemaInfo>,
    pub tables: Vec<TableNode>,
    pub refs: Vec<Ref>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct ProjectInfo {
    pub name: String,
    pub db: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct SchemaInfo {
    pub name: String,
    pub tables: usize,
    pub enums: usize,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct TableNode {
    pub schema: String,
    pub name: String,
    /// "table" in v1; extension point for view/function/procedure later.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "noteMd", skip_serializing_if = "Option::is_none")]
    pub note_md: Option<String>,
    pub columns: Vec<Column>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// primary key
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pk: bool,
    /// not null
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub nn: bool,
    /// column type is an enum
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub en: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Ref {
    pub from: RefEnd,
    pub to: RefEnd,
    /// FK on-delete action: cascade | restrict | set_null | set_default | no_action
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct RefEnd {
    pub s: String,
    pub t: String,
    pub c: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_dbd_schema_shape() {
        let model = SchemaModel {
            project: ProjectInfo { name: "p".into(), db: "postgresql".into(), note: None },
            schemas: vec![SchemaInfo { name: "config".into(), tables: 1, enums: 0 }],
            tables: vec![TableNode {
                schema: "config".into(),
                name: "lookups".into(),
                kind: "table".into(),
                note: None,
                note_md: None,
                columns: vec![Column {
                    name: "id".into(),
                    ty: "uuid".into(),
                    pk: true,
                    nn: true,
                    en: false,
                    def: Some("gen_random_uuid()".into()),
                    note: None,
                }],
            }],
            refs: vec![],
        };
        let v: serde_json::Value = serde_json::to_value(&model).unwrap();
        // false flags omitted; true flags present; renamed keys.
        assert_eq!(v["tables"][0]["columns"][0]["pk"], serde_json::json!(true));
        assert_eq!(v["tables"][0]["columns"][0]["type"], serde_json::json!("uuid"));
        assert!(v["tables"][0]["columns"][0].get("en").is_none(), "false flag omitted");
        assert_eq!(v["tables"][0]["columns"][0]["def"], serde_json::json!("gen_random_uuid()"));
        assert!(v["project"].get("note").is_none(), "None note omitted");
    }
}
```

Then add to `crates/dbd-core/src/lib.rs` after the other `pub mod` lines:

```rust
pub mod schema_model;
```

and in the `pub use` block:

```rust
pub use schema_model::SchemaModel;
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core schema_model::tests::serializes_to_dbd_schema_shape`
Expected: compile error first (until the file is wired), then PASS once the types compile. If it fails to compile because `serde_json` isn't a dev-dep — it is already used across the crate's tests, so this compiles. Expected after wiring: PASS.

- [ ] **Step 3: (types are the implementation)** — no extra code; the structs above are the implementation.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p dbd-core schema_model::tests::serializes_to_dbd_schema_shape`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/schema_model.rs crates/dbd-core/src/lib.rs
git commit -m "feat(schema_model): SchemaModel types (DBD_SCHEMA JSON shape)"
```

---

## Task 2: `build()` — Design → SchemaModel

**Files:**
- Modify: `crates/dbd-core/src/schema_model.rs`
- Test fixture: `tests/fixtures/design.yaml` (existing — has `config`/`staging` schemas, `config.lookups`/`config.lookup_values` tables with an FK, `config.status` enum).

- [ ] **Step 1: Write the failing test** (add to the `tests` module)

```rust
    use crate::design::Design;
    use std::path::PathBuf;

    fn fixture_design() -> Design {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/design.yaml");
        Design::from_config(&p, "dev").unwrap()
    }

    #[test]
    fn build_full_model_from_fixture() {
        let d = fixture_design();
        let m = build(&d, None);

        // project
        assert_eq!(m.project.name, "example");
        assert_eq!(m.project.db, "postgresql");

        // schemas include config + staging with a table count
        let config = m.schemas.iter().find(|s| s.name == "config").expect("config schema");
        assert!(config.tables >= 2, "config has lookups + lookup_values");

        // tables: config.lookups present with an id column
        let lookups = m.tables.iter().find(|t| t.schema == "config" && t.name == "lookups").expect("lookups");
        assert_eq!(lookups.kind, "table");
        assert!(lookups.columns.iter().any(|c| c.name == "id" && c.pk), "id is pk");

        // refs: config.lookup_values → config.lookups (FK on lookup_id → id)
        assert!(
            m.refs.iter().any(|r|
                r.from.s == "config" && r.from.t == "lookup_values"
                && r.to.s == "config" && r.to.t == "lookups"),
            "FK edge present: {:?}", m.refs
        );

        // no view/function/procedure nodes in v1
        assert!(m.tables.iter().all(|t| t.kind == "table"));
    }

    #[test]
    fn build_scoped_filters_tables_and_refs() {
        let d = fixture_design();
        let scope = d.resolve_scope(Some("config_only"), None).unwrap();
        let m = build(&d, Some(&scope));
        assert!(m.tables.iter().all(|t| t.schema != "staging"), "staging dropped");
        assert!(m.tables.iter().any(|t| t.schema == "config"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core schema_model::tests::build_full_model_from_fixture`
Expected: FAIL — `build` is not defined.

- [ ] **Step 3: Implement `build()`** (add to `schema_model.rs`, above the `#[cfg(test)]` module)

```rust
/// Build a `SchemaModel` from a loaded design, optionally filtered to a scope.
/// v1 emits only tables + schemas + FK refs.
pub fn build(design: &Design, scope: Option<&ResolvedScope>) -> SchemaModel {
    let entities = match scope {
        Some(s) => design.scoped_entities(s).unwrap_or_default(),
        None => design.entities().to_vec(),
    };

    // Enum type names (bare + qualified) → used to flag `en` columns.
    let enum_names: std::collections::HashSet<String> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Enum)
        .flat_map(|e| {
            let bare = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();
            [e.name.clone(), bare]
        })
        .collect();

    // In-scope table ids (for ref target filtering).
    let table_ids: std::collections::HashSet<String> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::Table)
        .map(|e| e.name.clone())
        .collect();

    let mut tables = Vec::new();
    let mut refs = Vec::new();

    for e in entities.iter().filter(|e| e.entity_type == EntityType::Table) {
        let Some(def) = &e.table_def else { continue };
        let schema = e.schema.clone().unwrap_or_default();
        let name = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();

        // PK columns can come from a table-level PrimaryKey constraint too.
        let pk_cols: std::collections::HashSet<&str> = def
            .constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::PrimaryKey { columns, .. } => Some(columns.iter().map(|s| s.as_str())),
                _ => None,
            })
            .flatten()
            .collect();

        let columns = def
            .columns
            .iter()
            .map(|c| Column {
                name: c.name.clone(),
                ty: c.data_type.clone(),
                pk: c.is_pk || pk_cols.contains(c.name.as_str()),
                nn: !c.nullable,
                en: enum_names.contains(&c.data_type),
                def: c.default_value.clone(),
                note: c.comment.clone().or_else(|| def.comments.columns.get(&c.name).cloned()),
            })
            .collect();

        tables.push(TableNode {
            schema,
            name,
            kind: "table".into(),
            note: e_note_first_line(def),
            note_md: def.comments.table.clone(),
            columns,
        });

        // FKs: inline (column) + table-level constraints.
        let fks = collect_fks(def);
        for fk in fks {
            let to_schema = fk.ref_schema.clone().unwrap_or_else(|| e.schema.clone().unwrap_or_default());
            let to_id = format!("{to_schema}.{}", fk.ref_table);
            if !table_ids.contains(&to_id) {
                continue; // target out of scope / external — no edge
            }
            let action = fk.on_delete.map(fk_action_str);
            let from_schema = e.schema.clone().unwrap_or_default();
            let from_table = e.name.rsplit('.').next().unwrap_or(&e.name).to_string();
            for (i, local) in fk.columns.iter().enumerate() {
                let remote = fk.ref_columns.get(i).cloned().unwrap_or_default();
                refs.push(Ref {
                    from: RefEnd { s: from_schema.clone(), t: from_table.clone(), c: local.clone() },
                    to: RefEnd { s: to_schema.clone(), t: fk.ref_table.clone(), c: remote },
                    action: action.clone(),
                });
            }
        }
    }

    // schema list with table/enum counts (from in-scope entities).
    let mut schema_set: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for e in &entities {
        let Some(s) = &e.schema else { continue };
        let entry = schema_set.entry(s.clone()).or_insert((0, 0));
        match e.entity_type {
            EntityType::Table => entry.0 += 1,
            EntityType::Enum => entry.1 += 1,
            _ => {}
        }
    }
    let schemas = schema_set
        .into_iter()
        .map(|(name, (tables, enums))| SchemaInfo { name, tables, enums })
        .collect();

    tables.sort_by(|a, b| (a.schema.as_str(), a.name.as_str()).cmp(&(b.schema.as_str(), b.name.as_str())));

    SchemaModel {
        project: ProjectInfo {
            name: design.config().project.name.clone(),
            db: design.config().source.dialect.clone(),
            note: design.config().project.note.clone(),
        },
        schemas,
        tables,
        refs,
    }
}

/// First line of a table comment → the short `note`.
fn e_note_first_line(def: &crate::entity::TableDef) -> Option<String> {
    def.comments.table.as_ref().map(|t| t.lines().next().unwrap_or("").to_string())
}

/// All foreign keys on a table: inline column FKs + table-level FK constraints.
fn collect_fks(def: &crate::entity::TableDef) -> Vec<crate::entity::ForeignKey> {
    let mut out: Vec<crate::entity::ForeignKey> = def
        .columns
        .iter()
        .filter_map(|c| c.inline_fk.clone())
        .collect();
    for c in &def.constraints {
        if let TableConstraint::ForeignKey(fk) = c {
            out.push(fk.clone());
        }
    }
    out
}

fn fk_action_str(a: FkAction) -> String {
    match a {
        FkAction::Cascade => "cascade",
        FkAction::Restrict => "restrict",
        FkAction::SetNull => "set_null",
        FkAction::SetDefault => "set_default",
        FkAction::NoAction => "no_action",
    }
    .to_string()
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p dbd-core schema_model::tests`
Expected: PASS (both `build_full_model_from_fixture` and `build_scoped_filters_tables_and_refs`).
If `m.project.name` mismatch: open `tests/fixtures/design.yaml` and use the actual `project.name` value in the assertion.

- [ ] **Step 5: Run clippy + commit**

```bash
cargo clippy -p dbd-core --all-targets -- -D warnings
git add crates/dbd-core/src/schema_model.rs
git commit -m "feat(schema_model): build() from Design (tables/schemas/FK refs, scope-aware)"
```

---

## Task 3: Snapshot the model JSON

**Files:**
- Modify: `crates/dbd-core/src/schema_model.rs` (add an `insta` snapshot test). `insta` is already a dev-dependency.

- [ ] **Step 1: Write the snapshot test** (add to `tests` module)

```rust
    #[test]
    fn snapshot_fixture_model_json() {
        let d = fixture_design();
        let m = build(&d, None);
        let json = serde_json::to_string_pretty(&m).unwrap();
        insta::assert_snapshot!(json);
    }
```

- [ ] **Step 2: Run to generate the snapshot**

Run: `cargo test -p dbd-core schema_model::tests::snapshot_fixture_model_json`
Expected: FAIL (new snapshot pending). Review it:

Run: `cargo insta review` (accept if the JSON shape looks right — project/schemas/tables/refs, `type`/`noteMd` keys, omitted false flags). Or `INSTA_UPDATE=always cargo test -p dbd-core schema_model::tests::snapshot_fixture_model_json` after eyeballing.

- [ ] **Step 3: Verify it passes**

Run: `cargo test -p dbd-core schema_model::tests::snapshot_fixture_model_json`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/schema_model.rs crates/dbd-core/src/snapshots/
git commit -m "test(schema_model): snapshot the fixture model JSON"
```

---

## Task 4: `dbd diagram --json` command

**Files:**
- Create: `crates/dbd-cli/src/commands/diagram.rs`
- Modify: `crates/dbd-cli/src/cli.rs`, `crates/dbd-cli/src/commands/mod.rs`
- Test: `crates/dbd-core/tests/integration_test.rs` (model-level integration test — exercises the public `build` + serde the way the CLI does).

- [ ] **Step 1: Write a failing integration test** (append to `crates/dbd-core/tests/integration_test.rs`)

```rust
#[test]
fn diagram_model_json_round_trips() {
    let d = design(); // existing helper in this file
    let model = dbd_core::schema_model::build(&d, None);
    let json = serde_json::to_string(&model).unwrap();
    // The emitted JSON parses and carries the contract keys the viewer needs.
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["project"]["name"].is_string());
    assert!(v["schemas"].as_array().unwrap().iter().any(|s| s["name"] == "config"));
    assert!(v["tables"].as_array().unwrap().iter().any(|t| t["schema"] == "config" && t["name"] == "lookups"));
    assert!(v["refs"].is_array());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core --test integration_test diagram_model_json_round_trips`
Expected: FAIL — `schema_model` module/`build` not reachable until Task 1–2 are in. (If Tasks 1–2 are committed, this passes already; that's fine — it locks the contract. Then proceed to wire the CLI.)

- [ ] **Step 3: Add the CLI command.** In `crates/dbd-cli/src/cli.rs`, add to the `Commands` enum (near `Dbml`):

```rust
    /// Generate an interactive schema diagram (JSON model in v1; HTML in v2)
    Diagram {
        /// Destination file (default: schema.json)
        #[arg(short, long, default_value = "schema.json")]
        file: PathBuf,
        /// Emit the raw SchemaModel JSON (the only mode in v1)
        #[arg(long)]
        json: bool,
    },
```

Create `crates/dbd-cli/src/commands/diagram.rs`:

```rust
use std::path::Path;

use anyhow::{Context, Result};
use dbd_core::Design;

use super::safe_write;
use crate::output::{self, Verbosity};

#[allow(clippy::too_many_arguments)]
pub fn cmd_diagram(
    config: &Path,
    env: &str,
    project_dir: &Path,
    file: &Path,
    _json: bool, // v1: always JSON; flag reserved for v2 when HTML becomes the default
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config_with_dir(config, env, Some(project_dir))
        .context("Failed to load design")?;
    let resolved = design.resolve_scope(scope, deps)?;
    let model = dbd_core::schema_model::build(&design, Some(&resolved));
    let json = serde_json::to_string_pretty(&model)
        .context("Failed to serialize schema model")?;
    safe_write(project_dir, file, &json)?;
    output::info(verbosity, &format!("Wrote schema model to {}", file.display()));
    Ok(())
}
```

In `crates/dbd-cli/src/commands/mod.rs`, add `mod diagram;` with the others, and a dispatch arm (mirroring `Commands::Dbml`):

```rust
        Commands::Diagram { file, json } => {
            diagram::cmd_diagram(config, env, project_dir, file, *json, scope, deps, verbosity)
        }
```

- [ ] **Step 4: Run to verify it passes + manual smoke**

Run: `cargo test -p dbd-core --test integration_test diagram_model_json_round_trips`
Expected: PASS.

Manual smoke (from a project dir with `design.yaml` + DDL):
```bash
cd tests/fixtures && cargo run -q --manifest-path ../../Cargo.toml -p dbd-cli -- diagram -e dev --json -f /tmp/schema.json && head -c 400 /tmp/schema.json; rm /tmp/schema.json
```
Expected: a JSON object with `project`/`schemas`/`tables`/`refs`.

- [ ] **Step 5: clippy + commit**

```bash
cargo clippy --all-targets -- -D warnings && cargo test --workspace
git add crates/dbd-cli/src/cli.rs crates/dbd-cli/src/commands/diagram.rs crates/dbd-cli/src/commands/mod.rs crates/dbd-core/tests/integration_test.rs
git commit -m "feat(cli): dbd diagram --json emits the SchemaModel"
```

---

## Task 5: Document `dbd diagram` (JSON, v1)

**Files:**
- Modify: `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt`, `README.md`.

- [ ] **Step 1: Add a commands-guide section.** In `docs/guide/04-commands.md`, after the `## \`dbd graph\`` section, insert:

```markdown
## `dbd diagram`

Emit a dbd-native **schema model** (JSON) describing schemas, tables, columns, and FK relationships — the input to the interactive schema diagram viewer.

```sh
dbd diagram --json                 # writes schema.json
dbd diagram --json -f model.json   # custom path
dbd diagram --json --scope hub     # scope-aware (only the scope's tables/refs)
```

In v1 the command emits JSON only (`--json`). A later release renders this model into a self-contained interactive HTML diagram (the default output) — replacing the external dbdocs.io step.

---
```

- [ ] **Step 2: Add llms entries.** In `docs/llms/llms.txt` commands list (after the `dbd graph` line):

```markdown
- `dbd diagram --json` — emit the schema model JSON (schemas/tables/columns/FK refs) for the interactive diagram viewer (HTML render lands in a later release)
```

In `docs/llms/llms-full.txt`, after the `### dbd graph` section:

```markdown
### dbd diagram

```sh
dbd diagram --json -f model.json   # SchemaModel JSON (scope-aware)
```

dbd-native JSON model (`project`/`schemas`/`tables`/`refs`, column-level FK refs as `{s,t,c}`) — the input to the schema diagram viewer. Not DBML (so it extends to views/functions/procedures). v1 is JSON-only; a later release renders a self-contained interactive HTML diagram.
```

- [ ] **Step 3: Add to the README command table.** In `README.md`, in the Commands table after the `dbd graph` row:

```markdown
| `dbd diagram --json` | Emit the schema model JSON for the diagram viewer |
```

- [ ] **Step 4: Verify build is unaffected + commit**

Run: `cargo test --workspace` (docs don't affect tests; confirms nothing broke).
```bash
git add docs/guide/04-commands.md docs/llms/llms.txt docs/llms/llms-full.txt README.md
git commit -m "docs: document dbd diagram --json (schema model JSON, v1)"
```

---

## Self-review checklist (run before handoff)

- Spec coverage: `SchemaModel` shape (Task 1), `build()` from entities + FKs, scope-aware (Task 2), JSON snapshot (Task 3), `dbd diagram --json` scope-aware via `scoped_entities` (Task 4), docs (Task 5). The HTML/viewer half is explicitly Plan 2.
- No placeholders: all steps have real code/commands.
- Type consistency: `build(&Design, Option<&ResolvedScope>)`, `SchemaModel`/`TableNode`/`Column`/`Ref`/`RefEnd` names are used identically across tasks; `cmd_diagram` signature matches the dispatch arm; the `scoped_entities`/`resolve_scope` APIs match dbd-core's existing signatures.

## Done when

`dbd diagram --json` writes a `SchemaModel` JSON (scope-aware) that parses and carries `project`/`schemas`/`tables`/`refs`; `cargo test --workspace` + `cargo clippy --all-targets -- -D warnings` green. Plan 2 (Svelte+Rokkit viewer + self-contained HTML) consumes this model.
