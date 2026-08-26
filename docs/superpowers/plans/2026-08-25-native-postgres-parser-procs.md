# Native Postgres Parser — Function/Procedure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `EntityType::Function` and `EntityType::Procedure` onto libpg_query, behind the existing differential parity gate.

**Architecture:** `PgQueryDdl::native()` gains `Function` and `Procedure` arms calling a new `pg/procs.rs`. The routine's `LANGUAGE` decides the extraction path — a `sql` body is parsed as SQL, a `plpgsql` body goes through the existing PL/pgSQL walker — because the incumbent treats called functions as creation-order dependencies for one and not the other.

**Tech Stack:** Rust 2024, `pg_query` 6, `sqlparser` 0.62.

**Spec:** `docs/superpowers/specs/2026-08-24-native-postgres-parser-design.md` (rollout step 3)

**Prior increments:** the enum plan (`2026-08-24-native-postgres-parser.md`) and the view plan (`2026-08-25-native-postgres-parser-views.md`). Read the enum plan's "Execution record" before starting.

---

## Why this is one task, not several

Under `clippy --all-targets -- -D warnings`, a `parse_proc` that nothing dispatches to is dead code. `#[cfg(test)]` callers do not clear it (test code is invisible to dead-code analysis) and neither do integration tests (separate crate, cannot see `pub(crate)`). This was learned twice in the enum increment and once more in the view increment.

So parser, corpus and switchover land together. No prerequisite refactor is needed — the `pg` ↔ `extractors` cycle was already broken in the view increment, and both helpers this needs (`extract_proc_refs_via_pg_query`, `qualify_name_str`) already live in `pg/common.rs`.

---

## What the incumbent produces, and the rule that matters

The sqlparser arm calls `extractors::extract_proc_refs`, which fills `reads`, `writes` and `functions` via `apply_proc_refs`. It has three tiers:

1. **`LANGUAGE sql` bodies** — re-parsed with sqlparser, walked by the view visitor. Yields reads, writes **and called functions**.
2. **PL/pgSQL bodies** — `pg_query::parse_plpgsql`, in `pg::common::extract_proc_refs_via_pg_query`. Yields reads and writes, **`functions` deliberately empty**.
3. Anything neither accepts — regex scan.

That tier-1-only rule for called functions is deliberate and load-bearing. From the incumbent's own doc comment:

> Postgres validates a `LANGUAGE sql` body when the routine is created (with the default `check_function_bodies = on`), so a function it calls must exist first. A PL/pgSQL body resolves names at run time instead, so its calls are not creation-order dependencies.

**Reproducing this exactly is the point of the task.** Collecting called functions for PL/pgSQL would add phantom edges to the apply graph; omitting them for `LANGUAGE sql` would drop real ones.

## Measured parity

Across 7 routine bodies, the existing libpg_query tier agrees with the incumbent on **3 of 3 PL/pgSQL** cases and **0 of 4 `LANGUAGE sql`** cases — it only reads PL/pgSQL blocks.

The fix is to extract the body and parse it as SQL. Measured, that reproduces the incumbent **exactly** on all four:

| body | sqlparser | body parsed with `pg_query` |
| --- | --- | --- |
| `select count(*) from t` | r=`[app.t]` f=`[app.count]` | **identical** |
| `select app.myfn(1)` | f=`[app.myfn]` | **identical** |
| `insert into t(a) values (1)` | w=`[app.t]` | **identical** |
| `select * from t` (setof) | r=`[app.t]` | **identical** |

## Verified AST shape (do not re-derive)

`CREATE FUNCTION` and `CREATE PROCEDURE` both parse to `pg_query::NodeEnum::CreateFunctionStmt`, distinguished by its `is_procedure: bool`. Its `options` is a `Vec<Node>` of `DefElem`:

- `defname == "language"` → `arg` is `NodeEnum::String { sval }`, e.g. `"sql"` or `"plpgsql"`.
- `defname == "as"` → `arg` is `NodeEnum::List { items }` whose first item is `NodeEnum::String { sval }` holding the body text.

