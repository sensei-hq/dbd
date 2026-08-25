# Native Postgres Parser — View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `EntityType::View` onto libpg_query, behind the existing differential parity gate.

**Architecture:** `PgQueryDdl::native()` gains a `View` arm calling a new `pg/views.rs`. References come from libpg_query's `select_tables()` (tables) and `call_functions()` (functions). The parity harness gains one documented exclusion — soft function references — because Postgres and sqlparser disagree about whether SQL special forms are function calls, and that disagreement is inert.

**Tech Stack:** Rust 2024, `pg_query` 6, `sqlparser` 0.62, `serde_json` for structural comparison.

**Spec:** `docs/superpowers/specs/2026-08-24-native-postgres-parser-design.md` (rollout step 2)

**Prior increment:** `docs/superpowers/plans/2026-08-24-native-postgres-parser.md` — read its "Execution record" section before starting.

---

## Scope: View only. MaterializedView is deferred.

The spec pairs View and MaterializedView in one rollout step. **They must be split**, on evidence.

A matview stores its body text in `writes[0]`, which `emit_matview` and `reconcile` use to reconstruct the `CREATE`. The sqlparser path fills it with `create_view.query.to_string()` — sqlparser's own AST re-rendering, not source text. For parity, libpg_query must produce a byte-identical string. It does not. Measured across 14 realistic bodies, **10 agree and 4 differ**, all cosmetic renderer choices:

| body | sqlparser | libpg_query |
| --- | --- | --- |
| `select a::text from t` | `a::TEXT` | `a::text` |
| `select coalesce(a,'x') …` | `coalesce(…)` | `COALESCE(…)` |
| `select (data ->> 'k') …` | `(data ->> 'k')` | `data ->> 'k'` |
| `… tablesample bernoulli (10)` | `TABLESAMPLE BERNOULLI (10)` | `TABLESAMPLE bernoulli(10)` |

Roughly 30% of realistic matview bodies would fail the gate for reasons that have nothing to do with correctness. Closing that needs a decision about the `writes[0]` contract — most likely storing verbatim source sliced by `RawStmt.stmt_location`/`stmt_len` instead of a re-rendering, which changes `emit` output and is its own spec question.

**Plain views need none of this.** A View entity carries only `references`; nothing renders its body. So View is parity-clean and ships now; MaterializedView gets its own spec.

---

## What the View parser must produce

The sqlparser arm (`parser/mod.rs`, `EntityType::View`) sets exactly one field:

```rust
entity.references = extractors::extract_view_info(&statements, &entity.search_paths);
```

`extract_view_info` yields two kinds of reference:

- **Relations** — tables/views/matviews the body reads. Hard references; they drive apply order.
- **Function calls** — tagged [`REF_TYPE_FUNCTION`]. *Soft* references: `references::resolve_references` keeps the ones naming a known entity and silently drops the rest, because a body's built-in calls are indistinguishable from calls to a project-managed function.

`search_paths` is also set (by the shared code above the match), and the first entry qualifies unqualified names.

### Measured parity of the libpg_query equivalents

`select_tables()` alone agrees with sqlparser on 7 of 12 view bodies. Adding `call_functions()` closes 6 of the 5 remaining gaps — leaving one class, below.

| body | sqlparser | `select_tables` + `call_functions` |
| --- | --- | --- |
| `select (select max(x) from z) from t` | ✅ agree | ✅ |
| `select lower(name) from t` | ✅ | ✅ |
| `select count(*) from t` | ✅ | ✅ |
| `select a from generate_series(1,3)` | ✅ | ✅ |
| `select app.myfn(a) from t` | ✅ | ✅ |
| `select coalesce(a,'x') from t` | `["app.coalesce","app.t"]` | `["app.t"]` |

---

## The one real divergence: SQL special forms

Postgres's grammar parses `COALESCE`, `NULLIF`, `GREATEST` and `LEAST` as dedicated expression nodes, **not** `FuncCall`s, so `call_functions()` correctly does not report them. sqlparser reports them as ordinary function calls. Measured: 4 of 12 tested expression forms diverge this way.

This is inert. Those names resolve to nothing — no project has an entity called `app.coalesce` — so `resolve_references` drops them on both sides and the apply graph is identical. But the parity gate asserts JSON equality over the whole `Entity`, so it would fail on any view using `COALESCE`, which is very common SQL.

**Resolution:** the gate compares Entities with `REF_TYPE_FUNCTION` references removed from both sides, and Task 2 adds a targeted test proving a soft reference to a *real project function* still survives the pg path. Hard (relation) references are still compared exactly.

