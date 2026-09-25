//! `Dialect` — which SQL a file is written in, stated or detected.
//!
//! The markers here are ported from sensei's `indexer::lang::sql`, where they
//! were scored against a real multi-dialect corpus. They are kept as measured
//! rather than re-derived: a marker list invented from memory recognises the
//! SQL its author happened to think of.
//!
//! The rule that makes detection safe is that it **fails closed**. `CREATE
//! TABLE t (id int)` is valid in every dialect and says nothing about which one
//! it is in, so it is `Unstated` — not a default, and not a guess.

use dbd_core::parser::Dialect;

// ── Stated ──────────────────────────────────────────────────────────────────

#[test]
fn a_label_names_a_dialect() {
    assert_eq!(Dialect::from_label("postgresql"), Some(Dialect::PostgreSql));
    assert_eq!(Dialect::from_label("postgres"), Some(Dialect::PostgreSql));
    assert_eq!(Dialect::from_label("supabase"), Some(Dialect::PostgreSql));
    assert_eq!(Dialect::from_label("sqlite"), Some(Dialect::Sqlite));
    assert_eq!(Dialect::from_label("tsql"), Some(Dialect::TSql));
    assert_eq!(Dialect::from_label("mssql"), Some(Dialect::TSql));
    assert_eq!(Dialect::from_label("mysql"), Some(Dialect::MySql));
}

/// An unknown label is `None`, never `Unstated`. "I do not know this word" and
/// "this text states no dialect" are different facts, and a caller that cannot
/// tell them apart cannot report a typo in a config.
#[test]
fn an_unknown_label_is_none_not_unstated() {
    assert_eq!(Dialect::from_label("oracle"), None);
    assert_eq!(Dialect::from_label(""), None);
}

#[test]
fn a_label_round_trips() {
    for d in [Dialect::PostgreSql, Dialect::TSql, Dialect::MySql, Dialect::Sqlite] {
        assert_eq!(
            Dialect::from_label(d.as_label()),
            Some(d),
            "{d:?} must survive label round-trip"
        );
    }
}

// ── Detected ────────────────────────────────────────────────────────────────

#[test]
fn postgres_is_detected_from_markers_only_it_writes() {
    assert_eq!(
        Dialect::detect("create function f() returns trigger language plpgsql as $$ begin end $$;"),
        Dialect::PostgreSql
    );
    assert_eq!(
        Dialect::detect("set search_path to app;\ncreate table t (id serial primary key, d jsonb);"),
        Dialect::PostgreSql
    );
}

#[test]
fn tsql_is_detected_from_markers_only_it_writes() {
    assert_eq!(
        Dialect::detect("SET ANSI_NULLS ON\nGO\nCREATE PROCEDURE [dbo].[sp_X] @p int AS SELECT 1\nGO"),
        Dialect::TSql
    );
    assert_eq!(
        Dialect::detect("create table Users (Id uniqueidentifier, Name nvarchar(50));"),
        Dialect::TSql
    );
}

/// `GO` is a batch separator on a line of its own. As a substring it is inside
/// `category`, `logo` and a hundred other words, so matching it loosely would
/// make half a corpus read as T-SQL.
#[test]
fn go_counts_as_a_batch_only_on_its_own_line() {
    assert_eq!(
        Dialect::detect("GO\nGO\nGO"),
        Dialect::TSql,
        "three batch separators is a T-SQL file"
    );
    assert_eq!(
        Dialect::detect("select category, logo, gov_id from things where go_live is not null;"),
        Dialect::Unstated,
        "`go` inside a word is not a batch separator"
    );
}

#[test]
fn mysql_and_sqlite_are_detected() {
    assert_eq!(
        Dialect::detect("create table t (id int auto_increment primary key) engine=innodb;"),
        Dialect::MySql
    );
    assert_eq!(
        Dialect::detect("create table kv (k text primary key) without rowid;"),
        Dialect::Sqlite
    );
}

/// A backtick is MySQL's identifier quote — and also what everyone writes
/// around a word in a comment. A marker has to be something only that dialect
/// *writes*.
#[test]
fn a_backtick_in_a_comment_does_not_make_a_file_mysql() {
    assert_eq!(
        Dialect::detect("-- the `id` column is the primary key\ncreate table t (id int);"),
        Dialect::Unstated
    );
}

// ── Fails closed ────────────────────────────────────────────────────────────

/// The property the whole design rests on.
#[test]
fn sql_valid_in_every_dialect_states_no_dialect() {
    assert_eq!(Dialect::detect("CREATE TABLE t (id int);"), Dialect::Unstated);
    assert_eq!(Dialect::detect("select * from users where id = 1;"), Dialect::Unstated);
    assert_eq!(Dialect::detect(""), Dialect::Unstated);
}

/// Ambiguity is not a dialect. A file scoring equally for two is `Unstated`,
/// not whichever the tie-break happened to reach first — that would make the
/// answer depend on enum declaration order.
#[test]
fn a_tie_is_unstated_rather_than_whichever_came_first() {
    // One marker each: `nvarchar` (T-SQL) and `jsonb` (Postgres).
    let tied = "create table t (a nvarchar(10), b jsonb);";
    assert_eq!(Dialect::detect(tied), Dialect::Unstated);
}

/// A clear winner still wins when a weaker signal is present — a tie is a tie,
/// not "any contamination is ambiguous".
#[test]
fn a_clear_majority_still_decides() {
    let mostly_pg = "set search_path to app;\n\
                     create function f() returns trigger language plpgsql as $$ begin end $$;\n\
                     create table t (d jsonb, n nvarchar(10));";
    assert_eq!(Dialect::detect(mostly_pg), Dialect::PostgreSql);
}

// ── The seam into the parser ────────────────────────────────────────────────

/// `ParserChoice` is which reader dbd runs; `Dialect` is what the SQL is. The
/// mapping lives in one place so a config label and a detected dialect cannot
/// select different readers for the same SQL.
#[test]
fn a_dialect_selects_the_reader_that_reads_it() {
    use dbd_core::parser::ParserChoice;

    assert_eq!(
        ParserChoice::for_dialect_typed(Dialect::PostgreSql),
        ParserChoice::PgQuery
    );
    assert_eq!(ParserChoice::for_dialect_typed(Dialect::Sqlite), ParserChoice::Verbatim);

    // Unstated has no reader of its own. Falling back to PostgreSQL is what
    // dbd already does for an unrecognised `source.dialect`, and it keeps the
    // one thing a caller can rely on: an unreadable file reports why.
    assert_eq!(
        ParserChoice::for_dialect_typed(Dialect::Unstated),
        ParserChoice::PgQuery
    );
}

/// The string path and the typed path must agree, or `source.dialect: sqlite`
/// and a detected SQLite file would be read by different parsers.
#[test]
fn the_label_path_and_the_typed_path_agree() {
    use dbd_core::parser::ParserChoice;

    for label in ["postgresql", "postgres", "supabase", "sqlite"] {
        let typed = Dialect::from_label(label).expect("known label");
        assert_eq!(
            ParserChoice::resolve(label, None).unwrap(),
            ParserChoice::for_dialect_typed(typed),
            "{label}: config and detection must select the same reader"
        );
    }
}
