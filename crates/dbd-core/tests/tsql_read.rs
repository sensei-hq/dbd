//! Reading T-SQL — declarations, references, and which is which.
//!
//! The reader is a lexer and a statement-head walk, because dbd measured every
//! off-the-shelf parser against a 2,154-file corpus and none can read the
//! statements that matter: `MsSqlDialect` loses 99% of `CREATE PROCEDURE` and
//! 95% of `CREATE TABLE`.
//!
//! What it produces is identity and edges — **not** `table_def`. It reads
//! statement heads, not column lists, so `reconcile` cannot run on T-SQL. That
//! is the same position SQLite is in and is stated rather than discovered.

use dbd_core::entity::EntityType;
use dbd_core::parser::{Dialect, FileKind, parse_sql_as};

fn read(sql: &str) -> dbd_core::parser::ParsedFile {
    parse_sql_as(Dialect::TSql, sql).expect("the T-SQL reader does not fail on input")
}

fn names(p: &dbd_core::parser::ParsedFile) -> Vec<String> {
    p.entities.iter().map(|e| e.name.clone()).collect()
}

// ── The statement every other parser loses ──────────────────────────────────

/// 880 of 885 `CREATE PROCEDURE` batches in the corpus defeat `MsSqlDialect`,
/// because of exactly this: parameters with no parentheses.
#[test]
fn a_procedure_with_parenless_parameters_is_declared() {
    let p = read(
        "CREATE PROCEDURE [dbo].[sp_NewIssue]\n\
             @Id int,\n\
             @Name nvarchar(50)\n\
         AS\n\
         BEGIN\n\
             SELECT * FROM [dbo].[Issues]\n\
         END",
    );
    assert_eq!(names(&p), vec!["dbo.sp_NewIssue"]);
    assert_eq!(p.entities[0].entity_type, EntityType::Procedure);
    assert_eq!(p.kind, FileKind::Declaration);
    assert!(
        p.entities[0].reads().any(|r| r.name == "dbo.Issues"),
        "the body's table must be an edge, got {:?}",
        p.entities[0].reads().collect::<Vec<_>>()
    );
}

#[test]
fn each_statement_head_declares_the_kind_it_names() {
    for (sql, expected) in [
        ("CREATE TABLE dbo.T (Id int)", EntityType::Table),
        ("CREATE VIEW dbo.V AS SELECT 1", EntityType::View),
        ("CREATE PROCEDURE dbo.P AS SELECT 1", EntityType::Procedure),
        ("CREATE PROC dbo.P2 AS SELECT 1", EntityType::Procedure),
        (
            "CREATE FUNCTION dbo.F() RETURNS int AS BEGIN RETURN 1 END",
            EntityType::Function,
        ),
        (
            "CREATE TRIGGER dbo.Tr ON dbo.T AFTER INSERT AS SELECT 1",
            EntityType::Trigger,
        ),
    ] {
        let p = read(sql);
        assert_eq!(p.entities.first().map(|e| e.entity_type), Some(expected), "{sql}");
    }
}

// ── ALTER declares, or refers, depending on the object ──────────────────────

/// `ALTER PROCEDURE` carries the complete body — T-SQL's syntax requires it —
/// so a file whose only statement is one DECLARES the procedure. Measured at
/// 271 files in the corpus, so reading it as a mere change would lose them.
#[test]
fn alter_procedure_declares_because_it_carries_the_whole_body() {
    let p = read("ALTER PROCEDURE dbo.sp_X AS BEGIN SELECT * FROM dbo.Issues END");
    assert_eq!(names(&p), vec!["dbo.sp_X"]);
    assert_eq!(p.kind, FileKind::Declaration);
    assert!(p.entities[0].reads().any(|r| r.name == "dbo.Issues"));
}

/// `ALTER TABLE` never carries a definition — it is an edit to a table defined
/// elsewhere. Measured: it outnumbers `CREATE TABLE` 159 to 101, so reading it
/// as a declaration would mint a duplicate for the commonest statement there
/// is.
#[test]
fn alter_table_refers_because_it_never_carries_one() {
    let p = read("ALTER TABLE dbo.Issues ADD Closed bit NULL");
    assert!(
        p.entities.is_empty(),
        "a change script declares nothing: {:?}",
        names(&p)
    );
    assert_eq!(p.kind, FileKind::Migration);
}