This is a deliberate narrowing of the gate, and it is the only one. Record it in the harness so it cannot be mistaken for an oversight.

---

## File structure

| File | Responsibility |
| --- | --- |
| `crates/dbd-core/src/parser/pg/common.rs` | **Create** — libpg_query helpers shared by the pg-native parsers |
| `crates/dbd-core/src/parser/extractors.rs` | **Modify** — re-export from `pg::common`, breaking the module cycle |
| `crates/dbd-core/src/parser/pg/enums.rs` | **Modify** — import helpers from `pg::common`, not `extractors` |
| `crates/dbd-core/src/parser/pg/views.rs` | **Create** — native view parsing |
| `crates/dbd-core/src/parser/pg/mod.rs` | **Modify** — `View` arm in `native()`, `COVERED` |
| `crates/dbd-core/tests/parser_parity.rs` | **Modify** — soft-reference exclusion |
| `tests/fixtures/parser_corpus/ddl/view/app/*.ddl` | **Create** — corpus |

---

## Task boundaries — read this before decomposing further

The previous increment learned this the hard way, twice: **this crate is built with `clippy --all-targets -- -D warnings`, and a function reachable only from `#[cfg(test)]` code is `dead_code`.** Integration tests cannot rescue it either — they are a separate crate and cannot see `pub(crate)` items, and the warning comes from the lib target regardless.

So "write the parser now, wire it later" is not a valid split. Task 2 below deliberately bundles the parser, the gate change, the corpus and the switchover.

Task 1 is safe standalone because it only moves code that already has live callers.

---

## Task 1: Break the `pg` ↔ `extractors` module cycle

`pg/enums.rs` imports `crate::parser::extractors`, and `extractors.rs` imports `crate::parser::pg::enums::labels_from_parse_result`. Views will deepen this — `extract_view_refs_via_pg_query` lives in `extractors.rs` and is wanted by `pg/views.rs`, while the sqlparser error arm still calls it.

Move the libpg_query helpers into the `pg` module and have `extractors.rs` depend on `pg`, one directionally.

**Files:**
- Create: `crates/dbd-core/src/parser/pg/common.rs`
- Modify: `crates/dbd-core/src/parser/extractors.rs`
- Modify: `crates/dbd-core/src/parser/pg/enums.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`

- [ ] **Step 1: Confirm the current cycle, so you can prove it is gone**

Run: `rg -n 'use crate::parser::(extractors|pg)' crates/dbd-core/src/parser/ > /tmp/before.log 2>&1; cat /tmp/before.log`
Expected: at least one `pg/*.rs` importing `extractors`, and `extractors.rs` importing `pg::enums`.

- [ ] **Step 2: Move the helpers**

Move these four functions **verbatim** from `extractors.rs` into a new `crates/dbd-core/src/parser/pg/common.rs`, keeping their doc comments:

- `is_valid_postgres`
- `extract_search_paths_via_pg_query`
- `extract_view_refs_via_pg_query`
- `extract_enum_values_via_pg_query`

Plus the private helpers they use that are not used by anything else in `extractors.rs`. **Check each one before moving it** — `qualify_name_str`, `push_unique`, `const_str`, `collect_plpgsql_queries` and `DEFAULT_SEARCH_PATH` may be shared with sqlparser-side code. Anything shared must stay in `extractors.rs` and be made `pub(super)` so `pg::common` can use it, or be duplicated only if genuinely trivial. Report exactly which you moved and which you left, and why.

Head the new file:

```rust
//! libpg_query helpers shared by the Postgres-native parsers.
//!
//! These live under `pg` rather than beside the sqlparser extractors so the
//! dependency runs one way: `extractors` (sqlparser) may call into `pg`, never
//! the reverse. The previous arrangement had `pg::enums` importing `extractors`
//! while `extractors` imported `pg::enums`, a cycle that each new native parser
//! would deepen.
```

- [ ] **Step 3: Register it and repoint the imports**

In `pg/mod.rs` add `pub(crate) mod common;` beside the existing `mod enums;`.

In `extractors.rs`, replace the moved definitions with re-exports so external call sites keep working unchanged:

```rust
pub use super::pg::common::{
    extract_enum_values_via_pg_query, extract_search_paths_via_pg_query,
    extract_view_refs_via_pg_query, is_valid_postgres,
};
```

In `pg/enums.rs`, change `use crate::parser::extractors;` to `use super::common;` and update the call sites.

- [ ] **Step 4: Prove the cycle is gone**

