# Reverse-engineer (`dbd init --from-db` / `dbd merge`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate a dbd project (`design.yaml` + `ddl/<kind>/<schema>/<name>.sql`) from a Postgres/Supabase database, via `dbd init --from-db <conn>` (new project) and `dbd merge <conn>` (sync into an existing project).

**Architecture:** One core pipeline — `introspect (adapter) → Vec<Entity> → emit DDL text → build write-plan → apply/report`. Built bottom-up so every piece is unit-tested without a database except introspection itself. Spec: `docs/superpowers/specs/2026-06-15-reverse-engineer-design.md`.

**Tech Stack:** Rust, `sqlx` (Postgres), `clap`, `anyhow`/`DbdError`, `insta` (existing). Entity model in `crates/dbd-core/src/entity.rs`.

---

## File Structure

- **Create** `crates/dbd-core/src/emit.rs` — DDL emitter: `Entity`/`TableDef` → `CREATE …` SQL text (enum, table, view; schema/extension delegate to `script::ddl_from_entity`).
- **Create** `crates/dbd-core/src/reverse.rs` — the engine: schema selection, entity→path mapping, write-plan (create/skip/conflict/orphan), apply (with `.bak`), `design.yaml` generation, report types.
- **Modify** `crates/dbd-core/src/adapter/mod.rs` — add `introspect()` to the trait (default `Err(unsupported)`).
- **Modify** `crates/dbd-core/src/adapter/postgres.rs` — implement `introspect()` (catalog queries → `Vec<Entity>`).
- **Modify** `crates/dbd-core/src/lib.rs` — `pub mod emit; pub mod reverse;`.
- **Modify** `crates/dbd-cli/src/cli.rs` — add flags to `Init`, add `Merge` command.
- **Modify** `crates/dbd-cli/src/commands/mod.rs` — dispatch `Merge`; extend `Init` dispatch.
- **Create** `crates/dbd-cli/src/commands/reverse.rs` — `cmd_init_from_db` + `cmd_merge` (thin wrappers over `dbd_core::reverse`).

Build order: emitter → schema-select → entity-path → write-plan/apply → design.yaml → introspection → CLI.

---

## Task 1: DDL emitter — enums

**Files:**
- Create: `crates/dbd-core/src/emit.rs`
- Modify: `crates/dbd-core/src/lib.rs` (add `pub mod emit;`)

- [ ] **Step 1: Add the module declaration**

In `crates/dbd-core/src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod emit;
```

- [ ] **Step 2: Write the failing test**

Create `crates/dbd-core/src/emit.rs`:

```rust
//! Emit canonical `CREATE …` DDL text from an `Entity`/`TableDef`. The inverse of
//! the parser; used by the reverse-engineer engine. Output is intended to be
//! re-parseable (round-trip stable).

use crate::entity::{Entity, EntityType, TableConstraint};

/// Quote a SQL identifier.
fn q(ident: &str) -> String {
    format!("\"{ident}\"")
}

/// Bare (unqualified) name from a possibly-qualified entity name (`schema.name` → `name`).
fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// `CREATE TYPE "schema"."name" AS ENUM ('a', 'b');`
pub fn emit_enum(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let values = entity
        .enum_values
        .iter()
        .map(|v| format!("'{}'", v.name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TYPE {}.{} AS ENUM ({});", q(schema), q(name), values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EnumValue;

    #[test]
    fn emits_enum() {
        let mut e = Entity::new(EntityType::Enum, "shop.order_status");
        e.enum_values = vec![
            EnumValue { name: "pending".into(), note: None },
            EnumValue { name: "paid".into(), note: None },
        ];
        assert_eq!(
            emit_enum(&e),
            "CREATE TYPE \"shop\".\"order_status\" AS ENUM ('pending', 'paid');"
        );
    }
}
```

- [ ] **Step 3: Run the test, verify it fails**

Run: `cargo test -p dbd-core emit::tests::emits_enum`
Expected: FAIL (compile error: `emit` module / `emit_enum` references resolve, but the assertion is what we verify — if it compiles, it should pass; if `EnumValue` fields differ it fails to compile). If it compiles and passes immediately, that's fine — proceed.

- [ ] **Step 4: Confirm pass**

Run: `cargo test -p dbd-core emit::tests::emits_enum`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/emit.rs crates/dbd-core/src/lib.rs
git commit -m "feat(core): DDL emitter — enums"
```

---

## Task 2: DDL emitter — tables (columns, constraints, indexes, comments)

**Files:**
- Modify: `crates/dbd-core/src/emit.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/dbd-core/src/emit.rs`:

```rust
#[test]
fn emits_table_roundtrips_through_parser() {
    use crate::entity::{ColumnDef, ForeignKey, IndexColumn, IndexDef, TableConstraint, TableDef};
    let mut e = Entity::new(EntityType::Table, "shop.orders");
    e.table_def = Some(TableDef {
        columns: vec![
            ColumnDef { name: "id".into(), data_type: "uuid".into(), nullable: false,
                default_value: None, is_pk: true, is_unique: false, is_identity: false,
                comment: Some("Order PK".into()), inline_fk: None },
            ColumnDef { name: "customer_id".into(), data_type: "uuid".into(), nullable: false,
                default_value: None, is_pk: false, is_unique: false, is_identity: false,
                comment: None, inline_fk: None },
            ColumnDef { name: "total_cents".into(), data_type: "integer".into(), nullable: false,
                default_value: Some("0".into()), is_pk: false, is_unique: false,
                is_identity: false, comment: None, inline_fk: None },
        ],
        constraints: vec![
            TableConstraint::PrimaryKey { name: None, columns: vec!["id".into()] },
            TableConstraint::ForeignKey(ForeignKey {
                name: None, columns: vec!["customer_id".into()],
                ref_schema: Some("shop".into()), ref_table: "customers".into(),
                ref_columns: vec!["id".into()], on_delete: None, on_update: None }),
        ],
        indexes: vec![IndexDef {
            name: Some("orders_customer_id_idx".into()),
            columns: vec![IndexColumn { name: "customer_id".into(), order: None }],
            unique: false, index_type: None }],
        comments: Default::default(),
    });

    let sql = emit_table(&e);

    // Sanity on the emitted text:
    assert!(sql.contains("CREATE TABLE \"shop\".\"orders\""));
    assert!(sql.contains("\"id\" uuid NOT NULL"));
    assert!(sql.contains("\"total_cents\" integer NOT NULL DEFAULT 0"));
    assert!(sql.contains("PRIMARY KEY (\"id\")"));
    assert!(sql.contains("FOREIGN KEY (\"customer_id\") REFERENCES \"shop\".\"customers\" (\"id\")"));
    assert!(sql.contains("CREATE INDEX \"orders_customer_id_idx\" ON \"shop\".\"orders\" (\"customer_id\");"));
    assert!(sql.contains("COMMENT ON COLUMN \"shop\".\"orders\".\"id\" IS 'Order PK';"));

    // Round-trip: emitted DDL re-parses to a TableDef with the same column set.
    let parsed = crate::parser::tables::parse_table_file(&sql, "shop")
        .expect("emitted table DDL should parse");
    let cols: Vec<&str> = parsed.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cols, vec!["id", "customer_id", "total_cents"]);
}
```

> NOTE: confirm the parser entry point name/signature in `crates/dbd-core/src/parser/tables.rs` (it has an `extract_index`/table parse path used by the existing `extracts_index` test). If the public parse function differs from `parse_table_file(text, schema)`, adjust this assertion to call the real one (e.g. parse via `Entity::from_file` against a temp file, or the module's existing test helper). The round-trip is the goal; match the real API.

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p dbd-core emit::tests::emits_table_roundtrips_through_parser`
Expected: FAIL — `emit_table` not defined.

