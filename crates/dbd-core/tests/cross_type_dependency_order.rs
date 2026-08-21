//! Cross-type dependency ordering (issue #9).
//!
//! A view that calls a project-managed function must be applied after that
//! function, and a function whose body reads a view must be applied after the
//! view. Both directions exist, so neither can be fixed by reordering the type
//! sequence — the apply order has to come from the dependency graph itself.
//!
//! These tests drive the real `Design::from_config` path (no database), so they
//! assert the order `dbd apply` actually executes.

use dbd_core::{Design, EntityType};
use std::path::{Path, PathBuf};

/// Write a throwaway dbd project under `tests/.tmp/<name>` and load it.
fn design_for(name: &str, files: &[(&str, &str)]) -> Design {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/.tmp")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: dbd_issue9\nsource:\n  dialect: postgresql\n\
         target:\n  postgres:\n    url: postgres://localhost/unused\nschemas:\n  - app\n",
    )
    .unwrap();

    for (rel, sql) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, sql).unwrap();
    }

    Design::from_config(&dir.join("design.yaml"), "dev").unwrap()
}

/// Position of an entity in the apply order, with a readable failure.
fn position(design: &Design, name: &str) -> usize {
    design
        .entities()
        .iter()
        .position(|e| e.name == name)
        .unwrap_or_else(|| {
            panic!(
                "{name} not in entities: {:?}",
                design.entities().iter().map(|e| &e.name).collect::<Vec<_>>()
            )
        })
}

/// Every entity that reached the apply order must be error-free — a cyclic flag
/// silently removes an entity from `apply` (see `Design::entities_in_scope`), so
/// a "passing" order assertion on a dropped entity would be meaningless.
fn assert_no_errors(design: &Design) {
    let bad: Vec<_> = design
        .entities()
        .iter()
        .filter(|e| !e.errors.is_empty())
        .map(|e| (e.name.clone(), e.errors.clone()))
        .collect();
    assert!(bad.is_empty(), "entities carry errors: {bad:?}");
}

const SCALAR_FN: (&str, &str) = (
    "ddl/function/app/scalar_fn.ddl",
    "set search_path to app;\n\
     create or replace function scalar_fn(n integer) returns integer\n\
     language sql immutable as $$ select n * 2 $$;\n",
);

const SRF_FN: (&str, &str) = (
    "ddl/function/app/srf_fn.ddl",
    "set search_path to app;\n\
     create or replace function srf_fn() returns table(n integer)\n\
     language sql immutable as $$ select generate_series(1,3)::int $$;\n",
);

/// The issue's headline case: a function called from the view's TARGET LIST.
/// No edge is extracted for this today, so it is a detection gap.
#[test]
fn view_calling_function_in_target_list_is_applied_after_it() {
    let design = design_for(
        "target_list",
        &[
            SCALAR_FN,
            (
                "ddl/view/app/v_target_list.ddl",
                "set search_path to app;\n\
                 create or replace view v_target_list as select app.scalar_fn(1) as doubled;\n",
            ),
        ],
    );
    assert_no_errors(&design);

    let view = design
        .entities()
        .iter()
        .find(|e| e.name == "app.v_target_list")
        .unwrap();
    assert!(
        view.refers.contains(&"app.scalar_fn".to_string()),
        "target-list function call should be a dependency: {:?}",
        view.refers
    );
    assert!(
        position(&design, "app.scalar_fn") < position(&design, "app.v_target_list"),
        "function must be applied before the view that calls it"
    );
}

/// A function called from the view's FROM clause. sqlparser already models a
/// table function as a relation, so the edge is detected today — this asserts
/// the *ordering*, which the per-type-bucket sort used to discard.
#[test]
fn view_calling_function_in_from_clause_is_applied_after_it() {
    let design = design_for(
        "from_clause",
        &[
            SRF_FN,
            (
                "ddl/view/app/v_from_clause.ddl",
                "set search_path to app;\n\
                 create or replace view v_from_clause as select n from app.srf_fn();\n",
            ),
        ],
    );
    assert_no_errors(&design);
    assert!(
        position(&design, "app.srf_fn") < position(&design, "app.v_from_clause"),
        "function must be applied before the view that selects from it"
    );
}

