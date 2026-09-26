//! Every entity carries the namespace context its file established.
//!
//! # Why this exists
//!
//! An unqualified name in a SQL file means nothing on its own. `FROM lookup`
//! resolves against whatever namespace context is in force, and each dialect
//! states that differently:
//!
//! - **PostgreSQL** — `SET search_path TO a, b`, an ordered list of schemas.
//! - **T-SQL / MySQL** — `USE db`, which sets the database (dbd's *catalog*).
//!
//! dbd already recorded the PostgreSQL case. Three things were wrong with it,
//! all measured:
//!
//! 1. **A file that states nothing was indistinguishable from one stating
//!    `public`.** Both produced `["public"]`. But Postgres's real default is
//!    `"$user", public` — verified against a live server — so with a schema
//!    named after the connecting role, a bare `lookup` resolves to
//!    `<role>.lookup`, not `public.lookup`. dbd cannot know which at parse
//!    time, and said `public` as if it could.
//!
//! 2. **`"$user"` was passed through as though it were a schema name.** A file
//!    writing Postgres's own default produced a reference to `"$user".lookup`
//!    — a schema that cannot exist, so an edge that can never resolve.
//!
//! 3. **`USE db` was ignored entirely.** Over a 2,154-file T-SQL corpus, 77
//!    files (3.6%) carry one, declaring 243 entities of which **241 had no
//!    catalog** — so `Issues` in two databases merged into one entity, the
//!    exact collision `Entity::catalog` exists to prevent.
//!
//! # What this does NOT change
//!
//! What a bare reference is pre-qualified *to*. A dbd project with no
//! `SET search_path` still qualifies against `public`, because that is the
//! behaviour every existing project's apply path depends on. This makes the
//! guess *visible* — `SchemaPath::stated` is false — so a caller can decline
//! to trust it. Changing the fallback itself is a separate decision.

use dbd_core::entity::PathEntry;
use dbd_core::parser::{Dialect, parse_sql, parse_sql_as};

fn one(sql: &str) -> dbd_core::entity::Entity {
    let p = parse_sql(sql).expect("reads");
    p.entities.into_iter().next().expect("an entity")
}

// ── PostgreSQL: the path the file stated ────────────────────────────────────

#[test]
fn a_stated_search_path_is_recorded_in_order() {
    let e = one("set search_path to app, shared;\ncreate table t (id int);");
    assert!(e.schema_path.stated(), "the file said this");
    assert_eq!(
        e.schema_path.schemas().collect::<Vec<_>>(),
        vec!["app", "shared"],
        "order matters — it is resolution order"
    );
}

/// Every entity type, not just tables. A consumer resolving a function body's
/// references needs the same context a table's foreign key does.
#[test]
fn every_entity_type_carries_the_path() {
    let sp = "set search_path to app, shared;\n";
    for (label, ddl) in [
        ("table", "create table t (id int);"),
        ("view", "create view v as select 1;"),
        ("matview", "create materialized view m as select 1;"),
        (
            "function",
            "create function f() returns int language sql as $$select 1$$;",
        ),
        ("procedure", "create procedure p() language sql as $$select 1$$;"),
        ("enum", "create type e as enum ('a','b');"),
    ] {
        let e = one(&format!("{sp}{ddl}"));
        assert_eq!(
            e.schema_path.schemas().collect::<Vec<_>>(),
            vec!["app", "shared"],
            "{label} lost its schema path"
        );
        assert!(e.schema_path.stated(), "{label}");
    }
}

#[test]
fn every_entity_in_a_multi_entity_file_gets_it() {
    let p = parse_sql("set search_path to app;\ncreate table a (id int);\ncreate table b (id int);").unwrap();
    assert_eq!(p.entities.len(), 2);
    for e in &p.entities {
        assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["app"], "{}", e.name);
    }
}

// ── PostgreSQL: the path the file did NOT state ─────────────────────────────

/// The distinction that did not exist before. Both cases used to produce
/// `["public"]` with nothing to tell them apart.
#[test]
fn an_unstated_path_is_marked_as_dbds_own() {
    let stated = one("set search_path to public;\ncreate table t (id int);");
    let unstated = one("create table t (id int);");

    assert!(
        stated.schema_path.stated(),
        "the file wrote `SET search_path TO public`"
    );
    assert!(!unstated.schema_path.stated(), "the file wrote nothing at all");
}

/// And the unstated default is Postgres's real one, not a convenient half of
/// it. Verified against a live server: `show search_path` → `"$user", public`.
#[test]
fn the_unstated_default_is_the_one_postgres_actually_uses() {
    let e = one("create table t (id int);");
    assert_eq!(
        e.schema_path.entries,
        vec![PathEntry::CurrentUser, PathEntry::Schema("public".into())],
        "a bare name resolves against the role's own schema FIRST, then public"
    );
}

// ── `$user` is not a schema name ────────────────────────────────────────────