Order-independent: it works when `LANGUAGE` follows the body (`… as $$ … $$ language sql;`).

---

## Task 1: Native routine parsing, corpus and switchover

**Files:**
- Create: `crates/dbd-core/src/parser/pg/procs.rs`
- Modify: `crates/dbd-core/src/parser/pg/mod.rs`
- Create: `tests/fixtures/parser_corpus/ddl/function/app/*.ddl`
- Create: `tests/fixtures/parser_corpus/ddl/procedure/app/*.ddl`

- [ ] **Step 1: Write the failing tests**

Create `crates/dbd-core/src/parser/pg/procs.rs` with ONLY this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, REF_TYPE_FUNCTION};

    fn parse(sql: &str) -> Entity {
        parse_proc(Entity::new(EntityType::Function, "app.f"), sql).unwrap()
    }

    #[test]
    fn sql_body_reads_are_captured() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language sql as $$ select count(*) from t $$;",
        );
        assert_eq!(e.reads, vec!["app.t".to_string()], "got {:?}", e.reads);
        assert!(e.errors.is_empty(), "got {:?}", e.errors);
    }

    #[test]
    fn sql_body_writes_are_captured() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns void language sql as $$ insert into t(a) values (1) $$;",
        );
        assert_eq!(e.writes, vec!["app.t".to_string()], "got {:?}", e.writes);
    }

    /// Postgres validates a `LANGUAGE sql` body at creation, so a function it
    /// calls must exist first — the call IS a creation-order dependency.
    #[test]
    fn sql_body_calls_become_soft_function_references() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language sql as $$ select app.myfn(1) $$;",
        );
        let myfn = e
            .references
            .iter()
            .find(|r| r.name == "app.myfn")
            .expect("called function missing");
        assert_eq!(myfn.ref_type.as_deref(), Some(REF_TYPE_FUNCTION));
    }

    #[test]
    fn plpgsql_body_reads_and_writes_are_captured() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns void language plpgsql as $$ begin insert into t(a) values (1); end $$;",
        );
        assert_eq!(e.writes, vec!["app.t".to_string()], "got {:?}", e.writes);
    }

    /// A PL/pgSQL body resolves names at run time, so its calls are NOT
    /// creation-order dependencies. The incumbent omits them deliberately;
    /// collecting them here would add phantom edges to the apply graph.
    #[test]
    fn plpgsql_body_calls_are_not_collected() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int language plpgsql as $$ begin return app.myfn(1); end $$;",
        );
        assert!(
            !e.refers.iter().any(|r| r == "app.myfn"),
            "plpgsql calls must not become dependencies, got {:?}",
            e.refers
        );
    }

    #[test]
    fn language_after_the_body_is_still_detected() {
        let e = parse(
            "set search_path to app;\n\
             create function f() returns int as $$ select count(*) from t $$ language sql;",
        );
        assert_eq!(e.reads, vec!["app.t".to_string()], "got {:?}", e.reads);
    }

    #[test]
    fn a_procedure_is_parsed_like_a_function() {
        let e = parse_proc(
            Entity::new(EntityType::Procedure, "app.p"),
            "set search_path to app;\n\
             create procedure p() language plpgsql as $$ begin insert into t(a) values (1); end $$;",
        )
        .unwrap();
        assert_eq!(e.writes, vec!["app.t".to_string()], "got {:?}", e.writes);
    }

    #[test]
    fn search_path_is_captured() {
        let e = parse("set search_path to app;\ncreate function f() returns int language sql as $$ select 1 $$;");
        assert_eq!(e.search_paths, vec!["app".to_string()]);
    }

    #[test]
    fn missing_search_path_defaults_to_public() {
        let e = parse("create function f() returns int language sql as $$ select 1 $$;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create function f() returns int language sql as ;;;");
        assert!(!e.errors.is_empty(), "invalid SQL must error");
        assert!(e.errors[0].contains("syntax error at or near"), "got {:?}", e.errors);
    }

    /// Mirrors the enum and view parsers.
    #[test]
    fn an_errored_routine_still_has_a_search_path() {
        let e = parse("create function f() returns int language sql as ;;;");
        assert_eq!(e.search_paths, vec!["public".to_string()]);
    }

    #[test]
    fn a_file_declaring_no_routine_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty(), "a routine file with no CREATE FUNCTION must error");
    }
}
```

- [ ] **Step 2: Register and confirm they fail**

In `pg/mod.rs` add `pub(crate) mod procs;` beside the existing module declarations.

Run: `cargo test -p dbd-core --lib parser::pg::procs > /tmp/t.log 2>&1; echo "exit: $?"; tail -10 /tmp/t.log`
Expected: non-zero — `cannot find function parse_proc in this scope`.

(Register FIRST — an unregistered file is not compiled and gives a misleading pass.)

- [ ] **Step 3: Implement**

Prepend to `crates/dbd-core/src/parser/pg/procs.rs`:

```rust
//! Function and procedure DDL, parsed with libpg_query.
//!
//! The routine's `LANGUAGE` decides how its body is read, and that split is
//! load-bearing rather than an optimisation. Postgres validates a `LANGUAGE sql`
//! body when the routine is created (`check_function_bodies = on` by default),
//! so a function it calls must exist first — the call is a creation-order
//! dependency. A PL/pgSQL body resolves names at run time, so its calls are not.
//! Collecting calls from a PL/pgSQL body would put phantom edges in the apply
//! graph; omitting them from a `LANGUAGE sql` body would drop real ones.

