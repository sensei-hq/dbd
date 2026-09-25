//! The SQL tokeniser — batches, comments, quoting, and nothing else.
//!
//! Ported from sensei's `indexer::lang::sql::lex`, which was written against a
//! real T-SQL corpus. It is deliberately a *lexer* rather than a grammar: dbd
//! measured every off-the-shelf parser on the same corpus and none of them can
//! read the statements that matter — `sqlparser`'s `MsSqlDialect` loses 99% of
//! `CREATE PROCEDURE` batches and 95% of `CREATE TABLE`.
//!
//! What a reader needs from this is *which word is here*, and where a batch
//! ends. Not expressions, not types, not control flow.

use dbd_core::parser::lex::{self, Tok};

fn words(sql: &str) -> Vec<String> {
    lex::tokens(sql)
        .iter()
        .filter_map(|t| t.name().map(str::to_string))
        .collect()
}

// ── GO is not SQL ───────────────────────────────────────────────────────────

/// `GO` is a client directive — sqlcmd and SSMS split a file on it and send
/// each batch separately. A parser handed a whole file chokes on the first one.
#[test]
fn go_separates_batches() {
    let sql = "CREATE TABLE a (id int)\nGO\nCREATE TABLE b (id int)\nGO";
    let batches = lex::batches(sql);
    assert_eq!(batches.len(), 2);
    assert!(batches[0].1.contains("TABLE a"));
    assert!(batches[1].1.contains("TABLE b"));
}

/// Each batch carries the line it starts on, so a reader can point at the file
/// rather than at an offset into a fragment it was handed.
#[test]
fn a_batch_knows_which_line_it_started_on() {
    let sql = "-- header\nCREATE TABLE a (id int)\nGO\nCREATE TABLE b (id int)";
    let batches = lex::batches(sql);
    assert_eq!(batches[0].0, 1);
    assert_eq!(batches[1].0, 4, "the batch after GO starts on line 4");
}

/// `GO 5` repeats a batch. It is still a separator.
#[test]
fn go_with_a_repeat_count_still_separates() {
    let batches = lex::batches("INSERT INTO t VALUES (1)\nGO 5\nSELECT 1");
    assert_eq!(batches.len(), 2);
}

/// The failure that would read half a corpus as one batch — or as none.
/// `go` is inside `category`, `logo` and `go_live`.
#[test]
fn go_inside_a_word_is_not_a_separator() {
    let sql = "select category, logo from things where go_live = 1";
    assert_eq!(lex::batches(sql).len(), 1);
}

#[test]
fn an_empty_batch_is_not_emitted() {
    assert_eq!(lex::batches("GO\nGO\n\nGO").len(), 0, "separators with nothing between");
    assert_eq!(lex::batches("").len(), 0);
}

/// A line starting with a multi-byte character must not panic the splitter.
/// A corpus exported from SSMS is full of them.
#[test]
fn a_multibyte_line_does_not_panic_the_splitter() {
    let sql = "-- café niño\nSELECT 1\nGO\n— em-dash comment\nSELECT 2";
    assert_eq!(lex::batches(sql).len(), 2);
}

// ── A comment is not SQL ────────────────────────────────────────────────────

/// The corpus is full of commented-out statements. A table named in one is not
/// a reference, and counting it would invent an edge.
#[test]
fn line_comments_are_consumed_not_tokenised() {
    assert_eq!(words("-- select * from Secrets\nselect 1"), vec!["select"]);
}

#[test]
fn block_comments_are_consumed() {
    assert_eq!(words("/* from Secrets */ select 1"), vec!["select"]);
}

/// T-SQL allows block comments to NEST, unlike most SQL. A non-nesting reader
/// stops at the first `*/` and then lexes the rest of the comment as code.
#[test]
fn block_comments_nest() {
    assert_eq!(
        words("/* outer /* inner */ from Secrets */ select 1"),
        vec!["select"],
        "the inner close must not end the outer comment"
    );
}

// ── Quoting ─────────────────────────────────────────────────────────────────

#[test]
fn bracketed_identifiers_are_read_whole() {
    assert_eq!(
        words("select * from [Order Details]"),
        vec!["select", "from", "Order Details"]
    );
}

#[test]
fn a_doubled_bracket_is_an_escaped_bracket() {
    assert_eq!(words("from [weird]]name]"), vec!["from", "weird]name"]);
}