- [ ] **Step 3: Implement `emit_table`**

Add to `crates/dbd-core/src/emit.rs`:

```rust
/// `CREATE TABLE "schema"."name" ( … );` + `CREATE INDEX …;` + `COMMENT ON …;`
pub fn emit_table(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let qname = format!("{}.{}", q(schema), q(name));
    let Some(def) = &entity.table_def else {
        return format!("CREATE TABLE {qname} ();");
    };

    let mut lines: Vec<String> = Vec::new();

    // Columns
    for c in &def.columns {
        let mut col = format!("  {} {}", q(&c.name), c.data_type);
        if !c.nullable {
            col.push_str(" NOT NULL");
        }
        if let Some(d) = &c.default_value {
            col.push_str(&format!(" DEFAULT {d}"));
        }
        lines.push(col);
    }

    // Table-level constraints
    for con in &def.constraints {
        match con {
            TableConstraint::PrimaryKey { columns, .. } => {
                lines.push(format!("  PRIMARY KEY ({})", quote_cols(columns)));
            }
            TableConstraint::Unique { columns, .. } => {
                lines.push(format!("  UNIQUE ({})", quote_cols(columns)));
            }
            TableConstraint::ForeignKey(fk) => {
                let ref_schema = fk.ref_schema.as_deref().unwrap_or(schema);
                let mut s = format!(
                    "  FOREIGN KEY ({}) REFERENCES {}.{} ({})",
                    quote_cols(&fk.columns),
                    q(ref_schema),
                    q(&fk.ref_table),
                    quote_cols(&fk.ref_columns),
                );
                if let Some(a) = fk.on_delete {
                    s.push_str(&format!(" ON DELETE {}", fk_action_sql(a)));
                }
                lines.push(s);
            }
            TableConstraint::Check { expression, .. } => {
                lines.push(format!("  CHECK ({expression})"));
            }
        }
    }

    let mut out = format!("CREATE TABLE {qname} (\n{}\n);", lines.join(",\n"));

    // Indexes (skip ones that merely back a PK/UNIQUE — emit explicit indexes only)
    for ix in &def.indexes {
        let cols = ix
            .columns
            .iter()
            .map(|c| match c.order {
                Some(crate::entity::SortOrder::Desc) => format!("{} DESC", q(&c.name)),
                _ => q(&c.name),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let unique = if ix.unique { "UNIQUE " } else { "" };
        let idx_name = ix.name.clone().unwrap_or_else(|| format!("{name}_idx"));
        out.push_str(&format!(
            "\nCREATE {unique}INDEX {} ON {qname} ({cols});",
            q(&idx_name)
        ));
    }

    // Comments
    if let Some(tc) = &def.comments.table {
        out.push_str(&format!("\nCOMMENT ON TABLE {qname} IS '{}';", esc(tc)));
    }
    for c in &def.columns {
        if let Some(cm) = &c.comment {
            out.push_str(&format!(
                "\nCOMMENT ON COLUMN {qname}.{} IS '{}';",
                q(&c.name),
                esc(cm)
            ));
        }
    }
    out
}

fn quote_cols(cols: &[String]) -> String {
    cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")
}

fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

fn fk_action_sql(a: crate::entity::FkAction) -> &'static str {
    use crate::entity::FkAction::*;
    match a {
        Cascade => "CASCADE",
        Restrict => "RESTRICT",
        SetNull => "SET NULL",
        SetDefault => "SET DEFAULT",
        NoAction => "NO ACTION",
    }
}
```

- [ ] **Step 4: Run the test, verify it passes** (adjust the round-trip call to the real parser API if needed)

Run: `cargo test -p dbd-core emit::tests::emits_table_roundtrips_through_parser`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/emit.rs
git commit -m "feat(core): DDL emitter — tables (cols/constraints/indexes/comments)"
```

---

## Task 3: DDL emitter — views + the `emit_entity` dispatcher

**Files:**
- Modify: `crates/dbd-core/src/emit.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests`:

```rust
#[test]
fn emits_view() {
    let mut e = Entity::new(EntityType::View, "shop.active_orders");
    // Views carry their definition SQL in table_def-adjacent storage; the
    // reverse engine stores the view body in entity.table_def? No — see Step 3:
    // the introspector puts the view definition in `entity` via a dedicated field.
    e.references = vec![]; // unused here
    // We store the view body in entity.writes[0] per the introspector contract:
    e.writes = vec!["SELECT * FROM shop.orders WHERE status = 'paid'".into()];
    let sql = emit_view(&e);
    assert_eq!(
        sql,
        "CREATE VIEW \"shop\".\"active_orders\" AS SELECT * FROM shop.orders WHERE status = 'paid';"
    );
}

