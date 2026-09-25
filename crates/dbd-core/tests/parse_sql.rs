//! `parse_sql` — the statement-level entry point for external embedders
//! (issue #19).
//!
//! `parse_entity` derives identity from the **path** (`ddl/<type>/<schema>/
//! <name>.ddl`), which is correct inside dbd's layout contract and wrong
//! outside it: an embedder with its own scanner got `EntityType::Table` named
//! after a directory, with no signal beyond `entity.errors` that the answer was
//! a fallback. Measured on a foreign corpus, `parse_entity` "succeeded" on
//! every file — reporting a stored procedure as `Table Users.sp_NewMCRIssue`.
//!
//! `parse_sql` asks the SQL what it declares instead. These tests pin that
//! contract: identity off the statement, one entity per declaration, and the
//! subordinate statements (indexes, comments) attached to the entity they
//! belong to rather than surfacing as entities of their own.

use dbd_core::entity::EntityType;
use dbd_core::parser::parse_sql;

fn one(sql: &str) -> dbd_core::Entity {
    let parsed = parse_sql(sql).expect("parse_sql must not fail on valid SQL");
    assert!(
        parsed.errors.is_empty(),
        "unexpected file-level errors: {:?}",
        parsed.errors
    );
    assert_eq!(
        parsed.entities.len(),
        1,
        "expected exactly one entity, got {:?}",
        parsed.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    parsed.entities.into_iter().next().unwrap()
}

// ── Identity from the statement ─────────────────────────────────────────────

#[test]
fn table_identity_comes_from_the_statement() {
    let e = one("create table app.users (id int primary key, name text not null);");
    assert_eq!(e.entity_type, EntityType::Table);
    assert_eq!(e.schema.as_deref(), Some("app"));
    assert_eq!(e.name, "app.users");
    assert!(e.table_def.is_some(), "a table must carry its structure");
}

/// The failure this whole ticket is about: a stored procedure read as a table
/// named after a path component.
#[test]
fn a_procedure_is_not_misread_as_a_table() {
    let e = one("create procedure app.sp_new_issue() language plpgsql as $$ begin end $$;");
    assert_eq!(e.entity_type, EntityType::Procedure);
    assert_eq!(e.name, "app.sp_new_issue");
}

#[test]
fn a_function_is_distinguished_from_a_procedure() {
    let e = one("create function app.f() returns int language sql as $$ select 1 $$;");
    assert_eq!(e.entity_type, EntityType::Function);
    assert_eq!(e.name, "app.f");
}

#[test]
fn a_matview_is_distinguished_from_a_view() {
    let view = one("create view app.v as select 1 as n;");
    assert_eq!(view.entity_type, EntityType::View);
    assert_eq!(view.name, "app.v");

    let mv = one("create materialized view app.mv as select 1 as n;");
    assert_eq!(
        mv.entity_type,
        EntityType::MaterializedView,
        "matview must not read as a view"
    );
    assert_eq!(mv.name, "app.mv");
}

#[test]
fn enum_sequence_and_role_are_identified() {
    let en = one("create type app.status as enum ('active', 'archived');");
    assert_eq!(en.entity_type, EntityType::Enum);
    assert_eq!(en.name, "app.status");
    let labels: Vec<&str> = en.enum_values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(labels, vec!["active", "archived"]);

    let seq = one("create sequence app.s start with 5;");
    assert_eq!(seq.entity_type, EntityType::Sequence);
    assert_eq!(seq.name, "app.s");

    // A role is not schema-qualified, so its name must stay bare.
    let role = one("create role app_ro;");
    assert_eq!(role.entity_type, EntityType::Role);
    assert_eq!(role.name, "app_ro");
    assert_eq!(role.schema, None, "a role has no schema");
}

/// An unqualified name is what Postgres resolves against `search_path`, so the
/// file's own `SET` decides — not `public`, and not a directory name.
#[test]
fn an_unqualified_name_is_qualified_by_the_search_path() {
    let e = one("set search_path to app;\ncreate table users (id int primary key);");
    assert_eq!(e.name, "app.users");
    assert_eq!(e.schema.as_deref(), Some("app"));
}

#[test]
fn an_unqualified_name_falls_back_to_public() {
    let e = one("create table users (id int primary key);");
    assert_eq!(e.name, "public.users");
}

// ── One entity per declaration ──────────────────────────────────────────────

#[test]
fn several_declarations_in_one_file_become_several_entities() {
    let parsed = parse_sql(
        "set search_path to app;\n\
         create table users (id int primary key);\n\
         create table orders (id int primary key, uid int references users(id));\n\
         create view recent as select * from orders;",
    )
    .unwrap();

    let names: Vec<&str> = parsed.entities.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["app.users", "app.orders", "app.recent"]);

    let types: Vec<EntityType> = parsed.entities.iter().map(|e| e.entity_type).collect();
    assert_eq!(types, vec![EntityType::Table, EntityType::Table, EntityType::View]);
}