/// An unqualified call, resolved through the view's own `search_path`.
#[test]
fn view_calling_unqualified_function_is_applied_after_it() {
    let design = design_for(
        "bare_call",
        &[
            SCALAR_FN,
            (
                "ddl/view/app/v_bare.ddl",
                "set search_path to app;\n\
                 create or replace view v_bare as select scalar_fn(1) as doubled;\n",
            ),
        ],
    );
    assert_no_errors(&design);
    assert!(
        position(&design, "app.scalar_fn") < position(&design, "app.v_bare"),
        "bare function call must still order the function first"
    );
}

/// The reverse direction: a `LANGUAGE sql` function body that reads a view. The
/// edge is already extracted into `reads`; only the ordering was broken. This is
/// why the type sequence must NOT simply be reordered to put functions first.
#[test]
fn function_reading_a_view_is_applied_after_it() {
    let design = design_for(
        "reverse",
        &[
            (
                "ddl/view/app/v_base.ddl",
                "set search_path to app;\ncreate or replace view v_base as select 1 as x;\n",
            ),
            (
                "ddl/function/app/counts.ddl",
                "set search_path to app;\n\
                 create or replace function counts() returns bigint\n\
                 language sql stable as $$ select count(*) from app.v_base $$;\n",
            ),
        ],
    );
    assert_no_errors(&design);
    assert!(
        position(&design, "app.v_base") < position(&design, "app.counts"),
        "view must be applied before the function whose body reads it"
    );
}

/// Both directions in one project, which no fixed type sequence can satisfy.
#[test]
fn both_directions_ordered_in_one_project() {
    let design = design_for(
        "both",
        &[
            SCALAR_FN,
            (
                "ddl/view/app/v_uses_fn.ddl",
                "set search_path to app;\n\
                 create or replace view v_uses_fn as select app.scalar_fn(2) as doubled;\n",
            ),
            (
                "ddl/function/app/reads_view.ddl",
                "set search_path to app;\n\
                 create or replace function reads_view() returns bigint\n\
                 language sql stable as $$ select count(*) from app.v_uses_fn $$;\n",
            ),
        ],
    );
    assert_no_errors(&design);

    let scalar_fn = position(&design, "app.scalar_fn");
    let view = position(&design, "app.v_uses_fn");
    let reads_view = position(&design, "app.reads_view");
    assert!(
        scalar_fn < view && view < reads_view,
        "expected scalar_fn < v_uses_fn < reads_view, got {scalar_fn} / {view} / {reads_view}"
    );
}

/// Built-in and aggregate calls must not become dependencies, and must not
/// produce "Unresolved reference" warnings — a view body is full of them.
#[test]
fn builtin_function_calls_are_not_dependencies_and_do_not_warn() {
    let design = design_for(
        "builtins",
        &[
            (
                "ddl/table/app/events.ddl",
                "set search_path to app;\n\
                 create table if not exists events (id int primary key, ts timestamptz, amt numeric);\n",
            ),
            (
                "ddl/view/app/v_rollup.ddl",
                "set search_path to app;\n\
                 create or replace view v_rollup as\n\
                 select date_trunc('day', ts) as day, sum(amt) as total, count(*) as n,\n\
                        coalesce(max(amt), 0) as peak, now() as generated_at\n\
                   from app.events group by 1;\n",
            ),
        ],
    );
    assert_no_errors(&design);

    let view = design
        .entities()
        .iter()
        .find(|e| e.name == "app.v_rollup")
        .unwrap();
    assert_eq!(
        view.refers,
        vec!["app.events".to_string()],
        "only the real table should be a dependency"
    );
    assert!(
        view.warnings.is_empty(),
        "built-in calls must not warn: {:?}",
        view.warnings
    );
}