#[test]
fn double_quoted_identifiers_are_read_whole() {
    assert_eq!(words(r#"from "Order""#), vec!["from", "Order"]);
}

#[test]
fn a_doubled_quote_is_an_escaped_quote() {
    assert_eq!(words(r#"from "we""ird""#), vec!["from", r#"we"ird"#]);
}

/// A quoted identifier is never a keyword, however it is spelled. `[TABLE]` is
/// a table called `TABLE`, which is exactly why the source bracketed it.
#[test]
fn a_quoted_identifier_is_never_a_keyword() {
    let toks = lex::tokens("[TABLE]");
    assert!(!toks[0].is("table"), "a bracketed name must not match a keyword");
    assert_eq!(toks[0].name(), Some("TABLE"));
}

/// Non-ASCII survives quoting. Reading byte-by-byte would turn every accented
/// character into Latin-1 mojibake, and SSMS exports are where such names live.
#[test]
fn a_non_ascii_quoted_name_is_not_mangled() {
    assert_eq!(words("from [café_niño]"), vec!["from", "café_niño"]);
}

/// An unterminated quote returns what was read rather than looping. A
/// truncated file is a real thing and must not hang a scan.
#[test]
fn an_unterminated_quoted_identifier_terminates() {
    assert_eq!(words("from [never_closed"), vec!["from", "never_closed"]);
}

// ── Literals and variables are consumed, not named ──────────────────────────

#[test]
fn string_literals_are_not_names() {
    assert_eq!(words("where name = 'Users'"), vec!["where", "name"]);
}

#[test]
fn a_doubled_quote_inside_a_literal_does_not_end_it() {
    assert_eq!(
        words("where s = 'it''s from Secrets' and x = 1"),
        vec!["where", "s", "and", "x"],
        "the escaped quote must not end the literal early"
    );
}

/// `@p` is a parameter and `@@ROWCOUNT` a global. Neither is ever an object
/// name — and both must be consumed WHOLE, or their tail lexes as a bare
/// identifier and becomes a phantom table.
#[test]
fn variables_are_consumed_whole() {
    assert_eq!(words("select @@ROWCOUNT, @p from t"), vec!["select", "from", "t"]);
    assert!(matches!(lex::tokens("@p")[0], Tok::Var("@p")));
    assert!(matches!(lex::tokens("@@ROWCOUNT")[0], Tok::Var("@@ROWCOUNT")));
}

/// `#temp` and `##global` are temp tables. Consumed whole for the same reason.
#[test]
fn temp_table_names_are_consumed_whole() {
    assert_eq!(words("insert into #temp select 1"), vec!["insert", "into", "select"]);
    assert!(matches!(lex::tokens("##global")[0], Tok::Var("##global")));
}

#[test]
fn numbers_are_literals_not_names() {
    assert_eq!(words("values (1, 2.5, 0x1F)"), vec!["values"]);
}

// ── Enough structure for a statement head ───────────────────────────────────

/// The whole grammar a statement-head reader needs: a keyword, then a
/// dot-separated name.
#[test]
fn a_qualified_name_lexes_as_words_and_dots() {
    let toks = lex::tokens("CREATE PROCEDURE [dbo].[sp_X]");
    let shape: Vec<&str> = toks
        .iter()
        .map(|t| match t {
            Tok::Word(_) => "word",
            Tok::Quoted(_) => "quoted",
            Tok::Punct('.') => "dot",
            Tok::Punct(_) => "punct",
            Tok::Var(_) => "var",
            Tok::Literal => "literal",
        })
        .collect();
    assert_eq!(shape, vec!["word", "word", "quoted", "dot", "quoted"]);
}

/// The parenless parameter form that defeats every off-the-shelf parser —
/// 99% of `CREATE PROCEDURE` batches in the corpus. A lexer has no opinion
/// about it, which is the point.
#[test]
fn the_parenless_procedure_form_lexes_without_complaint() {
    let sql = "CREATE PROCEDURE dbo.sp_X @Id int, @Name nvarchar(50) AS SELECT * FROM dbo.Issues";
    let names = words(sql);
    assert!(names.contains(&"PROCEDURE".to_string()));
    assert!(
        names.contains(&"Issues".to_string()),
        "the referenced table survives: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with('@')),
        "no parameter leaked in as a name: {names:?}"
    );
}