Run: `rg -n 'use crate::parser::extractors|use super::extractors' crates/dbd-core/src/parser/pg/ > /tmp/after.log 2>&1; echo "exit: $?"; cat /tmp/after.log`
Expected: no matches (exit 1 from ripgrep means zero hits — that is the pass condition here).

- [ ] **Step 5: Verify — this is a pure move, behaviour must not change**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`. The exit code IS the assertion.

Run: `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"`
Expected: `exit: 0`

Run: `cargo test -p dbd-core --test parser_parity > /tmp/p.log 2>&1; echo "exit: $?"`
Expected: `exit: 0` — the enum gate must still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/parser/
git commit -F - <<'MSG'
refactor(parser): move the libpg_query helpers under pg/

`pg::enums` imported `extractors` while `extractors` imported `pg::enums`
— a cycle each new native parser would deepen, and views would have made
it worse since the view-reference helper lives on the sqlparser side.

The helpers now live in `pg::common` and the dependency runs one way:
extractors (sqlparser) may call into pg, never the reverse. `extractors`
re-exports them so existing call sites are untouched. Pure move, no
behaviour change.
MSG
```

---

## Task 2: Native view parsing, gate change, corpus and switchover

Bundled deliberately — see "Task boundaries" above.

**Files:**
- Create: `crates/dbd-core/src/parser/pg/views.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`
- Modify: `crates/dbd-core/tests/parser_parity.rs`
- Create: `tests/fixtures/parser_corpus/ddl/view/app/*.ddl`

- [ ] **Step 1: Write the failing tests**

Create `crates/dbd-core/src/parser/pg/views.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, REF_TYPE_FUNCTION};

    fn parse(sql: &str) -> Entity {
        parse_view(Entity::new(EntityType::View, "app.v"), sql).unwrap()
    }

    #[test]
    fn relation_references_are_captured_and_qualified() {
        let e = parse("set search_path to app;\ncreate view v as select a from t;");
        assert!(e.refers.contains(&"app.t".to_string()), "got {:?}", e.refers);
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn an_explicit_schema_is_not_overridden_by_the_search_path() {
        let e = parse("set search_path to app;\ncreate view v as select a from shop.orders;");
        assert!(e.refers.contains(&"shop.orders".to_string()), "got {:?}", e.refers);
    }

    /// Function calls are soft references: the resolver keeps the ones naming a
    /// known entity and drops the rest, so a body's built-ins are harmless.
    #[test]
    fn function_calls_are_captured_as_soft_references() {
        let e = parse("set search_path to app;\ncreate view v as select app.myfn(a) from t;");
        let myfn = e
            .references
            .iter()
            .find(|r| r.name == "app.myfn")
            .expect("function reference missing");
        assert_eq!(myfn.ref_type.as_deref(), Some(REF_TYPE_FUNCTION));
    }

    /// A CTE name is query-local, not a real relation.
    #[test]
    fn cte_names_are_not_references() {
        let e = parse("set search_path to app;\ncreate view v as with r as (select 1 n) select n from r;");
        assert!(!e.refers.contains(&"app.r".to_string()), "CTE leaked: {:?}", e.refers);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate view v as select a from t;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create view v as select a from t;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    /// libpg_query names the offending token but carries no line/column.
    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create view v as select * from ;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    /// Mirrors the enum parser: an error path still yields the `["public"]`
    /// default, because references are qualified against it.
    #[test]
    fn an_errored_view_still_has_a_search_path() {
        let e = parse("create view v as select * from ;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn a_file_declaring_no_view_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty(), "a view file with no CREATE VIEW must error");
    }
}
```

- [ ] **Step 2: Register the module and confirm the tests fail**

In `pg/mod.rs`, add `pub(crate) mod views;` beside `mod enums;`.

Run: `cargo test -p dbd-core --lib parser::pg::views > /tmp/t.log 2>&1; echo "exit: $?"; tail -10 /tmp/t.log`
Expected: non-zero exit — `cannot find function parse_view in this scope`.

(An unregistered file is simply not compiled, so register first or you get a misleading pass.)

- [ ] **Step 3: Write the implementation**

Prepend to `crates/dbd-core/src/parser/pg/views.rs`:

```rust
//! View DDL, parsed with libpg_query.

use crate::entity::{Entity, REF_TYPE_FUNCTION, Reference};
use crate::error::Result;

use super::common;

/// Parse a view DDL file.
///
/// A view entity carries only its references — nothing renders its body — so
/// unlike a materialized view it needs no verbatim SQL, which is what makes it
/// parity-clean against the incumbent.
pub(crate) fn parse_view(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Set the search path before any early return: references are qualified
    // against it, and an errored entity that reports `[]` instead of the
    // `["public"]` default is an invariant break the enum parser already hit.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    if !declares_a_view(&parsed) {
        entity
            .errors
            .push("this view file declares no `CREATE VIEW`".to_string());
        return Ok(entity);
    }

    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    // Relations the body reads — hard references, they drive apply order.
    let mut references = common::extract_view_refs_via_pg_query(sql, &default_schema);

    // Function calls — soft references. `resolve_references` keeps the ones
    // naming a known entity and drops the rest, because a body's built-in calls
    // are indistinguishable here from calls to a project-managed function.
    for name in parsed.call_functions() {
        let Some(qualified) = common::qualify_name_str(&name, &default_schema) else {
            continue;
        };
        if references.iter().any(|r| r.name == qualified) {
            continue;
        }
        references.push(Reference {
            name: qualified,
            ref_type: Some(REF_TYPE_FUNCTION.to_string()),
        });
    }

    entity.refers = references.iter().map(|r| r.name.clone()).collect();
    entity.references = references;
    Ok(entity)
}

/// Whether the file contains a `CREATE VIEW`.
fn declares_a_view(parsed: &pg_query::ParseResult) -> bool {
    parsed
        .protobuf
        .stmts
        .iter()
        .filter_map(|s| s.stmt.as_ref()?.node.as_ref())
        .any(|n| matches!(n, pg_query::NodeEnum::ViewStmt(_)))
}
```

`common::qualify_name_str` must be reachable — if Task 1 left it in `extractors.rs`, either make it `pub(super)` there and import it, or move it now. Report which you did.

- [ ] **Step 4: Confirm the unit tests pass**

Run: `cargo test -p dbd-core --lib parser::pg::views > /tmp/t.log 2>&1; echo "exit: $?"; tail -10 /tmp/t.log`
Expected: exit 0, 9 tests pass.

- [ ] **Step 5: Narrow the parity gate for soft references**

In `crates/dbd-core/tests/parser_parity.rs`, add above the comparison:

```rust
/// Strip soft (function) references before comparing.
///
/// Postgres parses `COALESCE`, `NULLIF`, `GREATEST` and `LEAST` as dedicated
/// expression nodes rather than function calls, so libpg_query does not report
/// them; sqlparser does. The difference is inert — those names resolve to no
/// entity, so `resolve_references` drops them on both sides and the apply graph
/// is identical — but it would fail a whole-Entity comparison on any view using
/// `COALESCE`, which is very common SQL.
///
/// This is the ONLY narrowing of the gate. Relation references, which drive
/// apply order, are still compared exactly.
fn without_soft_refs(entity: &Entity) -> serde_json::Value {
    let mut e = entity.clone();
    e.references.retain(|r| r.ref_type.as_deref() != Some("function"));
    e.refers = e.references.iter().map(|r| r.name.clone()).collect();
    serde_json::to_value(&e).expect("Entity serializes")
}
```

Replace the `assert_eq!(json(&old), json(&new), …)` call with `without_soft_refs`. Keep the improvement arm unchanged.

`REF_TYPE_FUNCTION`'s value is `"function"` — verify that in `crates/dbd-core/src/entity.rs` and use the constant if it is exported to integration tests; otherwise the literal with a comment naming the constant.

- [ ] **Step 6: Add the corpus**

`tests/fixtures/parser_corpus/ddl/view/app/plain.ddl`:
```sql
set search_path to app;

create view plain as select id, name from t where id > 0;
```

`tests/fixtures/parser_corpus/ddl/view/app/joins_and_cte.ddl`:
```sql
set search_path to app;

create view joins_and_cte as
with recent as (select id from t where id > 100)
select r.id, u.email
from recent r
join shop.users u on u.id = r.id;
```

`tests/fixtures/parser_corpus/ddl/view/app/calls_a_function.ddl`:
```sql
set search_path to app;

create view calls_a_function as select app.myfn(id) as v from t;
```

`tests/fixtures/parser_corpus/ddl/view/app/special_forms.ddl`:
```sql
set search_path to app;

create view special_forms as
select coalesce(name, 'none') as n, nullif(id, 0) as i, greatest(a, b) as g
from t;
```

`tests/fixtures/parser_corpus/ddl/view/app/trim_syntax.ddl` — sqlparser rejects this (`Expected: ), found: a`), libpg_query reads it, so it exercises the gate's improvement arm:
```sql
set search_path to app;

create view trim_syntax as select trim(both from name) as n from t;
```

- [ ] **Step 7: Switch View over**

In `pg/mod.rs`, add `EntityType::View` to `COVERED` and add the `native()` arm:

```rust
            EntityType::View => Some(views::parse_view),
```

The `covered_and_dispatch_cannot_drift` test will now require both — that is the guard working.

- [ ] **Step 8: Run the gate**

Run: `cargo test -p dbd-core --test parser_parity > /tmp/p.log 2>&1; echo "exit: $?"; tail -30 /tmp/p.log`
Expected: exit 0.

**If it FAILS, do not adjust the test to pass.** Report the exact diff. A disagreement is the gate doing its job and the coordinator needs to see it — that is how the enum `search_paths` asymmetry was caught last increment.

- [ ] **Step 9: Full verification**

Run: `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → `exit: 0`
Run: `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → `exit: 0`

- [ ] **Step 10: Verify live — a view must still order after its dependencies**

Unit tests will not catch an apply-order regression.

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-view-check
rm -rf $R && mkdir -p $R/ddl/table/vw $R/ddl/function/vw $R/ddl/view/vw
cat > $R/design.yaml <<'YAML'
project:
  name: vw
  version: 1
source:
  dialect: postgresql
schemas:
  - vw
YAML
cat > $R/ddl/table/vw/t.ddl <<'DDL'
set search_path to vw;
create table if not exists t (id int primary key, name text);
DDL
cat > $R/ddl/function/vw/myfn.ddl <<'DDL'
set search_path to vw;
create or replace function myfn(n int) returns int language sql immutable as $$ select n * 2 $$;
DDL
cat > $R/ddl/view/vw/v.ddl <<'DDL'
set search_path to vw;
create or replace view v as select id, myfn(id) as doubled, coalesce(name, 'none') as n from t;
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_vw' -c 'CREATE DATABASE dbd_vw'
cd $R
$DBD graph -s . | python3 -c "import json,sys; g=json.load(sys.stdin); print('edges :',g['edges']); print('layers:',g['layers'])"
$DBD apply -d postgresql://Jerry@localhost/dbd_vw -s .
$DBD reconcile -d postgresql://Jerry@localhost/dbd_vw -s . | tail -1
$DBD reconcile -d postgresql://Jerry@localhost/dbd_vw -s . | tail -1
$DBD diff -d postgresql://Jerry@localhost/dbd_vw -s . | tail -1
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_vw'
```

**Required:** the graph must show `vw.v -> vw.t` AND `vw.v -> vw.myfn` (the soft function reference resolving to a real project entity — this is what proves function refs are not merely decorative); apply succeeds; both reconciles report `0 created, 0 altered`; diff reports in sync.

Report the actual output, not a summary.

- [ ] **Step 11: Commit**

```bash
git add crates/dbd-core/src/parser/pg/ crates/dbd-core/tests/parser_parity.rs tests/fixtures/parser_corpus/ddl/view
git commit -F - <<'MSG'
feat(parser): parse views with libpg_query behind the parity gate

Second entity type to go native. Relations come from select_tables() and
function calls from call_functions(); together they match the incumbent on
every view body measured except SQL special forms.

Postgres parses COALESCE, NULLIF, GREATEST and LEAST as dedicated
expression nodes rather than function calls, so libpg_query does not
report them and sqlparser does. That difference is inert — the names
resolve to no entity, so resolve_references drops them on both sides — but
it would fail a whole-Entity comparison on very common SQL. The gate now
excludes soft function references and compares relation references, which
drive apply order, exactly. That is its only narrowing.

MaterializedView is deliberately NOT included: it stores body text in
writes[0], and the two parsers' renderings differ on ~30% of realistic
bodies (a::TEXT vs a::text, coalesce vs COALESCE, paren preservation), so
byte parity is unreachable without first changing that contract.
MSG
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -p dbd-core --test parser_parity` exits 0 and is non-vacuous
- [ ] No `use crate::parser::extractors` remains under `parser/pg/`
- [ ] Live: `vw.v -> vw.myfn` edge present, two reconciles at `0 created, 0 altered`
- [ ] `MaterializedView` is NOT in `COVERED`

## Follow-ups this plan creates

- **MaterializedView** needs its own spec, resolving whether `writes[0]` should hold verbatim source (sliced via `RawStmt.stmt_location`/`stmt_len`) instead of a re-rendering. That change touches `emit` output.
- **`trim(both from a)`** is an 8th construct sqlparser rejects that Postgres accepts, found while writing this plan. Worth adding to the spec's rejection table.