/// A built-in set-returning function in the FROM clause is a function call, not
/// a missing table, so it must not be reported as an unresolved reference.
#[test]
fn builtin_set_returning_function_in_from_clause_does_not_warn() {
    let design = design_for(
        "from_builtin",
        &[(
            "ddl/view/app/v_series.ddl",
            "set search_path to app;\n\
             create or replace view v_series as select g as n from generate_series(1, 3) g;\n",
        )],
    );
    assert_no_errors(&design);

    let view = design
        .entities()
        .iter()
        .find(|e| e.name == "app.v_series")
        .unwrap();
    assert!(
        view.warnings.is_empty(),
        "built-in SRF in FROM must not warn: {:?}",
        view.warnings
    );
    assert!(
        view.refers.is_empty(),
        "built-in SRF must not be a dependency: {:?}",
        view.refers
    );
}

/// Type ordering still decides when no dependency does: with no cross-type
/// edges, the existing sequence (schemas → tables → views → matviews →
/// functions) is unchanged.
#[test]
fn type_order_still_applies_without_dependencies() {
    let design = design_for(
        "type_order",
        &[
            (
                "ddl/table/app/t.ddl",
                "set search_path to app;\ncreate table if not exists t (id int primary key);\n",
            ),
            (
                "ddl/view/app/v.ddl",
                "set search_path to app;\ncreate or replace view v as select 1 as x;\n",
            ),
            (
                "ddl/materialized_view/app/mv.ddl",
                "set search_path to app;\ncreate materialized view mv as select 1 as x;\n",
            ),
            (
                "ddl/function/app/f.ddl",
                "set search_path to app;\n\
                 create or replace function f() returns int language sql immutable as $$ select 1 $$;\n",
            ),
        ],
    );
    assert_no_errors(&design);

    // Schemas first, then the type sequence among mutually independent entities.
    let schema = position(&design, "app");
    let t = position(&design, "app.t");
    let v = position(&design, "app.v");
    let mv = position(&design, "app.mv");
    let f = position(&design, "app.f");
    assert!(
        schema < t && t < v && v < mv && mv < f,
        "expected app < t < v < mv < f, got {schema} / {t} / {v} / {mv} / {f}"
    );
    assert_eq!(
        design.entities()[schema].entity_type,
        EntityType::Schema,
        "sanity: `app` is the schema entity"
    );
}

/// A matview calling a function has the same requirement as a view.
#[test]
fn matview_calling_function_is_applied_after_it() {
    let design = design_for(
        "matview",
        &[
            SCALAR_FN,
            (
                "ddl/materialized_view/app/mv_doubled.ddl",
                "set search_path to app;\n\
                 create materialized view mv_doubled as select app.scalar_fn(3) as doubled;\n",
            ),
        ],
    );
    assert_no_errors(&design);
    assert!(
        position(&design, "app.scalar_fn") < position(&design, "app.mv_doubled"),
        "function must be applied before the matview that calls it"
    );
}

/// A `LANGUAGE sql` function calling another project function: Postgres
/// validates the body at CREATE time, so the callee must exist first. The names
/// are chosen so alphabetical order is the wrong order.
#[test]
fn function_calling_another_function_is_applied_after_it() {
    let design = design_for(
        "fn_to_fn",
        &[
            (
                "ddl/function/app/zzz_base.ddl",
                "set search_path to app;\n\
                 create or replace function zzz_base(n integer) returns integer\n\
                 language sql immutable as $$ select n * 2 $$;\n",
            ),
            (
                "ddl/function/app/aaa_caller.ddl",
                "set search_path to app;\n\
                 create or replace function aaa_caller(n integer) returns integer\n\
                 language sql immutable as $$ select app.zzz_base(n) + 1 $$;\n",
            ),
        ],
    );
    assert_no_errors(&design);
    assert!(
        position(&design, "app.zzz_base") < position(&design, "app.aaa_caller"),
        "callee must be applied before the caller, against alphabetical order"
    );
}

/// A PL/pgSQL body resolves names at run time, so a call it makes is not a
/// creation-order dependency and must not constrain the order (nor create a
/// false cycle with a function that calls back into it).
#[test]
fn plpgsql_function_calls_do_not_constrain_order() {
    let design = design_for(
        "plpgsql_calls",
        &[
            (
                "ddl/function/app/a_proc.ddl",
                "set search_path to app;\n\
                 create or replace function a_proc() returns void\n\
                 language plpgsql as $$ begin perform app.b_proc(); end; $$;\n",
            ),
            (
                "ddl/function/app/b_proc.ddl",
                "set search_path to app;\n\
                 create or replace function b_proc() returns void\n\
                 language plpgsql as $$ begin perform app.a_proc(); end; $$;\n",
            ),
        ],
    );
    // Mutual recursion across PL/pgSQL bodies is legal in Postgres, so it must
    // not be reported as a dependency cycle (which would drop both from apply).
    assert_no_errors(&design);
}