#[test]
fn alter_view_and_alter_function_declare_too() {
    assert_eq!(read("ALTER VIEW dbo.V AS SELECT 1").kind, FileKind::Declaration);
    assert_eq!(
        read("ALTER FUNCTION dbo.F() RETURNS int AS BEGIN RETURN 1 END").kind,
        FileKind::Declaration
    );
}

/// `CREATE OR ALTER` declares whether or not one was there. 194 batches in the
/// corpus.
#[test]
fn create_or_alter_declares() {
    let p = read("CREATE OR ALTER PROCEDURE dbo.sp_X AS SELECT 1");
    assert_eq!(names(&p), vec!["dbo.sp_X"]);
    assert_eq!(p.kind, FileKind::Declaration);
}

/// A redeploy script names the object twice. One object, one node.
#[test]
fn a_drop_then_create_redeploy_declares_the_object_once() {
    let p = read(
        "IF EXISTS (SELECT 1 FROM sys.objects WHERE name = 'sp_X')\n\
         DROP PROCEDURE dbo.sp_X\n\
         GO\n\
         CREATE PROCEDURE dbo.sp_X AS SELECT 1\n\
         GO",
    );
    assert_eq!(names(&p), vec!["dbo.sp_X"], "declared once, not twice");
    assert_eq!(
        p.kind,
        FileKind::Declaration,
        "the DROP names the object this file declares — it is the redeploy \
         idiom, not a change to something else"
    );
}

// ── References ──────────────────────────────────────────────────────────────

/// The read/write split, which is the thing an embedder cannot get from any
/// other language's indexer.
#[test]
fn reads_and_writes_are_kept_apart() {
    let p = read(
        "CREATE PROCEDURE dbo.sp_Sync AS\n\
         BEGIN\n\
           INSERT INTO dbo.Target SELECT * FROM dbo.Source\n\
           UPDATE dbo.Audit SET n = 1\n\
         END",
    );
    let e = &p.entities[0];
    assert!(
        e.reads().any(|r| r.name == "dbo.Source"),
        "reads: {:?}",
        e.reads().collect::<Vec<_>>()
    );
    assert!(
        e.writes().any(|r| r.name == "dbo.Target"),
        "writes: {:?}",
        e.writes().collect::<Vec<_>>()
    );
    assert!(
        e.writes().any(|r| r.name == "dbo.Audit"),
        "writes: {:?}",
        e.writes().collect::<Vec<_>>()
    );
    assert!(!e.reads().any(|r| r.name == "dbo.Target"), "a write is not a read");
}

#[test]
fn a_join_is_a_read_and_a_foreign_key_names_its_target() {
    let p = read("CREATE VIEW dbo.V AS SELECT * FROM dbo.A JOIN dbo.B ON A.id = B.id");
    let reads: Vec<&str> = p.entities[0].reads().map(|r| r.name.as_str()).collect();
    assert!(reads.contains(&"dbo.A"), "{reads:?}");
    assert!(reads.contains(&"dbo.B"), "{reads:?}");

    let p = read("CREATE TABLE dbo.Orders (UserId int REFERENCES dbo.Users(Id))");
    assert!(
        p.entities[0].refers_to("dbo.Users"),
        "an FK must be an edge: {:?}",
        p.entities[0].refers().collect::<Vec<_>>()
    );
}

/// T-SQL REQUIRES a scalar UDF to be schema-qualified and a built-in never is,
/// so the qualification is the whole distinction — read off the grammar rather
/// than a list of built-in names that would go stale.
#[test]
fn a_qualified_call_is_an_edge_and_a_bare_one_is_a_builtin() {
    let p = read(
        "CREATE PROCEDURE dbo.sp_X AS\n\
         SELECT dbo.fnCalc(1), GETDATE(), ISNULL(a, 0) FROM dbo.T",
    );
    let e = &p.entities[0];
    assert!(
        e.refers_to("dbo.fnCalc"),
        "the qualified call is an edge: {:?}",
        e.refers().collect::<Vec<_>>()
    );
    for builtin in ["GETDATE", "ISNULL"] {
        assert!(
            !e.refers().collect::<Vec<_>>().iter().any(|r| r.contains(builtin)),
            "a bare call is a built-in, not an edge: {:?}",
            e.refers().collect::<Vec<_>>()
        );
    }
}

