# Materialized View Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a first-class `materialized_view` entity type to dbd — folder discovery, parsing, emission, Postgres introspection, and pg_cron–driven scheduled refresh (global default + per-view overrides) declared in `design.yaml`, plus an on-demand `dbd refresh` command.

**Architecture:** Materialized views reuse the existing view machinery (body in `entity.writes[0]`) and table machinery (trailing `CREATE INDEX` statements → `entity.table_def.indexes`). A new `EntityType::MaterializedView` variant threads through discovery, partition/apply-order, emit, and introspect. Refresh is handled in-database: dbd owns pg_cron jobs named `dbd:refresh:<schema>.<name>`, synced on apply/reconcile from a new `materialized_views` config block. An on-demand `dbd refresh` command issues `REFRESH MATERIALIZED VIEW [CONCURRENTLY]`.

**Tech Stack:** Rust (edition 2024), sqlx (Postgres, PG17+), sqlparser, serde/serde_yaml, indexmap, clap. Spec: `docs/superpowers/specs/2026-08-03-materialized-views-design.md`.

**Test/build commands:** `cargo test -p dbd-core <name>` for a single test, `cargo test` for the crate, `cargo clippy --all-targets`. The pre-commit hook runs the full suite + clippy on every `git commit`.

---

## File Structure

Files created/modified, by responsibility:

- `crates/dbd-core/src/entity.rs` — add `EntityType::MaterializedView`; `TYPES_WITH_SCHEMA`; `from_folder_name`; new `folder_name()` method (folder ≠ tag for matview).
- `crates/dbd-core/src/parser/mod.rs` — `COMMENT ON MATERIALIZED VIEW` regex fix; `CREATE MATERIALIZED VIEW` extraction handling.
- `crates/dbd-core/src/emit.rs` — `emit_matview`; wire into `emit_entity`.
- `crates/dbd-core/src/design.rs` — `partition_entities` bucket + apply-order slot (matviews after views).
- `crates/dbd-core/src/reverse.rs` — folder mapping via `folder_name()`; add to `MANAGED_KINDS`.
- `crates/dbd-core/src/config.rs` — `MaterializedViewsConfig`, `MatviewOptions`, `MatviewOverride`, and a `resolve()` helper for effective per-view settings.
- `crates/dbd-core/src/adapter/postgres.rs` — `introspect_matviews`; pg_cron job sync (`sync_refresh_jobs`); `refresh_matview`.
- `crates/dbd-core/src/adapter/mod.rs` — trait default methods `sync_refresh_jobs` and `refresh_matview` (no-op/error defaults).
- `crates/dbd-core/src/adapter/sqlite.rs`, `adapter/convex.rs` — error on matview apply.
- `crates/dbd-core/src/reconcile.rs` — drop+recreate matview on definition/index drift; call cron sync.
- `crates/dbd-core/src/scope.rs` — include matviews in scope resolution.
- `crates/dbd-cli/src/cli.rs` — `Refresh` subcommand.
- `crates/dbd-cli/src/commands/mod.rs` — dispatch `Commands::Refresh`.
- `crates/dbd-cli/src/commands/schema.rs` — `cmd_refresh`; matview validations in `cmd_inspect`.
- `tests/fixtures/` + `crates/dbd-core/tests/` — fixtures and integration coverage.
- `README.md` — document matviews + `dbd refresh`.

---

## Task 1: `EntityType::MaterializedView` + folder mapping

**Files:**
- Modify: `crates/dbd-core/src/entity.rs`
- Test: `crates/dbd-core/src/entity.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `entity.rs`:

```rust
#[test]
fn entity_type_from_folder_name_matview() {
    assert_eq!(EntityType::from_folder_name("materialized_view"), Some(EntityType::MaterializedView));
    assert_eq!(EntityType::from_folder_name("materialized_views"), Some(EntityType::MaterializedView));
    assert_eq!(EntityType::from_folder_name("matview"), Some(EntityType::MaterializedView));
}

#[test]
fn matview_has_schema_and_folder_name() {
    assert!(EntityType::MaterializedView.has_schema());
    assert_eq!(EntityType::MaterializedView.folder_name(), "materialized_view");
    assert_eq!(EntityType::Table.folder_name(), "table");
}