#[test]
fn emit_entity_dispatches_by_type() {
    let s = Entity::new(EntityType::Schema, "shop");
    assert_eq!(emit_entity(&s).unwrap(), "CREATE SCHEMA IF NOT EXISTS \"shop\";");
    let ext_none = Entity::new(EntityType::External, "auth.users");
    assert!(emit_entity(&ext_none).is_none());
}
```

> NOTE: the view body needs a home on `Entity`. Rather than add a field, this plan reuses `entity.writes` as the view-definition carrier (a `Vec<String>` already on `Entity`; the introspector sets `writes = vec![definition]`). If the team prefers, add a `view_def: Option<String>` to `Entity` in a tiny separate step — but reusing `writes` avoids touching the struct + all its constructors. Decide at implementation; the test above encodes the `writes[0]` choice.

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-core emit::tests::emits_view`
Expected: FAIL — `emit_view`/`emit_entity` undefined.

- [ ] **Step 3: Implement view emit + dispatcher**

Add to `emit.rs`:

```rust
/// `CREATE VIEW "schema"."name" AS <definition>;`
/// The view body is carried in `entity.writes[0]` (set by the introspector).
pub fn emit_view(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let body = entity.writes.first().map(String::as_str).unwrap_or("SELECT 1").trim();
    let body = body.trim_end_matches(';');
    format!("CREATE VIEW {}.{} AS {body};", q(schema), q(name))
}

/// Emit DDL text for any reverse-engineerable entity, or `None` for kinds we
/// don't generate (External, file-based Function/Procedure in this cut).
pub fn emit_entity(entity: &Entity) -> Option<String> {
    match entity.entity_type {
        EntityType::Schema | EntityType::Extension | EntityType::Role => {
            crate::script::ddl_from_entity(entity)
        }
        EntityType::Enum => Some(emit_enum(entity)),
        EntityType::Table => Some(emit_table(entity)),
        EntityType::View => Some(emit_view(entity)),
        _ => None,
    }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-core emit::tests`
Expected: PASS (all emit tests)

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/emit.rs
git commit -m "feat(core): DDL emitter — views + emit_entity dispatcher"
```

---

## Task 4: Reverse engine — schema selection

**Files:**
- Create: `crates/dbd-core/src/reverse.rs`
- Modify: `crates/dbd-core/src/lib.rs` (add `pub mod reverse;`)

- [ ] **Step 1: Add module declaration** in `lib.rs`:

```rust
pub mod reverse;
```

- [ ] **Step 2: Write the failing test**

Create `crates/dbd-core/src/reverse.rs`:

```rust
//! Reverse-engineer engine: turn introspected entities into a write-plan over a
//! dbd project folder. Pure (no DB) except where it calls an adapter's
//! `introspect()`. See docs/superpowers/specs/2026-06-15-reverse-engineer-design.md.

/// Schemas always excluded (Postgres internals), regardless of flags.
pub const ALWAYS_EXCLUDED: &[&str] = &["pg_catalog", "information_schema"];

/// Supabase platform schemas excluded by default (overridable with `all=true`).
pub const SUPABASE_DENYLIST: &[&str] = &[
    "auth", "storage", "realtime", "_realtime", "extensions", "graphql",
    "graphql_public", "vault", "pgsodium", "pgsodium_masks", "supabase_functions",
    "supabase_migrations", "cron", "net", "pgbouncer", "_analytics", "_supavisor",
    "pgtle",
];

fn is_internal(schema: &str) -> bool {
    ALWAYS_EXCLUDED.contains(&schema)
        || schema.starts_with("pg_toast")
        || schema.starts_with("pg_temp")
}

/// Options controlling which schemas are reverse-engineered.
#[derive(Debug, Clone, Default)]
pub struct SchemaSelect {
    /// Allowlist — when non-empty, only these schemas are kept.
    pub only: Vec<String>,
    /// Extra schemas to exclude.
    pub exclude: Vec<String>,
    /// Bypass the Supabase denylist.
    pub all: bool,
}

