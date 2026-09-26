//! Emitting a PostgreSQL project as another engine's DDL (#23).
//!
//! # The rule these tests encode
//!
//! A construct the target cannot express is **downgraded, not refused** — the
//! output is always a complete schema — and every *lossy* downgrade is
//! reported twice: as a comment at the site in the emitted file, and in a
//! machine-readable list.
//!
//! The second half matters as much as the first. If nothing is refused, the
//! report is the only safeguard against DDL that applies cleanly and means
//! something different. So a downgrade that is emitted and not reported is the
//! bug to hunt for, and several tests below exist only to catch it.
//!
//! The converse is equally load-bearing: a **faithful** mapping must NOT be
//! reported. MySQL has a native `ENUM`, so emitting one loses nothing — listing
//! it would pad the report until nobody reads the entries that matter.
//!
//! # Only PostgreSQL can be the source
//!
//! `ParserChoice::produces_structure()` is true for libpg_query alone; the
//! statement-head readers know a table's name and not one of its columns. So
//! this is one-directional by construction, and asking to emit *from* a T-SQL
//! project is refused rather than silently emitting nothing.

use dbd_core::Design;
use dbd_core::emit_dialect::{Downgrade, emit_schema};
use dbd_core::parser::Dialect;
use std::path::Path;

/// A PostgreSQL project exercising the constructs that map differently.
fn project(dir: &Path) -> Design {
    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: emitted\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = dir.join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("orders.ddl"),
        "set search_path to app;\n\
         create table if not exists orders (\n  \
           id        integer primary key\n, \
           code      varchar(20) not null unique\n, \
           total     numeric(10,2) not null default 0\n, \
           tags      text[]\n, \
           payload   jsonb not null default '{}'\n, \
           created   timestamptz not null default now()\n, \
           note      text\n\
         );\n\
         create index orders_code_idx on orders (code);\n",
    )
    .unwrap();
    let v = dir.join("ddl/view/app");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("recent.ddl"),
        "set search_path to app;\ncreate or replace view recent as select id, code from orders;\n",
    )
    .unwrap();
    Design::from_config_with_dir(&dir.join("design.yaml"), "dev", Some(dir)).expect("load")
}

fn emit(dialect: Dialect) -> (String, Vec<Downgrade>) {
    let tmp = tempfile::tempdir().unwrap();
    let design = project(tmp.path());
    emit_schema(&design, dialect, None).expect("emits")
}

// ── The schema comes out whole ──────────────────────────────────────────────

#[test]
fn every_target_emits_the_tables_and_views() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, _) = emit(dialect);
        assert!(sql.to_lowercase().contains("create table"), "{dialect:?}: {sql}");
        assert!(sql.contains("orders"), "{dialect:?} lost the table: {sql}");
        assert!(sql.contains("recent"), "{dialect:?} lost the view: {sql}");
    }
}

/// Tables before the views that read them — an emitted script that cannot be
/// run in order is not a schema.
#[test]
fn a_view_is_emitted_after_the_table_it_reads() {
    let (sql, _) = emit(Dialect::MySql);
    let table = sql.find("orders").expect("table");
    let view = sql.find("recent").expect("view");
    assert!(table < view, "the view must follow its table:\n{sql}");
}

// ── Types are translated, not copied ────────────────────────────────────────

#[test]
fn postgres_types_become_the_targets_own() {
    let (mysql, _) = emit(Dialect::MySql);
    assert!(mysql.contains("JSON"), "jsonb -> JSON: {mysql}");
    assert!(!mysql.contains("jsonb"), "and the Postgres spelling is gone: {mysql}");

    let (tsql, _) = emit(Dialect::TSql);
    assert!(tsql.contains("nvarchar"), "text/varchar -> nvarchar: {tsql}");
    assert!(!tsql.to_lowercase().contains("timestamptz"), "{tsql}");

    let (sqlite, _) = emit(Dialect::Sqlite);
    assert!(sqlite.contains("TEXT"), "{sqlite}");
}