/// An index and a comment are not entities — Postgres has no way to nest either
/// inside `CREATE TABLE`, so they arrive as siblings and must be folded into
/// the entity they name.
#[test]
fn indexes_and_comments_attach_to_their_table() {
    let parsed = parse_sql(
        "create table app.users (id int primary key, email text);\n\
         create unique index users_email_uidx on app.users (email);\n\
         comment on table app.users is 'people';",
    )
    .unwrap();

    assert_eq!(
        parsed.entities.len(),
        1,
        "index and comment must not become entities: {:?}",
        parsed.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    let td = parsed.entities[0].table_def.as_ref().expect("table_def");
    assert_eq!(td.indexes.len(), 1, "the index must land on its table");
    assert!(td.indexes[0].unique);
    assert_eq!(td.comments.table.as_deref(), Some("people"));
}

/// Statements are attributed to the table they name, not to whichever
/// declaration happens to precede them.
///
/// The index and the comment both name the **second** table. Pointing them at
/// the first would let an implementation that ignores the name entirely pass —
/// confirmed by mutation: replacing the name match with `true` leaves the
/// naive fixture green and fails this one.
#[test]
fn an_index_lands_on_the_table_it_names_not_the_first_one() {
    let parsed = parse_sql(
        "create table app.a (id int primary key, x text);\n\
         create table app.b (id int primary key, y text);\n\
         create index b_y_idx on app.b (y);\n\
         comment on table app.b is 'second';",
    )
    .unwrap();

    let a = parsed.entities.iter().find(|e| e.name == "app.a").expect("app.a");
    let b = parsed.entities.iter().find(|e| e.name == "app.b").expect("app.b");

    let a_td = a.table_def.as_ref().unwrap();
    let b_td = b.table_def.as_ref().unwrap();
    assert_eq!(b_td.indexes.len(), 1, "the index names app.b");
    assert_eq!(a_td.indexes.len(), 0, "app.a must not collect app.b's index");
    assert_eq!(b_td.comments.table.as_deref(), Some("second"));
    assert_eq!(a_td.comments.table, None, "app.a must not collect app.b's comment");
}

/// An attachment naming a table this file does not declare must be dropped,
/// not reassigned to whatever entity happens to be available.
#[test]
fn an_orphan_index_is_not_reassigned_to_a_declared_table() {
    let parsed = parse_sql(
        "create table app.t (id int primary key);\n\
         create index i on other.elsewhere (x);",
    )
    .unwrap();

    assert_eq!(parsed.entities.len(), 1);
    assert_eq!(
        parsed.entities[0].table_def.as_ref().unwrap().indexes.len(),
        0,
        "app.t must not adopt an index on other.elsewhere"
    );
}

// ── Relations ───────────────────────────────────────────────────────────────

#[test]
fn a_foreign_key_becomes_a_reference() {
    let parsed = parse_sql(
        "create table app.users (id int primary key);\n\
         create table app.orders (id int primary key, uid int references app.users(id));",
    )
    .unwrap();

    let orders = parsed.entities.iter().find(|e| e.name == "app.orders").unwrap();
    assert!(
        orders.refers.contains(&"app.users".to_string()),
        "FK must become an edge, got {:?}",
        orders.refers
    );
}

/// A constraint added by `ALTER TABLE` has to reach the entity through this
/// entry point too, or `parse_sql` reports a weaker graph than `parse_entity`
/// on the same bytes. It needs both halves: the statement grouped onto its
/// table here, and the subcommand read by the table parser.
#[test]
fn a_constraint_added_by_alter_table_reaches_the_entity() {
    let parsed = parse_sql(
        "create table app.users (id int primary key);\n\
         create table app.orders (id int primary key, uid int not null);\n\
         alter table app.orders add constraint orders_uid_fk foreign key (uid) references app.users(id);",
    )
    .unwrap();

    let orders = parsed.entities.iter().find(|e| e.name == "app.orders").unwrap();
    assert!(
        orders.refers.contains(&"app.users".to_string()),
        "an ALTER-added FK must be an edge here too, got {:?}",
        orders.refers
    );
    assert_eq!(
        orders.table_def.as_ref().unwrap().constraints.len(),
        1,
        "the constraint must reach table_def"
    );

    // And it must land on the table it names, not the first one declared.
    let users = parsed.entities.iter().find(|e| e.name == "app.users").unwrap();
    assert!(
        users.table_def.as_ref().unwrap().constraints.is_empty(),
        "app.users must not absorb app.orders' constraint"
    );
}

/// The read/write split is the thing an embedder cannot get from any other
/// language's indexer, so it has to survive this entry point.
#[test]
fn a_routine_keeps_its_reads_and_writes_separated() {
    let e = one("set search_path to app;\n\
         create procedure sync() language plpgsql as $$ begin \
         insert into target select * from source; end $$;");
    assert!(e.reads.iter().any(|r| r == "app.source"), "reads: {:?}", e.reads);
    assert!(e.writes.iter().any(|w| w == "app.target"), "writes: {:?}", e.writes);
}

// ── Failure is reported, not fabricated ─────────────────────────────────────

#[test]
fn unparseable_sql_reports_an_error_and_declares_nothing() {
    let parsed = parse_sql("THIS IS NOT SQL AT ALL ;;;").unwrap();
    assert!(parsed.entities.is_empty(), "nothing was declared");
    assert!(!parsed.errors.is_empty(), "a parse failure must be reported");
}

/// Valid SQL that declares no entity is not an error — it is an empty answer.
/// An embedder scanning a repo hits these constantly (migrations that only
/// `INSERT`, `GRANT` scripts, plain `SELECT`s).
#[test]
fn sql_that_declares_nothing_is_empty_not_an_error() {
    let parsed = parse_sql("insert into app.t (id) values (1);").unwrap();
    assert!(parsed.entities.is_empty());
    assert!(parsed.errors.is_empty(), "got: {:?}", parsed.errors);
}

// ── Corroboration against the path-derived parser ───────────────────────────

/// The **type** is `parse_sql`'s core claim, and inside dbd's layout the path
/// is an independent second opinion on it. They must agree on every fixture,
/// with no exceptions — run against real DDL rather than a constructed case.
#[test]
fn parse_sql_agrees_with_parse_entity_on_type_across_the_fixture_corpus() {
    let mut checked = 0;
    for (path, from_path, from_sql) in fixture_pairs() {
        assert!(
            from_sql.errors.is_empty(),
            "{}: parse_sql errors {:?}",
            path.display(),
            from_sql.errors
        );
        let types: Vec<EntityType> = from_sql.entities.iter().map(|e| e.entity_type).collect();
        assert!(
            types.contains(&from_path.entity_type),
            "{}: path says {:?}, statements say {:?}",
            path.display(),
            from_path.entity_type,
            types
        );
        checked += 1;
    }
    assert!(checked >= 7, "the fixture corpus should not have shrunk: {checked}");
}

/// Where the path and the statement disagree on the **name**, the divergence is
/// the point, not a defect — and it must be exactly the one dbd already knows
/// about.
///
/// `schema_model::build` records the limitation: it matches a column's type
/// against the enum entity's *file-stem* name, "not necessarily the `CREATE
/// TYPE` identifier … Works when they coincide; a stricter match is future
/// work." `enum/config/status.sql` is the fixture where they do not coincide —
/// the file declares `status_type`, the path says `status`, and `emit_enum`
/// silently emits the path's answer.
///
/// `parse_sql` is what supplies the missing half. Pinned as a set so a *new*
/// divergence fails here rather than passing under a relaxed assertion.
#[test]
fn parse_sql_recovers_the_declared_name_where_the_path_disagrees() {
    let mut divergent: Vec<(String, String, String)> = Vec::new();

    for (path, from_path, from_sql) in fixture_pairs() {
        let declared: Vec<&str> = from_sql.entities.iter().map(|e| e.name.as_str()).collect();
        if !declared.contains(&from_path.name.as_str()) {
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            divergent.push((file, from_path.name.clone(), declared.join(", ")));
        }
    }

    assert_eq!(
        divergent,
        vec![(
            "status.sql".to_string(),
            "config.status".to_string(),
            "config.status_type".to_string(),
        )],
        "the set of path-vs-statement name divergences changed"
    );
}

/// `(path, parse_entity result, parse_sql result)` for every DDL fixture.
fn fixture_pairs() -> Vec<(std::path::PathBuf, dbd_core::Entity, dbd_core::parser::ParsedFile)> {
    use dbd_core::parser::parse_entity;
    use std::path::Path;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ddl");
    walk(&root)
        .into_iter()
        .map(|path| {
            let sql = std::fs::read_to_string(&path).expect("fixture readable");
            let from_path = parse_entity(&path, &sql).expect("parse_entity");
            let from_sql = parse_sql(&sql).expect("parse_sql");
            (path, from_path, from_sql)
        })
        .collect()
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
fn the_files_search_path_is_reported() {
    let parsed = parse_sql("set search_path to app, shared;\ncreate table t (id int);").unwrap();
    assert_eq!(parsed.search_paths, vec!["app".to_string(), "shared".to_string()]);
}

// ── What KIND of file is this? ──────────────────────────────────────────────
//
// A corpus is mostly not declarations. Measured over 2,154 real T-SQL files,
// and measured independently by sensei over its own corpus, `ALTER TABLE`
// outnumbers `CREATE TABLE` 159 to 101 — so the commonest statement in a SQL
// codebase declares nothing at all.
//
// That matters to a caller building a graph. A change script that MINTED an
// identity for the table it alters would produce two nodes for one table, and
// a data script that minted one would produce a node for a table that lives
// somewhere else entirely. The kind is what lets a caller tell "this file owns
// this entity" from "this file touches it".

use dbd_core::parser::{Dialect, FileKind};

#[test]
fn a_file_of_creates_is_a_declaration() {
    let parsed = parse_sql("create table app.users (id int primary key);").unwrap();
    assert_eq!(parsed.kind, FileKind::Declaration);
}

/// The commonest shape in a real corpus, and the one that must not mint an
/// identity: it edits a table defined somewhere else.
#[test]
fn a_file_that_only_alters_is_a_migration() {
    let parsed = parse_sql(
        "alter table app.users add constraint users_email_uq unique (email);\n\
         drop index if exists app.users_old_idx;",
    )
    .unwrap();

    assert_eq!(parsed.kind, FileKind::Migration);
    assert!(
        parsed.entities.is_empty(),
        "a change script declares nothing — minting a node here doubles the table: {:?}",
        parsed.entities.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

#[test]
fn a_file_that_only_moves_data_is_data() {
    let parsed = parse_sql(
        "insert into app.lookups (id, label) values (1, 'a');\n\
         update app.settings set v = '2' where k = 'version';",
    )
    .unwrap();
    assert_eq!(parsed.kind, FileKind::Data);
    assert!(parsed.entities.is_empty());
}

/// A table file's own index and comment belong to the table it declares, so
/// they are not a change to something else. Classing this as `Mixed` would put
/// every ordinary dbd table file in the ambiguous bucket.
#[test]
fn a_declaration_with_its_own_index_and_comment_is_still_a_declaration() {
    let parsed = parse_sql(
        "create table app.users (id int primary key, email text);\n\
         create unique index users_email_uidx on app.users (email);\n\
         comment on table app.users is 'people';",
    )
    .unwrap();
    assert_eq!(parsed.kind, FileKind::Declaration);
}

/// But an ALTER naming a table this file does NOT declare is a change to
/// something else, and that makes the file mixed.
#[test]
fn a_declaration_plus_a_change_to_another_table_is_mixed() {
    let parsed = parse_sql(
        "create table app.new_thing (id int primary key);\n\
         alter table app.existing add column note text;",
    )
    .unwrap();
    assert_eq!(parsed.kind, FileKind::Mixed);
    assert_eq!(parsed.entities.len(), 1, "only the declared table is an entity");
    assert_eq!(parsed.entities[0].name, "app.new_thing");
}

#[test]
fn a_file_with_nothing_dbd_recognises_is_empty() {
    assert_eq!(parse_sql("select 1;").unwrap().kind, FileKind::Empty);
    assert_eq!(parse_sql("-- just a comment\n").unwrap().kind, FileKind::Empty);
}

/// A `SET search_path` is ambient — it applies to whatever else is in the file
/// and is not itself a statement of any kind.
#[test]
fn an_ambient_set_does_not_change_the_kind() {
    let parsed = parse_sql("set search_path to app;\ncreate table t (id int);").unwrap();
    assert_eq!(parsed.kind, FileKind::Declaration);
}

// ── Which dialect was it read as? ───────────────────────────────────────────

/// Reported so a caller never has to remember what it asked for, and so a
/// detected-but-unstated file says so rather than claiming PostgreSQL.
#[test]
fn the_dialect_the_file_was_read_as_is_reported() {
    assert_eq!(
        parse_sql("create table t (id int);").unwrap().dialect,
        Dialect::PostgreSql,
        "the default entry point states PostgreSQL"
    );
}

/// The honest answer for a file nothing identified. It was read with the
/// PostgreSQL reader because that is the fallback — but saying `PostgreSql`
/// would claim the file stated something it did not.
#[test]
fn an_unstated_file_is_reported_as_unstated_not_as_postgres() {
    use dbd_core::parser::parse_sql_as;

    let sql = "CREATE TABLE t (id int);";
    assert_eq!(Dialect::detect(sql), Dialect::Unstated, "precondition");

    let parsed = parse_sql_as(Dialect::Unstated, sql).unwrap();
    assert_eq!(parsed.dialect, Dialect::Unstated);
    assert_eq!(parsed.entities.len(), 1, "and it is still read, by the fallback reader");
}