/// Filter `db_schemas` (all schemas discovered in the DB) down to the set to emit.
pub fn select_schemas(db_schemas: &[String], opts: &SchemaSelect) -> Vec<String> {
    db_schemas
        .iter()
        .filter(|s| !is_internal(s))
        .filter(|s| opts.all || !SUPABASE_DENYLIST.contains(&s.as_str()))
        .filter(|s| !opts.exclude.iter().any(|e| e == *s))
        .filter(|s| opts.only.is_empty() || opts.only.iter().any(|o| o == *s))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(xs: &[&str]) -> Vec<String> { xs.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn excludes_internal_and_supabase_by_default() {
        let db = v(&["public", "app", "pg_catalog", "auth", "storage", "pg_toast_x"]);
        let got = select_schemas(&db, &SchemaSelect::default());
        assert_eq!(got, v(&["public", "app"]));
    }

    #[test]
    fn all_schemas_keeps_supabase_but_not_pg_internal() {
        let db = v(&["public", "auth", "pg_catalog"]);
        let got = select_schemas(&db, &SchemaSelect { all: true, ..Default::default() });
        assert_eq!(got, v(&["public", "auth"]));
    }

    #[test]
    fn only_allowlist_wins() {
        let db = v(&["public", "app", "reporting"]);
        let got = select_schemas(&db, &SchemaSelect { only: v(&["app"]), ..Default::default() });
        assert_eq!(got, v(&["app"]));
    }

    #[test]
    fn exclude_adds_to_denies() {
        let db = v(&["public", "app"]);
        let got = select_schemas(&db, &SchemaSelect { exclude: v(&["app"]), ..Default::default() });
        assert_eq!(got, v(&["public"]));
    }
}
```

- [ ] **Step 3: Run, verify pass** (this task's code + tests are in one file)

Run: `cargo test -p dbd-core reverse::tests`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/reverse.rs crates/dbd-core/src/lib.rs
git commit -m "feat(core): reverse engine — schema selection"
```

---

## Task 5: Reverse engine — entity→path mapping

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs`

- [ ] **Step 1: Write the failing test**

Append to `reverse.rs` `tests`:

```rust
#[test]
fn entity_paths_follow_ddl_convention() {
    use crate::entity::{Entity, EntityType};
    use std::path::PathBuf;
    let t = Entity::new(EntityType::Table, "shop.orders");
    assert_eq!(entity_path(&t), PathBuf::from("ddl/table/shop/orders.sql"));
    let e = Entity::new(EntityType::Enum, "shop.order_status");
    assert_eq!(entity_path(&e), PathBuf::from("ddl/enum/shop/order_status.sql"));
    let s = Entity::new(EntityType::Schema, "shop");
    assert_eq!(entity_path(&s), PathBuf::from("ddl/schema/shop.sql"));
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-core reverse::tests::entity_paths_follow_ddl_convention`
Expected: FAIL — `entity_path` undefined.

- [ ] **Step 3: Implement**

Add to `reverse.rs` (note: schemas/extensions are emitted into `ddl/schema/` and `ddl/extension/` even though `from_file` treats them as non-schema-qualified — they get `ddl/<kind>/<name>.sql`):

```rust
use crate::entity::{Entity, EntityType};
use std::path::PathBuf;

/// Map an entity to its DDL file path: `ddl/<kind>/<schema>/<name>.sql` for
/// schema-qualified kinds, `ddl/<kind>/<name>.sql` otherwise.
pub fn entity_path(entity: &Entity) -> PathBuf {
    let kind = entity.entity_type.tag(); // "table", "enum", "view", "schema", "extension"
    let name = entity.name.rsplit('.').next().unwrap_or(&entity.name);
    let mut p = PathBuf::from("ddl");
    p.push(&kind);
    if entity.entity_type.has_schema() {
        if let Some(schema) = &entity.schema {
            p.push(schema);
        }
    }
    p.push(format!("{name}.sql"));
    p
}

/// The entity kinds this command generates (used to scope orphan detection).
pub const MANAGED_KINDS: &[EntityType] = &[
    EntityType::Schema, EntityType::Extension, EntityType::Enum,
    EntityType::Table, EntityType::View,
];
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-core reverse::tests::entity_paths_follow_ddl_convention`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reverse.rs
git commit -m "feat(core): reverse engine — entity→path mapping"
```

---

## Task 6: Reverse engine — write-plan (create/skip/conflict/orphan)

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs`

- [ ] **Step 1: Write the failing test**

Append to `reverse.rs` `tests`:

```rust
#[test]
fn build_plan_classifies_files() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // existing files: one identical, one differing, one orphan
    fs::create_dir_all(root.join("ddl/table/shop")).unwrap();
    fs::write(root.join("ddl/table/shop/orders.sql"), "SAME").unwrap();
    fs::write(root.join("ddl/table/shop/customers.sql"), "OLD").unwrap();
    fs::write(root.join("ddl/table/shop/legacy.sql"), "ORPHAN").unwrap();
    // an unmanaged kind that must NOT be flagged as orphan:
    fs::create_dir_all(root.join("ddl/function/shop")).unwrap();
    fs::write(root.join("ddl/function/shop/f.sql"), "FN").unwrap();

    let generated = vec![
        (PathBuf::from("ddl/table/shop/orders.sql"), "SAME".to_string()),     // skip
        (PathBuf::from("ddl/table/shop/customers.sql"), "NEW".to_string()),   // conflict
        (PathBuf::from("ddl/table/shop/products.sql"), "NEW".to_string()),    // create
    ];
    let plan = build_plan(root, generated, &["shop".to_string()]);

    let by = |a: FileAction| plan.items.iter().filter(|i| i.action == a)
        .map(|i| i.path.clone()).collect::<Vec<_>>();
    assert_eq!(by(FileAction::Skip), vec![PathBuf::from("ddl/table/shop/orders.sql")]);
    assert_eq!(by(FileAction::Conflict), vec![PathBuf::from("ddl/table/shop/customers.sql")]);
    assert_eq!(by(FileAction::Create), vec![PathBuf::from("ddl/table/shop/products.sql")]);
    assert_eq!(plan.orphans, vec![PathBuf::from("ddl/table/shop/legacy.sql")]);
    // function file is unmanaged → never an orphan
    assert!(!plan.orphans.iter().any(|p| p.to_string_lossy().contains("function")));
}
```

> Add `tempfile` to `[dev-dependencies]` in `crates/dbd-core/Cargo.toml` if absent (`tempfile = "3"`). Check first: `grep tempfile crates/dbd-core/Cargo.toml`.

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-core reverse::tests::build_plan_classifies_files`
Expected: FAIL — `FileAction`/`build_plan` undefined.

- [ ] **Step 3: Implement**

Add to `reverse.rs`:

```rust
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction { Create, Skip, Conflict }

#[derive(Debug, Clone)]
pub struct PlanItem {
    /// Path relative to the project root.
    pub path: PathBuf,
    pub content: String,
    pub action: FileAction,
}

#[derive(Debug, Default)]
pub struct WritePlan {
    pub items: Vec<PlanItem>,
    /// Existing managed-kind files under a selected schema with no generated counterpart.
    pub orphans: Vec<PathBuf>,
}

/// Classify each generated file vs disk, and detect orphans within `selected_schemas`
/// for the managed kinds only.
pub fn build_plan(
    root: &Path,
    generated: Vec<(PathBuf, String)>,
    selected_schemas: &[String],
) -> WritePlan {
    let mut items = Vec::new();
    let generated_paths: std::collections::HashSet<PathBuf> =
        generated.iter().map(|(p, _)| p.clone()).collect();

    for (rel, content) in generated {
        let abs = root.join(&rel);
        let action = match std::fs::read_to_string(&abs) {
            Ok(existing) if existing == content => FileAction::Skip,
            Ok(_) => FileAction::Conflict,
            Err(_) => FileAction::Create,
        };
        items.push(PlanItem { path: rel, content, action });
    }

    // Orphans: walk ddl/<managed-kind>/<selected-schema>/*.sql not in generated.
    let mut orphans = Vec::new();
    for kind in MANAGED_KINDS {
        if !kind.has_schema() {
            continue; // schema/extension live flat; skip orphan scan for them in v1
        }
        for schema in selected_schemas {
            let dir = root.join("ddl").join(kind.tag()).join(schema);
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("sql") {
                    continue;
                }
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                if !generated_paths.contains(&rel) {
                    orphans.push(rel);
                }
            }
        }
    }
    orphans.sort();
    WritePlan { items, orphans }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-core reverse::tests::build_plan_classifies_files`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reverse.rs crates/dbd-core/Cargo.toml
git commit -m "feat(core): reverse engine — write-plan (create/skip/conflict/orphan)"
```

---

## Task 7: Reverse engine — apply (with `.bak`), dry-run, report

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs`

- [ ] **Step 1: Write the failing tests**

Append to `reverse.rs` `tests`:

```rust
fn item(p: &str, c: &str, a: FileAction) -> PlanItem {
    PlanItem { path: PathBuf::from(p), content: c.into(), action: a }
}

#[test]
fn apply_without_force_aborts_on_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let plan = WritePlan {
        items: vec![item("ddl/table/s/a.sql", "NEW", FileAction::Conflict)],
        orphans: vec![],
    };
    let err = apply_plan(dir.path(), &plan, /*force*/ false, /*dry_run*/ false).unwrap_err();
    assert!(err.to_string().contains("conflict"));
}

#[test]
fn apply_with_force_backs_up_and_writes() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("ddl/table/s")).unwrap();
    fs::write(dir.path().join("ddl/table/s/a.sql"), "OLD").unwrap();
    let plan = WritePlan {
        items: vec![
            item("ddl/table/s/a.sql", "NEW", FileAction::Conflict),
            item("ddl/table/s/b.sql", "B", FileAction::Create),
        ],
        orphans: vec![PathBuf::from("ddl/table/s/legacy.sql")],
    };
    let report = apply_plan(dir.path(), &plan, true, false).unwrap();
    assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.sql")).unwrap(), "NEW");
    assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/a.sql.bak")).unwrap(), "OLD");
    assert_eq!(fs::read_to_string(dir.path().join("ddl/table/s/b.sql")).unwrap(), "B");
    assert_eq!(report.created, 1);
    assert_eq!(report.overwritten, 1);
    assert_eq!(report.orphans, 1);
}

#[test]
fn dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let plan = WritePlan {
        items: vec![item("ddl/table/s/b.sql", "B", FileAction::Create)],
        orphans: vec![],
    };
    apply_plan(dir.path(), &plan, false, true).unwrap();
    assert!(!dir.path().join("ddl/table/s/b.sql").exists());
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-core reverse::tests::apply_with_force_backs_up_and_writes`
Expected: FAIL — `apply_plan`/`Report` undefined.

- [ ] **Step 3: Implement**

Add to `reverse.rs`:

```rust
use crate::error::{DbdError, Result};

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub created: usize,
    pub unchanged: usize,
    pub overwritten: usize,
    pub orphans: usize,
}

/// Pick a non-colliding `.bak` path: `a.sql.bak`, `a.sql.bak.1`, …
fn backup_path(file: &Path) -> PathBuf {
    let base = format!("{}.bak", file.display());
    let mut p = PathBuf::from(&base);
    let mut n = 1;
    while p.exists() {
        p = PathBuf::from(format!("{base}.{n}"));
        n += 1;
    }
    p
}

/// Apply a write-plan. Aborts (no writes) if there are conflicts and `!force`.
/// `dry_run` performs no writes. Orphans are counted, never touched.
pub fn apply_plan(root: &Path, plan: &WritePlan, force: bool, dry_run: bool) -> Result<Report> {
    let conflicts: Vec<&PlanItem> =
        plan.items.iter().filter(|i| i.action == FileAction::Conflict).collect();
    if !conflicts.is_empty() && !force {
        let list = conflicts.iter().map(|i| i.path.display().to_string())
            .collect::<Vec<_>>().join(", ");
        return Err(DbdError::Config(format!(
            "{} file conflict(s) — re-run with --force-overwrite to back up and replace: {list}",
            conflicts.len()
        )));
    }

    let mut report = Report { orphans: plan.orphans.len(), ..Default::default() };
    for it in &plan.items {
        match it.action {
            FileAction::Skip => report.unchanged += 1,
            FileAction::Create => {
                report.created += 1;
                if !dry_run {
                    let abs = root.join(&it.path);
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| DbdError::Config(format!("mkdir {}: {e}", parent.display())))?;
                    }
                    std::fs::write(&abs, &it.content)
                        .map_err(|e| DbdError::Config(format!("write {}: {e}", abs.display())))?;
                }
            }
            FileAction::Conflict => {
                report.overwritten += 1;
                if !dry_run {
                    let abs = root.join(&it.path);
                    let bak = backup_path(&abs);
                    std::fs::rename(&abs, &bak)
                        .map_err(|e| DbdError::Config(format!("backup {}: {e}", abs.display())))?;
                    std::fs::write(&abs, &it.content)
                        .map_err(|e| DbdError::Config(format!("write {}: {e}", abs.display())))?;
                }
            }
        }
    }
    Ok(report)
}
```

> NOTE: confirm `DbdError::Config` is the right variant (it's used throughout `postgres.rs`). If there's a more fitting variant (e.g. `DbdError::Io`), use it; keep `Result` = `crate::error::Result`.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-core reverse::tests`
Expected: PASS (all reverse tests)

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reverse.rs
git commit -m "feat(core): reverse engine — apply (.bak), dry-run, report"
```

---

## Task 8: Reverse engine — design.yaml generation

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs`

- [ ] **Step 1: Write the failing test**

Append to `reverse.rs` `tests`:

```rust
#[test]
fn generates_design_yaml() {
    let yaml = design_yaml("shopdb", "postgresql", &["public".into(), "app".into()], 1);
    assert!(yaml.contains("name: shopdb"));
    assert!(yaml.contains("version: 1"));
    assert!(yaml.contains("dialect: postgresql"));
    assert!(yaml.contains("url: $DATABASE_URL")); // never the literal connection string
    assert!(yaml.contains("- public"));
    assert!(yaml.contains("- app"));
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-core reverse::tests::generates_design_yaml`
Expected: FAIL — `design_yaml` undefined.

- [ ] **Step 3: Implement**

Add to `reverse.rs` (a YAML string template — matches how `init.rs` scaffolds; avoids depending on the full `Config` serializer and guarantees `$DATABASE_URL`, not the secret):

```rust
/// Render a `design.yaml` for a reverse-engineered project. The target URL is
/// always the env reference `$DATABASE_URL` — never the literal connection string.
pub fn design_yaml(project: &str, dialect: &str, schemas: &[String], version: u32) -> String {
    let target_key = if dialect == "sqlite" { "sqlite" } else { "postgres" };
    let schema_lines = schemas.iter().map(|s| format!("  - {s}")).collect::<Vec<_>>().join("\n");
    format!(
        "project:\n  name: {project}\n  version: {version}\n\n\
         source:\n  dialect: {dialect}\n\n\
         target:\n  {target_key}:\n    url: $DATABASE_URL\n\n\
         schemas:\n{schema_lines}\n"
    )
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-core reverse::tests::generates_design_yaml`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reverse.rs
git commit -m "feat(core): reverse engine — design.yaml generation"
```

---

## Task 9: Adapter trait — `introspect()` default + Postgres implementation

**Files:**
- Modify: `crates/dbd-core/src/adapter/mod.rs`
- Modify: `crates/dbd-core/src/adapter/postgres.rs`

- [ ] **Step 1: Add the trait method** in `adapter/mod.rs` (after `list_entities`):

```rust
/// Introspect the live database into reverse-engineerable entities (schemas,
/// extensions, enums, tables, views). Default: unsupported.
async fn introspect(&self) -> Result<Vec<Entity>> {
    Err(crate::error::DbdError::Config(
        "introspection is not supported for this adapter yet".into(),
    ))
}
```

- [ ] **Step 2: Write the introspection integration test** (env-gated — skips when no DB)

Add to the `#[cfg(test)] mod tests` in `postgres.rs` (match the file's existing test style; if the file gates DB tests differently, follow that pattern):

```rust
#[tokio::test]
async fn introspect_roundtrips_a_table() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("skipping: set TEST_DATABASE_URL to run introspection tests");
        return;
    };
    let mut a = PostgresAdapter::new(&url, "test");
    a.connect().await.unwrap();
    a.execute_script(
        "CREATE SCHEMA IF NOT EXISTS revtest; \
         DROP TABLE IF EXISTS revtest.widget; \
         CREATE TABLE revtest.widget (id uuid PRIMARY KEY, name text NOT NULL UNIQUE);"
    ).await.unwrap();

    let entities = a.introspect().await.unwrap();
    let widget = entities.iter()
        .find(|e| e.name == "revtest.widget" && e.entity_type == EntityType::Table)
        .expect("widget table introspected");
    let def = widget.table_def.as_ref().unwrap();
    assert!(def.columns.iter().any(|c| c.name == "id" && c.is_pk));
    assert!(def.columns.iter().any(|c| c.name == "name" && !c.nullable));
}
```

> Confirm `PostgresAdapter::new(url, project)` constructor signature from the top of `postgres.rs`; adjust the constructor call to match.

- [ ] **Step 3: Run, verify fail (or skip)**

Run: `TEST_DATABASE_URL=postgres://… cargo test -p dbd-core introspect_roundtrips_a_table`
Expected: FAIL — `introspect` returns the unsupported error. (Without a DB: skips — acceptable, but implement against a real one before marking done.)

- [ ] **Step 4: Implement `introspect()` in `postgres.rs`**

Add an `impl PostgresAdapter` helper set + override the trait method. Build `Vec<Entity>` from catalog queries — reuse the `self.pool` + `sqlx::query` + `row.get` pattern already in `list_entities` (postgres.rs:513). Concretely, query and assemble in this order, applying the same internal-schema filter:

1. **schemas** — `SELECT nspname FROM pg_namespace WHERE nspname NOT IN ('pg_catalog','information_schema') AND nspname NOT LIKE 'pg_toast%' AND nspname NOT LIKE 'pg_temp%'` → `Entity::new(Schema, nspname)`.
2. **extensions** — `SELECT e.extname, n.nspname FROM pg_extension e JOIN pg_namespace n ON e.extnamespace = n.oid` → `Entity{type:Extension, name:extname, schema:Some(nspname)}`.
3. **enums** — for each enum type (the `list_entities` enum query), `SELECT enumlabel FROM pg_enum WHERE enumtypid = $1 ORDER BY enumsortorder` → `Entity{type:Enum, name:"schema.typname", enum_values:[…]}`.
4. **tables** — for each base table: columns from `information_schema.columns` (`column_name, data_type/udt_name, is_nullable, column_default, is_identity`), PK/unique/FK/check from `pg_constraint` (`contype in ('p','u','f','c')`, `pg_get_constraintdef(oid)` for the human form or decompose via `conkey`/`confrelid`), indexes from `pg_indexes`/`pg_index` (exclude those backing a PK/unique constraint), table/column comments from `pg_description` via `obj_description`/`col_description`. Assemble a `TableDef` and set `entity.table_def`.
5. **views** — `SELECT schemaname, viewname, definition FROM pg_views WHERE schemaname = ANY($selected)` → `Entity{type:View, name:"schema.viewname", writes:vec![definition]}` (matches the `emit_view` contract).

Return the entities (order: schemas, extensions, enums, tables, views). Each implemented as a small private `async fn introspect_<kind>(&self) -> Result<Vec<Entity>>` so they're independently reviewable, then `introspect()` concatenates them.

> This is the largest task. Keep each `introspect_<kind>` focused; map DB types verbatim into `ColumnDef.data_type` (lossless). Constraint decomposition: prefer reconstructing PK/Unique/FK from `pg_constraint` columns so the emitter can re-emit them; CHECK can use `pg_get_constraintdef`-style expression text.

- [ ] **Step 5: Run, verify pass (against a real Postgres)**

Run: `TEST_DATABASE_URL=postgres://… cargo test -p dbd-core introspect_roundtrips_a_table`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/adapter/mod.rs crates/dbd-core/src/adapter/postgres.rs
git commit -m "feat(core): postgres introspection → entities"
```

---

## Task 10: Engine entry point — `reverse_engineer()` orchestration

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs`

- [ ] **Step 1: Write the failing test** (pure — feeds entities directly, no DB)

Append to `reverse.rs` `tests`:

```rust
#[test]
fn plan_from_entities_emits_and_classifies() {
    use crate::entity::{Entity, EntityType, EnumValue};
    let dir = tempfile::tempdir().unwrap();
    let mut en = Entity::new(EntityType::Enum, "shop.status");
    en.enum_values = vec![EnumValue { name: "a".into(), note: None }];
    let entities = vec![Entity::new(EntityType::Schema, "shop"), en];

    let plan = plan_from_entities(dir.path(), &entities, &["shop".into()]);
    let paths: Vec<String> = plan.items.iter().map(|i| i.path.display().to_string()).collect();
    assert!(paths.contains(&"ddl/schema/shop.sql".to_string()));
    assert!(paths.contains(&"ddl/enum/shop/status.sql".to_string()));
    // content was emitted
    let enum_item = plan.items.iter().find(|i| i.path.ends_with("status.sql")).unwrap();
    assert!(enum_item.content.contains("CREATE TYPE"));
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-core reverse::tests::plan_from_entities_emits_and_classifies`
Expected: FAIL — `plan_from_entities` undefined.

- [ ] **Step 3: Implement** in `reverse.rs`:

```rust
/// Emit DDL for each entity and build a write-plan against `root`.
/// Entities whose kind has no emitter (External, Function/Procedure) are skipped.
pub fn plan_from_entities(root: &Path, entities: &[Entity], selected_schemas: &[String]) -> WritePlan {
    let generated: Vec<(PathBuf, String)> = entities
        .iter()
        .filter(|e| MANAGED_KINDS.contains(&e.entity_type))
        .filter_map(|e| crate::emit::emit_entity(e).map(|sql| (entity_path(e), format!("{}\n", sql.trim_end()))))
        .collect();
    build_plan(root, generated, selected_schemas)
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-core reverse::tests::plan_from_entities_emits_and_classifies`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reverse.rs
git commit -m "feat(core): reverse engine — plan_from_entities orchestration"
```

---

## Task 11: CLI — `init --from-db` flags + `merge` command (definitions)

**Files:**
- Modify: `crates/dbd-cli/src/cli.rs`

- [ ] **Step 1: Write the failing test**

In `cli.rs` `tests`, add `"merge"` to the `every_subcommand_parses` list and a focused parse test:

```rust
#[test]
fn init_from_db_and_merge_parse() {
    let init = Cli::try_parse_from(["dbd", "init", "--from-db", "postgres://x", "--version", "2"]);
    assert!(init.is_ok(), "init --from-db: {init:?}");
    let merge = Cli::try_parse_from(["dbd", "merge", "postgres://x", "--dry-run", "--all-schemas"]);
    assert!(merge.is_ok(), "merge: {merge:?}");
}
```

Also add `"merge"` to the `cmds` array in `every_subcommand_parses` — but note `merge` requires a positional conn, so parse it as `["dbd", "merge", "postgres://x"]` (give that command its own line if the loop passes no args).

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p dbd-cli cli::tests::init_from_db_and_merge_parse`
Expected: FAIL — unknown args / `Merge` variant missing.

- [ ] **Step 3: Extend the `Init` variant and add `Merge`** in the `Commands` enum:

```rust
/// Initialize a new dbd project
Init {
    /// Project name (defaults to current directory name, or the DB name with --from-db)
    #[arg(short, long)]
    name: Option<String>,
    /// Target platform
    #[arg(short, long, default_value = "postgres")]
    target: String,
    /// Reverse-engineer the project from a database connection string (or $DATABASE_URL)
    #[arg(long, value_name = "CONN")]
    from_db: Option<String>,
    /// Base project version written to design.yaml
    #[arg(long, default_value_t = 1)]
    version: u32,
    /// Limit to these schemas (repeatable)
    #[arg(long = "schema", value_name = "SCHEMA")]
    schemas: Vec<String>,
    /// Exclude these schemas (repeatable)
    #[arg(long = "exclude-schema", value_name = "SCHEMA")]
    exclude_schemas: Vec<String>,
    /// Include Supabase platform schemas (bypass the denylist)
    #[arg(long)]
    all_schemas: bool,
    /// On conflict, back up existing files to .bak and overwrite
    #[arg(long)]
    force_overwrite: bool,
    /// Print the plan without writing
    #[arg(long)]
    dry_run: bool,
},
/// Sync a database into the current dbd project (reverse-engineer + merge)
Merge {
    /// Database connection string (or $DATABASE_URL)
    conn: Option<String>,
    #[arg(long = "schema", value_name = "SCHEMA")]
    schemas: Vec<String>,
    #[arg(long = "exclude-schema", value_name = "SCHEMA")]
    exclude_schemas: Vec<String>,
    #[arg(long)]
    all_schemas: bool,
    #[arg(long)]
    force_overwrite: bool,
    #[arg(long)]
    dry_run: bool,
},
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p dbd-cli cli::tests`
Expected: PASS (incl. `cli_definition_is_valid`)

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli/src/cli.rs
git commit -m "feat(cli): init --from-db flags + merge command"
```

---

## Task 12: CLI — wire `init --from-db` and `merge` to the engine

**Files:**
- Create: `crates/dbd-cli/src/commands/reverse.rs`
- Modify: `crates/dbd-cli/src/commands/mod.rs`

- [ ] **Step 1: Implement the command handlers**

Create `crates/dbd-cli/src/commands/reverse.rs`:

```rust
use std::path::Path;
use anyhow::{bail, Context, Result};
use dbd_core::reverse::{self, SchemaSelect};

/// Resolve the connection string: explicit arg, else $DATABASE_URL.
fn resolve_conn(arg: Option<&str>) -> Result<String> {
    if let Some(c) = arg { return Ok(c.to_string()); }
    std::env::var("DATABASE_URL")
        .context("no connection given: pass it as an argument or set $DATABASE_URL")
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_init_from_db(
    project_dir: &Path, conn: &str, name: Option<&str>, version: u32,
    sel: SchemaSelect, force: bool, dry_run: bool,
) -> Result<()> {
    if project_dir.join("design.yaml").exists() {
        bail!("design.yaml already exists here — use `dbd merge` to sync a DB into an existing project");
    }
    run(project_dir, conn, name, Some(version), sel, force, dry_run, /*write_config*/ true).await
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_merge(
    project_dir: &Path, conn: Option<&str>, sel: SchemaSelect, force: bool, dry_run: bool,
) -> Result<()> {
    if !project_dir.join("design.yaml").exists() {
        bail!("no design.yaml here — use `dbd init --from-db <conn>` to start a new project");
    }
    let conn = resolve_conn(conn)?;
    run(project_dir, &conn, None, None, sel, force, dry_run, /*write_config*/ false).await
}

#[allow(clippy::too_many_arguments)]
async fn run(
    project_dir: &Path, conn: &str, name: Option<&str>, version: Option<u32>,
    sel: SchemaSelect, force: bool, dry_run: bool, write_config: bool,
) -> Result<()> {
    // 1. connect + introspect
    let mut adapter = dbd_core::connect(conn, "reverse").await
        .context("failed to connect to the database")?;
    let entities = adapter.introspect().await.context("introspection failed")?;

    // 2. select schemas (from the schemas present on the introspected entities)
    let db_schemas: Vec<String> = {
        let mut s: Vec<String> = entities.iter().filter_map(|e| e.schema.clone()).collect();
        s.sort(); s.dedup(); s
    };
    let selected = reverse::select_schemas(&db_schemas, &sel);
    if selected.is_empty() {
        println!("No user schemas to reverse-engineer (after filtering). Nothing to do.");
        return Ok(());
    }
    let kept: Vec<_> = entities.into_iter()
        .filter(|e| e.schema.as_ref().is_none_or(|s| selected.contains(s)))
        .collect();

    // 3. plan
    let plan = reverse::plan_from_entities(project_dir, &kept, &selected);

    // 4. design.yaml (init only, and only if absent)
    if write_config && !dry_run {
        let project = name.map(String::from)
            .unwrap_or_else(|| db_name_from_conn(conn).unwrap_or_else(|| "project".into()));
        let yaml = reverse::design_yaml(&project, "postgresql", &selected, version.unwrap_or(1));
        std::fs::write(project_dir.join("design.yaml"), yaml)?;
    }

    // 5. apply + report
    let report = reverse::apply_plan(project_dir, &plan, force, dry_run)?;
    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "{prefix}{} created · {} unchanged · {} overwritten (.bak) · {} orphan(s) left as-is",
        report.created, report.unchanged, report.overwritten, report.orphans
    );
    for o in &plan.orphans {
        println!("  orphan (no DB entity): {}", o.display());
    }
    // warn about written schemas missing from an existing design.yaml (merge)
    Ok(())
}

/// Parse the database name out of a connection string for the default project name.
fn db_name_from_conn(conn: &str) -> Option<String> {
    let after = conn.rsplit('/').next()?;
    let db = after.split(['?', '#']).next()?;
    if db.is_empty() { None } else { Some(db.to_string()) }
}
```

> NOTE: confirm `dbd_core::connect(url, project)` returns a `Box<dyn DatabaseAdapter>` already connected (or call `.connect()`); match `commands/mod.rs::get_adapter`. If `connect` needs a `&mut` adapter + explicit `.connect()`, mirror that. `is_none_or` requires a recent Rust; if MSRV is older use `map_or(true, …)`.

- [ ] **Step 2: Dispatch** — in `crates/dbd-cli/src/commands/mod.rs`, register `mod reverse;` and handle the variants. For `Commands::Init { from_db: Some(conn), .. }` call `reverse::cmd_init_from_db(...)`; for `from_db: None` keep the existing init behavior; add a `Commands::Merge { .. }` arm calling `reverse::cmd_merge(...)`. Build `SchemaSelect { only: schemas, exclude: exclude_schemas, all: all_schemas }`.

- [ ] **Step 3: Build + clippy**

Run: `cargo build -p dbd-cli && cargo clippy -p dbd-cli --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Manual end-to-end smoke (against a real DB)**

```bash
tmp=$(mktemp -d)
cargo run -q -p dbd-cli -- init --from-db "$TEST_DATABASE_URL" --dry-run > "$tmp/plan.txt"
cat "$tmp/plan.txt"   # expect a create plan; no files written (dry-run)
```
Expected: a plan listing created files; nothing on disk.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli/src/commands/reverse.rs crates/dbd-cli/src/commands/mod.rs
git commit -m "feat(cli): wire init --from-db and merge to the reverse engine"
```

---

## Task 13: Docs + final verification

**Files:**
- Modify: `README.md` / `docs/guide/04-commands.md` (document `init --from-db` + `merge`)

- [ ] **Step 1:** Add a short section to the commands guide documenting `dbd init --from-db <conn>` and `dbd merge <conn>`, the flags (`--schema`/`--exclude-schema`/`--all-schemas`/`--force-overwrite`/`--dry-run`/`--version`), the conflict/.bak/orphan behavior, and that secrets are never written (`$DATABASE_URL`).

- [ ] **Step 2: Full workspace gate** (matches the pre-commit hook + `make bump`):

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all pass, no warnings. (Introspection DB tests skip without `TEST_DATABASE_URL`.)

- [ ] **Step 3: Commit**

```bash
git add README.md docs/guide/04-commands.md
git commit -m "docs: document dbd init --from-db and merge"
```

---

## Release

After all tasks: push `main`, then `make bump minor` (v0.4.11 → **v0.5.0**) — it re-runs the gate, bumps versions, commits, tags, pushes.

---

## Self-review notes (author)

- **Spec coverage:** commands (T11/T12), shared engine (T4–T10), pg/supabase introspection + Supabase denylist (T4, T9), data-model entity coverage incl. indexes (T2/T9), DDL emitter + round-trip (T1–T3), write-plan create/skip/conflict/orphan + `.bak` + dry-run + no-delete (T6/T7), `$DATABASE_URL` in config (T8), `--version` default 1 (T8/T11), errors/zero-schemas (T12), testing (each task), versioning (Release). Covered.
- **Confirm-at-implementation flags** (deliberate, not placeholders — they pin behavior but ask the worker to match an existing API exactly): the parser entry name (T2), `DbdError` variant (T7), `PostgresAdapter::new` + `connect` shape (T9/T12), the view-body carrier choice `entity.writes` (T3). Each names the exact file to check.
- **Type consistency:** `SchemaSelect`, `WritePlan`/`PlanItem`/`FileAction`, `Report`, `entity_path`, `MANAGED_KINDS`, `emit_entity`, `plan_from_entities`, `design_yaml`, `select_schemas` are defined once and reused with consistent signatures across tasks.