/// A reference belongs to the most recent declaration in its batch. A T-SQL
/// procedure body runs to the end of its batch, so that is the owner rather
/// than a guess.
#[test]
fn a_reference_belongs_to_the_procedure_it_is_inside() {
    let p = read(
        "CREATE PROCEDURE dbo.First AS SELECT * FROM dbo.A\n\
         GO\n\
         CREATE PROCEDURE dbo.Second AS SELECT * FROM dbo.B\n\
         GO",
    );
    let first = p.entities.iter().find(|e| e.name == "dbo.First").unwrap();
    let second = p.entities.iter().find(|e| e.name == "dbo.Second").unwrap();
    assert_eq!(
        first.reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["dbo.A"],
        "First must not see B's table"
    );
    assert_eq!(
        second.reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["dbo.B"]
    );
}

/// A temp table is not an object anything else can reference.
#[test]
fn a_temp_table_is_not_a_reference() {
    let p = read("CREATE PROCEDURE dbo.sp_X AS INSERT INTO #staging SELECT * FROM dbo.Real");
    let e = &p.entities[0];
    assert!(
        !e.writes().any(|r| r.name.contains("staging")),
        "a #temp must not become a table: {:?}",
        e.writes().collect::<Vec<_>>()
    );
    assert!(e.reads().any(|r| r.name == "dbo.Real"));
}

// ── The catalog level ───────────────────────────────────────────────────────

/// A three-part name keeps its database. Dropping it would merge
/// `OtherDb.dbo.Users` with the local `dbo.Users` — the silent merge the
/// catalog level exists to prevent.
#[test]
fn a_three_part_name_keeps_its_database() {
    let p = read("CREATE PROCEDURE dbo.sp_X AS SELECT * FROM OtherDb.dbo.Users");
    assert_eq!(
        p.entities[0].reads().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["OtherDb.dbo.Users"],
        "the database must survive into the edge"
    );

    let p = read("CREATE TABLE ReportDb.dbo.Summary (Id int)");
    assert_eq!(p.entities[0].catalog.as_deref(), Some("ReportDb"));
    assert_eq!(p.entities[0].name, "dbo.Summary", "name stays two-part");
}

// ── Not declaring things that were not declared ─────────────────────────────

#[test]
fn a_commented_out_declaration_is_not_one() {
    let p = read("-- CREATE PROCEDURE dbo.sp_Ghost AS SELECT 1\nCREATE TABLE dbo.Real (Id int)");
    assert_eq!(names(&p), vec!["dbo.Real"]);
}

#[test]
fn a_pure_data_script_declares_nothing() {
    let p = read("INSERT INTO dbo.Lookup (Id, Name) VALUES (1, 'a')\nGO");
    assert!(p.entities.is_empty());
    assert_eq!(p.kind, FileKind::Data);
}

/// A head that names nothing declares nothing — and must not panic reaching
/// past the end of the token stream.
#[test]
fn a_statement_head_with_no_name_declares_nothing() {
    for sql in ["CREATE PROCEDURE", "CREATE", "ALTER", "ALTER TABLE", "DROP"] {
        let p = read(sql);
        assert!(p.entities.is_empty(), "{sql:?} declared {:?}", names(&p));
    }
}

/// A truncated *name*, though, is still a name. The lexer's contract is that
/// an unterminated quote returns what was read — the honest answer for a
/// truncated file — so `CREATE TABLE [unclosed` declares `unclosed`.
///
/// Recording this rather than suppressing it: a reader that silently dropped
/// the declaration would hide the truncation, and the caller would see a file
/// that declares nothing instead of one that declares something odd.
#[test]
fn a_truncated_name_is_still_declared_as_what_was_read() {
    let p = read("CREATE TABLE [unclosed");
    assert_eq!(names(&p), vec!["unclosed"]);
    assert_eq!(p.entities[0].entity_type, EntityType::Table);
}

/// The reader reports the dialect it read as, and never a `table_def` — it
/// reads statement heads, not column lists, and claiming structure it does not
/// have is what would let `reconcile` run on a model that is not there.
#[test]
fn a_tsql_entity_carries_no_table_def() {
    let p = read("CREATE TABLE dbo.T (Id int NOT NULL, Name nvarchar(50))");
    assert_eq!(p.dialect, Dialect::TSql);
    assert!(
        p.entities[0].table_def.is_none(),
        "a statement-head reader has no columns to offer, and must not pretend"
    );
}