/// A built-in set-returning function in a function body's FROM clause is a call,
/// not a missing table, so it must not warn.
#[test]
fn function_body_from_clause_builtin_does_not_warn() {
    let design = design_for(
        "body_from_builtin",
        &[
            (
                "ddl/table/app/docs.ddl",
                "set search_path to app;\n\
                 create table if not exists docs (id int primary key, tsv tsvector);\n",
            ),
            (
                "ddl/function/app/search.ddl",
                "set search_path to app;\n\
                 create or replace function search(q text) returns bigint\n\
                 language sql stable as $$\n\
                   select count(*) from app.docs d, websearch_to_tsquery('english', q) tsq\n\
                    where d.tsv @@ tsq $$;\n",
            ),
        ],
    );
    assert_no_errors(&design);

    let func = design
        .entities()
        .iter()
        .find(|e| e.name == "app.search")
        .unwrap();
    assert!(
        func.warnings.is_empty(),
        "FROM-clause built-in in a function body must not warn: {:?}",
        func.warnings
    );
    assert_eq!(
        func.refers,
        vec!["app.docs".to_string()],
        "only the real table should remain a dependency"
    );
}

/// Every table is applied before any view, matview, function or procedure, even
/// when the routine has no detected dependencies and the table sits deep in an
/// FK chain.
///
/// This is why views/functions are sorted as their own group rather than folded
/// into one global sort: body extraction is best-effort, and Postgres compiles a
/// `LANGUAGE sql` body at creation time, so a routine whose table reference was
/// missed must still land after every table.
#[test]
fn all_tables_precede_every_view_and_routine() {
    let design = design_for(
        "tables_first",
        &[
            (
                "ddl/table/app/a.ddl",
                "set search_path to app;\ncreate table if not exists a (id int primary key);\n",
            ),
            (
                "ddl/table/app/b.ddl",
                "set search_path to app;\n\
                 create table if not exists b (id int primary key, a_id int references app.a(id));\n",
            ),
            (
                "ddl/table/app/c.ddl",
                "set search_path to app;\n\
                 create table if not exists c (id int primary key, b_id int references app.b(id));\n",
            ),
            (
                "ddl/view/app/v_free.ddl",
                "set search_path to app;\ncreate or replace view v_free as select 1 as x;\n",
            ),
            (
                "ddl/function/app/f_free.ddl",
                "set search_path to app;\n\
                 create or replace function f_free() returns int language sql immutable as $$ select 1 $$;\n",
            ),
        ],
    );
    assert_no_errors(&design);

    let last_table = design
        .entities()
        .iter()
        .rposition(|e| e.entity_type == EntityType::Table)
        .expect("no tables");
    let first_routine = design
        .entities()
        .iter()
        .position(|e| {
            matches!(
                e.entity_type,
                EntityType::View
                    | EntityType::MaterializedView
                    | EntityType::Function
                    | EntityType::Procedure
            )
        })
        .expect("no routines");

    assert!(
        last_table < first_routine,
        "a routine ({}) was scheduled before the last table ({})",
        design.entities()[first_routine].name,
        design.entities()[last_table].name
    );
}

/// Sanity: the fixture project still loads and orders without errors, so the
/// grouped sort has not introduced a cycle in real-world DDL.
#[test]
fn fixture_project_orders_without_cycles() {
    let config = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/design.yaml");
    assert!(Path::new(&config).exists(), "fixture design.yaml missing");
    let design = Design::from_config(&config, "dev").unwrap();
    let cyclic: Vec<_> = design
        .entities()
        .iter()
        .filter(|e| e.errors.iter().any(|m| m.contains("Cyclic")))
        .map(|e| e.name.clone())
        .collect();
    assert!(cyclic.is_empty(), "fixture project reports cycles: {cyclic:?}");
}