/// The bug: a file writing Postgres's own default produced a reference to a
/// schema called `$user`, which cannot exist.
#[test]
fn the_current_user_placeholder_is_never_used_as_a_schema() {
    let e = one("set search_path to \"$user\", public;\ncreate view v as select * from lookup;");
    assert_eq!(
        e.schema_path.entries,
        vec![PathEntry::CurrentUser, PathEntry::Schema("public".into())]
    );
    assert!(
        !e.refs.iter().any(|r| r.name.contains("$user")),
        "a reference was qualified with `$user`: {:?}",
        e.refs.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
}

/// `schemas()` yields only the entries that name one, so a caller walking the
/// path for candidates never has to filter the placeholder out itself.
#[test]
fn schemas_skips_the_placeholder() {
    let e = one("set search_path to \"$user\", app, public;\ncreate table t (id int);");
    assert_eq!(e.schema_path.schemas().collect::<Vec<_>>(), vec!["app", "public"]);
    assert!(
        e.schema_path.entries.contains(&PathEntry::CurrentUser),
        "but it is still recorded — a caller with a connection can resolve it"
    );
}

// ── T-SQL and MySQL: `USE db` ───────────────────────────────────────────────

/// 241 entities in the measured corpus carried no catalog because this was
/// ignored, so the same table name in two databases merged into one entity.
#[test]
fn tsql_use_sets_the_catalog_for_what_follows() {
    let p = parse_sql_as(Dialect::TSql, "USE Ethico;\nGO\nCREATE TABLE dbo.Issues (Id int);").unwrap();
    let e = &p.entities[0];
    assert_eq!(e.catalog.as_deref(), Some("Ethico"));
    assert_eq!(e.name, "dbo.Issues");
    assert_eq!(e.qualified_key(), "Ethico.dbo.Issues");
}

#[test]
fn tsql_use_accepts_the_bracketed_form() {
    let p = parse_sql_as(Dialect::TSql, "USE [Ethico Reports];\nGO\nCREATE TABLE dbo.T (Id int);").unwrap();
    assert_eq!(p.entities[0].catalog.as_deref(), Some("Ethico Reports"));
}

/// The collision this prevents.
#[test]
fn the_same_table_under_two_use_statements_is_two_entities() {
    let p = parse_sql_as(
        Dialect::TSql,
        "USE Alpha;\nGO\nCREATE TABLE dbo.Issues (Id int);\nGO\nUSE Beta;\nGO\nCREATE TABLE dbo.Issues (Id int);",
    )
    .unwrap();
    let keys: Vec<String> = p.entities.iter().map(|e| e.qualified_key()).collect();
    assert_eq!(keys, vec!["Alpha.dbo.Issues", "Beta.dbo.Issues"]);
}

/// A three-part name states its own database and must win over the `USE`.
#[test]
fn an_explicit_database_beats_the_use_statement() {
    let p = parse_sql_as(Dialect::TSql, "USE Alpha;\nGO\nCREATE TABLE Beta.dbo.Issues (Id int);").unwrap();
    assert_eq!(p.entities[0].catalog.as_deref(), Some("Beta"));
}

#[test]
fn mysql_use_sets_the_catalog_too() {
    let p = parse_sql_as(Dialect::MySql, "USE shop;\nCREATE TABLE users (id INT);").unwrap();
    assert_eq!(p.entities[0].catalog.as_deref(), Some("shop"));
    assert_eq!(p.entities[0].qualified_key(), "shop.users");
}

#[test]
fn no_use_statement_means_no_catalog() {
    let p = parse_sql_as(Dialect::MySql, "CREATE TABLE users (id INT);").unwrap();
    assert_eq!(
        p.entities[0].catalog, None,
        "the connection decides, and no file states it"
    );
}

/// A `USE` inside a string literal is not a `USE`.
///
/// This is the one corpus file the reader does not take a catalog from, and
/// **that is the correct answer** rather than a gap to close. Both of its
/// occurrences are dynamic SQL:
///
/// - `USE [?]` — the `sp_MSforeachdb` idiom, where `?` stands for *every*
///   database in turn. There is no single catalog to name.
/// - `USE ' + @db + '` — the database is a runtime variable.
///
/// The lexer strips string literals, so no `use` token is produced and nothing
/// is invented. Pinned because the temptation on seeing "1 file missed" is to
/// go looking inside strings, which would mint a database called `?`.
///
/// Corpus accounting, which closes exactly: 77 files carry a `USE` line — 13
/// declare nothing to attach a catalog to, 63 get one, and this is the 1.
#[test]
fn a_use_inside_dynamic_sql_mints_no_database() {
    let foreachdb = parse_sql_as(
        Dialect::TSql,
        "EXEC sp_MSforeachdb N'\n  USE [?];\n  CREATE TABLE dbo.Audit (Id int);\n';",
    )
    .unwrap();
    assert!(
        foreachdb.entities.iter().all(|e| e.catalog.is_none()),
        "`?` is a placeholder for every database, not the name of one: {:?}",
        foreachdb.entities.iter().map(|e| &e.catalog).collect::<Vec<_>>()
    );

    let concatenated = parse_sql_as(
        Dialect::TSql,
        "DECLARE @sql nvarchar(max) = 'USE ' + @db + '; CREATE TABLE dbo.T (Id int);';\nEXEC(@sql);",
    )
    .unwrap();
    assert!(
        concatenated.entities.iter().all(|e| e.catalog.is_none()),
        "the database is a runtime variable and cannot be known statically"
    );
}

/// Neither dialect has a search_path, and inventing one would be a claim the
/// source never made. The path is empty and unstated.
#[test]
fn the_statement_head_dialects_state_no_schema_path() {
    for d in [Dialect::TSql, Dialect::MySql] {
        let p = parse_sql_as(d, "CREATE TABLE t (id int);").unwrap();
        assert!(p.entities[0].schema_path.entries.is_empty(), "{d:?}");
        assert!(!p.entities[0].schema_path.stated(), "{d:?}");
    }
}

// ── The file-level answer agrees with the entity-level one ──────────────────

#[test]
fn the_parsed_file_reports_the_same_path_as_its_entities() {
    let p = parse_sql("set search_path to app, shared;\ncreate table t (id int);").unwrap();
    assert_eq!(p.schema_path, p.entities[0].schema_path);
}