#[test]
fn entity_from_matview_file() {
    let e = Entity::from_file(Path::new("ddl/materialized_view/analytics/daily_sales.ddl"));
    assert_eq!(e.entity_type, EntityType::MaterializedView);
    assert_eq!(e.name, "analytics.daily_sales");
    assert_eq!(e.schema, Some("analytics".to_string()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core matview -- --list` then `cargo test -p dbd-core entity_type_from_folder_name_matview`
Expected: FAIL — `no variant named MaterializedView` / `no method folder_name`.

- [ ] **Step 3: Implement**

In the `EntityType` enum add the variant (after `View`):

```rust
    View,
    MaterializedView,
```

Add to `TYPES_WITH_SCHEMA`:

```rust
pub const TYPES_WITH_SCHEMA: &[EntityType] = &[
    EntityType::Sequence,
    EntityType::Enum,
    EntityType::Table,
    EntityType::View,
    EntityType::MaterializedView,
    EntityType::Function,
    EntityType::Procedure,
];
```

Add match arms in `from_folder_name` (after the `view` arm):

```rust
            "materialized_view" | "materialized_views" | "matview" | "matviews" => {
                Some(Self::MaterializedView)
            }
```

Add a `folder_name` method (the derive-based `tag()` would produce `"materializedview"`, which is not the on-disk folder — keep them separate):

```rust
    /// On-disk DDL folder name for this type (e.g. `materialized_view`).
    /// Differs from `tag()` only where the lowercased variant name is not a
    /// readable folder (currently just `MaterializedView`).
    pub fn folder_name(&self) -> String {
        match self {
            EntityType::MaterializedView => "materialized_view".to_string(),
            other => other.tag(),
        }
    }
```

Note: `#[serde(rename_all = "lowercase")]` serializes the variant as `materializedview` in snapshots/JSON — that is internal and consistent, so leave it. Folder/CLI surfaces use `folder_name()`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core entity::tests`
Expected: PASS (all entity tests, including the 3 new ones).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/entity.rs
git commit -m "feat(entity): add MaterializedView entity type + folder_name mapping"
```

---

## Task 2: Parser — accept `COMMENT ON MATERIALIZED VIEW`

**Files:**
- Modify: `crates/dbd-core/src/parser/mod.rs:53-55`
- Test: `crates/dbd-core/src/parser/mod.rs` (inline tests)

- [ ] **Step 1: Write failing test**

Add to the parser test module (find the existing `#[cfg(test)] mod tests` in `parser/mod.rs`; if none, add one):

```rust
#[test]
fn comment_on_materialized_view_is_stripped() {
    let sql = "COMMENT ON MATERIALIZED VIEW analytics.daily_sales IS 'daily rollup';";
    let cleaned = super::preprocess_sql(sql);
    assert!(!cleaned.to_lowercase().contains("comment on materialized view"),
        "expected COMMENT ON MATERIALIZED VIEW to be stripped, got: {cleaned}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core comment_on_materialized_view_is_stripped`
Expected: FAIL — the regex leaves the statement intact.

- [ ] **Step 3: Implement**

Edit the regex at `parser/mod.rs:53-54` to add `materialized\s+view` to the alternation:

```rust
        let re = regex::Regex::new(
            r"(?is)\bcomment\s+on\s+(?:materialized\s+view|view|function|procedure|trigger|index|schema|extension|type)\s+\S+\s+is\s+'[^']*(?:''[^']*)*'\s*;"
        ).unwrap();
```

(Order matters: `materialized\s+view` must precede `view` so the longer alternative wins.)

- [ ] **Step 4: Run test**

Run: `cargo test -p dbd-core comment_on_materialized_view_is_stripped`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/parser/mod.rs
git commit -m "fix(parser): accept COMMENT ON MATERIALIZED VIEW"
```

---

## Task 3: Parser — extract `CREATE MATERIALIZED VIEW`

**Files:**
- Modify: `crates/dbd-core/src/parser/mod.rs`
- Test: `crates/dbd-core/src/parser/mod.rs` (inline tests)

- [ ] **Step 1: Probe sqlparser behaviour first**

Run this one-off check (do NOT commit it) to learn whether the PG dialect already parses matviews:

```bash
cat > /tmp/mvprobe.rs <<'EOF'
fn main() {
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;
    let sql = "CREATE MATERIALIZED VIEW analytics.daily AS SELECT 1 AS x WITH DATA;";
    match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
        Ok(ast) => println!("PARSED OK: {ast:?}"),
        Err(e) => println!("PARSE ERR: {e}"),
    }
}
EOF
echo "Inspect the sqlparser version in Cargo.lock, then reason about the result."
grep -A1 'name = "sqlparser"' Cargo.lock | head -2
```

If `CREATE MATERIALIZED VIEW` parses (recent sqlparser versions support it), skip the rewrite and only add the test in Step 3 asserting body extraction. If it errors, add the preprocess rewrite below.

- [ ] **Step 2: Write failing test**

Add to `parser/mod.rs` tests:

```rust
#[test]
fn parses_create_materialized_view_body_and_index() {
    let sql = "CREATE MATERIALIZED VIEW analytics.daily_sales AS \
               SELECT date_trunc('day', created_at) AS day, sum(total) AS revenue \
               FROM shop.orders GROUP BY 1 WITH DATA;\n\
               CREATE UNIQUE INDEX daily_sales_day_uidx ON analytics.daily_sales(day);";
    // parse_ddl is the crate's entry point used for view/table extraction.
    let parsed = super::parse_ddl(sql).expect("matview DDL should parse");
    assert!(parsed.body.to_lowercase().contains("from shop.orders"),
        "expected the SELECT body to be captured, got: {:?}", parsed.body);
    assert_eq!(parsed.indexes.len(), 1, "expected one unique index");
    assert!(parsed.indexes[0].unique);
}
```

Note: replace `super::parse_ddl` / `parsed.body` / `parsed.indexes` with the crate's actual parse entry point and result shape (inspect the existing view+table parse path in `parser/mod.rs` and `parser/tables.rs`; views populate `writes[0]`, tables populate `TableDef.indexes`). Keep the assertions (body captured, one unique index) — only the accessor names change.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p dbd-core parses_create_materialized_view_body_and_index`
Expected: FAIL (parse error or empty body/indexes).

- [ ] **Step 4: Implement**

Only if Step 1 showed a parse error, add a workaround block in `preprocess_sql` (next to the `PROCEDURE → FUNCTION` block, `parser/mod.rs:61`):

```rust
    // WORKAROUND: sqlparser-create-materialized-view
    // Limitation: some sqlparser versions do not parse CREATE MATERIALIZED VIEW.
    // Fix:        Rewrite to CREATE VIEW for AST extraction only. The emitter
    //             writes the real keyword; we only need the SELECT body + reads.
    //             Also drop the trailing `WITH [NO] DATA` clause, which is not
    //             part of a plain CREATE VIEW.
    {
        let re = regex::Regex::new(r"(?is)\bcreate\s+materialized\s+view\b").unwrap();
        if re.is_match(&result) {
            result = std::borrow::Cow::Owned(re.replace_all(&result, "CREATE VIEW").to_string());
            let with_data = regex::Regex::new(r"(?is)\s+with\s+(?:no\s+)?data\s*(;|$)").unwrap();
            result = std::borrow::Cow::Owned(with_data.replace_all(&result, "$1").to_string());
        }
    }
```

Wire the matview body into `writes[0]` wherever views set theirs: locate the branch in `parser/mod.rs` that assigns a view's definition and ensure `EntityType::MaterializedView` takes the same path (both carry body in `writes[0]`; matviews additionally carry `table_def.indexes` from the trailing `CREATE INDEX` statements, which the table-index extractor already handles).

- [ ] **Step 5: Run test**

Run: `cargo test -p dbd-core parses_create_materialized_view`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/parser/mod.rs
git commit -m "feat(parser): extract CREATE MATERIALIZED VIEW body + indexes"
```

---

## Task 4: Emit — `emit_matview`

**Files:**
- Modify: `crates/dbd-core/src/emit.rs` (add `emit_matview`, wire into `emit_entity`)
- Test: `crates/dbd-core/src/emit.rs` (inline tests)

- [ ] **Step 1: Write failing test**

Add to the `tests` module in `emit.rs`:

```rust
#[test]
fn emits_materialized_view() {
    let mut e = Entity::new(EntityType::MaterializedView, "analytics.daily_sales");
    e.writes = vec!["SELECT 1 AS x".to_string()];
    let sql = emit_matview(&e);
    assert_eq!(
        sql,
        "CREATE MATERIALIZED VIEW \"analytics\".\"daily_sales\" AS SELECT 1 AS x WITH DATA;"
    );
}

#[test]
fn emit_entity_dispatches_matview() {
    let mut e = Entity::new(EntityType::MaterializedView, "analytics.daily_sales");
    e.writes = vec!["SELECT 1 AS x".to_string()];
    let sql = emit_entity(&e).expect("matview should emit");
    assert!(sql.starts_with("CREATE MATERIALIZED VIEW"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core emits_materialized_view emit_entity_dispatches_matview`
Expected: FAIL — `emit_matview` undefined; `emit_entity` returns `None` for matview.

- [ ] **Step 3: Implement**

Add after `emit_view` (`emit.rs:247`):

```rust
/// `CREATE MATERIALIZED VIEW "schema"."name" AS <definition> WITH DATA;`
/// The body is carried in `entity.writes[0]` (same contract as `emit_view`).
/// Index statements, if any, are emitted separately by the caller/apply path
/// from `entity.table_def.indexes`.
pub fn emit_matview(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let body = entity.writes.first().map(String::as_str).unwrap_or("SELECT 1");
    let body = body.trim().trim_end_matches(';');
    format!("CREATE MATERIALIZED VIEW {}.{} AS {body} WITH DATA;", q(schema), q(name))
}
```

Wire into `emit_entity` (`emit.rs:287`), after the `View` arm:

```rust
        EntityType::View => Some(emit_view(entity)),
        EntityType::MaterializedView => Some(emit_matview(entity)),
```

If the entity carries indexes (`entity.table_def`), append them using the same index-emit helper `emit_table` uses. Check `emit_table` for the existing index-rendering function; reuse it so a matview with indexes emits `CREATE MATERIALIZED VIEW …; CREATE [UNIQUE] INDEX …;`. Add a test asserting the index line appears when `table_def.indexes` is non-empty.

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core emit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/emit.rs
git commit -m "feat(emit): emit_matview (CREATE MATERIALIZED VIEW ... WITH DATA)"
```

---

## Task 5: Apply order — partition + slot after views

**Files:**
- Modify: `crates/dbd-core/src/design.rs:582-594` (apply concat), `design.rs:1869-1908` (`partition_entities`)
- Test: `crates/dbd-core/src/design.rs` (inline tests)

- [ ] **Step 1: Write failing test**

Add to the `tests` module in `design.rs`:

```rust
#[test]
fn matview_applied_after_views_before_functions() {
    use crate::entity::EntityType;
    let ents = vec![
        Entity::new(EntityType::Function, "app.f"),
        Entity::new(EntityType::MaterializedView, "app.mv"),
        Entity::new(EntityType::View, "app.v"),
        Entity::new(EntityType::Table, "app.t"),
    ];
    let ordered = order_entities_for_test(ents); // small test wrapper, see Step 3
    let pos = |name: &str| ordered.iter().position(|e| e.name == name).unwrap();
    assert!(pos("app.t") < pos("app.v"));
    assert!(pos("app.v") < pos("app.mv"));
    assert!(pos("app.mv") < pos("app.f"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core matview_applied_after_views_before_functions`
Expected: FAIL — `MaterializedView` currently falls into the `_ => tables` bucket (applied with tables, before views) and `order_entities_for_test` does not exist.

- [ ] **Step 3: Implement**

Extend `partition_entities` to a 10-tuple. Add the field and match arm:

```rust
    let mut views = Vec::new();
    let mut matviews = Vec::new();
    let mut functions = Vec::new(); // functions + procedures
```

```rust
            EntityType::View => views.push(entity),
            EntityType::MaterializedView => matviews.push(entity),
```

Return `(schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals)` and update the tuple type signature (add one `Vec<Entity>`).

Update the destructure + sort + concat in `from_config` (`design.rs:582-594`):

```rust
        let (schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals) =
            partition_entities(entities);
        let sorted_roles = dependency::sort_by_dependencies(&roles);
        let sorted_enums = dependency::sort_by_dependencies(&enums);
        let sorted_tables = dependency::sort_by_dependencies(&tables);
        let sorted_views = dependency::sort_by_dependencies(&views);
        let sorted_matviews = dependency::sort_by_dependencies(&matviews);
        let sorted_functions = dependency::sort_by_dependencies(&functions);

        let entities = [
            schemas, extensions, sorted_roles, sequences, sorted_enums,
            sorted_tables, sorted_views, sorted_matviews, sorted_functions, externals,
        ]
        .concat();
```

Update the apply-order comment on `design.rs:579` to include `→ materialized views` after `views`.

For the test, add a small `#[cfg(test)]` helper in the same module that reuses `partition_entities` + the same concat, so the ordering logic is exercised without a full `Design::from_config`:

```rust
#[cfg(test)]
fn order_entities_for_test(entities: Vec<Entity>) -> Vec<Entity> {
    let (schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals) =
        partition_entities(entities);
    [schemas, extensions, roles, sequences, enums, tables, views, matviews, functions, externals].concat()
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core design::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(apply): order materialized views after views, before functions"
```

---

## Task 6: Reverse-engineering folder mapping

**Files:**
- Modify: `crates/dbd-core/src/reverse.rs:127` (use `folder_name()`), `reverse.rs:139-143` (`MANAGED_KINDS`)
- Test: `crates/dbd-core/src/reverse.rs` (inline tests)

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn matview_reverse_path_uses_underscored_folder() {
    let e = Entity::new(EntityType::MaterializedView, "analytics.daily_sales");
    let p = entity_path(&e);
    assert_eq!(p, std::path::PathBuf::from("ddl/materialized_view/analytics/daily_sales.ddl"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core matview_reverse_path_uses_underscored_folder`
Expected: FAIL — `entity_path` uses `tag()` → `ddl/materializedview/...`.

- [ ] **Step 3: Implement**

At `reverse.rs:127` swap `tag()` for `folder_name()`:

```rust
    let kind = entity.entity_type.folder_name(); // "table", "view", "materialized_view", ...
```

Add `EntityType::MaterializedView` to `MANAGED_KINDS` (`reverse.rs:139-143`), after `EntityType::View`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core reverse`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/reverse.rs
git commit -m "feat(reverse): map materialized views to ddl/materialized_view/ folder"
```

---

## Task 7: `materialized_views` config block + resolution

**Files:**
- Modify: `crates/dbd-core/src/config.rs` (add field to `DesignConfig`; new structs; `resolve` helper)
- Test: `crates/dbd-core/src/config.rs` (inline tests)

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn resolves_matview_refresh_settings() {
    let yaml = r#"
project:
  name: t
materialized_views:
  options:
    refresh: "0 2 * * *"
    concurrently: true
  overrides:
    analytics.top_products:
      refresh: "*/30 * * * *"
    analytics.realtime:
      concurrently: false
"#;
    let cfg: DesignConfig = serde_yaml::from_str(yaml).unwrap();
    let mv = &cfg.materialized_views;

    // global default
    let d = mv.resolve("analytics.daily_sales");
    assert_eq!(d.refresh.as_deref(), Some("0 2 * * *"));
    assert!(d.concurrently);

    // schedule override
    let t = mv.resolve("analytics.top_products");
    assert_eq!(t.refresh.as_deref(), Some("*/30 * * * *"));
    assert!(t.concurrently); // inherited

    // concurrently override only
    let r = mv.resolve("analytics.realtime");
    assert_eq!(r.refresh.as_deref(), Some("0 2 * * *")); // inherited
    assert!(!r.concurrently);
}

#[test]
fn matview_without_global_schedule_has_no_refresh() {
    let yaml = "project:\n  name: t\n";
    let cfg: DesignConfig = serde_yaml::from_str(yaml).unwrap();
    let d = cfg.materialized_views.resolve("analytics.x");
    assert!(d.refresh.is_none());
    assert!(!d.concurrently);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core resolves_matview_refresh_settings matview_without_global_schedule_has_no_refresh`
Expected: FAIL — `materialized_views` field and structs don't exist.

- [ ] **Step 3: Implement**

Add the field to `DesignConfig` (after `export`):

```rust
    #[serde(default)]
    pub materialized_views: MaterializedViewsConfig,
```

Add the structs (near the Import section, ~`config.rs:222`):

```rust
// ── Materialized views ───────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct MaterializedViewsConfig {
    #[serde(default)]
    pub options: MatviewOptions,
    #[serde(default)]
    pub overrides: HashMap<String, MatviewOverride>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatviewOptions {
    /// Shared cron schedule applied to every matview (pg_cron 5-field expression).
    #[serde(default)]
    pub refresh: Option<String>,
    /// Shared default for REFRESH ... CONCURRENTLY.
    #[serde(default)]
    pub concurrently: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatviewOverride {
    #[serde(default)]
    pub refresh: Option<String>,
    #[serde(default)]
    pub concurrently: Option<bool>,
}

/// Effective, resolved refresh settings for a single matview.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMatview {
    pub refresh: Option<String>,
    pub concurrently: bool,
}

impl MaterializedViewsConfig {
    /// Resolve effective settings for a matview by qualified name
    /// (`schema.name`): overrides overlay the global `options`.
    pub fn resolve(&self, name: &str) -> ResolvedMatview {
        let ov = self.overrides.get(name);
        ResolvedMatview {
            refresh: ov
                .and_then(|o| o.refresh.clone())
                .or_else(|| self.options.refresh.clone()),
            concurrently: ov
                .and_then(|o| o.concurrently)
                .unwrap_or(self.options.concurrently),
        }
    }
}
```

Confirm `HashMap` is imported in `config.rs` (it is — used by `ImportTableEntry`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core matview`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/config.rs
git commit -m "feat(config): materialized_views block with global+override resolution"
```

---

## Task 8: Postgres introspection — `introspect_matviews`

**Files:**
- Modify: `crates/dbd-core/src/adapter/postgres.rs` (add method; call it in `introspect`)
- Test: `crates/dbd-core/tests/` integration test guarded by a live/embedded PG (mirror the existing view introspection test)

- [ ] **Step 1: Write failing test**

Find the existing Postgres introspection integration test (search: `grep -rn "introspect" crates/dbd-core/tests/`). Add, in the same file and using the same DB-guard pattern:

```rust
#[tokio::test]
async fn introspects_materialized_view() {
    // (Use the same skip-if-no-DB guard the neighbouring tests use.)
    let adapter = /* connect as neighbouring tests do */;
    adapter.execute_script(
        "CREATE SCHEMA IF NOT EXISTS mvtest; \
         CREATE MATERIALIZED VIEW mvtest.mv AS SELECT 1 AS x WITH DATA; \
         CREATE UNIQUE INDEX mv_x_uidx ON mvtest.mv(x);"
    ).await.unwrap();

    let ents = adapter.introspect().await.unwrap();
    let mv = ents.iter().find(|e| e.name == "mvtest.mv")
        .expect("materialized view should be introspected");
    assert_eq!(mv.entity_type, EntityType::MaterializedView);
    assert!(mv.writes[0].to_lowercase().contains("select"));
    assert_eq!(mv.table_def.as_ref().map(|t| t.indexes.len()), Some(1));

    adapter.execute_script("DROP SCHEMA mvtest CASCADE;").await.unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core introspects_materialized_view -- --nocapture`
Expected: FAIL — matview not returned (or `entity_type` mismatch).

- [ ] **Step 3: Implement**

Add next to `introspect_views` (`postgres.rs:788`):

```rust
    async fn introspect_matviews(&self) -> crate::error::Result<Vec<Entity>> {
        let ns_filter = Self::schema_filter_column("schemaname");
        let sql = format!(
            "SELECT schemaname, matviewname, definition \
             FROM pg_matviews \
             WHERE {ns_filter} \
             ORDER BY schemaname, matviewname"
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DbdError::Config(format!("introspect_matviews failed: {e}")))?;

        let mut entities = Vec::with_capacity(rows.len());
        for row in &rows {
            let schema: String = row.get("schemaname");
            let name: String = row.get("matviewname");
            let definition: String = row.get("definition");
            let mut e = Entity::new(EntityType::MaterializedView, &format!("{schema}.{name}"));
            e.writes = vec![definition];
            // Attach indexes (matviews live in pg_indexes like tables).
            e.table_def = Some(self.introspect_indexes_for(&schema, &name).await?);
            entities.push(e);
        }
        Ok(entities)
    }
```

For `introspect_indexes_for`, reuse whatever the table introspector already uses to read `pg_indexes` into `IndexDef`s (search `pg_indexes` in `postgres.rs`). If the table path builds indexes inline, extract a small helper `introspect_indexes_for(schema, rel) -> Result<TableDef>` (columns empty, indexes populated) and call it from both. Keep the change minimal — a `TableDef { columns: vec![], constraints: vec![], indexes, comments: Default::default() }`.

Register it in `introspect` next to the views call (`postgres.rs:1355`):

```rust
        out.extend(self.introspect_views().await?);
        out.extend(self.introspect_matviews().await?);
```

- [ ] **Step 4: Run test**

Run: `cargo test -p dbd-core introspects_materialized_view`
Expected: PASS (or SKIP if no DB — then verify against a live PG per the mandatory "verify against live data" rule before marking done).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/adapter/postgres.rs crates/dbd-core/tests/
git commit -m "feat(introspect): reverse-engineer materialized views from pg_matviews"
```

---

## Task 9: pg_cron job sync

**Files:**
- Modify: `crates/dbd-core/src/adapter/mod.rs` (trait default methods), `crates/dbd-core/src/adapter/postgres.rs` (impl)
- Test: `crates/dbd-core/src/adapter/postgres.rs` inline (pure SQL-builder test) + integration (job sync) if DB available

- [ ] **Step 1: Write failing test (pure SQL builder)**

Add a pure function `refresh_job_sql(name, schedule, concurrently)` and test it without a DB:

```rust
#[test]
fn builds_cron_schedule_sql_concurrently() {
    let sql = refresh_job_sql("analytics.daily_sales", "0 2 * * *", true);
    assert!(sql.contains("dbd:refresh:analytics.daily_sales"));
    assert!(sql.contains("REFRESH MATERIALIZED VIEW CONCURRENTLY \"analytics\".\"daily_sales\""));
    assert!(sql.contains("'0 2 * * *'"));
}

#[test]
fn builds_cron_schedule_sql_non_concurrent() {
    let sql = refresh_job_sql("analytics.x", "*/5 * * * *", false);
    assert!(sql.contains("REFRESH MATERIALIZED VIEW \"analytics\".\"x\""));
    assert!(!sql.contains("CONCURRENTLY"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core builds_cron_schedule_sql_concurrently builds_cron_schedule_sql_non_concurrent`
Expected: FAIL — `refresh_job_sql` undefined.

- [ ] **Step 3: Implement the builder + sync**

In `postgres.rs` add the pure builder (module-level fn):

```rust
/// Build a `cron.schedule(...)` call that (re)registers a dbd-owned refresh job.
/// `cron.schedule(job_name, schedule, command)` upserts by name, so this is
/// idempotent and also updates an existing job's schedule/command.
fn refresh_job_sql(qualified: &str, schedule: &str, concurrently: bool) -> String {
    let (schema, name) = qualified.split_once('.').unwrap_or(("public", qualified));
    let conc = if concurrently { "CONCURRENTLY " } else { "" };
    let job = format!("dbd:refresh:{qualified}");
    let command = format!("REFRESH MATERIALIZED VIEW {conc}\"{schema}\".\"{name}\"");
    // Escape single quotes for SQL string literals.
    let job_lit = job.replace('\'', "''");
    let sched_lit = schedule.replace('\'', "''");
    let cmd_lit = command.replace('\'', "''");
    format!("SELECT cron.schedule('{job_lit}', '{sched_lit}', '{cmd_lit}');")
}

/// Unschedule any dbd-owned refresh job whose matview is no longer scheduled.
fn unschedule_stale_sql(keep_job_names: &[String]) -> String {
    // keep is a SQL array literal of job names to preserve.
    let keep = keep_job_names
        .iter()
        .map(|n| format!("'{}'", n.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let keep_clause = if keep.is_empty() { String::from("TRUE") } else { format!("jobname <> ALL(ARRAY[{keep}])") };
    format!(
        "SELECT cron.unschedule(jobid) FROM cron.job \
         WHERE jobname LIKE 'dbd:refresh:%' AND {keep_clause};"
    )
}
```

Add trait default methods to `DatabaseAdapter` (`adapter/mod.rs`), so non-PG targets are a no-op:

```rust
    /// Sync pg_cron refresh jobs for the given (qualified_name, ResolvedMatview)
    /// set. Default: no-op (targets without pg_cron).
    async fn sync_refresh_jobs(&self, _jobs: &[(String, crate::config::ResolvedMatview)]) -> Result<()> {
        Ok(())
    }

    /// Refresh one matview now. Default: unsupported.
    async fn refresh_matview(&self, _qualified: &str, _concurrently: bool) -> Result<()> {
        Err(crate::error::DbdError::Config(
            "REFRESH MATERIALIZED VIEW is not supported by this target".into(),
        ))
    }
```

Implement both for the Postgres adapter. `sync_refresh_jobs`:

```rust
    async fn sync_refresh_jobs(&self, jobs: &[(String, ResolvedMatview)]) -> Result<()> {
        let mut keep = Vec::new();
        for (name, r) in jobs {
            if let Some(schedule) = &r.refresh {
                self.execute_script(&refresh_job_sql(name, schedule, r.concurrently)).await?;
                keep.push(format!("dbd:refresh:{name}"));
            }
        }
        self.execute_script(&unschedule_stale_sql(&keep)).await?;
        Ok(())
    }

    async fn refresh_matview(&self, qualified: &str, concurrently: bool) -> Result<()> {
        let (schema, name) = qualified.split_once('.').unwrap_or(("public", qualified));
        let conc = if concurrently { "CONCURRENTLY " } else { "" };
        self.execute_script(&format!(
            "REFRESH MATERIALIZED VIEW {conc}\"{schema}\".\"{name}\";"
        )).await
    }
```

- [ ] **Step 4: Wire the sync into apply**

In `cmd_apply` (`crates/dbd-cli/src/commands/schema.rs:226`), after entities are applied, resolve the matview set and sync jobs:

```rust
    // After entity apply succeeds:
    let mv_jobs: Vec<(String, dbd_core::config::ResolvedMatview)> = design
        .entities
        .iter()
        .filter(|e| e.entity_type == dbd_core::entity::EntityType::MaterializedView)
        .map(|e| (e.name.clone(), design.config.materialized_views.resolve(&e.name)))
        .collect();
    adapter.sync_refresh_jobs(&mv_jobs).await?;
```

(Match the actual variable names for `design`/`adapter` in `cmd_apply`.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p dbd-core builds_cron_schedule_sql`
Expected: PASS. If a live PG with pg_cron is available, run an integration test asserting `cron.job` contains `dbd:refresh:analytics.daily_sales` after apply, and that removing the schedule unschedules it.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/adapter/mod.rs crates/dbd-core/src/adapter/postgres.rs crates/dbd-cli/src/commands/schema.rs
git commit -m "feat(refresh): sync pg_cron refresh jobs on apply (dbd:refresh: prefix)"
```

---

## Task 10: `inspect` validations

**Files:**
- Modify: `crates/dbd-cli/src/commands/schema.rs` (`cmd_inspect`) or the core inspect/validation path it calls
- Test: `crates/dbd-core/src/` inline (prefer validating in a pure core function `validate_materialized_views(design) -> Vec<String>` so it is unit-testable without the CLI)

- [ ] **Step 1: Write failing test**

Add a pure validation function in core (e.g. in `design.rs` or a small `validate.rs`) and test it:

```rust
#[test]
fn matview_concurrently_requires_unique_index() {
    let mut mv = Entity::new(EntityType::MaterializedView, "a.m");
    mv.writes = vec!["SELECT 1".into()];
    mv.table_def = Some(TableDef { columns: vec![], constraints: vec![], indexes: vec![], comments: Default::default() });

    let mut cfg = DesignConfig::default_for_test(); // or construct minimally
    cfg.materialized_views.options.concurrently = true;
    cfg.materialized_views.options.refresh = Some("0 2 * * *".into());

    let errs = validate_materialized_views(&[mv], &cfg, &["pg_cron".to_string()]);
    assert!(errs.iter().any(|e| e.contains("unique index")),
        "expected concurrently-without-unique-index error, got: {errs:?}");
}

#[test]
fn matview_schedule_requires_pg_cron_extension() {
    let mut mv = Entity::new(EntityType::MaterializedView, "a.m");
    mv.writes = vec!["SELECT 1".into()];
    let mut cfg = DesignConfig::default_for_test();
    cfg.materialized_views.options.refresh = Some("0 2 * * *".into());

    let errs = validate_materialized_views(&[mv], &cfg, &[] /* no extensions */);
    assert!(errs.iter().any(|e| e.contains("pg_cron")),
        "expected pg_cron-missing error, got: {errs:?}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core matview_concurrently_requires_unique_index matview_schedule_requires_pg_cron_extension`
Expected: FAIL — `validate_materialized_views` undefined.

- [ ] **Step 3: Implement**

```rust
/// Validate matview refresh config against the resolved entities and the set of
/// installed/declared extensions. Returns human-readable error strings.
pub fn validate_materialized_views(
    entities: &[Entity],
    cfg: &DesignConfig,
    extensions: &[String],
) -> Vec<String> {
    let mut errs = Vec::new();
    let mut any_scheduled = false;
    for e in entities.iter().filter(|e| e.entity_type == EntityType::MaterializedView) {
        let r = cfg.materialized_views.resolve(&e.name);
        if r.refresh.is_some() {
            any_scheduled = true;
        }
        if r.concurrently {
            let has_unique = e
                .table_def
                .as_ref()
                .is_some_and(|t| t.indexes.iter().any(|i| i.unique));
            if !has_unique {
                errs.push(format!(
                    "materialized view {}: concurrently refresh requires a UNIQUE index",
                    e.name
                ));
            }
        }
        if let Some(sched) = &r.refresh
            && !is_valid_cron(sched)
        {
            errs.push(format!("materialized view {}: invalid cron expression '{sched}'", e.name));
        }
    }
    if any_scheduled && !extensions.iter().any(|x| x == "pg_cron") {
        errs.push(
            "materialized view refresh scheduling requires the pg_cron extension \
             (add 'pg_cron' under target.postgres.extensions)".to_string(),
        );
    }
    errs
}

/// Minimal 5-field cron validation (field count + allowed chars). Not a full
/// cron parser — catches obvious mistakes.
fn is_valid_cron(expr: &str) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    fields.len() == 5
        && fields.iter().all(|f| f.chars().all(|c| c.is_ascii_digit() || "*/,-".contains(c)))
}
```

Call `validate_materialized_views` from `cmd_inspect`, pushing the returned strings as errors (use the config's declared `target.postgres.extensions` for the extension list; the offline path uses declared extensions, `--from-db` may use the live catalog). Add `default_for_test()` as a `#[cfg(test)]` helper if `DesignConfig` has no `Default`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core matview`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/ crates/dbd-cli/src/commands/schema.rs
git commit -m "feat(inspect): validate matview concurrently/pg_cron/cron-expression"
```

---

## Task 11: `dbd refresh` command

**Files:**
- Modify: `crates/dbd-cli/src/cli.rs` (add `Refresh` variant), `crates/dbd-cli/src/commands/mod.rs` (dispatch), `crates/dbd-cli/src/commands/schema.rs` (`cmd_refresh`)
- Test: `crates/dbd-cli/src/cli.rs` (arg-parse test) + core-level refresh selection unit test

- [ ] **Step 1: Write failing arg-parse test**

In `cli.rs` tests (mirror the existing `Commands::Import` parse tests, ~`cli.rs:561`):

```rust
#[test]
fn parses_refresh_all() {
    let cli = Cli::try_parse_from(["dbd", "refresh"]).unwrap();
    assert!(matches!(&cli.command, Commands::Refresh { name: None }));
}

#[test]
fn parses_refresh_named() {
    let cli = Cli::try_parse_from(["dbd", "refresh", "-n", "analytics.daily_sales"]).unwrap();
    assert!(matches!(&cli.command, Commands::Refresh { name: Some(n) } if n == "analytics.daily_sales"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-cli parses_refresh_all parses_refresh_named`
Expected: FAIL — no `Refresh` variant.

- [ ] **Step 3: Implement the command**

Add to the `Commands` enum in `cli.rs` (after `Import`):

```rust
    /// Refresh materialized views (REFRESH MATERIALIZED VIEW [CONCURRENTLY])
    Refresh {
        /// Refresh a specific materialized view (or `schema.*` wildcard). Omit to refresh all.
        #[arg(short, long)]
        name: Option<String>,
    },
```

Dispatch in `commands/mod.rs` (next to `Commands::Import`, ~line 46):

```rust
        Commands::Refresh { name } => {
            schema::cmd_refresh(config, project_dir, name.as_deref(), &db, verbosity).await
        }
```

(Match the surrounding arm's exact parameters — connection handle, verbosity, etc.)

Implement `cmd_refresh` in `commands/schema.rs`:

```rust
pub async fn cmd_refresh(
    config: &Path,
    project_dir: &Path,
    name: Option<&str>,
    db: &DbArgs,          // match the type used by cmd_apply/cmd_import
    verbosity: Verbosity,
) -> Result<()> {
    let design = Design::from_config(config, /* env */ "dev")?; // match how cmd_apply loads it
    let adapter = /* connect exactly as cmd_apply does */;

    let selected: Vec<&Entity> = design
        .entities
        .iter()
        .filter(|e| e.entity_type == EntityType::MaterializedView)
        .filter(|e| match name {
            None => true,
            Some(sel) if sel.ends_with(".*") => {
                let schema = sel.trim_end_matches(".*");
                e.schema.as_deref() == Some(schema)
            }
            Some(sel) => e.name == sel,
        })
        .collect();

    if selected.is_empty() {
        // honest-empty: say so rather than silently succeeding
        output::info(verbosity, "No materialized views to refresh.");
        return Ok(());
    }

    for e in selected {
        let r = design.config.materialized_views.resolve(&e.name);
        output::info(verbosity, &format!("Refreshing {} ...", e.name));
        adapter.refresh_matview(&e.name, r.concurrently).await?;
    }
    Ok(())
}
```

Match `output`/`Verbosity`/`DbArgs`/`Design::from_config` signatures to the neighbouring commands. Refresh in `design.entities` order (already dependency-sorted, matviews contiguous).

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-cli parses_refresh`
Expected: PASS. Build the CLI: `cargo build -p dbd-cli` (Expected: compiles).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli/src/cli.rs crates/dbd-cli/src/commands/mod.rs crates/dbd-cli/src/commands/schema.rs
git commit -m "feat(cli): dbd refresh command (all / by name / schema.* wildcard)"
```

---

## Task 12: SQLite & Convex error on matview apply

**Files:**
- Modify: `crates/dbd-core/src/adapter/sqlite.rs`, `crates/dbd-core/src/adapter/convex.rs`
- Test: inline in each adapter (mirror the existing Function/Procedure error tests)

- [ ] **Step 1: Write failing tests**

Find the existing tests asserting SQLite/Convex reject `Function`/`Procedure` (search `grep -rn "not supported\|EntityType::Function" crates/dbd-core/src/adapter/sqlite.rs crates/dbd-core/src/adapter/convex.rs`). Add sibling tests:

```rust
#[tokio::test]
async fn sqlite_rejects_materialized_view() {
    let adapter = /* build in-memory sqlite adapter as neighbours do */;
    let e = Entity::new(EntityType::MaterializedView, "app.mv");
    let err = adapter.apply_entity(&e).await.unwrap_err();
    assert!(err.to_string().to_lowercase().contains("materialized"));
}
```

(Convex: analogous test in `convex.rs`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core sqlite_rejects_materialized_view`
Expected: FAIL — matview falls through to a generic path or is mis-handled.

- [ ] **Step 3: Implement**

In each adapter's `apply_entity` match on `entity_type`, add a `MaterializedView` arm alongside the existing `Function`/`Procedure` error arm, returning the same error style, e.g.:

```rust
        EntityType::MaterializedView => Err(DbdError::Config(
            "SQLite does not support materialized views".into(),
        )),
```

(Convex: `"Convex does not support materialized views".into()`.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p dbd-core rejects_materialized_view`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/adapter/sqlite.rs crates/dbd-core/src/adapter/convex.rs
git commit -m "feat(adapters): SQLite/Convex reject materialized views with clear error"
```

---

## Task 13: Reconcile — drop+recreate on matview drift

**Files:**
- Modify: `crates/dbd-core/src/reconcile.rs`
- Test: `crates/dbd-core/src/reconcile.rs` inline (SQL-generation) + integration if DB available

- [ ] **Step 1: Understand current view handling**

Read `reconcile.rs` around the view/idempotent-reapply path (comment at `reconcile.rs:71`). Matviews cannot `CREATE OR REPLACE`; on definition/index drift they need `DROP MATERIALIZED VIEW <name> CASCADE;` then recreate (from `emit_matview` + indexes), which repopulates. On no drift, do nothing.

- [ ] **Step 2: Write failing test**

Add a pure helper `matview_reconcile_sql(entity) -> String` (drop-if-exists + recreate) and test it:

```rust
#[test]
fn matview_reconcile_drops_then_recreates() {
    let mut e = Entity::new(EntityType::MaterializedView, "a.m");
    e.writes = vec!["SELECT 1 AS x".into()];
    let sql = matview_reconcile_sql(&e);
    assert!(sql.contains("DROP MATERIALIZED VIEW IF EXISTS \"a\".\"m\" CASCADE;"));
    assert!(sql.contains("CREATE MATERIALIZED VIEW \"a\".\"m\" AS SELECT 1 AS x WITH DATA;"));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p dbd-core matview_reconcile_drops_then_recreates`
Expected: FAIL — helper undefined.

- [ ] **Step 4: Implement**

```rust
/// Reconcile SQL for a materialized view: drop then recreate (matviews cannot
/// be CREATE OR REPLACE'd; recreate repopulates). Includes index statements.
fn matview_reconcile_sql(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = entity.name.rsplit('.').next().unwrap_or(&entity.name);
    let create = crate::emit::emit_entity(entity).unwrap_or_default();
    format!("DROP MATERIALIZED VIEW IF EXISTS \"{schema}\".\"{name}\" CASCADE;\n{create}")
}
```

Integrate into the reconcile flow: when a matview's live definition differs from the design (compare normalized `writes[0]` and index set from `introspect_matviews`), run `matview_reconcile_sql`; otherwise skip. After reconciling entities, call `sync_refresh_jobs` (as in Task 9 for apply) so schedules track the reconciled set.

- [ ] **Step 5: Run test**

Run: `cargo test -p dbd-core matview_reconcile`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/reconcile.rs
git commit -m "feat(reconcile): drop+recreate materialized views on drift; sync cron jobs"
```

---

## Task 14: Scope resolution includes matviews

**Files:**
- Modify: `crates/dbd-core/src/scope.rs:43` (add `EntityType::MaterializedView` to the managed-type list)
- Test: `crates/dbd-core/src/scope.rs` inline

- [ ] **Step 1: Write failing test**

Mirror an existing scope test that checks a `schema.*` wildcard includes views; add a matview to the fixture set and assert it is selected:

```rust
#[test]
fn scope_wildcard_includes_materialized_views() {
    let ents = vec![
        Entity::new(EntityType::Table, "analytics.orders"),
        Entity::new(EntityType::MaterializedView, "analytics.daily_sales"),
    ];
    let selected = resolve_scope_names(&ents, &["analytics.*".to_string()]); // match real fn
    assert!(selected.iter().any(|n| n == "analytics.daily_sales"));
}
```

(Replace `resolve_scope_names` with the actual scope-resolution entry point in `scope.rs`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core scope_wildcard_includes_materialized_views`
Expected: FAIL if matviews are excluded from the scope-eligible type set at `scope.rs:43`.

- [ ] **Step 3: Implement**

Add `EntityType::MaterializedView` to the type list at `scope.rs:43` (the `|`-chain that currently includes `View`).

- [ ] **Step 4: Run test**

Run: `cargo test -p dbd-core scope`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/scope.rs
git commit -m "feat(scope): include materialized views in scope resolution"
```

---

## Task 15: Snapshot handling

**Files:**
- Modify: `crates/dbd-core/src/snapshot.rs` (only if views are included in snapshots)
- Test: `crates/dbd-core/src/snapshot.rs` inline

- [ ] **Step 1: Determine current view treatment**

Read the note at `snapshot.rs:1794` ("View entity excluded from snapshot"). Materialized views must follow the **same** treatment as views for consistency — if views are excluded, exclude matviews; if included, include them.

- [ ] **Step 2: Write the matching test**

Add a test mirroring the existing view snapshot test (`snapshot.rs:1797`), substituting `EntityType::MaterializedView`, asserting the same inclusion/exclusion outcome as views.

- [ ] **Step 3: Run test to verify current behaviour**

Run: `cargo test -p dbd-core snapshot`
Expected: FAIL only if matviews are handled differently from views.

- [ ] **Step 4: Implement**

Wherever `EntityType::View` is special-cased in `snapshot.rs`, add `| EntityType::MaterializedView` so both behave identically.

- [ ] **Step 5: Run test**

Run: `cargo test -p dbd-core snapshot`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/snapshot.rs
git commit -m "feat(snapshot): treat materialized views like views"
```

---

## Task 16: Fixtures + end-to-end integration test

**Files:**
- Create: `tests/fixtures/ddl/materialized_view/config/genders_mv.ddl`
- Modify: `tests/fixtures/design.yaml` (add a `materialized_views` block)
- Modify: `crates/dbd-core/src/init.rs:175` (gitkeep count — a matview folder may be scaffolded)
- Test: `crates/dbd-core/tests/integration_test.rs` (discovery + emit round-trip, no DB)

- [ ] **Step 1: Add the fixture DDL**

Create `tests/fixtures/ddl/materialized_view/config/genders_mv.ddl`:

```sql
create materialized view genders_mv as
select id, name from config.genders
with data;

create unique index genders_mv_id_uidx on genders_mv(id);
```

- [ ] **Step 2: Add design.yaml block** (edit `tests/fixtures/design.yaml`)

```yaml
materialized_views:
  options:
    refresh: "0 3 * * *"
    concurrently: true
  overrides:
    config.genders_mv:
      refresh: "*/15 * * * *"
```

- [ ] **Step 3: Write failing integration test**

In `crates/dbd-core/tests/integration_test.rs`:

```rust
#[test]
fn discovers_and_emits_materialized_view_from_fixture() {
    let design = load_fixture_design(); // use the helper other tests use
    let mv = design.entities.iter()
        .find(|e| e.name == "config.genders_mv")
        .expect("matview discovered from ddl/materialized_view/");
    assert_eq!(mv.entity_type, dbd_core::entity::EntityType::MaterializedView);

    let sql = dbd_core::emit::emit_entity(mv).expect("emits");
    assert!(sql.contains("CREATE MATERIALIZED VIEW"));

    // resolution honours the per-view override
    let r = design.config.materialized_views.resolve("config.genders_mv");
    assert_eq!(r.refresh.as_deref(), Some("*/15 * * * *"));
    assert!(r.concurrently); // inherited from options
}
```

- [ ] **Step 4: Run test to verify it fails, then passes**

Run: `cargo test -p dbd-core discovers_and_emits_materialized_view_from_fixture`
Expected: initially FAIL if any wiring is incomplete; PASS once Tasks 1–7 are in. If `init.rs:175` asserts a gitkeep count, update it to include the new `materialized_view` folder and adjust the `6` accordingly.

- [ ] **Step 5: Full suite + clippy**

Run: `cargo test && cargo clippy --all-targets`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures crates/dbd-core/tests/integration_test.rs crates/dbd-core/src/init.rs
git commit -m "test(matview): fixtures + end-to-end discovery/emit/resolve"
```

---

## Task 17: Documentation

**Files:**
- Modify: `README.md` (commands table + a short matview section)

- [ ] **Step 1: Update the commands table**

Add a row to the Commands table in `README.md`:

```markdown
| `dbd refresh` | Refresh materialized views now (`REFRESH MATERIALIZED VIEW [CONCURRENTLY]`); scheduled refresh is managed via pg_cron |
```

- [ ] **Step 2: Add a Materialized Views section**

Document: the `ddl/materialized_view/<schema>/<name>.ddl` layout; that indexes are declared as trailing `CREATE [UNIQUE] INDEX` statements; the `materialized_views` design.yaml block (global `options` + `overrides`); the pg_cron requirement for scheduling; that `concurrently: true` needs a unique index; and that SQLite/Convex don't support matviews. Keep it consistent with the existing "Adapter notes" tone.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): document materialized views + dbd refresh"
```

---

## Self-Review

**Spec coverage:**
- §1 Entity model & discovery → Tasks 1, 6, 16.
- §2 Parser (COMMENT ON, CREATE MATERIALIZED VIEW) → Tasks 2, 3.
- §3 Emission & introspection → Tasks 4, 8.
- §4 Apply/diff/reconcile → Tasks 5, 13.
- §5 Scheduled refresh via pg_cron (global+override, ownership, validation) → Tasks 7, 9, 10.
- §6 On-demand refresh command → Task 11.
- §7 Targets (SQLite/Convex error) → Task 12.
- Scope + snapshot (touchpoints) → Tasks 14, 15.
- Docs → Task 17.

**Placeholder scan:** The parser/introspection/CLI tasks name real accessors to confirm against the codebase (e.g. `parse_ddl`, `DbArgs`, `load_fixture_design`) because those exact identifiers must be read from neighbouring code before use — each such note states the concrete assertion to keep and only flags the identifier to match. No "TBD"/"handle edge cases"/"add validation" placeholders remain; every code step carries real code.

**Type consistency:** `ResolvedMatview { refresh: Option<String>, concurrently: bool }` and `MaterializedViewsConfig::resolve(&str)` are used consistently across Tasks 7, 9, 10, 11, 13. `folder_name()` (Task 1) is consumed in Task 6. `emit_matview`/`emit_entity` (Task 4) is reused in Tasks 13, 16. `sync_refresh_jobs`/`refresh_matview` trait methods (Task 9) are called from Tasks 11, 13. `refresh_job_sql` job-name format `dbd:refresh:<qualified>` matches `unschedule_stale_sql`'s `LIKE 'dbd:refresh:%'`.