use crate::entity::{Entity, REF_TYPE_FUNCTION, Reference};
use crate::error::Result;

use super::common;

/// Parse a function or procedure DDL file.
pub(crate) fn parse_proc(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Before any early return: references are qualified against the search path,
    // and an errored entity reporting `[]` instead of the `["public"]` default is
    // an invariant break the enum parser already hit once.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    let Some(routine) = routine_body(&parsed) else {
        entity
            .errors
            .push("this file declares no `CREATE FUNCTION` or `CREATE PROCEDURE`".to_string());
        return Ok(entity);
    };

    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    let (reads, writes, functions) = match routine.language.as_str() {
        // A SQL body is itself SQL: parse it directly. Called functions count,
        // because Postgres resolves them when the routine is created.
        "sql" => {
            let Ok(body) = pg_query::parse(&routine.body) else {
                // The body is not standalone-parseable (e.g. a bare expression
                // body). Fall back to the PL/pgSQL walker rather than erroring:
                // the routine itself is valid, we just cannot read its refs.
                let (r, w) = common::extract_proc_refs_via_pg_query(sql, &default_schema)
                    .unwrap_or_default();
                return Ok(finish(entity, r, w, Vec::new()));
            };
            (
                qualify_all(body.select_tables(), &default_schema),
                qualify_all(body.dml_tables(), &default_schema),
                qualify_all(body.call_functions(), &default_schema),
            )
        }
        // A PL/pgSQL body resolves names at run time, so its calls are NOT
        // creation-order dependencies — `functions` stays empty deliberately.
        _ => {
            let (r, w) = common::extract_proc_refs_via_pg_query(sql, &default_schema)
                .unwrap_or_default();
            (r, w, Vec::new())
        }
    };

    Ok(finish(entity, reads, writes, functions))
}

/// Fill the entity's reference fields.
///
/// Mirrors `parser::apply_proc_refs`: reads and writes become hard references,
/// called functions become soft ones tagged [`REF_TYPE_FUNCTION`], which
/// `references::resolve_references` keeps only when they name a known entity.
fn finish(mut entity: Entity, reads: Vec<String>, writes: Vec<String>, functions: Vec<String>) -> Entity {
    let mut references: Vec<Reference> = reads
        .iter()
        .chain(writes.iter())
        .map(|name| Reference {
            name: name.clone(),
            ref_type: None,
        })
        .collect();
    for name in functions {
        if references.iter().any(|r| r.name == name) {
            continue;
        }
        references.push(Reference {
            name,
            ref_type: Some(REF_TYPE_FUNCTION.to_string()),
        });
    }
    entity.refers = references.iter().map(|r| r.name.clone()).collect();
    entity.references = references;
    entity.reads = reads;
    entity.writes = writes;
    entity
}