// ── Lossy downgrades are reported; faithful ones are not ────────────────────

/// An array has no equivalent in any of the three, so all three must report it.
#[test]
fn an_array_column_is_downgraded_and_reported_everywhere() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, report) = emit(dialect);
        assert!(
            report.iter().any(|d| d.column.as_deref() == Some("tags")),
            "{dialect:?}: an array column must be reported: {report:?}"
        );
        assert!(
            sql.contains("-- dbd:") || sql.contains("/* dbd:"),
            "{dialect:?}: and the file must say so at the site: {sql}"
        );
    }
}

/// MySQL has a native `ENUM`, so emitting one loses nothing and must not be
/// reported. A report padded with faithful mappings is a report nobody reads.
#[test]
fn a_faithful_mapping_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("design.yaml"),
        "project:\n  name: e\n\nsource:\n  dialect: postgresql\n  search_path: [app]\n\nschemas:\n  - app\n",
    )
    .unwrap();
    let t = tmp.path().join("ddl/table/app");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("plain.ddl"),
        "set search_path to app;\n\
         create table if not exists plain (id integer primary key, name varchar(20) not null);\n",
    )
    .unwrap();
    let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();

    let (_, report) = emit_schema(&design, Dialect::MySql, None).expect("emits");
    assert!(
        report.is_empty(),
        "integer and varchar map faithfully to MySQL; nothing to report: {report:?}"
    );
}

/// Every report entry names where it happened, so it can be acted on.
#[test]
fn a_report_entry_says_what_was_lost_and_where() {
    let (_, report) = emit(Dialect::TSql);
    let tags = report
        .iter()
        .find(|d| d.column.as_deref() == Some("tags"))
        .expect("the array downgrade");
    assert!(!tags.entity.is_empty(), "the entity: {tags:?}");
    assert!(!tags.from.is_empty() && !tags.to.is_empty(), "from and to: {tags:?}");
    assert!(
        tags.reason.len() > 20,
        "and a reason worth reading, not a label: {tags:?}"
    );
}

/// The comment is in the target's own syntax, or the file will not parse.
#[test]
fn the_downgrade_comment_is_valid_in_the_target() {
    for dialect in [Dialect::MySql, Dialect::TSql, Dialect::Sqlite] {
        let (sql, _) = emit(dialect);
        for line in sql.lines().filter(|l| l.contains("dbd:")) {
            assert!(
                line.trim_start().starts_with("--") || line.trim_start().starts_with("/*"),
                "{dialect:?}: a note must be a comment: {line}"
            );
        }
    }
}

// ── Only PostgreSQL can be the source ───────────────────────────────────────

/// A project dbd reads without structure has no columns to emit. Refusing is
/// the honest answer; emitting an empty schema would look like success.
#[test]
fn emitting_from_a_structureless_project_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("design.yaml"),
        "project:\n  name: t\n\nsource:\n  dialect: tsql\n\nschemas:\n  - dbo\n",
    )
    .unwrap();
    let t = tmp.path().join("ddl/table/dbo");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("i.ddl"), "CREATE TABLE dbo.Issues (Id int NOT NULL);").unwrap();
    let design = Design::from_config_with_dir(&tmp.path().join("design.yaml"), "dev", Some(tmp.path())).unwrap();

    let err = match emit_schema(&design, Dialect::MySql, None) {
        Ok((sql, _)) => panic!("must refuse, emitted: {sql}"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("structure") || err.contains("columns"), "{err}");
}

/// Emitting PostgreSQL from a PostgreSQL project is a no-op worth refusing —
/// `combine` already does that, and doing both invites confusion about which
/// produces the applyable script.
#[test]
fn emitting_the_source_dialect_points_at_combine() {
    let tmp = tempfile::tempdir().unwrap();
    let design = project(tmp.path());
    let err = match emit_schema(&design, Dialect::PostgreSql, None) {
        Ok(_) => panic!("must refuse"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("combine"), "it should name the command that does this: {err}");
}
