# Native Postgres Parser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a `DdlParser` trait with two implementations — the incumbent sqlparser one and a new libpg_query one — dispatched on dialect, and switch the first entity type (Enum) over behind a differential parity gate.

**Architecture:** `parse_entity_with(ParserChoice, file, sql)` dispatches to `SqlparserDdl` or `PgQueryDdl`. `PgQueryDdl` parses only the entity types listed in its `COVERED` constant and delegates everything else to `SqlparserDdl`, so every task leaves the tree releasable. A parity test runs both parsers over a corpus and asserts they agree on covered types, which is what licenses each type's switchover.

**Tech Stack:** Rust 2024, `pg_query` 6 (libpg_query / PostgreSQL's C parser), `sqlparser` 0.62, `serde_json` for structural comparison.

**Spec:** `docs/superpowers/specs/2026-08-24-native-postgres-parser-design.md`

**Scope:** Foundation + rollout step 1 (Enum). Steps 2–5 (View/MatView, Function/Procedure, Role, Table) are follow-on plans; Table needs its own spec per the design doc.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/dbd-core/src/config.rs` | **Modify** — add `SourceConfig::parser` |
| `crates/dbd-core/src/parser/mod.rs` | **Modify** — `ParserChoice`, `DdlParser` trait, `SqlparserDdl`, dispatch entry points |
| `crates/dbd-core/src/parser/pg/mod.rs` | **Create** — `PgQueryDdl`, `COVERED`, per-type dispatch |
| `crates/dbd-core/src/parser/pg/enums.rs` | **Create** — native enum parsing |
| `crates/dbd-core/src/parser/extractors.rs` | **Modify** — reuse the shared label extractor from `pg/enums.rs` |
| `crates/dbd-core/src/design/mod.rs` | **Modify** — resolve the choice, pass it to the scan loop |
| `crates/dbd-core/tests/parser_choice.rs` | **Create** — config validation at load |
| `crates/dbd-core/tests/parser_parity.rs` | **Create** — differential gate |
| `tests/fixtures/parser_corpus/ddl/enum/app/*.ddl` | **Create** — corpus files |

**Prerequisite landed during Task 2 (commit `c415f3c`):** a `DO`-guarded enum
used to return from the recovery arm before search-path extraction, recording
`search_paths=[]` where an identical plain `create type` recorded `["app"]`.
Both spellings now agree. Without that fix the Task 7 parity gate would have
compared `[]` against `["app"]` and failed on `guarded.ddl`.

**Note on `pg` module placement:** the four libpg_query helpers currently in `extractors.rs` (`is_valid_postgres`, `extract_search_paths_via_pg_query`, `extract_view_refs_via_pg_query`, `extract_enum_values_via_pg_query`) stay there for now — they are still called by the fallback in the sqlparser path. Per the spec they move into `parser/pg/` when that fallback retires at the end of rollout. Moving them now would make the sqlparser implementation depend on the pg module for no gain.

---

## Task 1: `ParserChoice` and the `source.parser` config field

**Files:**
- Modify: `crates/dbd-core/src/config.rs:135-148`
- Modify: `crates/dbd-core/src/parser/mod.rs` (add near the top, after the imports)

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block at the bottom of `crates/dbd-core/src/parser/mod.rs`:

```rust
    // ── ParserChoice ────────────────────────────────────────────────────────

    #[test]
    fn postgres_dialects_default_to_pg_query() {
        assert_eq!(ParserChoice::resolve("postgresql", None).unwrap(), ParserChoice::PgQuery);
        assert_eq!(ParserChoice::resolve("supabase", None).unwrap(), ParserChoice::PgQuery);
    }

    #[test]
    fn other_dialects_keep_sqlparser() {
        assert_eq!(ParserChoice::resolve("sqlite", None).unwrap(), ParserChoice::Sqlparser);
    }

    #[test]
    fn explicit_parser_overrides_the_dialect() {
        assert_eq!(
            ParserChoice::resolve("postgresql", Some("sqlparser")).unwrap(),
            ParserChoice::Sqlparser
        );
        assert_eq!(
            ParserChoice::resolve("sqlite", Some("pg_query")).unwrap(),
            ParserChoice::PgQuery
        );
    }

    /// `source.parser` is public API, so a typo must not silently leave the
    /// project on a parser the author did not ask for.
    #[test]
    fn an_unknown_parser_errors_and_names_the_valid_values() {
        let err = ParserChoice::resolve("postgresql", Some("pgquery"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("pg_query"), "got: {err}");
        assert!(err.contains("sqlparser"), "got: {err}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dbd-core --lib parser::tests::postgres_dialects 2>&1 | tail -5`
Expected: FAIL — `cannot find type ParserChoice in this scope`

- [ ] **Step 3: Add the config field**

In `crates/dbd-core/src/config.rs`, replace the `SourceConfig` struct and its `Default` impl:

```rust
#[derive(Debug, Deserialize)]
pub struct SourceConfig {
    #[serde(default = "default_dialect")]
    pub dialect: String,
    /// Which DDL parser reads this project's files. `None` lets `dialect`
    /// decide; set it only to override that choice.
    #[serde(default)]
    pub parser: Option<String>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            dialect: default_dialect(),
            parser: None,
        }
    }
}
```

- [ ] **Step 4: Add `ParserChoice`**

In `crates/dbd-core/src/parser/mod.rs`, change the error import on line 7 from

```rust
use crate::error::Result;
```

to

```rust
use crate::error::{DbdError, Result};
```

Then add, immediately after the `pub use extractors::extract_search_paths;` line:

```rust
/// Which parser reads a project's DDL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserChoice {
    /// sqlparser-rs — multi-dialect, the historical default.
    Sqlparser,
    /// libpg_query — PostgreSQL's own grammar, vendored from the server.
    PgQuery,
}

impl ParserChoice {
    /// `explicit` (`source.parser`) wins when set; otherwise the dialect decides.
    ///
    /// An unrecognised value is an error rather than a silent fallback: quietly
    /// ignoring a typo would leave the project on a parser its author did not
    /// choose, which is exactly the class of invisible behaviour this migration
    /// exists to remove.
    pub fn resolve(dialect: &str, explicit: Option<&str>) -> Result<Self> {
        match explicit {
            Some("pg_query") => Ok(Self::PgQuery),
            Some("sqlparser") => Ok(Self::Sqlparser),
            Some(other) => Err(DbdError::Config(format!(
                "unknown source.parser {other:?} — expected \"pg_query\" or \"sqlparser\""
            ))),
            None => Ok(match dialect {
                "postgresql" | "supabase" => Self::PgQuery,
                _ => Self::Sqlparser,
            }),
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib parser::tests:: 2>&1 | tail -5`
Expected: PASS, all parser tests green

- [ ] **Step 6: Run the full suite (the config change touches deserialization)**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`. `cargo test` exits non-zero if any test fails, so the exit
code is the whole assertion — do not count test-result lines, that number drifts
every time a task adds a test binary.

- [ ] **Step 7: Commit**

```bash
git add crates/dbd-core/src/config.rs crates/dbd-core/src/parser/mod.rs
git commit -m "feat(parser): add ParserChoice and the source.parser config field

Resolves which DDL parser a project uses: an explicit source.parser wins,
otherwise the dialect decides. Nothing dispatches on it yet.

An unrecognised value is a config error rather than a silent fallback —
it is public API, and quietly ignoring a typo would leave the project on
a parser its author did not choose."
```

---

## Task 2: `DdlParser` trait and `SqlparserDdl`

Extract today's `parse_entity` body behind a trait. Behaviour must not change.

**Files:**
- Modify: `crates/dbd-core/src/parser/mod.rs:163` (the `parse_entity` signature)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/dbd-core/src/parser/mod.rs`:

```rust
    // ── DdlParser ───────────────────────────────────────────────────────────

    /// Object safety is a real requirement: dispatch selects an implementation
    /// at runtime, so the trait must be usable behind a reference.
    #[test]
    fn sqlparser_ddl_is_usable_as_a_trait_object() {
        let parser: &dyn DdlParser = &SqlparserDdl;
        let entity = parser
            .parse(
                Path::new("ddl/enum/app/s.ddl"),
                "create type s as enum ('a', 'b');",
            )
            .unwrap();
        assert_eq!(entity.enum_values.len(), 2);
        assert!(entity.errors.is_empty(), "got: {:?}", entity.errors);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core --lib sqlparser_ddl_is_usable 2>&1 | tail -5`
Expected: FAIL — `cannot find trait DdlParser in this scope`

- [ ] **Step 3: Add the trait and rename the existing function**

In `crates/dbd-core/src/parser/mod.rs`, change the signature on line 163 from

```rust
pub fn parse_entity(file: &Path, sql: &str) -> Result<Entity> {
```

to

```rust
fn parse_with_sqlparser(file: &Path, sql: &str) -> Result<Entity> {
```

Leave the entire body unchanged. Then add immediately above it:

```rust
/// Reads a DDL file into an [`Entity`].
///
/// Two implementations exist so the Postgres-native parser can be built and
/// verified beside the incumbent rather than replacing it in one step.
pub(crate) trait DdlParser {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity>;
}

/// sqlparser-rs. Historical behaviour, unchanged.
pub(crate) struct SqlparserDdl;

impl DdlParser for SqlparserDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        parse_with_sqlparser(file, sql)
    }
}

/// Parse a DDL file with sqlparser, regardless of `source.parser`.
///
/// Still the entry point for the project scan (`design::from_config_with_dir`)
/// until dispatch lands; the round-trip callers that reconstruct DDL and read it
/// straight back (`emit`, `dbml_parse` tests) keep using it afterwards.
pub fn parse_entity(file: &Path, sql: &str) -> Result<Entity> {
    SqlparserDdl.parse(file, sql)
}
```

- [ ] **Step 4: Run the full suite — this must be behaviour-neutral**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`. `cargo test` exits non-zero if any test fails, so the exit
code is the whole assertion — do not count test-result lines, that number drifts
every time a task adds a test binary.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/parser/mod.rs
git commit -m "refactor(parser): put the sqlparser implementation behind a DdlParser trait

Pure extraction, no behaviour change: parse_entity's body becomes
parse_with_sqlparser and SqlparserDdl wraps it. parse_entity stays the
entry point for the emit and dbml_parse round-trip callers.

The trait is the seam the Postgres-native parser is built beside."
```

---

## Task 3: `PgQueryDdl` skeleton that delegates everything

**Files:**
- Create: `crates/dbd-core/src/parser/pg/mod.rs`
- Modify: `crates/dbd-core/src/parser/mod.rs` (register the module)

- [ ] **Step 1: Write the failing test**

Create `crates/dbd-core/src/parser/pg/mod.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SqlparserDdl;
    use std::path::Path;

    fn json(entity: &Entity) -> serde_json::Value {
        serde_json::to_value(entity).expect("Entity serializes")
    }

    /// Until a type is in COVERED, PgQueryDdl must be byte-identical to the
    /// incumbent — that is what makes every step of the migration releasable.
    #[test]
    fn uncovered_types_delegate_identically() {
        let path = Path::new("ddl/table/app/t.ddl");
        let sql = "set search_path to app;\ncreate table t (id int primary key);";

        let old = SqlparserDdl.parse(path, sql).unwrap();
        let new = PgQueryDdl.parse(path, sql).unwrap();

        assert_eq!(json(&old), json(&new));
    }

    #[test]
    fn nothing_is_covered_yet() {
        assert!(PgQueryDdl::COVERED.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core --lib parser::pg 2>&1 | tail -5`
Expected: FAIL — `file not found for module pg` (the module is not registered yet)

- [ ] **Step 3: Register the module**

In `crates/dbd-core/src/parser/mod.rs`, change lines 1-2 from

```rust
mod extractors;
mod tables;
```

to

```rust
mod extractors;
pub(crate) mod pg;
mod tables;
```

- [ ] **Step 4: Write the implementation**

Prepend to `crates/dbd-core/src/parser/pg/mod.rs`, above the test module:

```rust
//! The Postgres-native DDL parser, built on libpg_query.
//!
//! Covers entity types incrementally. Anything not yet native delegates to
//! [`SqlparserDdl`], so the tree is releasable at every step of the migration
//! rather than only at the end.

use std::path::Path;

use crate::entity::{Entity, EntityType};
use crate::error::Result;
use crate::parser::{DdlParser, SqlparserDdl};

/// libpg_query — PostgreSQL's own grammar.
pub(crate) struct PgQueryDdl;

impl PgQueryDdl {
    /// Entity types this parser handles itself.
    ///
    /// Single source of truth: dispatch below and `tests/parser_parity.rs` both
    /// read it, so a type cannot be switched over without also coming under the
    /// parity gate.
    pub(crate) const COVERED: &'static [EntityType] = &[];

    pub(crate) fn covers(entity_type: EntityType) -> bool {
        Self::COVERED.contains(&entity_type)
    }
}

impl DdlParser for PgQueryDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        // Nothing is native yet, so every type delegates. Types move into
        // COVERED one at a time, each behind the parity gate.
        SqlparserDdl.parse(file, sql)
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib parser::pg 2>&1 | tail -6`
Expected: PASS — 2 tests

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/parser/pg/mod.rs crates/dbd-core/src/parser/mod.rs
git commit -m "feat(parser): add the PgQueryDdl skeleton, delegating every type

Covers nothing yet — every entity type falls through to SqlparserDdl, so
this is behaviour-neutral. The COVERED constant is the single source of
truth that dispatch and the parity gate will both read, so a type cannot
be switched over without also being verified."
```

---

## Task 4: Wire dispatch into the scan loop

**Files:**
- Modify: `crates/dbd-core/src/parser/mod.rs` (add `parse_entity_with`)
- Modify: `crates/dbd-core/src/design/mod.rs:393-404`
- Create: `crates/dbd-core/tests/parser_choice.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/dbd-core/tests/parser_choice.rs`:

```rust
//! `source.parser` is validated when the design loads.

use dbd_core::Design;
use std::path::PathBuf;

fn project_with_source_block(name: &str, source_block: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/.tmp")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        format!(
            "project:\n  name: parser_choice\nsource:\n{source_block}\
             target:\n  postgres:\n    url: postgres://localhost/unused\nschemas:\n  - app\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table t (id int primary key);\n",
    )
    .unwrap();
    dir.join("design.yaml")
}

#[test]
fn an_unknown_source_parser_fails_to_load() {
    let config = project_with_source_block(
        "parser_choice_bad",
        "  dialect: postgresql\n  parser: pgquery\n",
    );
    let err = Design::from_config(&config, "dev")
        .expect_err("an unrecognised source.parser must not load")
        .to_string();
    assert!(err.contains("pg_query"), "must name the valid values, got: {err}");
    assert!(err.contains("sqlparser"), "must name the valid values, got: {err}");
}

#[test]
fn an_explicit_valid_parser_loads() {
    let config = project_with_source_block(
        "parser_choice_ok",
        "  dialect: postgresql\n  parser: sqlparser\n",
    );
    Design::from_config(&config, "dev").expect("an explicit valid parser must load");
}

#[test]
fn omitting_source_parser_loads() {
    let config = project_with_source_block("parser_choice_absent", "  dialect: postgresql\n");
    Design::from_config(&config, "dev").expect("source.parser is optional");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core --test parser_choice 2>&1 | tail -8`
Expected: FAIL — `an unrecognised source.parser must not load` (the value is currently ignored)

- [ ] **Step 3: Add the dispatching entry point**

In `crates/dbd-core/src/parser/mod.rs`, replace the `parse_entity` function added in Task 2 with:

```rust
/// Parse a DDL file with an explicit parser choice.
pub fn parse_entity_with(choice: ParserChoice, file: &Path, sql: &str) -> Result<Entity> {
    match choice {
        ParserChoice::Sqlparser => SqlparserDdl.parse(file, sql),
        ParserChoice::PgQuery => pg::PgQueryDdl.parse(file, sql),
    }
}

/// Parse a DDL file with the Postgres default parser.
///
/// Kept for callers that reconstruct DDL and read it straight back
/// (`emit`, `dbml_parse`) rather than scanning a project directory.
pub fn parse_entity(file: &Path, sql: &str) -> Result<Entity> {
    parse_entity_with(ParserChoice::PgQuery, file, sql)
}

/// Entity types the Postgres-native parser handles itself.
///
/// Public so the parity gate reads the same list dispatch does.
pub fn pg_native_types() -> &'static [EntityType] {
    pg::PgQueryDdl::COVERED
}
```

- [ ] **Step 4: Resolve the choice when the design loads**

In `crates/dbd-core/src/design/mod.rs`, find the scan loop at line 393. Immediately before `let ddl_files = scanner::scan_ddl(&project_dir)?;`, insert:

```rust
        // Validate and resolve the parser before reading any file, so a bad
        // `source.parser` fails at load rather than partway through the scan.
        let parser_choice = crate::parser::ParserChoice::resolve(
            &design_config.source.dialect,
            design_config.source.parser.as_deref(),
        )?;
```

Then change line 400 from

```rust
            if let Ok(mut entity) = parser::parse_entity(relative, &sql) {
```

to

```rust
            if let Ok(mut entity) = parser::parse_entity_with(parser_choice, relative, &sql) {
```

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p dbd-core --test parser_choice 2>&1 | tail -8`
Expected: PASS — 3 tests

- [ ] **Step 6: Run the full suite — still behaviour-neutral (PgQueryDdl delegates)**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`

- [ ] **Step 8: Commit**

```bash
git add crates/dbd-core/src/parser/mod.rs crates/dbd-core/src/design/mod.rs crates/dbd-core/tests/parser_choice.rs
git commit -m "feat(parser): dispatch the scan loop on the resolved parser choice

parse_entity_with routes to SqlparserDdl or PgQueryDdl; the scan loop in
Design::from_config_with_dir resolves the choice once, before reading any
file, so a bad source.parser fails at load rather than partway through.

Still behaviour-neutral: PgQueryDdl covers no type yet and delegates
everything."
```

---

## Task 5: Native enum parsing

**Files:**
- Create: `crates/dbd-core/src/parser/pg/enums.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs` (register the submodule)
- Modify: `crates/dbd-core/src/parser/extractors.rs` (reuse the shared label extractor)

- [ ] **Step 1: Write the failing tests**

Create `crates/dbd-core/src/parser/pg/enums.rs` with only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn parse(sql: &str) -> Entity {
        let entity = Entity::new(EntityType::Enum, "app.status");
        parse_enum(entity, sql).unwrap()
    }

    #[test]
    fn plain_create_type_yields_its_labels() {
        let e = parse("set search_path to app;\ncreate type status as enum ('a', 'b');");
        let names: Vec<&str> = e.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(e.errors.is_empty(), "got: {:?}", e.errors);
    }

    /// Postgres has no `CREATE TYPE IF NOT EXISTS`, so the guarded DO block is
    /// the only idiom for a conditional enum.
    #[test]
    fn do_guarded_create_type_yields_its_labels() {
        let e = parse(
            "set search_path to app;\n\
             do $$ begin\n  create type status as enum ('a', 'b');\n\
             exception when duplicate_object then null;\nend $$;",
        );
        let names: Vec<&str> = e.enum_values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(e.errors.is_empty(), "got: {:?}", e.errors);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate type status as enum ('a');");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    /// Matches the sqlparser path's default so the two agree under parity.
    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create type status as enum ('a');");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    /// libpg_query names the offending token but reports no line/column — its
    /// Rust binding keeps only `error.message` and drops `cursorpos` (spec F5).
    /// Assert the token, which is what we can actually guarantee.
    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create type status as enum (;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(
            e.errors[0].contains("syntax error at or near"),
            "got: {:?}",
            e.errors
        );
        assert!(e.enum_values.is_empty());
    }

    #[test]
    fn valid_sql_declaring_no_enum_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty(), "an enum file with no CREATE TYPE must error");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core --lib parser::pg::enums 2>&1 | tail -5`
Expected: FAIL — `file not found for module enums`

- [ ] **Step 3: Register the submodule**

In `crates/dbd-core/src/parser/pg/mod.rs`, add directly below the `//!` doc comment block:

```rust
pub(crate) mod enums;
```

- [ ] **Step 4: Write the implementation**

Prepend to `crates/dbd-core/src/parser/pg/enums.rs`, above the test module:

```rust
//! Enum DDL, parsed with libpg_query.

use crate::entity::{Entity, EnumValue};
use crate::error::Result;
use crate::parser::extractors;

/// Parse an enum DDL file.
///
/// Handles both spellings: a bare `CREATE TYPE … AS ENUM (…)`, and one wrapped
/// in the `DO $$ … EXCEPTION WHEN duplicate_object $$` guard that is Postgres's
/// only idiom for a conditional CREATE TYPE.
pub(crate) fn parse_enum(mut entity: Entity, sql: &str) -> Result<Entity> {
    // libpg_query is Postgres's own grammar, so its rejection is the definition
    // of invalid SQL. Recording an error only here keeps the invariant
    // `Design::ensure_fully_parsed` relies on: apply refuses only on real
    // breakage, never on a parser limitation.
    if let Err(e) = pg_query::parse(sql) {
        entity.errors.push(format!("Parse error: {e}"));
        return Ok(entity);
    }

    entity.search_paths = extractors::extract_search_paths_via_pg_query(sql);
    entity.enum_values = enum_values(sql);

    if entity.enum_values.is_empty() {
        entity
            .errors
            .push("no `CREATE TYPE … AS ENUM (…)` found in this enum file".to_string());
    }

    Ok(entity)
}

/// Enum labels, whichever spelling the file uses.
fn enum_values(sql: &str) -> Vec<EnumValue> {
    if let Ok(parsed) = pg_query::parse(sql) {
        let values = labels_from_parse_result(&parsed);
        if !values.is_empty() {
            return values;
        }
    }
    // Guarded form: the CREATE lives inside a PL/pgSQL block, which the
    // top-level statement walk above cannot see into.
    extractors::extract_enum_values_via_pg_query(sql)
}

/// Labels from the first `CreateEnumStmt` in a parsed statement list.
///
/// Shared with [`extractors::extract_enum_values_via_pg_query`], which runs this
/// over the statements it recovers from inside a `DO` block.
pub(crate) fn labels_from_parse_result(parsed: &pg_query::ParseResult) -> Vec<EnumValue> {
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::CreateEnumStmt(create)) =
            stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        let values: Vec<EnumValue> = create
            .vals
            .iter()
            .filter_map(|v| match v.node.as_ref() {
                Some(pg_query::NodeEnum::String(s)) => Some(EnumValue {
                    name: s.sval.clone(),
                    note: None,
                }),
                _ => None,
            })
            .collect();
        if !values.is_empty() {
            return values;
        }
    }
    Vec::new()
}
```

- [ ] **Step 5: Remove the duplicated label extraction in `extractors.rs`**

In `crates/dbd-core/src/parser/extractors.rs`, replace the body of
`extract_enum_values_via_pg_query` — the `for query in &queries { … }` loop —
with a call to the shared helper, so the label-reading logic exists once:

```rust
    for query in &queries {
        let Ok(parsed) = pg_query::parse(query) else {
            continue;
        };
        let values = crate::parser::pg::enums::labels_from_parse_result(&parsed);
        if !values.is_empty() {
            return values;
        }
    }
    Vec::new()
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p dbd-core --lib parser:: 2>&1 | tail -6`
Expected: PASS — all parser tests including the 6 new enum tests

- [ ] **Step 7: Run the full suite and clippy**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "test exit: $?"; cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "clippy exit: $?"`
Expected: both `exit: 0`

- [ ] **Step 8: Commit**

```bash
git add crates/dbd-core/src/parser/pg/enums.rs crates/dbd-core/src/parser/pg/mod.rs crates/dbd-core/src/parser/extractors.rs
git commit -m "feat(parser): parse enum DDL natively with libpg_query

Handles both the bare CREATE TYPE … AS ENUM and the DO-guarded form, and
records an error only when libpg_query itself rejects the file — which
keeps the invariant Design::ensure_fully_parsed relies on.

Not wired into dispatch yet; Enum joins COVERED once the parity gate is
in place. The label extraction the DO-block fallback duplicated now lives
here and is shared."
```

---

## Task 6: Differential parity harness

**Files:**
- Create: `crates/dbd-core/tests/parser_parity.rs`
- Create: `tests/fixtures/parser_corpus/ddl/enum/app/plain.ddl`
- Create: `tests/fixtures/parser_corpus/ddl/enum/app/guarded.ddl`
- Create: `tests/fixtures/parser_corpus/ddl/enum/app/quoted_search_path.ddl`

- [ ] **Step 1: Create the corpus files**

`tests/fixtures/parser_corpus/ddl/enum/app/plain.ddl`:

```sql
set search_path to app;

create type plain as enum ('draft', 'published', 'archived');
```

`tests/fixtures/parser_corpus/ddl/enum/app/guarded.ddl`:

```sql
set search_path to app;

do $$ begin
  create type guarded as enum ('active', 'archived');
exception when duplicate_object then null;
end $$;
```

`tests/fixtures/parser_corpus/ddl/enum/app/quoted_search_path.ddl`:

```sql
set search_path to 'app';

create type quoted_search_path as enum ('one', 'two');
```

- [ ] **Step 2: Write the parity harness**

Create `crates/dbd-core/tests/parser_parity.rs`:

```rust
//! Differential gate: the Postgres-native parser must agree with the incumbent
//! on every type it claims, and must do better on files the incumbent rejects.
//!
//! Restricting the sweep to `pg_native_types()` is load-bearing. A type that
//! still delegates would compare `SqlparserDdl` against itself and pass for
//! free — a green test proving nothing.

use dbd_core::parser::{parse_entity_with, pg_native_types, ParserChoice};
use dbd_core::Entity;
use std::path::{Path, PathBuf};

/// Every `.ddl`/`.sql` file under `root`, recursively.
fn ddl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(ddl_files(&path));
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("ddl") | Some("sql")
        ) {
            out.push(path);
        }
    }
    out
}

fn corpus() -> Vec<PathBuf> {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut files = ddl_files(&fixtures);
    files.sort();
    files
}

fn json(entity: &Entity) -> serde_json::Value {
    serde_json::to_value(entity).expect("Entity serializes")
}

#[test]
fn native_types_match_sqlparser_on_every_corpus_file() {
    let covered = pg_native_types();
    let mut checked = 0usize;

    for file in corpus() {
        let sql = std::fs::read_to_string(&file).expect("corpus file is readable");
        // The path decides the entity type, exactly as the scan loop does.
        if !covered.contains(&Entity::from_file(&file).entity_type) {
            continue;
        }
        checked += 1;

        let old = parse_entity_with(ParserChoice::Sqlparser, &file, &sql).unwrap();
        let new = parse_entity_with(ParserChoice::PgQuery, &file, &sql).unwrap();

        if old.errors.is_empty() {
            // No regression: identical Entity for anything the incumbent reads.
            assert_eq!(
                json(&old),
                json(&new),
                "parsers disagree on {}",
                file.display()
            );
        } else {
            // Improvement: the native parser must read what the incumbent could not.
            assert!(
                new.errors.is_empty(),
                "{} is valid Postgres the native parser should read, got: {:?}",
                file.display(),
                new.errors
            );
        }
    }

    if !covered.is_empty() {
        assert!(
            checked > 0,
            "no corpus file exercises the covered types {covered:?} — the gate is vacuous"
        );
    }
}
```

- [ ] **Step 3: Run it — passes vacuously while COVERED is empty**

Run: `cargo test -p dbd-core --test parser_parity 2>&1 | tail -6`
Expected: PASS — 1 test (nothing covered yet, so nothing compared; the vacuity guard is skipped because `covered` is empty)

- [ ] **Step 4: Run the full suite and clippy**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "test exit: $?"; cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "clippy exit: $?"`
Expected: both `exit: 0`

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/tests/parser_parity.rs tests/fixtures/parser_corpus
git commit -m "test(parser): add the differential parity gate

Runs both parsers over every corpus file whose type is in COVERED and
asserts a two-way invariant: identical Entity where sqlparser succeeds
(no regression), and a clean parse where sqlparser fails (improvement).

Restricting the sweep to the covered types is load-bearing — a delegated
type would compare sqlparser against itself and pass for free. The vacuity
guard catches the matching trap: a covered type with no corpus file."
```

---

## Task 7: Switch Enum over

**Files:**
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`

- [ ] **Step 1: Update the skeleton test to expect coverage**

In `crates/dbd-core/src/parser/pg/mod.rs`, replace the `nothing_is_covered_yet` test with:

```rust
    #[test]
    fn enum_is_covered() {
        assert!(PgQueryDdl::covers(EntityType::Enum));
        assert!(!PgQueryDdl::covers(EntityType::Table));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-core --lib parser::pg::tests::enum_is_covered 2>&1 | tail -5`
Expected: FAIL — assertion failed, `COVERED` is still empty

- [ ] **Step 3: Add Enum to COVERED and dispatch to it**

In `crates/dbd-core/src/parser/pg/mod.rs`, change the constant from

```rust
    pub(crate) const COVERED: &'static [EntityType] = &[];
```

to

```rust
    pub(crate) const COVERED: &'static [EntityType] = &[EntityType::Enum];
```

and replace the `DdlParser` impl with:

```rust
impl DdlParser for PgQueryDdl {
    fn parse(&self, file: &Path, sql: &str) -> Result<Entity> {
        let entity = Entity::from_file(file);
        if !Self::covers(entity.entity_type) {
            return SqlparserDdl.parse(file, sql);
        }
        match entity.entity_type {
            EntityType::Enum => enums::parse_enum(entity, sql),
            // Unreachable while COVERED and this match agree; delegating rather
            // than panicking keeps a mismatch a non-event.
            _ => SqlparserDdl.parse(file, sql),
        }
    }
}
```

- [ ] **Step 4: Run the parity gate — this is the switchover licence**

Run: `cargo test -p dbd-core --test parser_parity 2>&1 | tail -10`
Expected: PASS — the enum fixture (`tests/fixtures/ddl/enum/config/status.sql`) and the three corpus files now compare, and both parsers agree

- [ ] **Step 5: Run the full suite and clippy**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "test exit: $?"; cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "clippy exit: $?"`
Expected: both `exit: 0`

- [ ] **Step 6: Verify end-to-end against a live database**

```bash
cargo build --release
R=/tmp/dbd-native-enum
rm -rf $R && mkdir -p $R/ddl/enum/ne $R/ddl/table/ne
cat > $R/design.yaml <<'YAML'
project:
  name: native_enum
  version: 1
source:
  dialect: postgresql
schemas:
  - ne
YAML
cat > $R/ddl/enum/ne/status_t.ddl <<'DDL'
set search_path to ne;

do $$ begin
  create type status_t as enum ('active','archived');
exception when duplicate_object then null;
end $$;
DDL
cat > $R/ddl/table/ne/t.ddl <<'DDL'
set search_path to ne;
create table if not exists t (id int primary key, s status_t not null);
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_native_enum' -c 'CREATE DATABASE dbd_native_enum'
cd $R && /Users/Jerry/Developer/dbd/target/release/dbd apply -d postgresql://Jerry@localhost/dbd_native_enum -s .
psql -qtA -d dbd_native_enum -c "select string_agg(e.enumlabel, ',' order by e.enumsortorder) from pg_type t join pg_enum e on e.enumtypid=t.oid where t.typname='status_t'"
```

Expected: `3 entities applied` and `active,archived`

- [ ] **Step 7: Confirm reconcile converges, then clean up**

```bash
cd /tmp/dbd-native-enum
/Users/Jerry/Developer/dbd/target/release/dbd reconcile -d postgresql://Jerry@localhost/dbd_native_enum -s . | tail -2
/Users/Jerry/Developer/dbd/target/release/dbd diff -d postgresql://Jerry@localhost/dbd_native_enum -s . | tail -2
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_native_enum'
```

Expected: `0 created, 0 altered` and `Live database is in sync with the design — no differences.`

- [ ] **Step 8: Commit**

```bash
git add crates/dbd-core/src/parser/pg/mod.rs
git commit -m "feat(parser): parse enums with libpg_query for the Postgres dialect

First type to switch over. The parity gate compares both parsers across
the fixture and corpus enums and finds them identical, which is what
licenses the change.

Every other type still delegates to sqlparser."
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0 (the exit code is the assertion; test-binary counts drift)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -p dbd-core --test parser_parity` compares at least one file (not vacuous)
- [ ] A project with `source.parser: pgquery` fails to load, naming the valid values
- [ ] A project with `source.parser: sqlparser` loads and uses the incumbent
- [ ] A DO-guarded enum applies and reconcile converges to no drift
- [ ] A broken enum file still errors, and the message names the offending token

## Accepted regression

Native parse errors lose line and column. libpg_query reports a `cursorpos`, but
its Rust binding keeps only `error.message`:

| parser | message for `create type status as enum (;;;` |
| --- | --- |
| sqlparser | `Expected: identifier, found: ; at Line: 1, Column: 29` |
| libpg_query | `syntax error at or near ";"` |

`dbd inspect` output gets less precise for enum files. Accepted for this
rollout; recovering the position is spec follow-up F5.

## What this plan does NOT do

Deliberately deferred to follow-on plans, per the spec:

- Rollout steps 2–5: View/MaterializedView, Function/Procedure, Role, Table
- Retiring the three `preprocess_sql` regex workarounds and `extract_role_memberships`
- Removing the libpg_query validation fallback in `parse_with_sqlparser` (it stays until every type is native)
- Moving the four libpg_query helpers out of `extractors.rs` into `parser/pg/`
- The formatter (`F1`), unconditional validation (`F3`), and the view `CREATE OR REPLACE` / `dbd inspect` exit-code gaps (`F4`)