/// Qualify libpg_query's bare names and sort them.
///
/// Sorted because `select_tables`/`dml_tables`/`call_functions` are built from a
/// `HashSet`, so their order differs on every process run — see the determinism
/// fix in `common::extract_view_refs_via_pg_query`.
fn qualify_all(names: Vec<String>, default_schema: &str) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .filter_map(|n| common::qualify_name_str(n, default_schema))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The `LANGUAGE` and body text of the first routine in the file.
struct Routine {
    language: String,
    body: String,
}

fn routine_body(parsed: &pg_query::ParseResult) -> Option<Routine> {
    for stmt in &parsed.protobuf.stmts {
        let Some(pg_query::NodeEnum::CreateFunctionStmt(create)) =
            stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        else {
            continue;
        };
        let (mut language, mut body) = (None, None);
        for opt in &create.options {
            let Some(pg_query::NodeEnum::DefElem(def)) = opt.node.as_ref() else {
                continue;
            };
            match def.defname.as_str() {
                "language" => {
                    if let Some(pg_query::NodeEnum::String(s)) =
                        def.arg.as_ref().and_then(|a| a.node.as_ref())
                    {
                        language = Some(s.sval.to_lowercase());
                    }
                }
                "as" => {
                    if let Some(pg_query::NodeEnum::List(list)) =
                        def.arg.as_ref().and_then(|a| a.node.as_ref())
                        && let Some(pg_query::NodeEnum::String(s)) =
                            list.items.first().and_then(|i| i.node.as_ref())
                    {
                        body = Some(s.sval.clone());
                    }
                }
                _ => {}
            }
        }
        return Some(Routine {
            language: language.unwrap_or_default(),
            body: body.unwrap_or_default(),
        });
    }
    None
}
```

If a type path is wrong, fix it and REPORT the correct one. If `common::qualify_name_str` is not visible from `procs.rs`, widen it the same way the view increment did (`pub(in crate::parser)`) and say so.

- [ ] **Step 4: Confirm the unit tests pass**

Run: `cargo test -p dbd-core --lib parser::pg::procs > /tmp/t.log 2>&1; echo "exit: $?"; tail -12 /tmp/t.log`
Expected: exit 0, 12 tests pass.

- [ ] **Step 5: Add the corpus**

`tests/fixtures/parser_corpus/ddl/function/app/sql_reads.ddl`:
```sql
set search_path to app;

create or replace function sql_reads() returns bigint language sql stable
as $$ select count(*) from t $$;
```

`tests/fixtures/parser_corpus/ddl/function/app/sql_calls.ddl`:
```sql
set search_path to app;

create or replace function sql_calls(n int) returns int language sql immutable
as $$ select app.myfn(n) $$;
```

`tests/fixtures/parser_corpus/ddl/function/app/plpgsql_writes.ddl`:
```sql
set search_path to app;

create or replace function plpgsql_writes() returns void language plpgsql
as $$
begin
  insert into t(a) values (1);
end
$$;
```

`tests/fixtures/parser_corpus/ddl/function/app/language_last.ddl`:
```sql
set search_path to app;

create or replace function language_last() returns bigint
as $$ select count(*) from t $$ language sql;
```

`tests/fixtures/parser_corpus/ddl/procedure/app/proc_writes.ddl`:
```sql
set search_path to app;

create or replace procedure proc_writes() language plpgsql
as $$
begin
  insert into t(a) values (1);
end
$$;
```

- [ ] **Step 6: Switch Function and Procedure over**

In `pg/mod.rs`, add both to `COVERED` and to the `native()` match:

```rust
            EntityType::Function | EntityType::Procedure => Some(procs::parse_proc),
