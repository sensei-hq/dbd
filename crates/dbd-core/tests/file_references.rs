//! References that belong to the file rather than to anything it declares.
//!
//! # Why this exists
//!
//! The statement-head walk attributes every reference to the most recent
//! declaration in its batch, and dropped any reference made before there was
//! one. The reasoning was sound as far as it went — attaching a reference to
//! whatever happens to be declared next would fabricate an edge — but the
//! conclusion did not follow. "It belongs to the file" is a third answer, and
//! dbd had nowhere to put it.
//!
//! Measured over a 2,154-file T-SQL corpus (issue #21): `refer()` was called
//! **43,754** times and **20,929 of those (47.8%) were dropped for want of an
//! owner**. Two-thirds of the loss was 278 pure data scripts, where the
//! references *are* the entire content of the file — an `INSERT INTO a SELECT
//! FROM b` that dbd reported as nothing whatsoever.
//!
//! An independent reader over the same corpus found 43,737 references, within
//! 0.04% of what dbd's walk already saw. Nothing was being missed; it was being
//! thrown away at the last step.
//!
//! # What it recovered
//!
//! 20,929 is an *occurrence* count; these are deduplicated per file, as
//! entity-level references already were. Over the same corpus that is **4,059
//! file-level references**, taking the total from 10,695 to 14,754 (+38%) — and
//! **817 of 2,154 files (38%) went from reporting nothing at all to reporting
//! something**. That last number is the one worth caring about: those files were
//! invisible.
//!
//! # What this does NOT do
//!
//! It does not attach anything to an entity. A file-level reference is reported
//! as the file's, which is what it is.

use dbd_core::parser::{Dialect, parse_sql_as};

fn read(dialect: Dialect, sql: &str) -> dbd_core::parser::ParsedFile {
    parse_sql_as(dialect, sql).expect("the reader does not fail on input")
}

// ── A script that declares nothing still says something ─────────────────────

/// The headline case: two-thirds of the measured loss.
#[test]
fn a_pure_data_script_reports_what_it_touched() {
    let p = read(Dialect::TSql, "INSERT INTO dbo.Target (id) SELECT id FROM dbo.Source;");
    assert!(p.entities.is_empty(), "a data script declares nothing");
    assert_eq!(p.references.writes, vec!["dbo.Target"]);
    assert_eq!(p.references.reads, vec!["dbo.Source"]);
}

#[test]
fn a_migration_script_reports_the_table_it_changes() {
    let p = read(Dialect::TSql, "ALTER TABLE dbo.Issues ADD Archived bit NOT NULL;");
    assert!(p.entities.is_empty());
    assert_eq!(p.references.writes, vec!["dbo.Issues"]);
}

/// A read-only script — the `FileKind::Empty` case, 109 files in the corpus.
#[test]
fn a_bare_select_reports_what_it_read() {
    let p = read(Dialect::TSql, "SELECT * FROM dbo.Issues JOIN dbo.Users ON 1=1;");
    assert!(p.entities.is_empty());
    assert_eq!(p.references.reads, vec!["dbo.Issues", "dbo.Users"]);
}

#[test]
fn a_qualified_call_outside_a_declaration_is_a_file_level_call() {
    let p = read(Dialect::TSql, "SELECT dbo.fnFormat(@x);");
    assert_eq!(p.references.calls, vec!["dbo.fnFormat"]);
}

#[test]
fn an_exec_outside_a_declaration_is_a_file_level_call() {
    let p = read(Dialect::TSql, "EXEC dbo.sp_Rebuild;");
    assert_eq!(p.references.calls, vec!["dbo.sp_Rebuild"]);
}

// ── What a declaration owns still belongs to the declaration ────────────────

/// The property the old behaviour was protecting. It must not regress: a
/// reference a procedure makes is the procedure's, not the file's.
#[test]
fn a_declarations_own_references_do_not_leak_to_the_file() {
    let p = read(
        Dialect::TSql,
        "CREATE PROCEDURE dbo.sync AS\n\
         BEGIN\n\
           INSERT INTO dbo.Target SELECT * FROM dbo.Source;\n\
         END",
    );
    assert_eq!(p.entities.len(), 1);
    assert_eq!(
        p.entities[0].reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["dbo.Source"]
    );
    assert_eq!(
        p.entities[0].writes().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["dbo.Target"]
    );
    assert!(
        p.references.reads.is_empty() && p.references.writes.is_empty(),
        "the procedure's references are its own, not the file's: {:?}",
        p.references
    );
}

/// A mixed file splits: the batch that declares keeps its references, the
/// batch that does not gives its to the file.
#[test]
fn a_mixed_file_splits_by_batch() {
    let p = read(
        Dialect::TSql,
        "CREATE PROCEDURE dbo.sync AS\n\
           SELECT * FROM dbo.Source;\n\
         GO\n\
         INSERT INTO dbo.Audit (n) VALUES (1);\n\
         GO",
    );
    assert_eq!(p.entities.len(), 1);
    assert_eq!(
        p.entities[0].reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["dbo.Source"]
    );
    assert_eq!(
        p.references.writes,
        vec!["dbo.Audit"],
        "the second batch declares nothing, so its write is the file's"
    );
    assert!(p.references.reads.is_empty());
}

// ── Nothing invented, nothing doubled ───────────────────────────────────────

#[test]
fn a_file_level_reference_is_recorded_once() {
    let p = read(
        Dialect::TSql,
        "SELECT * FROM dbo.Issues;\nSELECT * FROM dbo.Issues;\nSELECT * FROM dbo.Issues;",
    );
    assert_eq!(p.references.reads, vec!["dbo.Issues"]);
}

/// A declaration that *does* refer to something — so this fails if owned
/// references leak into the file set, rather than passing because there was
/// nothing to leak.
#[test]
fn a_declaration_only_file_has_no_file_level_references() {
    let p = read(
        Dialect::TSql,
        "CREATE TABLE dbo.Orders (\n\
           id int NOT NULL,\n\
           UserId int NOT NULL REFERENCES dbo.Users(id)\n\
         );",
    );
    assert_eq!(p.entities.len(), 1);
    assert_eq!(
        p.entities[0].reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["dbo.Users"],
        "precondition: the declaration makes a reference that could leak"
    );
    assert!(p.references.is_empty(), "but it is the table's, not the file's");
}

#[test]
fn an_empty_file_refers_to_nothing() {
    let p = read(Dialect::TSql, "-- just a comment\n");
    assert!(p.references.is_empty());
}

// ── The same walk, so the same answer for MySQL ─────────────────────────────

#[test]
fn mysql_reports_file_level_references_too() {
    let p = read(Dialect::MySql, "INSERT INTO `target` SELECT * FROM `source`;");
    assert!(p.entities.is_empty());
    assert_eq!(p.references.writes, vec!["target"]);
    assert_eq!(p.references.reads, vec!["source"]);
}

/// MySQL's `a.b` is `database.object`, and a file-level reference keeps it —
/// the same rule the entity-level one follows.
#[test]
fn a_mysql_file_level_reference_keeps_its_database() {
    let p = read(Dialect::MySql, "INSERT INTO crm.contacts SELECT * FROM shop.users;");
    assert_eq!(p.references.writes, vec!["crm.contacts"]);
    assert_eq!(p.references.reads, vec!["shop.users"]);
}