```

`covered_and_dispatch_cannot_drift` requires both — the guard working is expected.

- [ ] **Step 7: Run the gate**

Run: `cargo test -p dbd-core --test parser_parity > /tmp/p.log 2>&1; echo "exit: $?"; tail -40 /tmp/p.log`

**If it FAILS, do NOT adjust the test, the fixtures, or the gate to make it pass.** Report the exact diff. A disagreement is the gate doing its job — that is how the enum `search_paths` asymmetry and the libpg_query ordering nondeterminism were both caught.

Then run it **three times in a row** and confirm it passes each time. `select_tables`/`dml_tables`/`call_functions` are `HashSet`-derived, so a single pass can be luck if the sort in `qualify_all` were wrong.

- [ ] **Step 8: Full verification**

- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

- [ ] **Step 9: Verify live — the creation-order rule is the thing unit tests miss**

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-proc-check
rm -rf $R; mkdir -p $R/ddl/table/pr $R/ddl/function/pr
cat > $R/design.yaml <<'YAML'
project:
  name: pr
  version: 1
source:
  dialect: postgresql
schemas:
  - pr
YAML
cat > $R/ddl/table/pr/t.ddl <<'DDL'
set search_path to pr;
create table if not exists t (id int primary key, a int);
DDL
cat > $R/ddl/function/pr/base.ddl <<'DDL'
set search_path to pr;
create or replace function base(n int) returns int language sql immutable as $$ select n * 2 $$;
DDL
cat > $R/ddl/function/pr/caller.ddl <<'DDL'
set search_path to pr;
create or replace function caller() returns int language sql stable as $$ select pr.base(count(*)::int) from t $$;
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_pr' -c 'CREATE DATABASE dbd_pr'
cd $R
$DBD graph -s . | python3 -c "import json,sys; g=json.load(sys.stdin); print('edges :',g['edges']); print('layers:',g['layers'])"
$DBD apply -d postgresql://Jerry@localhost/dbd_pr -s .
$DBD reconcile -d postgresql://Jerry@localhost/dbd_pr -s . | tail -1
$DBD reconcile -d postgresql://Jerry@localhost/dbd_pr -s . | tail -1
$DBD diff -d postgresql://Jerry@localhost/dbd_pr -s . | tail -1
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_pr'
```

**Required:** the graph shows `pr.caller -> pr.base` (the `LANGUAGE sql` call becoming a real creation-order edge — this is the whole point of the language split) and `pr.caller -> pr.t`; `pr.base` appears in an earlier layer than `pr.caller`; apply succeeds; both reconciles report `0 created, 0 altered`; diff in sync.

Report the ACTUAL output. If the `pr.caller -> pr.base` edge is missing, that is a real finding — report it rather than papering over it.

- [ ] **Step 10: Commit**

```bash
git add crates/dbd-core/src/parser/pg/ tests/fixtures/parser_corpus/ddl/function tests/fixtures/parser_corpus/ddl/procedure
git commit -F - <<'MSG'
feat(parser): parse functions and procedures with libpg_query

Third and fourth entity types to go native. The routine's LANGUAGE decides
how its body is read, and that split is load-bearing: Postgres validates a
LANGUAGE sql body at creation, so a function it calls must exist first and
the call is a creation-order dependency; a PL/pgSQL body resolves names at
run time, so its calls are not. Collecting calls from PL/pgSQL would add
phantom edges to the apply graph, and omitting them from LANGUAGE sql would
drop real ones.

The existing libpg_query tier only read PL/pgSQL blocks — measured, it
agreed with the incumbent on 3 of 3 PL/pgSQL bodies and 0 of 4 SQL bodies.
Extracting the body from the CreateFunctionStmt and parsing it as SQL
reproduces the incumbent exactly on all four.
MSG
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo test -p dbd-core --test parser_parity` exits 0 three consecutive times
- [ ] Live: `pr.caller -> pr.base` edge present, `pr.base` in an earlier layer
- [ ] `MaterializedView` still NOT in `COVERED`

## Follow-up this plan does not address

`common::extract_proc_refs_via_pg_query` returns `Some(([], []))` rather than `None` for a body that is not PL/pgSQL, which makes the regex tier beneath it unreachable in `extractors::extract_proc_refs`. Latent — tier 1 catches those bodies first, and in the case tested the regex tier would also have found nothing — but the chain does not fall through as its own doc comment describes. Worth fixing when the sqlparser path is retired.
